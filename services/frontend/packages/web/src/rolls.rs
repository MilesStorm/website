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
//!
//! Long-lived connections (the SSE stream and the camera WebSocket) re-check every
//! minute that their session still exists and still holds `arcane`, so logging out
//! or losing the permission ends them.
//!
//! Tracing (TRACING.md, "Streams"): a connection is not one span. It gets a short open
//! span and a close span ([`Session`]), and each roll is a trace of its own: ai_pipeline's
//! context arrives in the roll message's `_trace` field ([`split_trace`]), `roll publish`
//! continues it into Redis, and every stream that delivers the roll adds a `roll deliver`
//! span. `_trace` never reaches a browser, the extension, `last_roll` or SurrealDB.

use std::borrow::Cow;
use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::Extension;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::stream;
use api::trace::{Carried, RedisTracing};
use opentelemetry::trace::SpanContext;
use opentelemetry::KeyValue;
use tokio::sync::broadcast::{self, error::RecvError};
use tokio::sync::mpsc;
use tokio::time::{interval_at, timeout, Instant, Interval};
use tokio_util::sync::CancellationToken;
use tower_sessions::session::Id;
use tower_sessions::SessionStore;
use tower_sessions_redis_store::fred::clients::SubscriberClient;
use tower_sessions_redis_store::fred::prelude::*;
use tower_sessions_redis_store::RedisStore;
use tracing::Instrument as _;
use tracing_opentelemetry::OpenTelemetrySpanExt as _;

const CHANNEL_PREFIX: &str = "arcane:rolls:";
/// The channel as span attributes name it: the real one ends in a username.
const CHANNEL_TEMPLATE: &str = "arcane:rolls:{user}";
/// The field of a roll message that carries its trace context between processes.
const TRACE_FIELD: &str = "_trace";
/// A roll delivered later than this after it was published starts a trace of its own
/// (linked to the roll's), so it can't stretch a trace that is already finished.
const LIVE: Duration = Duration::from_secs(10);
pub(crate) const LAST_ROLL_PREFIX: &str = "arcane:last_roll:";
const LAST_ROLL_TTL_SECS: i64 = 3600;
/// Rolls queued per user for a slow viewer before it skips ahead.
const PER_USER_BUFFER: usize = 16;
/// Proxies (and Chrome) drop idle streams; a comment every 20s keeps them open.
const KEEP_ALIVE: Duration = Duration::from_secs(20);
/// How often long-lived connections re-check the session and permission.
pub const RECHECK: Duration = Duration::from_secs(60);
/// Open roll streams allowed per user (side panels, tabs, devices).
const MAX_STREAMS_PER_USER: usize = 8;
/// A subscriber connection only reads, so a silently dead socket would go
/// unnoticed; a PING this often surfaces it and triggers a reconnect.
const SUBSCRIBER_PING: Duration = Duration::from_secs(30);
const PING_TIMEOUT: Duration = Duration::from_secs(10);
/// Rolls waiting to be written to Redis per camera connection.
const PUBLISH_QUEUE: usize = 16;

/// The trace context a roll message arrived with ([`split_trace`]).
#[derive(Default)]
pub struct Envelope {
    /// The sender's span; empty when the message had none.
    pub cx: opentelemetry::Context,
    /// When this replica's peer published it to Redis; `None` from ai_pipeline.
    published: Option<SystemTime>,
}

/// The top-level `_trace` member of a roll message, as written in the message.
#[derive(serde::Deserialize)]
struct Traced<'a> {
    #[serde(borrow, rename = "_trace")]
    trace: Option<&'a serde_json::value::RawValue>,
}

#[derive(Default, serde::Deserialize)]
struct TraceField {
    traceparent: Option<String>,
    published_ms: Option<u64>,
}

/// Takes the `_trace` field off a roll message: its trace context, and the message as
/// everyone else gets it, which is the sender's text byte for byte apart from that field
/// (key order, spacing and how numbers are written stay the sender's). Messages without
/// the field come back untouched.
pub fn split_trace(text: &str) -> (Envelope, Cow<'_, str>) {
    let untouched = || (Envelope::default(), Cow::Borrowed(text));
    if !text.contains("\"_trace\"") {
        return untouched();
    }
    let cut = match serde_json::from_str::<Traced>(text) {
        Ok(Traced { trace: Some(raw) }) => without_member(text, raw.get()).map(|rest| (raw.get().to_string(), rest)),
        _ => None,
    };
    // A field that can't be cut out of the text (written twice, or null) still
    // comes off: the message is read and written again, at the cost of the sender's layout.
    let (trace, rest) = match cut {
        Some(cut) => cut,
        None => {
            let Ok(serde_json::Value::Object(mut message)) = serde_json::from_str(text) else { return untouched() };
            let Some(trace) = message.remove(TRACE_FIELD) else { return untouched() };
            (trace.to_string(), serde_json::Value::Object(message).to_string())
        }
    };
    let trace: TraceField = serde_json::from_str(&trace).unwrap_or_default();
    let envelope = Envelope {
        cx: trace.traceparent.as_deref().map(api::trace::context_from_traceparent).unwrap_or_default(),
        published: trace.published_ms.and_then(|ms| UNIX_EPOCH.checked_add(Duration::from_millis(ms))),
    };
    (envelope, Cow::Owned(rest))
}

/// The JSON object `text` without its member `"_trace": value` and that member's comma.
/// `value` is the member's value as serde_json borrowed it from `text`. None when it isn't
/// where it should be (a key written with escapes, a message that is an array).
fn without_member(text: &str, value: &str) -> Option<String> {
    let start = (value.as_ptr() as usize).checked_sub(text.as_ptr() as usize)?;
    let end = start.checked_add(value.len())?;
    if text.get(start..end) != Some(value) {
        return None;
    }
    let before = text[..start].trim_end().strip_suffix(':')?.trim_end().strip_suffix("\"_trace\"")?;
    let after = &text[end..];
    Some(match after.trim_start().strip_prefix(',') {
        // Not the last member: its comma is the one after it.
        Some(rest) => format!("{before}{}", rest.trim_start()),
        None => format!("{}{after}", before.trim_end().strip_suffix(',').unwrap_or(before)),
    })
}

/// `roll` with a `_trace` field naming `span` and the time, for Redis: the replicas that
/// receive it take the field off again and have `roll` byte for byte. `roll` as it is when
/// it isn't a JSON object or there is no trace to carry.
fn with_trace(roll: &str, span: &tracing::Span) -> String {
    let Some(traceparent) = api::trace::traceparent_of(span) else { return roll.to_string() };
    let end = roll.trim_end().len();
    let object = roll.trim_start().strip_prefix('{').zip(roll[..end].strip_suffix('}'));
    let Some((members, head)) = object.filter(|_| serde_json::from_str::<serde::de::IgnoredAny>(roll).is_ok()) else {
        return roll.to_string();
    };
    // `members` still ends with the closing brace.
    let comma = if members.trim() == "}" { "" } else { "," };
    let published_ms = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
    let trace = serde_json::json!({ "traceparent": traceparent, "published_ms": published_ms });
    format!("{head}{comma}\"{TRACE_FIELD}\":{trace}}}{}", &roll[end..])
}

/// A long-lived connection (a camera WebSocket, a roll stream) in traces: not one span,
/// which would only be exported when it ends, but a short open span, a close span, and
/// a trace per unit of work in between (a roll, a permission recheck). The close span and
/// every unit link to the open span, and all of them carry the same `session.id`, which
/// is how to get from the open span to the rest (a span that has ended can't be given a
/// link). The close span is made when this is dropped, so a connection that is cut off
/// gets one too.
pub struct Session {
    name: &'static str,
    id: String,
    open: Option<SpanContext>,
    started: Instant,
    reason: Mutex<&'static str>,
    frames: AtomicU64,
    rolls: AtomicU64,
    drops: AtomicU64,
    /// Drops not yet put on a unit span ([`Self::unit`]).
    unreported: AtomicU64,
}

impl Session {
    /// `open` is the session's open span (named `session open`, as ai_pipeline's), still
    /// open. `name` tells the kinds of session apart, as `session.kind`.
    pub fn open(name: &'static str, open: &tracing::Span) -> Self {
        use opentelemetry_sdk::trace::{IdGenerator as _, RandomIdGenerator};
        let id = RandomIdGenerator::default().new_span_id().to_string();
        open.set_attribute("session.id", id.clone());
        open.set_attribute("session.kind", name);
        Self {
            name,
            id,
            open: api::trace::span_context(open),
            started: Instant::now(),
            reason: Mutex::new("disconnected"),
            frames: AtomicU64::new(0),
            rolls: AtomicU64::new(0),
            drops: AtomicU64::new(0),
            unreported: AtomicU64::new(0),
        }
    }

    /// The open span, for the session's unit traces to link to.
    pub fn link(&self) -> Option<SpanContext> {
        self.open.clone()
    }

    /// Why the session ended, for the close span.
    pub fn set_reason(&self, reason: &'static str) {
        *self.reason.lock().unwrap() = reason;
    }

    pub fn frame(&self) {
        self.frames.fetch_add(1, Ordering::Relaxed);
    }

    pub fn roll(&self) {
        self.rolls.fetch_add(1, Ordering::Relaxed);
    }

    /// `n` messages were dropped (a full queue, a viewer that fell behind).
    pub fn dropped(&self, n: u64) {
        self.drops.fetch_add(n, Ordering::Relaxed);
        self.unreported.fetch_add(n, Ordering::Relaxed);
    }

    /// Makes `span` one of the session's unit spans: linked to the open span, with the
    /// session's ID, and with the drops since the last unit as a `dropped` event.
    pub fn unit(&self, span: &tracing::Span) {
        if let Some(open) = self.link() {
            span.add_link(open);
        }
        span.set_attribute("session.id", self.id.clone());
        span.set_attribute("session.kind", self.name);
        let dropped = self.unreported.swap(0, Ordering::Relaxed);
        if dropped > 0 {
            span.add_event("dropped", vec![KeyValue::new("dropped.count", dropped as i64)]);
        }
    }
}

impl Drop for Session {
    /// The close span: a trace of its own, with what the session did.
    fn drop(&mut self) {
        let close = api::unit_span!(
            None,
            "arcane.session_close",
            otel.name = "session close",
            session.duration_ms = self.started.elapsed().as_millis() as u64,
            session.frames = self.frames.load(Ordering::Relaxed),
            session.rolls = self.rolls.load(Ordering::Relaxed),
            session.drops = self.drops.load(Ordering::Relaxed),
            session.close_reason = *self.reason.lock().unwrap(),
        );
        self.unit(&close);
    }
}

/// One roll on its way to this replica's viewers, without its `_trace` field.
#[derive(Clone)]
struct Delivery {
    roll: Arc<str>,
    /// When it was published; `None` if it was read back from `last_roll` instead.
    published: Option<SystemTime>,
}

/// Shared fan-out point for roll events; cheap to clone.
#[derive(Clone)]
pub struct RollHub {
    pub(crate) pool: Pool,
    sessions: RedisStore<Pool>,
    users: Arc<Mutex<HashMap<String, broadcast::Sender<Carried<Delivery>>>>>,
    // Held so the subscriber connection lives as long as the hub.
    _subscriber: SubscriberClient,
    /// Cancelled when the server shuts down: roll streams and camera sessions end.
    pub(crate) shutdown: CancellationToken,
}

impl RollHub {
    /// Connect this replica's single Redis subscriber and start fanning messages out.
    /// `pool` is the shared session pool, used for PUBLISH/SET/GET and session reads;
    /// `connection` should carry the same TCP keepalive settings as the pool.
    pub async fn connect(
        config: Config,
        connection: ConnectionConfig,
        pool: Pool,
        shutdown: CancellationToken,
    ) -> anyhow::Result<Self> {
        // No command spans: after start-up it only sends the liveness PING below, from a
        // loop. fred makes no span for a received message either way.
        let subscriber = api::trace::redis_subscriber(config, connection, RedisTracing::Off)?;
        subscriber.init().await?;
        // Re-issues the PSUBSCRIBE after every reconnect.
        subscriber.manage_subscriptions();
        subscriber.psubscribe(format!("{CHANNEL_PREFIX}*")).await?;

        let hub = Self {
            sessions: RedisStore::new(pool.clone()),
            pool,
            users: Arc::new(Mutex::new(HashMap::new())),
            _subscriber: subscriber.clone(),
            shutdown,
        };

        // Liveness: a PING that fails or hangs forces a reconnect.
        let pinger = subscriber.clone();
        api::trace::spawn_loop("arcane roll subscriber: liveness PING", async move {
            let mut every = interval_at(Instant::now() + SUBSCRIBER_PING, SUBSCRIBER_PING);
            loop {
                every.tick().await;
                let ok = matches!(timeout(PING_TIMEOUT, pinger.ping::<Value>(None)).await, Ok(Ok(_)));
                if !ok {
                    tracing::warn!("arcane roll subscriber ping failed; reconnecting");
                    if let Err(e) = pinger.force_reconnection().await {
                        tracing::error!(error = %e, "arcane roll subscriber reconnect failed");
                    }
                }
            }
        });

        // Pub/sub drops messages sent while disconnected: after a reconnect, replay
        // each watched user's stored last roll (viewers drop exact repeats).
        let mut reconnects = subscriber.reconnect_rx();
        let replay = hub.clone();
        api::trace::spawn_loop("arcane roll subscriber: replays last rolls after a reconnect", async move {
            while reconnects.recv().await.is_ok() {
                let watched: Vec<String> = replay.users.lock().unwrap().keys().cloned().collect();
                // One trace per reconnect; each stream's delivery links to it.
                let span = api::unit_span!(None, "arcane.rolls_replay", users = watched.len());
                async {
                    for user in watched {
                        if let Some(roll) = replay.last_roll(&user).await {
                            if let Some(tx) = replay.users.lock().unwrap().get(&user) {
                                let _ = tx.send(Carried::new(Delivery { roll: Arc::from(roll), published: None }));
                            }
                        }
                    }
                }
                .instrument(span)
                .await;
            }
        });

        let mut messages = subscriber.message_rx();
        let users = hub.users.clone();
        api::trace::spawn_loop("arcane roll subscriber: fans rolls out to open streams", async move {
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
                    // No span here: each stream that delivers the roll makes its own.
                    let (envelope, roll) = split_trace(&payload);
                    let delivery = Delivery { roll: Arc::from(&*roll), published: envelope.published };
                    // Err means nobody is listening any more: forget the user.
                    if tx.send(Carried::with_context(envelope.cx, delivery)).is_err() {
                        users.remove(user);
                    }
                }
            }
            // Only happens if the client is torn down; live rolls would silently stop
            // on this replica, so let the orchestrator restart it.
            tracing::error!("arcane roll subscriber stopped; exiting");
            crate::flush_telemetry();
            #[allow(clippy::disallowed_methods, reason = "the roll subscriber is gone for good: exit (after flushing telemetry) so the orchestrator restarts the replica")]
            std::process::exit(1);
        });

        Ok(hub)
    }

    /// A queue for one camera connection's rolls. They are written to Redis in order
    /// on a single connection, so a roll's later, more readable update can never be
    /// overtaken by the earlier one. The writer ends when the sender is dropped.
    ///
    /// Each roll is sent with the context of its trace (ai_pipeline's, from the message);
    /// `session` is the camera session's open span, which every roll links to.
    pub fn publisher(&self, user: String, session: Option<SpanContext>) -> mpsc::Sender<Carried<String>> {
        let (tx, mut rx) = api::trace::channel::<String>(PUBLISH_QUEUE);
        let client = self.pool.next().clone();
        let writer = async move {
            while let Some(roll) = rx.recv().await {
                let span = tracing::info_span!(
                    parent: None,
                    "roll.publish",
                    otel.name = "roll publish",
                    otel.kind = "producer",
                    messaging.system = "redis",
                    messaging.operation.name = "publish",
                    "messaging.operation.type" = "send",
                    messaging.destination.template = CHANNEL_TEMPLATE,
                    // Queued: the span that received the roll has ended by now.
                    trace.relation = "follows",
                    trace_id = tracing::field::Empty,
                    span_id = tracing::field::Empty,
                );
                if let Some(session) = session.clone() {
                    span.add_link(session);
                }
                let roll = roll.enter(&span);
                publish(&client, &user, roll).instrument(span).await;
            }
        };
        // Per connection, not per process: shutdown waits for the rolls still queued.
        api::trace::spawn_loop("camera session: writes its rolls to Redis in order", api::trace::session(writer));
        tx
    }

    /// One permission recheck of a long-lived connection ([`Self::still_allowed`]), as a
    /// trace of its own, linked to the connection's `session`.
    pub async fn recheck(&self, session: &Session, session_id: Option<Id>, user: &str) -> bool {
        let span = api::unit_span!(None, "arcane.recheck", otel.name = "permission recheck");
        session.unit(&span);
        self.still_allowed(session_id, user).instrument(span).await
    }

    /// Whether the session behind a long-lived connection still exists (not logged
    /// out), still belongs to `user`, and still holds `arcane`.
    pub async fn still_allowed(&self, session_id: Option<Id>, user: &str) -> bool {
        let Some(id) = session_id else { return false };
        match self.sessions.load(&id).await {
            Ok(Some(record)) => {
                let token = record.data.get("opaque_token").and_then(|v| v.as_str());
                let name = record.data.get("username").and_then(|v| v.as_str());
                match (token, name) {
                    (Some(token), Some(name)) if name == user => api::has_arcane_permission(token).await,
                    _ => false,
                }
            }
            Ok(None) => false,
            Err(e) => {
                // A Redis hiccup shouldn't drop everyone; the next check decides.
                tracing::warn!(error = %e, "arcane session re-check failed");
                api::trace::failed("redis");
                true
            }
        }
    }
}

/// Record `payload` as `user`'s latest roll and notify every replica. Errors are
/// logged, never returned: a lost roll must not break the camera stream.
///
/// Runs in the `roll publish` span. The stored roll is `payload` as it is; the published
/// one also carries that span's context ([`with_trace`]), for the receiving replicas.
async fn publish(client: &Client, user: &str, payload: String) {
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
        api::trace::failed("redis");
    }
    let message = with_trace(&payload, &tracing::Span::current());
    let published: Result<i64, _> = client.publish(format!("{CHANNEL_PREFIX}{user}"), message).await;
    if let Err(e) = published {
        tracing::error!(error = %e, "publishing arcane roll failed");
        api::trace::failed("redis");
    }
}

impl RollHub {
    /// None when the user already has `MAX_STREAMS_PER_USER` streams open.
    fn subscribe(&self, user: &str) -> Option<broadcast::Receiver<Carried<Delivery>>> {
        let mut users = self.users.lock().unwrap();
        users.retain(|_, tx| tx.receiver_count() > 0);
        let tx = users
            .entry(user.to_string())
            .or_insert_with(|| api::trace::broadcast(PER_USER_BUFFER));
        (tx.receiver_count() < MAX_STREAMS_PER_USER).then(|| tx.subscribe())
    }

    async fn last_roll(&self, user: &str) -> Option<String> {
        let got: Result<Option<String>, _> = self.pool.next().get(format!("{LAST_ROLL_PREFIX}{user}")).await;
        let roll = got.unwrap_or_else(|e| {
            tracing::error!(error = %e, "reading last arcane roll failed");
            api::trace::failed("redis");
            None
        })?;
        // Stored without `_trace`; a replica from before that rule may have left one.
        Some(split_trace(&roll).1.into_owned())
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
/// served by the same host, so the origin's host must match `Host`, over HTTPS.
/// Plain-http origins are accepted only in debug builds (local `dx serve`). Extra
/// origins (e.g. the dev proxy) can be listed in `ARCANE_ALLOWED_ORIGINS`,
/// comma-separated. Clients that send no `Origin` are not browsers and carry no
/// ambient cookies, so they are let through to the normal session check.
pub fn origin_allowed(headers: &HeaderMap) -> bool {
    let extra = std::env::var("ARCANE_ALLOWED_ORIGINS").unwrap_or_default();
    origin_allowed_with(headers, &extra, cfg!(debug_assertions))
}

fn origin_allowed_with(headers: &HeaderMap, extra: &str, allow_http: bool) -> bool {
    let Some(origin) = headers.get(header::ORIGIN) else { return true };
    let Ok(origin) = origin.to_str() else { return false };
    if extra.split(',').map(str::trim).any(|o| !o.is_empty() && o == origin) {
        return true;
    }
    let origin_host = origin
        .strip_prefix("https://")
        .or_else(|| origin.strip_prefix("http://").filter(|_| allow_http));
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

struct RollStream {
    /// The stored last roll, sent first as a `replay` event.
    pending: Option<Arc<str>>,
    rx: broadcast::Receiver<Carried<Delivery>>,
    /// Last roll sent, to drop exact repeats.
    sent: Option<Arc<str>>,
    recheck: Interval,
    session_id: Option<Id>,
    user: String,
    hub: RollHub,
    /// The stream outlives its request's span (the body is polled after the handler
    /// returns): its rechecks and deliveries are traces of their own, linked to this.
    session: Session,
}

/// The CONSUMER span of one roll reaching one stream. A roll delivered live continues its
/// trace (a child of `roll publish`). One read back from `last_roll`, or delivered more
/// than [`LIVE`] after it was published, roots a trace and links to where it came from
/// (the roll's trace, or the replay after a reconnect).
fn deliver_span(session: &Session, roll: Carried<Delivery>, replay: bool) -> (tracing::Span, Arc<str>) {
    let span = tracing::info_span!(
        parent: None,
        "roll.deliver",
        otel.name = "roll deliver",
        otel.kind = "consumer",
        messaging.system = "redis",
        messaging.operation.name = "deliver",
        "messaging.operation.type" = "process",
        messaging.destination.template = CHANNEL_TEMPLATE,
        arcane.replay = replay,
        trace_id = tracing::field::Empty,
        span_id = tracing::field::Empty,
    );
    // A clock that says "published in the future" is skew between replicas: still live.
    let live = roll.published.is_some_and(|at| at.elapsed().map_or(true, |age| age <= LIVE));
    let delivery = if live {
        roll.enter(&span)
    } else {
        api::trace::start_unit(&span, roll.span_context());
        roll.into_inner()
    };
    session.unit(&span);
    (span, delivery.roll)
}

impl RollHub {
    /// The events of a new roll stream for `user`: the stored last roll as a `replay`
    /// event, then a `roll` event per settled roll, until the session behind
    /// `session_id` ends or the server shuts down. None when the user already has
    /// `MAX_STREAMS_PER_USER` streams open.
    pub(crate) async fn events(
        &self,
        user: String,
        session_id: Option<Id>,
    ) -> Option<impl futures_util::Stream<Item = Result<Event, Infallible>>> {
        // The stream's open span: the part of it that belongs to this request.
        let open = tracing::info_span!("arcane.rolls_stream", otel.name = "session open", user = %user);
        // Subscribe before reading the stored roll so nothing published in between is
        // lost; the duplicate this can cause is dropped below.
        let rx = self.subscribe(&user)?;
        let state = RollStream {
            pending: self.last_roll(&user).instrument(open.clone()).await.map(Arc::<str>::from),
            rx,
            sent: None,
            recheck: interval_at(Instant::now() + RECHECK, RECHECK),
            session_id,
            session: Session::open("arcane.rolls_stream", &open),
            user,
            hub: self.clone(),
        };
        drop(open);

        Some(stream::unfold(state, |mut st| async move {
            loop {
                let (next, replay) = match st.pending.take() {
                    Some(roll) => (Carried::with_context(Default::default(), Delivery { roll, published: None }), true),
                    None => tokio::select! {
                        got = st.rx.recv() => match got {
                            Ok(p) => (p, false),
                            Err(RecvError::Lagged(skipped)) => {
                                st.session.dropped(skipped);
                                continue;
                            }
                            Err(RecvError::Closed) => return None,
                        },
                        _ = st.recheck.tick() => {
                            if st.hub.recheck(&st.session, st.session_id, &st.user).await {
                                continue;
                            }
                            // Logged out or permission removed: end the stream; the
                            // client reconnects and gets 401/403.
                            st.session.set_reason("session ended or permission removed");
                            return None;
                        }
                        _ = st.hub.shutdown.cancelled() => {
                            st.session.set_reason("shutdown");
                            return None;
                        }
                    },
                };
                if st.sent.as_deref() == Some(&*next.roll) {
                    continue;
                }
                let (span, roll) = deliver_span(&st.session, next, replay);
                st.session.roll();
                st.sent = Some(roll.clone());
                let event = span.in_scope(|| Event::default().event(if replay { "replay" } else { "roll" }).data(&*roll));
                return Some((Ok::<_, Infallible>(event), st));
            }
        }))
    }
}

/// `GET /api/arcane/rolls`: server-sent events, one `roll` event per settled roll,
/// starting with the user's last roll from the past hour (if any) as a `replay`
/// event, so a viewer can show it without acting on it as a new roll (the extension
/// must not post an old roll to a game chat). 401/403 when not allowed, 429 above
/// `MAX_STREAMS_PER_USER` open streams.
pub async fn arcane_rolls(
    Extension(hub): Extension<RollHub>,
    session: tower_sessions::Session,
) -> Response {
    let user = match arcane_user(&session).await {
        Ok(u) => u,
        Err(Denied::LoggedOut) => return no_store(StatusCode::UNAUTHORIZED.into_response()),
        Err(Denied::NoPermission) => return no_store(StatusCode::FORBIDDEN.into_response()),
    };

    let Some(events) = hub.events(user, session.id()).await else {
        return no_store(StatusCode::TOO_MANY_REQUESTS.into_response());
    };

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
        assert!(origin_allowed_with(&headers(Some("https://milesstorm.com"), "milesstorm.com"), "", false));
        assert!(origin_allowed_with(&headers(Some("http://localhost:8080"), "localhost:8080"), "", true));
    }

    #[test]
    fn plain_http_origin_is_rejected_in_release() {
        assert!(!origin_allowed_with(&headers(Some("http://milesstorm.com"), "milesstorm.com"), "", false));
    }

    #[test]
    fn foreign_origin_is_rejected() {
        assert!(!origin_allowed_with(&headers(Some("https://evil.example"), "milesstorm.com"), "", false));
        assert!(!origin_allowed_with(&headers(Some("https://milesstorm.com.evil.example"), "milesstorm.com"), "", false));
        assert!(!origin_allowed_with(&headers(Some("null"), "milesstorm.com"), "", false));
    }

    #[test]
    fn listed_origin_is_allowed() {
        let h = headers(Some("http://localhost:8080"), "127.0.0.1:43210");
        assert!(!origin_allowed_with(&h, "", false));
        assert!(origin_allowed_with(&h, "https://x.example, http://localhost:8080", false));
    }

    #[test]
    fn missing_origin_is_allowed() {
        assert!(origin_allowed_with(&headers(None, "milesstorm.com"), "", false));
    }

    #[test]
    fn only_roll_messages_are_rolls() {
        assert!(is_roll(r#"{"type":"roll","roll_id":"a","dice":[],"total":null,"complete":false,"ts":1}"#));
        assert!(!is_roll(r#"{"type":"frame","detections":[],"frame_ms":3}"#));
        assert!(!is_roll(r#"{"type":"error","error":"roll"}"#));
        assert!(!is_roll("not json \"roll\""));
    }

    use crate::trace_tests::{attr, context_of, exporting, links, span};
    use opentelemetry::trace::{SpanId, SpanKind, TraceContextExt as _};

    const ROLL: &str = r#"{"complete":true,"dice":[],"roll_id":"18f3a2b-0badf00d","type":"roll"}"#;

    #[test]
    fn the_trace_field_is_taken_off_a_message_and_read() {
        let (_exporter, _guard) = exporting();
        let traceparent = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
        let message = format!(r#"{{"type":"roll","_trace":{{"traceparent":"{traceparent}"}},"roll_id":"18f3a2b-0badf00d","dice":[],"complete":true}}"#);

        let (envelope, roll) = split_trace(&message);
        assert_eq!(roll, r#"{"type":"roll","roll_id":"18f3a2b-0badf00d","dice":[],"complete":true}"#);
        let sc = envelope.cx.span().span_context().clone();
        assert_eq!(format!("00-{}-{}-01", sc.trace_id(), sc.span_id()), traceparent);
        assert!(sc.is_remote());

        // A field that isn't a usable context is still taken off.
        for bad in [r#""_trace":{"traceparent":"00-garbage"}"#, r#""_trace":7"#, r#""_trace":{}"#] {
            let message = format!(r#"{{"type":"roll",{bad}}}"#);
            let (envelope, roll) = split_trace(&message);
            assert_eq!(roll, r#"{"type":"roll"}"#);
            assert!(!envelope.cx.span().span_context().is_valid());
        }
    }

    #[test]
    fn messages_without_a_trace_field_are_left_as_they_are() {
        // Byte for byte: key order and spacing are ai_pipeline's.
        for message in [
            r#"{"type":"roll",  "roll_id":"a", "b":1}"#,
            r#"{"type":"frame","detections":[{"label":"_trace"}]}"#,
            r#"{"type":"frame","nested":{"_trace":{"traceparent":"x"}}}"#,
            "not json \"_trace\"",
        ] {
            let (envelope, same) = split_trace(message);
            assert!(matches!(same, Cow::Borrowed(_)), "{message}");
            assert_eq!(same, message);
            assert!(!envelope.cx.span().span_context().is_valid());
        }
    }

    #[test]
    fn a_roll_carries_its_publish_span_over_redis_and_arrives_without_it() {
        let (_exporter, _guard) = exporting();
        let publish = tracing::info_span!("roll.publish");
        let publish_sc = context_of(&publish);

        let on_wire = with_trace(ROLL, &publish);
        assert!(on_wire.contains("_trace"));
        let (envelope, roll) = split_trace(&on_wire);
        assert_eq!(roll, ROLL);
        let sc = envelope.cx.span().span_context().clone();
        assert_eq!((sc.trace_id(), sc.span_id()), (publish_sc.trace_id(), publish_sc.span_id()));
        assert!(envelope.published.is_some_and(|at| at.elapsed().is_ok_and(|age| age < LIVE)));

        // Not a JSON object: published as it is.
        assert_eq!(with_trace("[1]", &publish), "[1]");
        assert_eq!(with_trace("{1}", &publish), "{1}");
    }

    #[test]
    fn everything_but_the_trace_field_is_the_senders_text() {
        let (_exporter, _guard) = exporting();
        let publish = tracing::info_span!("roll.publish");
        // Keys out of alphabetical order, Python's spacing, a number that doesn't survive
        // being read as a float and written again, an escaped character.
        let trace = r#""_trace": {"traceparent": "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"}"#;
        let roll = r#"{"type": "roll", "roll_id": "b", "confidence": 0.12345678901234567890, "big": 1e400, "dice": [{"label": "d\u0032"}], "complete": true}"#;
        for message in [
            roll.replacen(r#""roll_id""#, &format!(r#"{trace}, "roll_id""#), 1),
            roll.replacen('{', &format!("{{{trace}, "), 1),
            format!("{}, {trace}}}", &roll[..roll.len() - 1]),
            format!("{} ,\n {trace} }}", &roll[..roll.len() - 1]),
        ] {
            let (envelope, stripped) = split_trace(&message);
            assert!(envelope.cx.span().span_context().is_valid(), "{message}");
            assert_eq!(stripped.split_whitespace().collect::<String>(), roll.split_whitespace().collect::<String>(), "{message}");
        }
        assert_eq!(split_trace(&roll.replacen(r#""roll_id""#, &format!(r#"{trace}, "roll_id""#), 1)).1, roll);

        // Over Redis and back: exactly what went in, so a stream's stored roll and the
        // same roll arriving live compare equal.
        for roll in [roll, "{}", " { } ", "{\"a\":1}\n", ROLL] {
            assert_eq!(split_trace(&with_trace(roll, &publish)).1, roll);
        }
        assert_eq!(split_trace(r#"{"_trace":{}}"#).1, "{}");

        // Written in a way that can't be cut out of the text: it still comes off.
        for odd in [
            r#"{"a":1,"_trace":null}"#,
            r#"{"_trace":{},"a":1,"_trace":{}}"#,
        ] {
            let stripped = split_trace(odd).1;
            let message: serde_json::Value = serde_json::from_str(&stripped).unwrap();
            assert!(message.get("_trace").is_none() && message["a"] == 1, "{odd} -> {stripped}");
        }
    }

    #[test]
    fn a_roll_delivered_late_or_from_storage_roots_its_own_trace() {
        let (exporter, _guard) = exporting();
        let open = tracing::info_span!("open");
        let session = Session::open("arcane.rolls_stream", &open);
        let open_sc = context_of(&open);
        drop(open);
        let publish = tracing::info_span!("roll.publish");
        let publish_sc = context_of(&publish);
        let roll = |published| {
            let _in = publish.enter();
            Carried::new(Delivery { roll: Arc::from(ROLL), published })
        };
        let deliver = |roll: Carried<Delivery>| {
            exporter.reset();
            drop(deliver_span(&session, roll, false));
            exporter.get_finished_spans().unwrap().into_iter().find(|s| s.name == "roll deliver").unwrap()
        };

        // Live: a child of the publish span.
        let live = deliver(roll(Some(SystemTime::now())));
        assert_eq!(live.span_kind, SpanKind::Consumer);
        assert_eq!(live.parent_span_id, publish_sc.span_id());
        assert_eq!(links(&live), std::slice::from_ref(&open_sc));
        // A publisher whose clock is a little ahead is still live.
        let ahead = deliver(roll(Some(SystemTime::now() + Duration::from_secs(2))));
        assert_eq!(ahead.parent_span_id, publish_sc.span_id());

        // Published too long ago, or read back from storage: a root that links to it.
        for published in [Some(SystemTime::now() - LIVE - Duration::from_secs(1)), None] {
            let late = deliver(roll(published));
            assert_eq!(late.parent_span_id, SpanId::INVALID);
            assert_ne!(late.span_context.trace_id(), publish_sc.trace_id());
            assert_eq!(links(&late), [publish_sc.clone(), open_sc.clone()]);
            assert_eq!(attr(&late, "trace_id"), Some(late.span_context.trace_id().to_string()));
        }
    }

    #[tokio::test]
    async fn drops_are_an_event_on_the_next_unit_span_and_counted_at_close() {
        let (exporter, _guard) = exporting();
        let open = tracing::info_span!("open");
        let session = Session::open("arcane.ws_session", &open);
        drop(open);

        session.frame();
        session.dropped(2);
        let unit = tracing::info_span!("unit");
        session.unit(&unit);
        drop(unit);
        let unit = span(&exporter, "unit").await;
        let dropped: Vec<_> = unit.events.iter().filter(|e| e.name == "dropped").collect();
        assert_eq!(dropped.len(), 1);
        assert_eq!(dropped[0].attributes[0].value.to_string(), "2");

        // Reported once: the next unit has none, the close span has the total.
        let next = tracing::info_span!("next unit");
        session.unit(&next);
        drop(next);
        assert!(span(&exporter, "next unit").await.events.iter().all(|e| e.name != "dropped"));
        session.dropped(1);
        drop(session);
        let close = span(&exporter, "session close").await;
        assert_eq!(attr(&close, "session.drops").as_deref(), Some("3"));
        assert_eq!(attr(&close, "session.frames").as_deref(), Some("1"));
        assert!(close.events.iter().any(|e| e.name == "dropped"), "the last drop has no later unit");
    }

    #[test]
    fn channel_user_strips_prefix() {
        assert_eq!(channel_user("arcane:rolls:miles"), Some("miles"));
        assert_eq!(channel_user("arcane:rolls:"), None);
        assert_eq!(channel_user("other:miles"), None);
    }
}
