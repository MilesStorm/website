//! Astro's pages (`web/dist`), served while `FRONTEND_UI=astro`; otherwise Dioxus renders every
//! page as before. Only the paths in [`api_types::PAGES`] are pages: each is its own route, so
//! its span is named after it (`GET /`). The HTML is read once at startup and split where the
//! request's data goes: into the `<html` tag (`data-session`, `data-perms`, for the CSS gates)
//! and at the `<meta name="bff-inject">` marker (the page request's `traceparent`, the
//! `service-version` and the [`Bootstrap`] JSON). Astro's hashed files under `/_astro/` are
//! served from disk and cached for good; no other file of the build is served.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use api_types::{Access, Bootstrap, PAGES, STATUS_PAGES};
use axum::extract::State;
use axum::http::header::{HeaderValue, CACHE_CONTROL, CONTENT_TYPE};
use axum::response::{IntoResponse, Response};
use axum::Router;

use crate::json_api;

/// Where the request's data goes in each page's `<head>`. An element, not a comment: Astro's
/// HTML compression may drop comments.
pub(crate) const MARKER: &str = r#"<meta name="bff-inject">"#;
const HTML_OPEN: &str = "<html";

/// Which pages the server answers with: `FRONTEND_UI=astro` or `dioxus` (the default).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ui {
    Dioxus,
    Astro,
}

impl Ui {
    /// The value of `FRONTEND_UI`; anything else stops the server from starting.
    pub fn parse(value: Option<&str>) -> anyhow::Result<Self> {
        match value.map(str::trim) {
            None | Some("") | Some("dioxus") => Ok(Self::Dioxus),
            Some("astro") => Ok(Self::Astro),
            Some(other) => anyhow::bail!("FRONTEND_UI={other:?}: expected \"astro\" or \"dioxus\""),
        }
    }
}

/// The Astro build when `FRONTEND_UI=astro`, read from `WEB_DIST` (default `web/dist`, as in a
/// checkout). With the flag off the build isn't looked at, so whatever is there can't stop the
/// server.
pub fn from_env() -> anyhow::Result<Option<Arc<Built>>> {
    match Ui::parse(std::env::var("FRONTEND_UI").ok().as_deref())? {
        Ui::Dioxus => Ok(None),
        Ui::Astro => {
            let dir = std::env::var_os("WEB_DIST").map_or_else(|| PathBuf::from("web/dist"), PathBuf::from);
            let built = Built::load(&dir)?;
            tracing::info!(dir = %dir.display(), pages = PAGES.len(), "serving the Astro pages");
            Ok(Some(Arc::new(built)))
        }
    }
}

/// A page split at its two insertion points.
#[derive(Debug)]
pub struct Split {
    /// Up to and including `<html`.
    start: String,
    /// The rest of the `<html …>` tag up to the marker.
    head: String,
    /// After the marker.
    rest: String,
}

/// What one request puts into a page.
pub struct Injected<'a> {
    pub traceparent: Option<&'a str>,
    pub version: Option<&'a str>,
    pub bootstrap: &'a Bootstrap,
}

impl Split {
    pub fn parse(html: &str) -> Result<Self, String> {
        let at = |needle: &str| match html.match_indices(needle).map(|(i, _)| i).collect::<Vec<_>>()[..] {
            [i] => Ok(i),
            [] => Err(format!("no {needle}")),
            _ => Err(format!("{needle} more than once")),
        };
        let open = at(HTML_OPEN)? + HTML_OPEN.len();
        let marker = at(MARKER)?;
        if marker < open {
            return Err(format!("{MARKER} before {HTML_OPEN}"));
        }
        Ok(Self {
            start: html[..open].to_string(),
            head: html[open..marker].to_string(),
            rest: html[marker + MARKER.len()..].to_string(),
        })
    }

    pub fn render(&self, with: &Injected) -> String {
        let viewer = with.bootstrap.viewer.as_ref();
        let session = if viewer.is_some() { "in" } else { "out" };
        let perms: Vec<&str> = viewer.map_or(vec![], |v| v.permissions.iter().map(|p| p.as_str()).collect());
        let json = script_json(with.bootstrap);

        let mut html = String::with_capacity(self.start.len() + self.head.len() + self.rest.len() + json.len() + 256);
        html.push_str(&self.start);
        // Values the site names itself (`in`/`out`, KnownPermission): nothing to escape.
        html.push_str(&format!(r#" data-session="{session}" data-perms="{}""#, perms.join(" ")));
        html.push_str(&self.head);
        if let Some(tp) = with.traceparent {
            html.push_str(&format!(r#"<meta name="traceparent" content="{}">"#, attr(tp)));
        }
        if let Some(version) = with.version {
            html.push_str(&format!(r#"<meta name="service-version" content="{}">"#, attr(version)));
        }
        html.push_str(r#"<script id="bootstrap" type="application/json">"#);
        html.push_str(&json);
        html.push_str("</script>");
        html.push_str(&self.rest);
        html
    }
}

/// `value` as JSON that is safe inside `<script>`: nothing in it can end the element (`</script>`),
/// open a comment (`<!--`) or an entity, or end a line for an old JavaScript parser (U+2028,
/// U+2029). The characters are escaped as `\uXXXX`, which only occurs inside JSON strings, so
/// the JSON reads back the same.
pub fn script_json(value: &impl serde::Serialize) -> String {
    let json = serde_json::to_string(value).expect("the bootstrap serializes");
    let mut out = String::with_capacity(json.len());
    for c in json.chars() {
        match c {
            '<' => out.push_str(r"\u003c"),
            '>' => out.push_str(r"\u003e"),
            '&' => out.push_str(r"\u0026"),
            '\u{2028}' => out.push_str(r"\u2028"),
            '\u{2029}' => out.push_str(r"\u2029"),
            c => out.push(c),
        }
    }
    out
}

/// `value` inside a double-quoted attribute.
fn attr(value: &str) -> String {
    value.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;").replace('>', "&gt;")
}

/// The Astro build: every page and status page, split.
#[derive(Debug)]
pub struct Built {
    dir: PathBuf,
    pages: HashMap<&'static str, Split>,
}

impl Built {
    /// Reads `dir`. Fails when a page or status page is missing, can't be split, or the build
    /// has an HTML file that is neither: a page without a row in `PAGES` would have no access rule.
    pub fn load(dir: &Path) -> anyhow::Result<Self> {
        let mut found = BTreeSet::new();
        html_files(dir, dir, &mut found).map_err(|e| anyhow::anyhow!("can't read the Astro build at {}: {e}", dir.display()))?;
        let expected: BTreeSet<&str> = PAGES.iter().map(|p| p.file).chain(STATUS_PAGES.iter().copied()).collect();
        let unlisted: Vec<_> = found.iter().filter(|f| !expected.contains(f.as_str())).collect();
        if !unlisted.is_empty() {
            anyhow::bail!("{}: HTML that isn't a page in PAGES or a status page: {unlisted:?}", dir.display());
        }
        let mut pages = HashMap::new();
        for file in expected {
            let path = dir.join(file);
            let html = std::fs::read_to_string(&path).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
            let split = Split::parse(&html).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
            pages.insert(file, split);
        }
        Ok(Self { dir: dir.to_path_buf(), pages })
    }

    fn page(&self, file: &str) -> &Split {
        &self.pages[file]
    }
}

/// The `.html` files under `dir`, by their path relative to `root`.
fn html_files(root: &Path, dir: &Path, found: &mut BTreeSet<String>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            html_files(root, &path, found)?;
        } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("html")) {
            let relative = path.strip_prefix(root).expect("under the root");
            found.insert(relative.to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(())
}

/// Adds the pages and `/_astro/*` to `router` when there is a build. Guards come with the pages
/// that need them: until then a page that isn't public stops the server from starting.
pub fn mount<S: Clone + Send + Sync + 'static>(router: Router<S>, built: Option<Arc<Built>>) -> anyhow::Result<Router<S>> {
    let Some(built) = built else { return Ok(router) };
    let mut router = router;
    for page in PAGES {
        if page.access != Access::Public {
            anyhow::bail!("page {} needs a guard: only public pages are served", page.path.as_str());
        }
        router = router.route(page.path.as_str(), axum::routing::get(serve).with_state((built.clone(), page.file)));
        // `/asshole/` is the same page: sent to its one address, rather than to Dioxus's
        // fallback, which would render the Dioxus page under the flag.
        if page.path.as_str() != "/" {
            router = router.route(&format!("{}/", page.path.as_str()), axum::routing::get(canonical));
        }
    }
    let files = tower_http::services::ServeDir::new(&built.dir).precompressed_br().precompressed_gzip();
    let assets = Router::new()
        .route_service("/_astro/{*path}", files)
        .layer(axum::middleware::map_response(immutable));
    Ok(router.merge(assets))
}

/// A page's path with a trailing slash: a permanent redirect to the path without it, keeping
/// the query.
async fn canonical(uri: axum::http::Uri) -> axum::response::Redirect {
    let path = uri.path().trim_end_matches('/');
    let to = match uri.query() {
        Some(query) => format!("{path}?{query}"),
        None => path.to_owned(),
    };
    axum::response::Redirect::permanent(&to)
}

/// Astro's file names carry a hash of their content, so a file never changes: cached for good,
/// a 304 included (a revalidation answered without the header would leave it uncacheable). The
/// body may be brotli or gzip, chosen by the request's `Accept-Encoding`.
async fn immutable(mut res: Response) -> Response {
    if res.status().is_success() || res.status() == axum::http::StatusCode::NOT_MODIFIED {
        let headers = res.headers_mut();
        headers.insert(CACHE_CONTROL, HeaderValue::from_static("public, max-age=31536000, immutable"));
        headers.insert(axum::http::header::VARY, HeaderValue::from_static("accept-encoding"));
    }
    res
}

/// A page, with this request's data spliced in.
async fn serve(
    State((built, file)): State<(Arc<Built>, &'static str)>,
    request: Option<axum::Extension<api::trace::RequestTraceContext>>,
    viewer: Result<json_api::Viewer, json_api::ApiError>,
) -> Response {
    let bootstrap = match viewer {
        Ok(viewer) => json_api::bootstrap(viewer.0).await,
        // Logged out is what the page then shows; the reason is logged by the extractor.
        Err(_) => Bootstrap { viewer: None },
    };
    let traceparent = request.and_then(|axum::Extension(cx)| api::trace::page_traceparent(&cx));
    let version = api::trace::service_version();
    let html = built.page(file).render(&Injected {
        traceparent: traceparent.as_deref(),
        version: version.as_deref(),
        bootstrap: &bootstrap,
    });
    (
        [
            (CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8")),
            // Per request (the viewer, the trace): never from a shared cache. The browser's
            // back/forward cache stays on.
            (CACHE_CONTROL, HeaderValue::from_static("private, no-cache")),
        ],
        html,
    )
        .into_response()
}

#[cfg(test)]
mod tests;
