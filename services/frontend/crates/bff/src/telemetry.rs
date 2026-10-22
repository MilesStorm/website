//! `POST /telemetry/collect`: the browser's Grafana Faro uploads, passed on to Alloy's
//! `faro.receiver` inside the cluster (TRACING.md, "Browser").
//!
//! The endpoint is public, so it checks what the Gateway's faro-guard checked for Alloy's own
//! public `/faro/` route: a `Content-Length` (411 without one), at most [`MAX_BODY`] (413), no
//! `Content-Encoding` (415), JSON, from this site, and at most [`PER_VISITOR`] uploads a minute per
//! visitor (429). The body must parse as a Faro payload (400).
//!
//! Error stack frames in the site's own scripts are mapped back to the source here, with the
//! build's source maps (`SOURCE_MAPS`, baked into the image and never served), so Alloy needs no
//! access to them. Only a frame whose file is exactly one of the build's `/_astro/*.js` files,
//! on this site's host, is mapped; every other frame is passed on as it came. The maps are
//! parsed on first use and kept: there are only as many as the build has files.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::Request;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Extension;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::limits::{self, Limit};
use crate::trace;

/// The largest upload taken (the Gateway's limit for Alloy's own route).
pub(crate) const MAX_BODY: usize = 64 * 1024;
/// Uploads per visitor a minute. A page sends a few batches as it loads, then one per second at
/// most while things happen; Alloy takes 10 a second from everyone together.
pub(crate) const PER_VISITOR: u32 = 60;
/// Where Alloy's `faro.receiver` listens (`FARO_COLLECT_URL`).
const DEFAULT_COLLECT_URL: &str = "http://alloy.monitoring.svc.cluster.local:12347/collect";
const FORWARD_TIMEOUT: Duration = Duration::from_secs(5);

/// The state of the endpoint: the per-visitor limit, the build's source maps and the client to
/// Alloy.
pub(crate) struct Telemetry {
    visitors: Limit,
    maps: SourceMaps,
    collect_url: String,
    client: trace::TracedClient,
}

impl Telemetry {
    /// From `SOURCE_MAPS` (default `web/sourcemaps`, as in a checkout; missing means no frame is
    /// mapped) and `FARO_COLLECT_URL`.
    pub(crate) fn from_env() -> anyhow::Result<Self> {
        let dir = std::env::var_os("SOURCE_MAPS").map_or_else(|| PathBuf::from("web/sourcemaps"), PathBuf::from);
        let url = std::env::var("FARO_COLLECT_URL").unwrap_or_else(|_| DEFAULT_COLLECT_URL.into());
        Self::new(SourceMaps::load(dir), url)
    }

    pub(crate) fn new(maps: SourceMaps, collect_url: String) -> anyhow::Result<Self> {
        let client = trace::client(trace::Peer { service: "alloy", system: None }, FORWARD_TIMEOUT, |b| b)?;
        Ok(Self { visitors: Limit::new(PER_VISITOR, Duration::from_secs(60)), maps, collect_url, client })
    }
}

/// The route, with its state.
pub(crate) fn router(telemetry: Arc<Telemetry>) -> axum::Router {
    axum::Router::new()
        .route("/telemetry/collect", axum::routing::post(collect))
        .route_layer(axum::middleware::from_fn(crate::json_api::same_origin))
        .layer(Extension(telemetry))
}

// ---- The Faro payload ----

/// A Faro upload. Only what is mapped is typed; everything else is kept as it came, so the
/// upload Alloy gets is the browser's apart from the mapped frames.
#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct Payload {
    meta: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    exceptions: Vec<Exception>,
    #[serde(flatten)]
    rest: Map<String, Value>,
}

#[derive(Debug, Deserialize, Serialize)]
struct Exception {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stacktrace: Option<Stacktrace>,
    #[serde(flatten)]
    rest: Map<String, Value>,
}

#[derive(Debug, Deserialize, Serialize)]
struct Stacktrace {
    #[serde(default)]
    frames: Vec<Frame>,
    #[serde(flatten)]
    rest: Map<String, Value>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq)]
struct Frame {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    filename: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    function: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lineno: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    colno: Option<u32>,
    #[serde(flatten)]
    rest: Map<String, Value>,
}

// ---- Source maps ----

/// Most maps kept parsed at once. The build has fewer; this only bounds a build that grows.
const MAX_PARSED: usize = 128;

/// The build's source maps: `<file>.js.map` in one directory, for the files under `/_astro/`.
pub(crate) struct SourceMaps {
    dir: PathBuf,
    /// The `.js` files that have a map, read once: no lookup reaches the disk for another name.
    files: HashSet<String>,
    parsed: Mutex<HashMap<String, Arc<sourcemap::SourceMap>>>,
}

impl SourceMaps {
    pub(crate) fn load(dir: PathBuf) -> Self {
        let files = match std::fs::read_dir(&dir) {
            Ok(entries) => entries
                .filter_map(|e| e.ok()?.file_name().into_string().ok())
                .filter_map(|name| name.strip_suffix(".map").map(str::to_owned))
                .filter(|name| name.ends_with(".js"))
                .collect(),
            Err(e) => {
                tracing::warn!(dir = %dir.display(), error = %e, "no source maps: browser errors stay unmapped");
                HashSet::new()
            }
        };
        tracing::info!(dir = %dir.display(), maps = files.len(), "source maps for browser errors");
        Self { dir, files, parsed: Mutex::new(HashMap::new()) }
    }

    /// The build's file a frame is in: `https://<host>/_astro/<file>.js` (query and fragment
    /// ignored), when `<file>.js` has a map.
    fn file_of(&self, filename: &str, host: &str) -> Option<String> {
        let url = reqwest::Url::parse(filename).ok()?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str()? != host.split(':').next()? {
            return None;
        }
        let name = url.path().strip_prefix("/_astro/")?;
        self.files.contains(name).then(|| name.to_owned())
    }

    /// The parsed map of `file`, read from disk the first time.
    async fn map(&self, file: &str) -> Option<Arc<sourcemap::SourceMap>> {
        if let Some(map) = self.parsed.lock().expect("maps lock").get(file) {
            return Some(map.clone());
        }
        let path = self.dir.join(format!("{file}.map"));
        let parse = move || {
            let bytes = std::fs::read(&path)?;
            anyhow::Ok(sourcemap::SourceMap::from_slice(&bytes)?)
        };
        let map = match trace::spawn_blocking("sourcemap.parse", parse).await {
            Ok(Ok(map)) => Arc::new(map),
            Ok(Err(e)) => {
                tracing::error!(file, error = %e, "a source map of the build can't be read");
                return None;
            }
            Err(e) => {
                tracing::error!(file, error = %e, "parsing a source map panicked");
                return None;
            }
        };
        let mut parsed = self.parsed.lock().expect("maps lock");
        if parsed.len() >= MAX_PARSED {
            parsed.clear();
        }
        parsed.insert(file.to_owned(), map.clone());
        Some(map)
    }

    /// Maps every frame in the build's files to its source; returns how many were mapped.
    async fn resolve(&self, payload: &mut Payload, host: &str) -> usize {
        let mut mapped = 0;
        for frame in payload.exceptions.iter_mut().filter_map(|e| e.stacktrace.as_mut()).flat_map(|s| s.frames.iter_mut()) {
            let (Some(filename), Some(line), Some(col)) = (frame.filename.as_deref(), frame.lineno, frame.colno) else {
                continue;
            };
            let Some(file) = self.file_of(filename, host) else { continue };
            let Some(map) = self.map(&file).await else { continue };
            // Faro's lines and columns count from 1, the map's from 0.
            let Some(token) = map.lookup_token(line.saturating_sub(1), col.saturating_sub(1)) else { continue };
            let Some(source) = token.get_source() else { continue };
            frame.filename = Some(source_path(source));
            frame.lineno = Some(token.get_src_line().saturating_add(1));
            frame.colno = Some(token.get_src_col().saturating_add(1));
            if let Some(name) = token.get_name() {
                frame.function = Some(name.to_owned());
            }
            mapped += 1;
        }
        mapped
    }
}

/// A map's source as a path in the repository: `../../src/islands/LoginForm.tsx` becomes
/// `src/islands/LoginForm.tsx`, `../../node_modules/@grafana/…` `node_modules/@grafana/…`.
fn source_path(source: &str) -> String {
    let mut path = source;
    for prefix in ["webpack://", "file://"] {
        path = path.strip_prefix(prefix).unwrap_or(path);
    }
    while let Some(rest) = path.strip_prefix("../").or_else(|| path.strip_prefix("./")) {
        path = rest;
    }
    path.to_owned()
}

// ---- The handler ----

/// What the guards refuse with: a status and a short reason (logged, and the response body).
fn refuse(status: StatusCode, reason: &'static str) -> Response {
    tracing::debug!(%status, reason, "telemetry upload refused");
    metrics::counter!("bff_telemetry_uploads_total", "status" => reason).increment(1);
    (status, reason).into_response()
}

/// Checks the upload's headers before its body is read: the refusal's status and reason.
fn check_headers(headers: &HeaderMap) -> Result<(), (StatusCode, &'static str)> {
    let Some(length) = headers.get(header::CONTENT_LENGTH) else {
        return Err((StatusCode::LENGTH_REQUIRED, "length_required"));
    };
    match length.to_str().ok().and_then(|l| l.parse::<usize>().ok()) {
        Some(n) if n <= MAX_BODY => {}
        Some(_) => return Err((StatusCode::PAYLOAD_TOO_LARGE, "too_large")),
        None => return Err((StatusCode::BAD_REQUEST, "bad_length")),
    }
    if headers.get(header::CONTENT_ENCODING).is_some_and(|e| e != "identity") {
        return Err((StatusCode::UNSUPPORTED_MEDIA_TYPE, "encoded"));
    }
    let json = headers
        .get(header::CONTENT_TYPE)
        .and_then(|t| t.to_str().ok())
        .is_some_and(|t| t.split(';').next().is_some_and(|m| m.trim().eq_ignore_ascii_case("application/json")));
    if !json {
        return Err((StatusCode::UNSUPPORTED_MEDIA_TYPE, "not_json"));
    }
    Ok(())
}

#[tracing::instrument(name = "telemetry.collect", skip_all, fields(frames_mapped))]
async fn collect(Extension(telemetry): Extension<Arc<Telemetry>>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    if let Err((status, reason)) = check_headers(&parts.headers) {
        return refuse(status, reason);
    }
    if let Some(visitor) = limits::visitor(&parts.headers) {
        let now = Instant::now();
        if telemetry.visitors.exceeded(&visitor, now) {
            return refuse(StatusCode::TOO_MANY_REQUESTS, "rate_limited");
        }
        telemetry.visitors.hit(&visitor, now);
    }
    // The length was checked, but a body may not keep to its header.
    let bytes: Bytes = match axum::body::to_bytes(body, MAX_BODY).await {
        Ok(bytes) => bytes,
        Err(_) => return refuse(StatusCode::PAYLOAD_TOO_LARGE, "too_large"),
    };
    let mut payload: Payload = match serde_json::from_slice(&bytes) {
        Ok(payload) => payload,
        Err(_) => return refuse(StatusCode::BAD_REQUEST, "not_faro"),
    };
    let host = parts.headers.get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or_default();
    let mapped = telemetry.maps.resolve(&mut payload, host).await;
    tracing::Span::current().record("frames_mapped", mapped);
    forward(&telemetry, &payload).await
}

/// Sends the upload on to Alloy; its status is the answer.
async fn forward(telemetry: &Telemetry, payload: &Payload) -> Response {
    let sent = telemetry.client.post(&telemetry.collect_url).json(payload).send().await;
    match sent {
        Ok(reply) if reply.status().is_success() => {
            metrics::counter!("bff_telemetry_uploads_total", "status" => "forwarded").increment(1);
            StatusCode::ACCEPTED.into_response()
        }
        Ok(reply) if reply.status() == reqwest::StatusCode::TOO_MANY_REQUESTS => {
            metrics::counter!("bff_telemetry_uploads_total", "status" => "collector_busy").increment(1);
            StatusCode::TOO_MANY_REQUESTS.into_response()
        }
        Ok(reply) => {
            tracing::warn!(status = reply.status().as_u16(), "the collector refused a telemetry upload");
            trace::failed("collector_refused");
            metrics::counter!("bff_telemetry_uploads_total", "status" => "collector_refused").increment(1);
            StatusCode::BAD_GATEWAY.into_response()
        }
        Err(e) => {
            tracing::warn!(error = %e, "the collector can't be reached");
            trace::failed("collector_down");
            metrics::counter!("bff_telemetry_uploads_total", "status" => "collector_down").increment(1);
            StatusCode::BAD_GATEWAY.into_response()
        }
    }
}

#[cfg(test)]
mod tests;
