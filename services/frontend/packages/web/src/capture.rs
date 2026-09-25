//! Keeping roll pictures for training (server only).
//!
//! The camera proxy numbers every frame it forwards (ai_pipeline numbers them the
//! same way and tags each result with `frame_seq`), keeps the last few, and when a
//! roll settles hands the roll plus the exact frame it was read from to a
//! per-connection capture worker. The worker:
//! - holds the user's latest roll and picture in Redis for 10 minutes, so the
//!   extension's "flag as wrong roll" can save it (flagging is the user's consent
//!   for that one picture); only the latest roll per user is held;
//! - for users who opted in (auth's PostgreSQL), stores rolls the model was unsure
//!   about and about one in `DATASET_SAMPLE_EVERY` others in SurrealDB (`dataset.rs`).
//!
//! It runs apart from the live roll publisher so a slow or down database can
//! never delay or drop rolls on their way to viewers.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::Extension;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tower_sessions_redis_store::fred::prelude::*;

use crate::dataset::{auto_reason, clean_values, dataset, is_roll_id, shares, Capture};
use crate::rolls::{arcane_user, origin_allowed, Denied, RollHub};

/// How long the latest roll's picture is kept for flagging.
pub const HOLD_SECS: i64 = 600;
/// Frames larger than this are never held or stored.
pub const MAX_FRAME_BYTES: usize = 2 * 1024 * 1024;
/// Recent frames kept per camera connection to match `frame_seq` (~1 s at 15 fps;
/// replies lag one or two frames).
const FRAME_RING: usize = 16;
const FLAGS_PER_HOUR: i64 = 30;
const AUTO_SAMPLES_PER_DAY: i64 = 300;
/// Captures waiting per connection; more are dropped (rolls are seconds apart).
const CAPTURE_QUEUE: usize = 4;
/// After a database error, skip automatic samples for this long.
const ERROR_PAUSE: Duration = Duration::from_secs(30);

fn held_meta_key(user: &str) -> String {
    format!("arcane:held:{user}")
}
fn held_jpeg_key(user: &str) -> String {
    format!("arcane:held_jpeg:{user}")
}

/// Only real pictures are kept: JPEG or PNG, by their first bytes.
pub fn is_image(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xFF, 0xD8, 0xFF]) || bytes.starts_with(b"\x89PNG\r\n\x1a\n")
}

/// The last `FRAME_RING` frames forwarded upstream, by 1-based sequence number.
/// Every binary message must be pushed (even oversized ones) so the numbering
/// stays in step with ai_pipeline's.
#[derive(Default)]
pub struct FrameRing {
    seq: u64,
    frames: VecDeque<(u64, Bytes)>,
}

impl FrameRing {
    pub fn push(&mut self, frame: &Bytes) {
        self.seq += 1;
        if frame.len() > MAX_FRAME_BYTES {
            return;
        }
        if self.frames.len() == FRAME_RING {
            self.frames.pop_front();
        }
        self.frames.push_back((self.seq, frame.clone()));
    }

    pub fn get(&self, seq: u64) -> Option<Bytes> {
        self.frames.iter().find(|(s, _)| *s == seq).map(|(_, f)| f.clone())
    }
}

/// One settled roll to keep: the roll JSON, the frame result it came with, and
/// the picture ai_pipeline read it from (if still in the ring).
pub struct CaptureJob {
    pub roll: String,
    pub frame: Option<String>,
    pub jpeg: Option<Bytes>,
}

impl RollHub {
    /// A capture queue for one camera connection; the worker ends with the sender.
    /// `token` is the connection's session token, used to look up the user's choice.
    pub fn capturer(&self, user: String, token: String) -> mpsc::Sender<CaptureJob> {
        let (tx, mut rx) = mpsc::channel::<CaptureJob>(CAPTURE_QUEUE);
        let hub = self.clone();
        tokio::spawn(async move {
            let mut paused_until: Option<Instant> = None;
            let mut last_counted = String::new();
            while let Some(job) = rx.recv().await {
                let Ok(roll) = serde_json::from_str::<Value>(&job.roll) else { continue };
                let Some(roll_id) = roll.get("roll_id").and_then(Value::as_str).filter(|r| is_roll_id(r))
                else {
                    continue;
                };
                // Nothing is kept unless the store is configured.
                let Some(ds) = dataset() else { continue };
                let Some(jpeg) = job.jpeg.filter(|j| is_image(j)) else {
                    tracing::debug!(roll_id, "roll picture missing or not an image; not kept");
                    continue;
                };
                let capture = Capture {
                    roll_id: roll_id.to_string(),
                    frame: job.frame.and_then(|f| serde_json::from_str(&f).ok()),
                    roll: roll.clone(),
                    jpeg: jpeg.to_vec(),
                };
                hub.hold(&user, &capture).await;

                if paused_until.is_some_and(|t| Instant::now() < t) {
                    continue;
                }
                // Decide first; only rolls that would be kept cost a consent lookup.
                let Some(reason) = auto_reason(&capture.roll, roll_id, ds.sample_every()) else { continue };
                match shares(&token).await {
                    Ok(true) => {}
                    Ok(false) => continue,
                    Err(e) => {
                        tracing::warn!(error = %e, "dataset consent lookup failed; pausing samples");
                        paused_until = Some(Instant::now() + ERROR_PAUSE);
                        continue;
                    }
                }
                // Re-emissions of the same roll refresh it without counting again.
                if last_counted != roll_id {
                    let n = hub.count_in_window(&format!("arcane:auto_count:{user}"), 86_400).await;
                    if n.is_none_or(|n| n > AUTO_SAMPLES_PER_DAY) {
                        continue;
                    }
                    last_counted = roll_id.to_string();
                }
                if let Err(e) = ds.save(&user, &capture, Some(reason), None).await {
                    tracing::warn!(error = %e, "saving dataset sample failed; pausing samples");
                    paused_until = Some(Instant::now() + ERROR_PAUSE);
                }
            }
        });
        tx
    }

    /// Keep `user`'s latest roll and picture for `HOLD_SECS`, replacing any earlier
    /// one. Both keys are written in one transaction so they always belong together.
    async fn hold(&self, user: &str, c: &Capture) {
        let meta = json!({"roll_id": c.roll_id, "roll": c.roll, "frame": c.frame}).to_string();
        let expire = Some(Expiration::EX(HOLD_SECS));
        let tx = self.pool.next().multi();
        let queued: Result<(), _> = async {
            tx.set::<(), _, _>(held_jpeg_key(user), Bytes::from(c.jpeg.clone()), expire.clone(), None, false)
                .await?;
            tx.set::<(), _, _>(held_meta_key(user), meta, expire, None, false).await?;
            tx.exec::<tower_sessions_redis_store::fred::types::Value>(true).await.map(|_| ())
        }
        .await;
        if let Err(e) = queued {
            tracing::warn!(error = %e, "holding roll picture failed");
        }
    }

    /// The held roll, if it is still there and is `roll_id`. Both keys are read in
    /// one command, so the picture always matches the roll.
    pub async fn held(&self, user: &str, roll_id: &str) -> Option<Capture> {
        let both: Vec<Option<Bytes>> = self
            .pool
            .next()
            .mget(vec![held_meta_key(user), held_jpeg_key(user)])
            .await
            .ok()?;
        let [meta, jpeg]: [Option<Bytes>; 2] = both.try_into().ok()?;
        let meta: Value = serde_json::from_slice(&meta?).ok()?;
        if meta.get("roll_id").and_then(Value::as_str) != Some(roll_id) {
            return None;
        }
        Some(Capture {
            roll_id: roll_id.to_string(),
            roll: meta.get("roll").cloned().unwrap_or(Value::Null),
            frame: meta.get("frame").cloned().filter(|f| !f.is_null()),
            jpeg: jpeg?.to_vec(),
        })
    }

    pub async fn clear_held(&self, user: &str) {
        let r: Result<i64, _> = self.pool.next().del(vec![held_meta_key(user), held_jpeg_key(user)]).await;
        if let Err(e) = r {
            tracing::warn!(error = %e, "clearing held roll failed");
        }
    }

    /// Increment a counter that resets `secs` after it was created; returns the new
    /// value, or None if Redis failed (callers refuse). The counter is created with
    /// its expiry in one command, so it can never be left without one.
    async fn count_in_window(&self, key: &str, secs: i64) -> Option<i64> {
        let client = self.pool.next();
        let created: Result<Option<String>, _> = client
            .set(key, 0, Some(Expiration::EX(secs)), Some(SetOptions::NX), false)
            .await;
        if let Err(e) = created {
            tracing::warn!(error = %e, "rate counter failed");
            return None;
        }
        client.incr(key).await.inspect_err(|e| tracing::warn!(error = %e, "rate counter failed")).ok()
    }
}

/// The flag endpoint is called by the extension: its Origin is the extension's own
/// (random per install) or absent. Web pages other than this site are refused;
/// they also can't send a JSON body cross-site without CORS, which is never granted.
fn flag_origin_allowed(headers: &HeaderMap) -> bool {
    match headers.get(header::ORIGIN).and_then(|o| o.to_str().ok()) {
        Some(o) if o.starts_with("moz-extension://") || o.starts_with("chrome-extension://") => true,
        _ => origin_allowed(headers),
    }
}

#[derive(Deserialize)]
pub struct FlagRequest {
    roll_id: String,
    #[serde(default)]
    values: Vec<Option<String>>,
}

fn error(status: StatusCode, code: &str) -> Response {
    (status, Json(json!({"error": code}))).into_response()
}

/// `POST /api/arcane/flag` `{"roll_id":..,"values":["5",null,..]}`: save the user's
/// latest roll and its picture as a wrong reading, with optional corrections.
pub async fn arcane_flag(
    Extension(hub): Extension<RollHub>,
    headers: HeaderMap,
    session: tower_sessions::Session,
    Json(req): Json<FlagRequest>,
) -> Response {
    if !flag_origin_allowed(&headers) {
        return error(StatusCode::FORBIDDEN, "origin");
    }
    let user = match arcane_user(&session).await {
        Ok(u) => u,
        Err(Denied::LoggedOut) => return error(StatusCode::UNAUTHORIZED, "login"),
        Err(Denied::NoPermission) => return error(StatusCode::FORBIDDEN, "permission"),
    };
    let Some(ds) = dataset() else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "unavailable");
    };
    if !is_roll_id(&req.roll_id) {
        return error(StatusCode::BAD_REQUEST, "roll_id");
    }
    let Some(capture) = hub.held(&user, &req.roll_id).await else {
        return error(StatusCode::GONE, "expired");
    };
    let dice = capture.roll.get("dice").and_then(Value::as_array).map_or(0, Vec::len);
    let Some(values) = clean_values(&req.values, dice) else {
        return error(StatusCode::BAD_REQUEST, "values");
    };
    // Counted only for flags that would be saved; refused if Redis can't count.
    match hub.count_in_window(&format!("arcane:flag_count:{user}"), 3600).await {
        Some(n) if n <= FLAGS_PER_HOUR => {}
        Some(_) => return error(StatusCode::TOO_MANY_REQUESTS, "rate"),
        None => return error(StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
    }
    match ds.save(&user, &capture, None, Some(&values)).await {
        Ok(()) => {
            tracing::info!(roll_id = %req.roll_id, "roll flagged as wrong");
            Json(json!({"ok": true})).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "saving flagged roll failed");
            error(StatusCode::BAD_GATEWAY, "store")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_numbers_every_frame_and_keeps_the_last_ones() {
        let mut ring = FrameRing::default();
        for i in 0..40u8 {
            ring.push(&Bytes::from(vec![i]));
        }
        assert_eq!(ring.get(40).as_deref(), Some(&[39u8][..]));
        assert_eq!(ring.get(25).as_deref(), Some(&[24u8][..]), "oldest kept");
        assert!(ring.get(24).is_none(), "older than the ring");
    }

    #[test]
    fn oversized_frames_are_counted_but_not_kept() {
        let mut ring = FrameRing::default();
        ring.push(&Bytes::from(vec![0u8; MAX_FRAME_BYTES + 1]));
        ring.push(&Bytes::from_static(b"ok"));
        assert!(ring.get(1).is_none());
        assert_eq!(ring.get(2).as_deref(), Some(&b"ok"[..]), "numbering stays in step");
    }

    #[test]
    fn only_jpeg_and_png_are_images() {
        assert!(is_image(b"\xff\xd8\xff\xe0rest"));
        assert!(is_image(b"\x89PNG\r\n\x1a\nrest"));
        assert!(!is_image(b"GIF89a"));
        assert!(!is_image(b"<svg"));
        assert!(!is_image(b""));
    }

    #[test]
    fn extension_and_same_site_origins_may_flag() {
        let h = |origin: &str| {
            let mut h = HeaderMap::new();
            h.insert(header::HOST, "milesstorm.com".parse().unwrap());
            h.insert(header::ORIGIN, origin.parse().unwrap());
            h
        };
        assert!(flag_origin_allowed(&h("moz-extension://1b2c3d")));
        assert!(flag_origin_allowed(&h("chrome-extension://abcdef")));
        assert!(flag_origin_allowed(&h("https://milesstorm.com")));
        assert!(!flag_origin_allowed(&h("https://evil.example")));
    }
}
