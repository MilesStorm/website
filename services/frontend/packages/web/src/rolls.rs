//! Live dice rolls for logged-in users (server only).
//!
//! The Arcane page streams camera frames through `/ws/arcane` to ai_pipeline. Every
//! settled roll it reports (`{"type":"roll",...}`) is published to Redis on
//! `arcane:rolls:<username>` and kept at `arcane:last_roll:<username>` for an hour.
//! Each BFF replica holds one Redis subscriber and fans rolls out in-process to that
//! user's open `/api/arcane/rolls` streams, so a viewer (the browser extension) can be
//! connected to a different replica, or a different device, than the camera.
//!
//! Keyed by username: usernames are unique in auth and never renamed.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::Extension;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::stream;
use tokio::sync::broadcast::{self, error::RecvError};
use tower_sessions_redis_store::fred::clients::SubscriberClient;
use tower_sessions_redis_store::fred::prelude::*;

const CHANNEL_PREFIX: &str = "arcane:rolls:";
const LAST_ROLL_PREFIX: &str = "arcane:last_roll:";
const LAST_ROLL_TTL_SECS: i64 = 3600;
/// Rolls queued per user for a slow viewer before it skips ahead.
const PER_USER_BUFFER: usize = 16;
/// Proxies (and Chrome) drop idle streams; a comment every 20s keeps them open.
const KEEP_ALIVE: Duration = Duration::from_secs(20);

/// Shared fan-out point for roll events; cheap to clone.
#[derive(Clone)]
pub struct RollHub {
    pool: Pool,
    users: Arc<Mutex<HashMap<String, broadcast::Sender<Arc<str>>>>>,
    // Held so the subscriber connection lives as long as the hub.
    _subscriber: SubscriberClient,
}

impl RollHub {
    /// Connect this replica's single Redis subscriber and start fanning messages out.
    /// `pool` is the shared session pool, used for PUBLISH/SET/GET.
    pub async fn connect(config: Config, pool: Pool) -> anyhow::Result<Self> {
        let subscriber = Builder::from_config(config)
            .set_policy(ReconnectPolicy::new_exponential(0, 100, 30_000, 2))
            .build_subscriber_client()?;
        subscriber.init().await?;
        // Re-issues the PSUBSCRIBE after every reconnect.
        subscriber.manage_subscriptions();
        subscriber.psubscribe(format!("{CHANNEL_PREFIX}*")).await?;

        let hub = Self {
            pool,
            users: Arc::new(Mutex::new(HashMap::new())),
            _subscriber: subscriber.clone(),
        };

        let mut messages = subscriber.message_rx();
        let users = hub.users.clone();
        tokio::spawn(async move {
            loop {
                let msg = match messages.recv().await {
                    Ok(m) => m,
                    Err(RecvError::Lagged(n)) => {
                        tracing::warn!(skipped = n, "arcane roll subscriber lagged");
                        continue;
                    }
                    Err(RecvError::Closed) => break,
                };
                let Some(user) = channel_user(&msg.channel) else { continue };
                let Ok(payload) = msg.value.convert::<String>() else { continue };
                let mut users = users.lock().unwrap();
                if let Some(tx) = users.get(user) {
                    // Err means nobody is listening any more: forget the user.
                    if tx.send(Arc::from(payload)).is_err() {
                        users.remove(user);
                    }
                }
            }
            tracing::error!("arcane roll subscriber stopped");
        });

        Ok(hub)
    }

    /// Record `payload` as `user`'s latest roll and notify every replica. Errors are
    /// logged, never returned: a lost roll must not break the camera stream.
    pub async fn publish(&self, user: &str, payload: String) {
        let client = self.pool.next();
        let set: Result<(), _> = client
            .set(
                format!("{LAST_ROLL_PREFIX}{user}"),
                payload.as_str(),
                Some(Expiration::EX(LAST_ROLL_TTL_SECS)),
                None,
                false,
            )
            .await;
        if let Err(e) = set {
            tracing::error!(error = %e, "storing last arcane roll failed");
        }
        let published: Result<i64, _> = client.publish(format!("{CHANNEL_PREFIX}{user}"), payload).await;
        if let Err(e) = published {
            tracing::error!(error = %e, "publishing arcane roll failed");
        }
    }

    fn subscribe(&self, user: &str) -> broadcast::Receiver<Arc<str>> {
        let mut users = self.users.lock().unwrap();
        users.retain(|_, tx| tx.receiver_count() > 0);
        users
            .entry(user.to_string())
            .or_insert_with(|| broadcast::channel(PER_USER_BUFFER).0)
            .subscribe()
    }

    async fn last_roll(&self, user: &str) -> Option<String> {
        let got: Result<Option<String>, _> = self.pool.next().get(format!("{LAST_ROLL_PREFIX}{user}")).await;
        got.unwrap_or_else(|e| {
            tracing::error!(error = %e, "reading last arcane roll failed");
            None
        })
    }
}

fn channel_user(channel: &str) -> Option<&str> {
    channel.strip_prefix(CHANNEL_PREFIX).filter(|u| !u.is_empty())
}

/// Cheap check on every upstream frame; only roll events are published.
pub fn is_roll(text: &str) -> bool {
    text.contains("\"roll\"")
        && serde_json::from_str::<serde_json::Value>(text)
            .is_ok_and(|v| v.get("type").and_then(|t| t.as_str()) == Some("roll"))
}

/// Why a session may not use Arcane.
pub enum Denied {
    LoggedOut,
    NoPermission,
}

/// The session's username, if it is logged in and holds the `arcane` permission.
pub async fn arcane_user(session: &tower_sessions::Session) -> Result<String, Denied> {
    let token: Option<String> = session.get("opaque_token").await.ok().flatten();
    let username: Option<String> = session.get("username").await.ok().flatten();
    let (Some(token), Some(username)) = (token, username) else {
        return Err(Denied::LoggedOut);
    };
    if api::has_arcane_permission(&token).await {
        Ok(username)
    } else {
        Err(Denied::NoPermission)
    }
}

/// Browsers always send `Origin` on WebSocket handshakes; the page and the socket are
/// served by the same host, so the origin's host must match `Host`. Extra origins
/// (e.g. the `dx serve` dev proxy) can be listed in `ARCANE_ALLOWED_ORIGINS`,
/// comma-separated. Clients that send no `Origin` are not browsers and carry no
/// ambient cookies, so they are let through to the normal session check.
pub fn origin_allowed(headers: &HeaderMap) -> bool {
    let extra = std::env::var("ARCANE_ALLOWED_ORIGINS").unwrap_or_default();
    origin_allowed_with(headers, &extra)
}

fn origin_allowed_with(headers: &HeaderMap, extra: &str) -> bool {
    let Some(origin) = headers.get(header::ORIGIN) else { return true };
    let Ok(origin) = origin.to_str() else { return false };
    if extra.split(',').map(str::trim).any(|o| !o.is_empty() && o == origin) {
        return true;
    }
    let origin_host = origin
        .strip_prefix("https://")
        .or_else(|| origin.strip_prefix("http://"));
    let host = headers.get(header::HOST).and_then(|h| h.to_str().ok());
    matches!((origin_host, host), (Some(o), Some(h)) if o.eq_ignore_ascii_case(h))
}

fn no_store(mut resp: Response) -> Response {
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

/// `GET /api/arcane/me`: always 200, so the extension can tell "logged out" from
/// "server down" without CORS or redirect handling.
pub async fn arcane_me(session: tower_sessions::Session) -> Response {
    let username: Option<String> = session.get("username").await.ok().flatten();
    let body = match arcane_user(&session).await {
        Ok(u) => serde_json::json!({"logged_in": true, "username": u, "has_arcane": true}),
        Err(Denied::NoPermission) => {
            serde_json::json!({"logged_in": true, "username": username, "has_arcane": false})
        }
        Err(Denied::LoggedOut) => {
            serde_json::json!({"logged_in": false, "username": null, "has_arcane": false})
        }
    };
    no_store(axum::Json(body).into_response())
}

/// `GET /api/arcane/rolls`: server-sent events, one `roll` event per settled roll,
/// starting with the user's last roll from the past hour (if any).
pub async fn arcane_rolls(
    Extension(hub): Extension<RollHub>,
    session: tower_sessions::Session,
) -> Response {
    let user = match arcane_user(&session).await {
        Ok(u) => u,
        Err(Denied::LoggedOut) => return no_store(StatusCode::UNAUTHORIZED.into_response()),
        Err(Denied::NoPermission) => return no_store(StatusCode::FORBIDDEN.into_response()),
    };

    // Subscribe before reading the stored roll so nothing published in between is
    // lost; the duplicate this can cause is dropped below.
    let rx = hub.subscribe(&user);
    let last = hub.last_roll(&user).await.map(Arc::<str>::from);

    let events = stream::unfold((last, rx, None::<Arc<str>>), |(mut pending, mut rx, mut sent)| async move {
        loop {
            let next = match pending.take() {
                Some(p) => p,
                None => match rx.recv().await {
                    Ok(p) => p,
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => return None,
                },
            };
            if sent.as_deref() == Some(&*next) {
                continue;
            }
            sent = Some(next.clone());
            let event = Event::default().event("roll").data(&*next);
            return Some((Ok::<_, Infallible>(event), (pending, rx, sent)));
        }
    });

    let mut resp = Sse::new(events)
        .keep_alive(KeepAlive::new().interval(KEEP_ALIVE))
        .into_response();
    // Tell nginx-style proxies not to buffer the stream.
    resp.headers_mut()
        .insert("x-accel-buffering", HeaderValue::from_static("no"));
    no_store(resp)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(origin: Option<&str>, host: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::HOST, HeaderValue::from_str(host).unwrap());
        if let Some(o) = origin {
            h.insert(header::ORIGIN, HeaderValue::from_str(o).unwrap());
        }
        h
    }

    #[test]
    fn same_host_origin_is_allowed() {
        assert!(origin_allowed_with(&headers(Some("https://milesstorm.com"), "milesstorm.com"), ""));
        assert!(origin_allowed_with(&headers(Some("http://localhost:8080"), "localhost:8080"), ""));
    }

    #[test]
    fn foreign_origin_is_rejected() {
        assert!(!origin_allowed_with(&headers(Some("https://evil.example"), "milesstorm.com"), ""));
        assert!(!origin_allowed_with(&headers(Some("https://milesstorm.com.evil.example"), "milesstorm.com"), ""));
        assert!(!origin_allowed_with(&headers(Some("null"), "milesstorm.com"), ""));
    }

    #[test]
    fn listed_origin_is_allowed() {
        let h = headers(Some("http://localhost:8080"), "127.0.0.1:43210");
        assert!(!origin_allowed_with(&h, ""));
        assert!(origin_allowed_with(&h, "https://x.example, http://localhost:8080"));
    }

    #[test]
    fn missing_origin_is_allowed() {
        assert!(origin_allowed_with(&headers(None, "milesstorm.com"), ""));
    }

    #[test]
    fn only_roll_messages_are_rolls() {
        assert!(is_roll(r#"{"type":"roll","roll_id":"a","dice":[],"total":null,"complete":false,"ts":1}"#));
        assert!(!is_roll(r#"{"type":"frame","detections":[],"frame_ms":3}"#));
        assert!(!is_roll(r#"{"type":"error","error":"roll"}"#));
        assert!(!is_roll("not json \"roll\""));
    }

    #[test]
    fn channel_user_strips_prefix() {
        assert_eq!(channel_user("arcane:rolls:miles"), Some("miles"));
        assert_eq!(channel_user("arcane:rolls:"), None);
        assert_eq!(channel_user("other:miles"), None);
    }
}
