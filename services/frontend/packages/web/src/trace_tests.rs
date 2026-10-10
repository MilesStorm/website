//! Trace structure of the roll hub and the camera proxy, against a real Redis (Docker) and
//! stand-ins for ai_pipeline, auth and SurrealDB. They assert what TRACING.md ("Streams")
//! promises: one trace per roll across the processes, sessions as open and close spans, and
//! that the `_trace` field never reaches a browser, Redis' stored roll or SurrealDB.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use api::trace::{Carried, RedisTracing};
use axum::response::IntoResponse as _;
use futures_util::{SinkExt as _, StreamExt as _};
use opentelemetry::trace::{SpanContext, SpanId, SpanKind, Status, TraceContextExt as _, TracerProvider as _};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SpanData};
use serde_json::{json, Value};
use testcontainers_modules::redis::{Redis, REDIS_PORT};
use testcontainers_modules::testcontainers::{runners::AsyncRunner as _, ContainerAsync, ImageExt as _};
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;
use tower_sessions_redis_store::fred::prelude::*;
use tracing_opentelemetry::OpenTelemetrySpanExt as _;
use tracing_subscriber::layer::SubscriberExt as _;

use crate::rolls::RollHub;

const TIMEOUT: Duration = Duration::from_secs(5);
/// What ai_pipeline's `roll.settle` span would send as the roll's `_trace`.
const SETTLE_TRACE: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
const SETTLE_SPAN: &str = "00f067aa0ba902b7";

/// The production pipeline with an exporter the test can read. Everything in these tests
/// runs on the test's own thread (`#[tokio::test]`), so the guard's subscriber sees it all.
pub(crate) fn exporting() -> (InMemorySpanExporter, tracing::subscriber::DefaultGuard) {
    opentelemetry::global::set_text_map_propagator(opentelemetry_sdk::propagation::TraceContextPropagator::new());
    let exporter = InMemorySpanExporter::default();
    let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let subscriber =
        tracing_subscriber::registry().with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));
    api::trace::several_subscribers();
    (exporter, tracing::subscriber::set_default(subscriber))
}

pub(crate) fn spans(exporter: &InMemorySpanExporter, name: &str) -> Vec<SpanData> {
    exporter.get_finished_spans().unwrap().into_iter().filter(|s| s.name == name).collect()
}

/// The span named `name`, waiting for it: spans of background work end a little later.
pub(crate) async fn span(exporter: &InMemorySpanExporter, name: &str) -> SpanData {
    for _ in 0..200 {
        if let Some(span) = spans(exporter, name).into_iter().next() {
            return span;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let names: Vec<_> = exporter.get_finished_spans().unwrap().iter().map(|s| s.name.to_string()).collect();
    panic!("no span {name:?} in {names:?}");
}

pub(crate) fn attr(span: &SpanData, key: &str) -> Option<String> {
    span.attributes.iter().find(|kv| kv.key.as_str() == key).map(|kv| kv.value.to_string())
}

pub(crate) fn links(span: &SpanData) -> Vec<SpanContext> {
    span.links.iter().map(|l| l.span_context.clone()).collect()
}

pub(crate) fn context_of(span: &tracing::Span) -> SpanContext {
    span.context().span().span_context().clone()
}

/// A Redis in Docker, gone when the handle is dropped.
async fn redis() -> (ContainerAsync<Redis>, Config) {
    let node = Redis::default().with_tag("7-alpine").start().await.expect("these tests need Docker (for Redis)");
    let url = format!("redis://{}:{}", node.get_host().await.unwrap(), node.get_host_port_ipv4(REDIS_PORT).await.unwrap());
    (node, Config::from_url(&url).unwrap())
}

/// The pool and hub as `router` builds them.
async fn hub(config: &Config, shutdown: CancellationToken) -> (RollHub, Pool) {
    let pool = api::trace::redis_pool(config.clone(), ConnectionConfig::default(), 2, RedisTracing::Commands).unwrap();
    pool.connect();
    pool.wait_for_connect().await.unwrap();
    let hub = RollHub::connect(config.clone(), ConnectionConfig::default(), pool.clone(), shutdown).await.unwrap();
    (hub, pool)
}

/// The next server-sent event of a response body, as it goes over the wire.
async fn next_event(body: &mut axum::body::BodyDataStream) -> String {
    let mut event = String::new();
    while !event.ends_with("\n\n") {
        let chunk = tokio::time::timeout(TIMEOUT, body.next()).await.expect("no event").expect("stream ended").unwrap();
        event.push_str(std::str::from_utf8(&chunk).unwrap());
    }
    event
}

#[tokio::test]
async fn a_roll_is_one_trace_from_the_camera_to_every_stream() {
    let (exporter, _guard) = exporting();
    let (_node, config) = redis().await;
    let (hub, pool) = hub(&config, CancellationToken::new()).await;

    // What goes over Redis, seen by a plain subscriber.
    let wire = api::trace::redis_subscriber(config.clone(), ConnectionConfig::default(), RedisTracing::Off).unwrap();
    wire.init().await.unwrap();
    wire.subscribe("arcane:rolls:miles").await.unwrap();
    let mut on_wire = wire.message_rx();

    let events = hub.events("miles".into(), None).await.unwrap();
    let mut stream = axum::response::sse::Sse::new(events).into_response().into_body().into_data_stream();

    // The camera side: the roll arrives in the trace of ai_pipeline's `roll.settle`.
    let settle = tracing::info_span!("roll.settle");
    let settle_sc = context_of(&settle);
    let roll = r#"{"complete":true,"dice":[],"roll_id":"18f3a2b-0badf00d","type":"roll"}"#;
    let publisher = hub.publisher("miles".into(), None);
    publisher.try_send(Carried::with_context(settle.context(), roll.to_string())).unwrap();
    drop(settle);

    // The viewer gets the roll as ai_pipeline sent it.
    assert_eq!(next_event(&mut stream).await, format!("event: roll\ndata: {roll}\n\n"));

    let publish = span(&exporter, "roll publish").await;
    assert_eq!(publish.span_kind, SpanKind::Producer);
    assert_eq!(publish.parent_span_id, settle_sc.span_id());
    assert_eq!(publish.span_context.trace_id(), settle_sc.trace_id());
    assert_eq!(attr(&publish, "messaging.system").as_deref(), Some("redis"));
    assert_eq!(attr(&publish, "messaging.destination.template").as_deref(), Some("arcane:rolls:{user}"));
    assert_eq!(publish.status, Status::Unset);

    // Its Redis commands are spans under it (fred's `partial-tracing`).
    let mut commands: Vec<String> = spans(&exporter, "fred.command")
        .iter()
        .filter(|c| c.parent_span_id == publish.span_context.span_id())
        .filter_map(|c| attr(c, "cmd.name"))
        .collect();
    commands.sort();
    assert_eq!(commands, ["PUBLISH", "SET"]);

    // Between replicas the context rides in the message; the stored roll doesn't have it.
    let message: Value = serde_json::from_str(&on_wire.recv().await.unwrap().value.convert::<String>().unwrap()).unwrap();
    let publish_sc = &publish.span_context;
    assert_eq!(message["_trace"]["traceparent"], format!("00-{}-{}-01", publish_sc.trace_id(), publish_sc.span_id()));
    let stored: String = pool.get("arcane:last_roll:miles").await.unwrap();
    assert_eq!(stored, roll);

    // The delivery continues the roll's trace, and links to the stream it went out on.
    let open = span(&exporter, "session open").await;
    let deliver = span(&exporter, "roll deliver").await;
    assert_eq!(deliver.span_kind, SpanKind::Consumer);
    assert_eq!(deliver.parent_span_id, publish_sc.span_id());
    assert_eq!(deliver.span_context.trace_id(), settle_sc.trace_id());
    assert_eq!(links(&deliver), std::slice::from_ref(&open.span_context));
    assert_eq!(attr(&deliver, "arcane.replay").as_deref(), Some("false"));

    // A stream opened later gets the stored roll as a replay: a trace of its own.
    exporter.reset();
    let events = hub.events("miles".into(), None).await.unwrap();
    let mut late = axum::response::sse::Sse::new(events).into_response().into_body().into_data_stream();
    assert_eq!(next_event(&mut late).await, format!("event: replay\ndata: {roll}\n\n"));
    let late_open = span(&exporter, "session open").await;
    let replay = span(&exporter, "roll deliver").await;
    assert_eq!(replay.parent_span_id, SpanId::INVALID);
    assert_ne!(replay.span_context.trace_id(), settle_sc.trace_id());
    assert_eq!(links(&replay), std::slice::from_ref(&late_open.span_context));
    assert_eq!(attr(&replay, "arcane.replay").as_deref(), Some("true"));
    // Reading the stored roll is part of opening the stream.
    let reads: Vec<_> = spans(&exporter, "fred.command")
        .into_iter()
        .filter(|c| c.parent_span_id == late_open.span_context.span_id())
        .filter_map(|c| attr(&c, "cmd.name"))
        .collect();
    assert_eq!(reads, ["GET"]);

    // Closing a stream leaves a close span: a root that links to the open span, with the
    // same session ID and what the stream did.
    drop(late);
    let close = span(&exporter, "session close").await;
    assert_eq!(close.parent_span_id, SpanId::INVALID);
    assert_eq!(links(&close), std::slice::from_ref(&late_open.span_context));
    assert_eq!(attr(&close, "session.rolls").as_deref(), Some("1"));
    assert_eq!(attr(&close, "session.close_reason").as_deref(), Some("disconnected"));
    let session_id = attr(&late_open, "session.id").expect("the open span has no session.id");
    assert_eq!(attr(&close, "session.id"), Some(session_id.clone()));
    assert_eq!(attr(&replay, "session.id"), Some(session_id));
    drop(stream);
}

#[tokio::test]
async fn only_clients_built_with_tracing_make_command_spans() {
    let (exporter, _guard) = exporting();
    let (_node, config) = redis().await;
    let traced = api::trace::redis_client(config.clone(), ConnectionConfig::default(), RedisTracing::Commands).unwrap();
    let untraced = api::trace::redis_client(config, ConnectionConfig::default(), RedisTracing::Off).unwrap();
    for client in [&traced, &untraced] {
        client.connect();
        client.wait_for_connect().await.unwrap();
    }

    // A loop's commands (the schema lock's renewal): no span around them, none made.
    let _: () = untraced.set("lock", "me", None, None, false).await.unwrap();
    assert!(exporter.get_finished_spans().unwrap().is_empty());

    let request = tracing::info_span!("request");
    let request_sc = context_of(&request);
    let _: Option<String> = tracing::Instrument::instrument(traced.get("lock"), request).await.unwrap();
    let command = span(&exporter, "fred.command").await;
    assert_eq!(command.parent_span_id, request_sc.span_id());
    assert_eq!(attr(&command, "cmd.name").as_deref(), Some("GET"));
    // The round trip is its child, so the wait in the client's queue shows as the difference.
    let rtt = span(&exporter, "fred.rtt").await;
    assert_eq!(rtt.parent_span_id, command.span_context.span_id());
}

/// What the stand-ins saw.
#[derive(Default)]
struct Seen {
    /// The `traceparent` of ai_pipeline's WebSocket handshake.
    ai_traceparent: Option<String>,
    /// The bodies of SurrealDB's `/rpc` requests.
    queries: Vec<String>,
}

/// ai_pipeline: answers each camera frame with a frame result and `roll`.
// The handshake callback's signature is tungstenite's.
#[allow(clippy::result_large_err)]
async fn ai_pipeline(roll: String, seen: Arc<Mutex<Seen>>) -> String {
    use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/", listener.local_addr().unwrap());
    api::trace::spawn_loop("test: stand-in ai_pipeline", async move {
        let (socket, _) = listener.accept().await.unwrap();
        let handshake = move |req: &Request, res: Response| -> Result<Response, ErrorResponse> {
            let traceparent = req.headers().get("traceparent").and_then(|v| v.to_str().ok());
            seen.lock().unwrap().ai_traceparent = traceparent.map(str::to_string);
            Ok(res)
        };
        let mut camera = tokio_tungstenite::accept_hdr_async(socket, handshake).await.unwrap();
        while let Some(Ok(message)) = camera.next().await {
            if message.is_binary() {
                let frame = json!({"type": "frame", "frame_seq": 1, "detections": []}).to_string();
                camera.send(Message::Text(frame.into())).await.unwrap();
                camera.send(Message::Text(roll.clone().into())).await.unwrap();
            }
        }
    });
    url
}

/// auth (everyone shares their rolls) and SurrealDB (accepts everything) in one server.
async fn auth_and_surrealdb(seen: Arc<Mutex<Seen>>) -> String {
    use axum::routing::post;

    let rpc = move |body: String| async move {
        seen.lock().unwrap().queries.push(body);
        json!({"id": 1, "result": [{"status": "OK", "result": null}]}).to_string()
    };
    let app = axum::Router::new()
        .route("/internal/dataset/consent", post(|| async { json!({"share": true}).to_string() }))
        .route("/signin", post(|| async { json!({"code": 200, "token": "token-1"}).to_string() }))
        .route("/rpc", post(rpc));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    api::trace::spawn_loop("test: stand-in auth and SurrealDB", async move { axum::serve(listener, app).await.unwrap() });
    base
}

#[tokio::test]
async fn a_camera_session_traces_each_roll_and_keeps_the_trace_field_to_itself() {
    let (exporter, _guard) = exporting();
    let (_node, config) = redis().await;
    let shutdown = CancellationToken::new();
    let (hub, pool) = hub(&config, shutdown.clone()).await;

    let seen = Arc::new(Mutex::new(Seen::default()));
    let roll = json!({
        "type": "roll", "roll_id": "18f3a2b-0badf00d", "frame_seq": 1, "model": "test",
        // Below the confidence that is always kept, so the capture goes all the way to the store.
        "dice": [{"value": "5", "conf": 0.5}], "total": 5, "complete": true,
    });
    let mut from_ai = roll.clone();
    from_ai["_trace"] = json!({"traceparent": format!("00-{SETTLE_TRACE}-{SETTLE_SPAN}-01")});
    let ai_url = ai_pipeline(from_ai.to_string(), seen.clone()).await;
    let backend = auth_and_surrealdb(seen.clone()).await;
    std::env::set_var("AUTH_SERVICE_URL", &backend);
    std::env::set_var("BFF_SERVICE_SECRET", "test-only");
    crate::dataset::install_for_tests(crate::dataset::Dataset::for_tests(backend, "pw"));

    // The proxy as `arcane_ws_proxy` runs it, without the login check.
    let proxy = {
        let hub = hub.clone();
        move |ws: axum::extract::ws::WebSocketUpgrade| async move {
            let open = api::detached_span!("arcane.ws_session", otel.name = "session open", user = "miles");
            ws.on_upgrade(move |socket| {
                api::trace::session(crate::proxy_ws(socket, ai_url, hub, "miles".into(), "token".into(), None, open))
            })
        }
    };
    let app = axum::Router::new().route("/ws/arcane", axum::routing::get(proxy));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/ws/arcane", listener.local_addr().unwrap());
    api::trace::spawn_loop("test: the proxy", async move { axum::serve(listener, app).await.unwrap() });

    // The browser: one camera frame (a JPEG by its first bytes), then what comes back.
    let mut browser = api::trace::connect_ws(&url, "frontend", TIMEOUT).await.unwrap();
    browser.send(Message::Binary(vec![0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3].into())).await.unwrap();
    let mut replies = Vec::new();
    while replies.len() < 2 {
        let reply = tokio::time::timeout(TIMEOUT, browser.next()).await.expect("no reply").unwrap().unwrap();
        replies.push(serde_json::from_str::<Value>(reply.to_text().unwrap()).unwrap());
    }
    assert_eq!(replies[1], roll, "the browser gets the roll without `_trace`");

    // The session's open span covers connecting to ai_pipeline, whose span joins the trace.
    let open = span(&exporter, "session open").await;
    let connect = span(&exporter, "GET /").await;
    assert_eq!(connect.span_kind, SpanKind::Client);
    assert_eq!(connect.parent_span_id, open.span_context.span_id());
    assert_eq!(attr(&connect, "peer.service").as_deref(), Some("ai-pipeline"));
    let connect_sc = &connect.span_context;
    assert_eq!(
        seen.lock().unwrap().ai_traceparent,
        Some(format!("00-{}-{}-01", connect_sc.trace_id(), connect_sc.span_id())),
    );

    // The roll continues ai_pipeline's trace: published to Redis ...
    let settle_span = SpanId::from_hex(SETTLE_SPAN).unwrap();
    let publish = span(&exporter, "roll publish").await;
    assert_eq!(publish.span_context.trace_id().to_string(), SETTLE_TRACE);
    assert_eq!(publish.parent_span_id, settle_span);
    assert_eq!(links(&publish), std::slice::from_ref(&open.span_context));
    // ... and queued for keeping its picture, which the capture worker picks up.
    let queued = span(&exporter, "roll capture").await;
    assert_eq!(queued.span_kind, SpanKind::Producer);
    assert_eq!(queued.parent_span_id, settle_span);
    assert_eq!(links(&queued), std::slice::from_ref(&open.span_context));
    let capture = span(&exporter, "arcane.capture").await;
    assert_eq!(capture.span_kind, SpanKind::Consumer);
    assert_eq!(capture.parent_span_id, queued.span_context.span_id());
    let save = span(&exporter, "dataset.save").await;
    assert_eq!(save.parent_span_id, capture.span_context.span_id());
    let query = span(&exporter, "surrealdb.query").await;
    assert_eq!(query.span_kind, SpanKind::Client);
    assert_eq!(query.parent_span_id, save.span_context.span_id());
    assert_eq!(query.span_context.trace_id().to_string(), SETTLE_TRACE);
    assert_eq!(attr(&query, "db.operation.name").as_deref(), Some("LET"));
    assert_eq!(attr(&query, "peer.service").as_deref(), Some("surrealdb"));
    let signin = span(&exporter, "surrealdb.signin").await;
    assert_eq!(signin.parent_span_id, save.span_context.span_id());

    // Nothing stored knows about the trace: the sample, the roll held for flagging, the
    // last roll.
    let queries = seen.lock().unwrap().queries.clone();
    let [saved] = &queries[..] else { panic!("{} SurrealDB queries", queries.len()) };
    assert!(!saved.contains("_trace") && !saved.contains(SETTLE_TRACE), "{saved}");
    let saved: Value = serde_json::from_str(saved).unwrap();
    assert_eq!(saved["params"][1]["roll"], roll);
    let last: String = pool.get("arcane:last_roll:miles").await.unwrap();
    assert_eq!(serde_json::from_str::<Value>(&last).unwrap(), roll);
    let held: String = pool.get("arcane:held:miles").await.unwrap();
    assert!(!held.contains("_trace") && !held.contains(SETTLE_TRACE), "{held}");

    // A shutdown ends the session; its close span says what it did.
    shutdown.cancel();
    while let Ok(Some(Ok(message))) = tokio::time::timeout(TIMEOUT, browser.next()).await {
        assert!(message.is_close(), "unexpected message at shutdown: {message:?}");
    }
    let close = span(&exporter, "session close").await;
    assert_eq!(close.parent_span_id, SpanId::INVALID);
    assert_eq!(links(&close), std::slice::from_ref(&open.span_context));
    assert_eq!(attr(&close, "session.id"), attr(&open, "session.id"));
    assert_eq!(attr(&open, "session.kind").as_deref(), Some("arcane.ws_session"));
    assert_eq!(attr(&close, "session.kind").as_deref(), Some("arcane.ws_session"));
    assert_eq!(attr(&close, "session.frames").as_deref(), Some("1"));
    assert_eq!(attr(&close, "session.rolls").as_deref(), Some("1"));
    assert_eq!(attr(&close, "session.drops").as_deref(), Some("0"));
    assert_eq!(attr(&close, "session.close_reason").as_deref(), Some("shutdown"));
}

#[tokio::test]
async fn a_permission_recheck_is_a_trace_of_its_own_linked_to_the_session() {
    let (exporter, _guard) = exporting();
    let (_node, config) = redis().await;
    let (hub, _pool) = hub(&config, CancellationToken::new()).await;

    let open = tracing::info_span!("open");
    let session = crate::rolls::Session::open("arcane.ws_session", &open);
    let open_sc = context_of(&open);
    drop(open);
    session.dropped(3);

    // A session ID that isn't in Redis: logged out.
    let gone = tower_sessions::session::Id::default();
    assert!(!hub.recheck(&session, Some(gone), "miles").await);

    let recheck = span(&exporter, "permission recheck").await;
    assert_eq!(recheck.parent_span_id, SpanId::INVALID);
    assert_ne!(recheck.span_context.trace_id(), open_sc.trace_id());
    assert_eq!(links(&recheck), [open_sc]);
    // The session read is under it, and the frames dropped since the last unit are on it.
    let read = span(&exporter, "fred.command").await;
    assert_eq!(read.parent_span_id, recheck.span_context.span_id());
    let dropped = recheck.events.iter().find(|e| e.name == "dropped").expect("no dropped event");
    assert_eq!(dropped.attributes[0].value.to_string(), "3");
}

/// A router with a stream that ends at shutdown (as the roll stream does) and one that
/// never ends, served by `serve_until`.
async fn serving(shutdown: CancellationToken, wait: Duration) -> (String, tokio::task::JoinHandle<()>) {
    let stops = shutdown.clone();
    let stream = move |cooperative: bool| {
        let stops = stops.clone();
        move || async move {
            let chunks = futures_util::stream::unfold(true, move |first| {
                let stops = stops.clone();
                async move {
                    if first {
                        return Some((Ok::<_, std::convert::Infallible>("open"), false));
                    }
                    if cooperative {
                        stops.cancelled().await;
                        return None;
                    }
                    std::future::pending().await
                }
            });
            axum::body::Body::from_stream(chunks)
        }
    };
    let app = axum::Router::new()
        .route("/stream", axum::routing::get(stream(true)))
        .route("/stuck", axum::routing::get(stream(false)));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = api::trace::spawn_loop("test: the server", crate::serve_until(listener, app, shutdown, wait));
    (base, server)
}

#[tokio::test]
async fn shutdown_ends_streams_and_waits_for_open_requests_only_so_long() {
    let peer = api::trace::Peer { service: "frontend", system: None };
    let client = api::trace::client(peer, Duration::from_secs(30), |b| b).unwrap();

    // Streams that watch the shutdown end, and the server stops once they have.
    let shutdown = CancellationToken::new();
    let (base, server) = serving(shutdown.clone(), Duration::from_secs(20)).await;
    let mut stream = client.get(format!("{base}/stream")).send().await.unwrap();
    assert_eq!(stream.chunk().await.unwrap().as_deref(), Some(&b"open"[..]));
    shutdown.cancel();
    assert_eq!(stream.chunk().await.unwrap(), None, "the stream ends at shutdown");
    tokio::time::timeout(Duration::from_secs(15), server).await.expect("still serving").unwrap();

    // A request that never finishes holds the shutdown up for `wait`, not longer.
    let shutdown = CancellationToken::new();
    let wait = Duration::from_millis(300);
    let (base, server) = serving(shutdown.clone(), wait).await;
    let mut stuck = client.get(format!("{base}/stuck")).send().await.unwrap();
    assert_eq!(stuck.chunk().await.unwrap().as_deref(), Some(&b"open"[..]));
    let started = std::time::Instant::now();
    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(5), server).await.expect("the wait has no end").unwrap();
    assert!(started.elapsed() >= wait, "gave up after {:?}", started.elapsed());
}
