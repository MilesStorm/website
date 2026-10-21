use std::collections::HashMap;

mod rolls;
mod capture;
mod dataset;
mod sharing;
#[cfg(test)]
mod trace_tests;
mod account;
mod emails;
mod invites;
mod json_api;
mod pages;

fn main() -> std::process::ExitCode {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        // `web openapi`: the JSON API's OpenAPI document, with no .env, Redis or network.
        Some("openapi") => {
            print!("{}", json_api::openapi_json());
            return std::process::ExitCode::SUCCESS;
        }
        // `web check-pages <dir>`: whether an Astro build has exactly the pages (the image's
        // verify stage; the server checks the same at startup).
        Some("check-pages") => {
            let dir = std::path::PathBuf::from(args.next().unwrap_or_else(|| "web/dist".into()));
            return match pages::Built::load(&dir, api_types::PAGES) {
                Ok(_) => {
                    println!("{}: every page in PAGES and no other", dir.display());
                    std::process::ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("{e:#}");
                    std::process::ExitCode::FAILURE
                }
            };
        }
        _ => {}
    }
    match dotenvy::dotenv() {
        Ok(_) => {}
        Err(_) if !cfg!(debug_assertions) => {}
        Err(e) => panic!("could not load .env: {e}"),
    }
    server_launch();
    std::process::ExitCode::SUCCESS
}

// ---- Server launch ----

/// The exporters' providers, kept to flush them before the process ends.
static TELEMETRY: std::sync::OnceLock<(opentelemetry_sdk::trace::SdkTracerProvider, opentelemetry_sdk::logs::SdkLoggerProvider)> =
    std::sync::OnceLock::new();

/// Sends the spans and log records still batched. Blocks until the collector has them.
pub(crate) fn flush_telemetry() {
    if let Some((traces, logs)) = TELEMETRY.get() {
        let _ = traces.force_flush();
        let _ = logs.force_flush();
    }
}

/// How long a shutdown waits for open connections, camera sessions and detached tasks.
/// Kubernetes kills the pod 30 s after SIGTERM; telemetry is flushed after the wait.
const SHUTDOWN_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

fn server_launch() {
    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry_otlp::WithExportConfig;
    use opentelemetry_sdk::{logs::SdkLoggerProvider, trace::SdkTracerProvider};
    use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

    let redis_host = std::env::var("REDIS_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let redis_port = std::env::var("REDIS_PORT").unwrap_or_else(|_| "6379".to_string());
    let redis_password = std::env::var("REDIS_PASSWORD")
        .ok()
        .filter(|s| !s.is_empty());
    let redis_url = match &redis_password {
        Some(p) => format!("redis://:{p}@{redis_host}:{redis_port}"),
        None => format!("redis://{redis_host}:{redis_port}"),
    };

    // Runtime for the OTLP (tonic) exporters, which the batch processors' own threads call
    // into. Kept until the providers have shut down (the last thing this function does).
    let _otel_rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .thread_name("otel-exporter")
        .build()
        .expect("failed to build OTel runtime");

    // Always register W3C trace-context propagator so OtelAxumLayer and
    // TracingMiddleware both work even when OTLP export is disabled.
    opentelemetry::global::set_text_map_propagator(
        opentelemetry_sdk::propagation::TraceContextPropagator::new(),
    );

    let (otel_layer, _otel_log_provider): (Option<_>, Option<SdkLoggerProvider>) = _otel_rt
        .block_on(async {
            match std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT") {
                Ok(endpoint) => {
                    let exporter = opentelemetry_otlp::SpanExporter::builder()
                        .with_tonic()
                        .with_endpoint(endpoint.clone())
                        .build()
                        .expect("failed to build OTLP exporter");

                    let provider = SdkTracerProvider::builder()
                        .with_batch_exporter(exporter)
                        .with_resource(otel_resource())
                        .build();

                    let tracer = provider.tracer("web");
                    opentelemetry::global::set_tracer_provider(provider.clone());

                    let log_exporter = opentelemetry_otlp::LogExporter::builder()
                        .with_tonic()
                        .with_endpoint(endpoint)
                        .build()
                        .expect("failed to build OTLP log exporter");

                    let log_provider = SdkLoggerProvider::builder()
                        .with_batch_exporter(log_exporter)
                        .with_resource(otel_resource())
                        .build();
                    let _ = TELEMETRY.set((provider, log_provider.clone()));

                    (
                        Some(tracing_opentelemetry::layer().with_tracer(tracer)),
                        Some(log_provider),
                    )
                }
                Err(_) => (None, None),
            }
        });

    let otel_log_layer = _otel_log_provider.as_ref().map(otel_log_bridge);

    // The OTel log bridge additionally ships log events via OTLP so Loki entries carry
    // trace_id/span_id, enabling Tempo → Loki correlation.
    // The filter also decides which spans exist (TRACING.md, "Log level").
    tracing_subscriber::registry()
        .with(EnvFilter::new(std::env::var("RUST_LOG").unwrap_or_else(
            |_| "info,tower_sessions=warn,opentelemetry=warn".into(),
        )))
        .with(tracing_subscriber::fmt::layer().json())
        .with(otel_layer)
        .with(otel_log_layer)
        .init();

    // Startup work stays out of requests (TRACING.md, "Rules for new code").
    api::init_http_client();

    let store = dataset::Dataset::from_env();
    if store.is_none() {
        tracing::warn!("SURREAL_URL/SURREAL_USER/SURREAL_PASS not set: roll sharing and flagging are off");
    }
    // Taken by the first server start only (the schema is applied once per process).
    let store = std::sync::Mutex::new(store);
    let shutdown = tokio_util::sync::CancellationToken::new();

    let app = {
        let shutdown = shutdown.clone();
        move || router(redis_url.clone(), store.lock().ok().and_then(|mut s| s.take()), shutdown.clone())
    };

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build the runtime")
        .block_on(serve(app, shutdown));

    // Everything still batched goes out before the process ends.
    if let Some((traces, logs)) = TELEMETRY.get() {
        if let Err(e) = traces.shutdown() {
            eprintln!("flushing traces at shutdown failed: {e}");
        }
        if let Err(e) = logs.shutdown() {
            eprintln!("flushing logs at shutdown failed: {e}");
        }
    }
}

/// Binds `IP`:`PORT` (default 127.0.0.1:8080) and serves `app`'s router until SIGTERM, then
/// shuts down gracefully.
async fn serve<F>(mut app: impl FnMut() -> F, shutdown: tokio_util::sync::CancellationToken)
where
    F: std::future::Future<Output = anyhow::Result<axum::Router>>,
{
    let ip = std::env::var("IP").ok().and_then(|ip| ip.parse().ok()).unwrap_or(std::net::Ipv4Addr::LOCALHOST.into());
    let port = std::env::var("PORT").ok().and_then(|port| port.parse().ok()).unwrap_or(8080);
    let addr = std::net::SocketAddr::new(ip, port);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .unwrap_or_else(|e| panic!("failed to bind to address {addr}: {e}"));
    let router = app().await.expect("failed to build the router");

    let stop = shutdown.clone();
    api::trace::spawn_loop("shutdown: waits for SIGTERM or Ctrl-C", async move {
        terminated().await;
        tracing::info!("shutting down: closing streams and waiting for open requests");
        stop.cancel();
    });
    serve_until(listener, router, shutdown, SHUTDOWN_WAIT).await;
}

/// Serves until `shutdown` is cancelled, then waits at most `wait` for what is still
/// running: open requests (roll streams end by themselves: they watch `shutdown` too), then
/// camera sessions and detached tasks (`api::trace`), which axum doesn't count.
async fn serve_until(
    listener: tokio::net::TcpListener,
    router: axum::Router,
    shutdown: tokio_util::sync::CancellationToken,
    wait: std::time::Duration,
) {
    let server = axum::serve(listener, router).with_graceful_shutdown(shutdown.clone().cancelled_owned());
    let drained = async {
        if let Err(e) = server.await {
            tracing::error!(error = %e, "the server stopped with an error");
        }
        api::trace::drain(wait).await
    };
    let deadline = async {
        shutdown.cancelled().await;
        tokio::time::sleep(wait).await;
    };
    tokio::select! {
        done = drained => {
            if done {
                tracing::info!("shut down: nothing left running");
            } else {
                tracing::warn!(waited = ?wait, "shutting down with tasks still running");
            }
        }
        _ = deadline => tracing::warn!(waited = ?wait, "shutting down with connections still open"),
    }
}

/// Resolves when the process is asked to stop: SIGTERM (Kubernetes) or Ctrl-C.
async fn terminated() {
    #[cfg(unix)]
    {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to listen for SIGTERM");
        tokio::select! {
            _ = sigterm.recv() => {}
            _ = tokio::signal::ctrl_c() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}

/// The website's router: the pages, the API and the other axum routes, with their Redis
/// clients and background loops. `shutdown` ends the long-lived streams.
async fn router(
    redis_url: String,
    store: Option<dataset::Dataset>,
    shutdown: tokio_util::sync::CancellationToken,
) -> anyhow::Result<axum::Router> {
    use api::trace::RedisTracing;
    use axum::{routing::get, Router};
    use axum_prometheus::PrometheusMetricLayer;
    use axum_tracing_opentelemetry::middleware::{OtelAxumLayer, OtelInResponseLayer};
    use tower_sessions::cookie::time::Duration;
    use tower_sessions::cookie::SameSite;
    use tower_sessions::{Expiry, SessionManagerLayer};
    use tower_sessions_redis_store::fred::prelude::*;
    use tower_sessions_redis_store::fred::socket2::TcpKeepalive;
    use tower_sessions_redis_store::RedisStore;

    let config = Config::from_url(&redis_url).expect("invalid Redis URL");
    let con_conf = ConnectionConfig {
        tcp: TcpConfig {
            nodelay: Some(true),
            keepalive: Some(
                TcpKeepalive::new()
                    .with_time(std::time::Duration::from_secs(30))
                    .with_interval(std::time::Duration::from_secs(10))
                    .with_retries(3),
            ),
            ..Default::default()
        },
        ..Default::default()
    };
    // Every command of the pool is used inside a span (a request, a roll, a recheck).
    let pool = api::trace::redis_pool(config.clone(), con_conf.clone(), 6, RedisTracing::Commands)
        .expect("failed to build Redis pool");
    pool.connect();
    pool.wait_for_connect()
        .await
        .expect("failed to connect to Redis");
    if let Some(store) = store {
        // The schema lock is polled and renewed in a loop: a client without command spans.
        let lock_redis = api::trace::redis_client(config.clone(), con_conf.clone(), RedisTracing::Off)
            .expect("failed to build the Redis client");
        lock_redis.connect();
        lock_redis
            .wait_for_connect()
            .await
            .expect("failed to connect to Redis");
        api::trace::spawn_loop("SurrealDB token: renewed before it expires", store.clone().keep_signed_in());
        api::trace::spawn_loop("roll-sharing schema: applied once at startup", dataset::start(store, lock_redis));
    }
    let roll_hub = rolls::RollHub::connect(config, con_conf, pool.clone(), shutdown)
        .await
        .expect("failed to start the arcane roll subscriber");
    let session_store = api::trace::TracedStore::new(RedisStore::new(pool), "redis");

    let layer = SessionManagerLayer::new(session_store)
        .with_secure(!cfg!(debug_assertions))
        .with_same_site(SameSite::Lax)
        .with_name("milesstorm.bff")
        .with_expiry(Expiry::OnInactivity(Duration::days(7)));

    let (prometheus_layer, metric_handle) = PrometheusMetricLayer::pair();
    // Astro's pages, and its 404 page for any other path.
    let built = pages::from_env()?;

    let routes = Router::new()
        .route("/oauth/start/{provider}", get(oauth_start))
        .route("/oauth/callback/{provider}", get(oauth_callback))
        .route("/ws/arcane", arcane_ws_route())
        .merge(json_api::router())
        .route(
            "/metrics",
            get(move || async move { metric_handle.render() }),
        );
    let router = pages::mount(routes, built)
        .layer(axum::Extension(roll_hub))
        .layer(axum::middleware::from_fn(no_store))
        .layer(layer)
        .layer(axum::middleware::from_fn(api::trace::capture_request_context))
        .layer(axum::middleware::from_fn(api::trace::end_span_with_body))
        .layer(OtelInResponseLayer)
        // Prometheus scrapes every 15s; a trace each would bury the real ones.
        .layer(OtelAxumLayer::default().filter(|path| path != "/metrics"))
        .layer(prometheus_layer);
    Ok(router)
}

/// `service.version` is the commit the image was built from (Dockerfile `GIT_SHA`).
/// `OTEL_RESOURCE_ATTRIBUTES` adds the rest (`deployment.environment.name`).
fn otel_resource() -> opentelemetry_sdk::Resource {
    opentelemetry_sdk::Resource::builder()
        .with_service_name("frontend")
        .with_attribute(opentelemetry::KeyValue::new(
            "service.version",
            std::env::var("GIT_SHA").unwrap_or_else(|_| "unknown".into()),
        ))
        .build()
}

/// The OTLP log bridge. Log records take their trace_id/span_id from the OTel context
/// tracing-opentelemetry activates with each span. The SDK's own logs (its warnings, such as
/// dropped spans, go to stdout at `warn`) stay out: exporting them would log more.
fn otel_log_bridge<S>(provider: &opentelemetry_sdk::logs::SdkLoggerProvider) -> impl tracing_subscriber::Layer<S>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    use tracing_subscriber::Layer as _;
    opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge::new(provider)
        .with_filter(tracing_subscriber::filter::filter_fn(|meta| !meta.target().starts_with("opentelemetry")))
}

/// `Cache-Control` on API responses (`no-store`) and pages (`private, no-cache`), unless the
/// handler chose its own. Neither may be replayed by a shared cache: pages carry the request's
/// `<meta name="traceparent">`, and the browser's clock correction takes its samples from both
/// (TRACING.md, "Browser clock"). Cloudflare caches neither today; this keeps a future rule from
/// changing that.
async fn no_store(req: axum::extract::Request, next: axum::middleware::Next) -> axum::response::Response {
    use axum::http::header::{HeaderValue, CACHE_CONTROL, CONTENT_TYPE};
    let path = req.uri().path();
    let api = path.starts_with("/api/");
    let mut res = next.run(req).await;
    let html = res
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("text/html"));
    if (api || html) && !res.headers().contains_key(CACHE_CONTROL) {
        // Pages may stay in the browser's back/forward cache, which `no-store` turns off.
        let value = if api { "no-store" } else { "private, no-cache" };
        res.headers_mut().insert(CACHE_CONTROL, HeaderValue::from_static(value));
    }
    res
}

// ---- Arcane WebSocket proxy ----

/// `/ws/arcane`: the origin is checked first, so a foreign page's handshake costs no call to auth.
fn arcane_ws_route() -> axum::routing::MethodRouter {
    axum::routing::get(arcane_ws_proxy).route_layer(axum::middleware::from_fn(camera_origin))
}

/// Browsers send `Origin` on a WebSocket handshake: it must be this site's ([`rolls::origin_allowed`]).
async fn camera_origin(req: axum::extract::Request, next: axum::middleware::Next) -> axum::response::Response {
    use axum::response::IntoResponse;
    if rolls::origin_allowed(req.headers()) {
        return next.run(req).await;
    }
    tracing::warn!(origin = ?req.headers().get(axum::http::header::ORIGIN), "arcane WebSocket rejected: foreign origin");
    axum::http::StatusCode::FORBIDDEN.into_response()
}

/// Upgrades to WebSocket and bidirectionally proxies to the ai_pipeline inference service.
/// Requires the `arcane` permission (and a same-host Origin, [`arcane_ws_route`]): 401, 403 or
/// 503 otherwise. Settled rolls are published for the user's other devices (see `rolls`).
async fn arcane_ws_proxy(
    viewer: Result<json_api::Viewer, json_api::ApiError>,
    ws: Result<axum::extract::ws::WebSocketUpgrade, axum::extract::ws::rejection::WebSocketUpgradeRejection>,
    uri: axum::http::Uri,
    hub: Option<axum::Extension<rolls::RollHub>>,
    session: tower_sessions::Session,
) -> axum::response::Response {
    use axum::{http::StatusCode, response::IntoResponse};

    // Who is asking: the one viewer of the request (one introspect), with the same rule as
    // the `/api/arcane/*` routes. Logged out 401, without `arcane` 403, auth down 503: a
    // handshake answer, without a body.
    let allowed = viewer.and_then(|v| {
        json_api::allowed(api_types::Access::Perm(api_types::KnownPermission::Arcane), &v)?;
        Ok(v)
    });
    let viewer = match allowed {
        Ok(v) => v,
        Err(e) => {
            if e.0.error == api_types::ErrorCode::Forbidden {
                tracing::warn!("arcane WebSocket rejected: permission denied");
            }
            return StatusCode::from_u16(e.0.status()).unwrap_or(StatusCode::FORBIDDEN).into_response();
        }
    };
    // Who may connect is decided first, for any request to this path.
    let ws = match ws {
        Ok(ws) => ws,
        Err(rejection) => return rejection.into_response(),
    };
    let (Ok(user), Ok(token)) = (viewer.username(), viewer.token()) else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let (user, token) = (user.to_string(), token.to_string());
    let Some(axum::Extension(hub)) = hub else {
        tracing::error!("no roll hub on a camera WebSocket request");
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };

    let ai_url = std::env::var("AI_PIPELINE_SERVICE_URL")
        .unwrap_or_else(|_| "ws://localhost:9000".to_string());

    tracing::info!(upstream = %ai_url, "upgrading arcane WebSocket");
    let session_id = session.id();
    // The camera session outlives this request (it ends at the 101). This span is its short
    // open span, in the request's trace: it ends once ai_pipeline is connected (whose
    // connection span joins it). What the session does after that is traced per roll.
    let open = api::detached_span!("arcane.ws_session", otel.name = "session open", user = %user);
    // The browser's `camera session` span (assets/trace.js), passed in the URL.
    if let Some(tp) = query_traceparent(&uri) {
        api::trace::link_traceparent(&open, tp);
    }
    // Camera frames are a few hundred KB; anything much larger isn't a frame.
    // axum spawns the callback itself; `session` makes it a task shutdown waits for.
    ws.max_message_size(4 * 1024 * 1024).on_upgrade(move |socket| {
        api::trace::session(proxy_ws(socket, ai_url, hub, user, token, session_id, open))
    })
}

/// ai_pipeline is in the cluster: a handshake that takes longer than this has failed.
const UPSTREAM_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

async fn proxy_ws(
    client: axum::extract::ws::WebSocket,
    upstream_url: String,
    hub: rolls::RollHub,
    user: String,
    token: String,
    session_id: Option<tower_sessions::session::Id>,
    open: tracing::Span,
) {
    use api::trace::Carried;
    use axum::extract::ws::Message as AxMsg;
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message as TngMsg;
    use tracing::Instrument as _;

    let connected = api::trace::connect_ws(&upstream_url, "ai-pipeline", UPSTREAM_CONNECT_TIMEOUT)
        .instrument(open.clone())
        .await;
    let session = rolls::Session::open("arcane.ws_session", &open);
    let upstream = match connected {
        Ok(upstream) => upstream,
        Err(e) => {
            open.in_scope(|| {
                tracing::error!(error = %e, upstream = %upstream_url, "ai_pipeline connect failed");
                api::trace::failed("upstream_connect");
            });
            session.set_reason("ai_pipeline connect failed");
            return;
        }
    };
    drop(open);

    let (mut upstream_tx, mut upstream_rx) = upstream.split();
    // Rolls go to Redis in order through one queue; never awaited here, so Redis
    // latency can't stall the frame stream.
    let rolls_tx = hub.publisher(user.clone(), session.link());
    // Separate queue for keeping roll pictures, so the database can't slow rolls.
    let capture_tx = hub.capturer(user.clone(), token);
    // Recent frames by number, to find the exact one each roll was read from.
    let frames = std::sync::Mutex::new(capture::FrameRing::default());
    let mut last_frame: Option<(u64, String)> = None;
    let (mut client_tx, mut client_rx) = client.split();

    let reason = tokio::select! {
        // Browser sends binary camera frames → forward to ai_pipeline.
        _ = async {
            while let Some(Ok(msg)) = client_rx.next().await {
                match msg {
                    AxMsg::Binary(b) => {
                        session.frame();
                        frames.lock().unwrap().push(&b);
                        if upstream_tx.send(TngMsg::Binary(b)).await.is_err() { break; }
                    }
                    AxMsg::Close(_) => break,
                    _ => {}
                }
            }
        } => "browser closed",
        // ai_pipeline sends JSON detection results → forward to browser.
        _ = async {
            while let Some(Ok(msg)) = upstream_rx.next().await {
                match msg {
                    TngMsg::Text(t) => {
                        // A roll comes with the context of its trace (`_trace`), which goes
                        // no further than this: not to the browser, Redis' `last_roll` or
                        // the stored sample.
                        let (trace, t) = rolls::split_trace(&t);
                        let (is_roll, seq) = reply_info(&t);
                        if is_roll {
                            session.roll();
                            // The CONSUMER of the message ai_pipeline's `ws.send` span sent
                            // (its child): handing the roll to the Redis publisher and the
                            // capture queue, whose spans are this one's children.
                            let receive = tracing::info_span!(
                                parent: None,
                                "roll.receive",
                                otel.name = "roll receive",
                                otel.kind = "consumer",
                                messaging.system = "websocket",
                                messaging.operation.name = "process",
                                "messaging.operation.type" = "process",
                                trace_id = tracing::field::Empty,
                                span_id = tracing::field::Empty,
                            );
                            api::trace::continue_from(&receive, trace.cx);
                            session.unit(&receive);
                            let received = tracing_opentelemetry::OpenTelemetrySpanExt::context(&receive);
                            let _receiving = receive.enter();
                            if rolls_tx.try_send(Carried::with_context(received.clone(), t.to_string())).is_err() {
                                tracing::warn!("arcane roll dropped: Redis publish queue full");
                                session.dropped(1);
                            }
                            // Reserve first, so a full queue makes no (empty) capture span.
                            match capture_tx.try_reserve() {
                                Ok(permit) => {
                                    let span = tracing::info_span!(
                                        parent: None,
                                        "roll.capture",
                                        otel.name = "roll capture",
                                        otel.kind = "producer",
                                        messaging.system = "tokio",
                                        messaging.operation.name = "send",
                                        "messaging.operation.type" = "send",
                                        messaging.destination.name = capture::CAPTURE_QUEUE_NAME,
                                        trace_id = tracing::field::Empty,
                                        span_id = tracing::field::Empty,
                                    );
                                    api::trace::continue_from(&span, received);
                                    session.unit(&span);
                                    permit.send(span.in_scope(|| Carried::new(capture::CaptureJob {
                                        roll: t.to_string(),
                                        frame: last_frame.take().filter(|(s, _)| Some(*s) == seq).map(|(_, f)| f),
                                        jpeg: seq.and_then(|s| frames.lock().unwrap().get(s)),
                                    })));
                                }
                                Err(_) => {
                                    tracing::debug!("roll capture skipped: queue full");
                                    session.dropped(1);
                                }
                            }
                        } else if let Some(s) = seq {
                            last_frame = Some((s, t.to_string()));
                        }
                        if client_tx.send(AxMsg::Text(t.into_owned().into())).await.is_err() { break; }
                    }
                    TngMsg::Close(_) => break,
                    _ => {}
                }
            }
        } => "ai_pipeline closed",
        // Logging out or losing the permission ends the camera session too.
        _ = async {
            let start = tokio::time::Instant::now() + rolls::RECHECK;
            let mut recheck = tokio::time::interval_at(start, rolls::RECHECK);
            loop {
                recheck.tick().await;
                if !hub.recheck(&session, session_id, &user).await { break; }
            }
        } => {
            tracing::info!("arcane WebSocket closed: session ended or permission removed");
            "session ended or permission removed"
        }
        _ = hub.shutdown.cancelled() => "shutdown",
    };
    session.set_reason(reason);
}

/// The `traceparent` query parameter of the camera WebSocket URL. Its value is hex and
/// dashes, so it needs no percent-decoding.
fn query_traceparent(uri: &axum::http::Uri) -> Option<&str> {
    uri.query()?.split('&').find_map(|pair| pair.strip_prefix("traceparent="))
}

/// From an ai_pipeline reply (parsed once): whether it is a roll event, and its
/// `frame_seq`, the number of the forwarded frame it was computed from.
fn reply_info(text: &str) -> (bool, Option<u64>) {
    if !text.contains("\"frame_seq\"") {
        return (rolls::is_roll(text), None);
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else { return (false, None) };
    let is_roll = v.get("type").and_then(|t| t.as_str()) == Some("roll");
    (is_roll, v.get("frame_seq").and_then(|s| s.as_u64()))
}

// ---- OAuth Axum handlers ----

const OAUTH_CSRF_KEY: &str = "oauth_csrf_state";
const OAUTH_PROVIDER_KEY: &str = "oauth_provider";
/// The page to land on after the login: a [`api_types::PagePath`], never the browser's string.
const OAUTH_NEXT_KEY: &str = "oauth_next";

/// Begin the OAuth flow for `provider`. Asks auth (cluster-internal) for the provider's
/// authorization URL, stashes the CSRF state in the BFF session, and redirects the browser.
/// `?next=` is kept in the session for the callback when it is one of the site's pages; anything
/// else is dropped (logged), and the login goes on.
async fn oauth_start(
    axum::extract::Path(provider): axum::extract::Path<String>,
    session: tower_sessions::Session,
    axum::extract::Query(params): axum::extract::Query<HashMap<String, String>>,
) -> axum::response::Response {
    use axum::response::{IntoResponse, Redirect};

    if provider != "github" && provider != "google" {
        return Redirect::to(&api_types::OAuthError::UnknownProvider.login_page()).into_response();
    }
    let next = params.get("next").and_then(|next| match next.parse::<api_types::PagePath>() {
        Ok(page) => Some(page),
        Err(()) => {
            let shown: String = next.chars().take(64).collect();
            tracing::warn!(next = ?shown, %provider, "oauth_start: next isn't a page of the site; dropped");
            None
        }
    });

    match api::start_oauth(&provider).await {
        Ok((auth_url, state)) => {
            if let Err(e) = session.insert(OAUTH_CSRF_KEY, &state).await {
                tracing::error!(error = %e, %provider, "oauth_start: failed to write CSRF state");
                return Redirect::to(&api_types::OAuthError::SessionFailed.login_page()).into_response();
            }
            if let Err(e) = session.insert(OAUTH_PROVIDER_KEY, &provider).await {
                tracing::error!(error = %e, %provider, "oauth_start: failed to write provider");
                return Redirect::to(&api_types::OAuthError::SessionFailed.login_page()).into_response();
            }
            // An earlier start's page goes either way.
            let stored = match next {
                Some(page) => session.insert(OAUTH_NEXT_KEY, page).await,
                None => session.remove::<api_types::PagePath>(OAUTH_NEXT_KEY).await.map(|_| ()),
            };
            if let Err(e) = stored {
                tracing::error!(error = %e, %provider, "oauth_start: failed to write next");
                return Redirect::to(&api_types::OAuthError::SessionFailed.login_page()).into_response();
            }
            Redirect::to(&auth_url).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, %provider, "oauth_start: api::start_oauth failed");
            Redirect::to(&api_types::OAuthError::StartFailed.login_page()).into_response()
        }
    }
}

/// Provider callback. Validates CSRF state, asks auth to exchange the code, and
/// stores the resulting opaque token + username on the BFF session. Lands on
/// [`json_api::next_page`]: the invite page, else the start's `next`, else `/`.
async fn oauth_callback(
    axum::extract::Path(provider): axum::extract::Path<String>,
    session: tower_sessions::Session,
    axum::extract::Query(params): axum::extract::Query<HashMap<String, String>>,
) -> axum::response::Response {
    use axum::response::{IntoResponse, Redirect};

    let Some(code) = params.get("code").cloned() else {
        return Redirect::to(&api_types::OAuthError::MissingCode.login_page()).into_response();
    };
    let Some(state) = params.get("state").cloned() else {
        return Redirect::to(&api_types::OAuthError::MissingState.login_page()).into_response();
    };

    let expected_state: Option<String> = session.get(OAUTH_CSRF_KEY).await.ok().flatten();
    let expected_provider: Option<String> = session.get(OAUTH_PROVIDER_KEY).await.ok().flatten();
    let _ = session.remove::<String>(OAUTH_CSRF_KEY).await;
    let _ = session.remove::<String>(OAUTH_PROVIDER_KEY).await;
    // A page that no longer exists (stored before a deploy) reads as none.
    let next: Option<api_types::PagePath> = session.remove(OAUTH_NEXT_KEY).await.ok().flatten();

    if expected_state.as_deref() != Some(&state) || expected_provider.as_deref() != Some(&provider)
    {
        return Redirect::to(&api_types::OAuthError::CsrfMismatch.login_page()).into_response();
    }

    match api::exchange_oauth_code(&provider, &code).await {
        Ok((token, username)) => {
            if api::fresh_session_id(&session).await.is_err() {
                return Redirect::to(&api_types::OAuthError::SessionFailed.login_page()).into_response();
            }
            if let Err(e) = session.insert("opaque_token", &token).await {
                tracing::error!(error = %e, %provider, "oauth_callback: failed to write opaque_token");
                return Redirect::to(&api_types::OAuthError::SessionFailed.login_page()).into_response();
            }
            if let Err(e) = session.insert("username", username).await {
                tracing::error!(error = %e, %provider, "oauth_callback: failed to write username");
                return Redirect::to(&api_types::OAuthError::SessionFailed.login_page()).into_response();
            }
            // Came from an invite link: back to it (the invite was just redeemed).
            api::redeem_pending_invite(&session, &token).await;
            Redirect::to(&json_api::next_page(&session, next).await.href()).into_response()
        }
        Err(e) if e.contains("email_exists") || e.contains("Email already in use") => {
            Redirect::to(&api_types::OAuthError::EmailExists.login_page()).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, %provider, "oauth_callback: exchange_oauth_code failed");
            Redirect::to(&api_types::OAuthError::ExchangeFailed.login_page()).into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use opentelemetry::trace::{TraceContextExt as _, TracerProvider as _};
    use opentelemetry_sdk::logs::{InMemoryLogExporter, SdkLoggerProvider};
    use tracing_opentelemetry::OpenTelemetrySpanExt as _;
    use tracing_subscriber::layer::SubscriberExt as _;

    fn test_client() -> api::trace::TracedClient {
        let peer = api::trace::Peer { service: "frontend", system: None };
        api::trace::client(peer, std::time::Duration::from_secs(5), |b| b).unwrap()
    }

    #[test]
    fn camera_websocket_traceparent_comes_from_the_query() {
        let tp = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
        let uri = |s: &str| s.parse::<axum::http::Uri>().unwrap();
        assert_eq!(super::query_traceparent(&uri(&format!("/ws/arcane?traceparent={tp}"))), Some(tp));
        assert_eq!(super::query_traceparent(&uri(&format!("/ws/arcane?x=1&traceparent={tp}"))), Some(tp));
        assert_eq!(super::query_traceparent(&uri("/ws/arcane?x=1")), None);
        assert_eq!(super::query_traceparent(&uri("/ws/arcane")), None);
    }

    #[tokio::test]
    async fn api_responses_and_pages_are_not_stored() {
        use axum::http::header::CACHE_CONTROL;
        use axum::response::{Html, IntoResponse};
        use axum::routing::{get, post};

        let app = axum::Router::new()
            .route("/api/session/login", post(|| async { "{}" }))
            .route("/login", get(|| async { Html("<html></html>") }))
            .route("/_astro/app.css", get(|| async { ([(axum::http::header::CONTENT_TYPE, "text/css")], "") }))
            .route("/api/account/picture", get(|| async { ([(CACHE_CONTROL, "private, max-age=60")], "").into_response() }))
            .layer(axum::middleware::from_fn(super::no_store));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        api::trace::spawn_loop("test: stand-in server", async move { axum::serve(listener, app).await.unwrap() });

        let client = test_client();
        let cache_control = |res: reqwest::Response| {
            res.headers().get(CACHE_CONTROL).map(|v| v.to_str().unwrap().to_string())
        };
        let api = client.post(format!("{base}/api/session/login")).send().await.unwrap();
        assert_eq!(cache_control(api).as_deref(), Some("no-store"));
        let page = client.get(format!("{base}/login")).send().await.unwrap();
        assert_eq!(cache_control(page).as_deref(), Some("private, no-cache"));
        let asset = client.get(format!("{base}/_astro/app.css")).send().await.unwrap();
        assert_eq!(cache_control(asset), None);
        let own = client.get(format!("{base}/api/account/picture")).send().await.unwrap();
        assert_eq!(cache_control(own).as_deref(), Some("private, max-age=60"), "the handler's own is kept");
    }

    /// Log records carry the trace and span of the span they were written in (I8), and
    /// the SDK's own logs stay out of the bridge.
    #[test]
    fn log_records_carry_the_span_and_skip_sdk_logs() {
        let exporter = InMemoryLogExporter::default();
        let logs = SdkLoggerProvider::builder().with_simple_exporter(exporter.clone()).build();
        let tracer = opentelemetry_sdk::trace::SdkTracerProvider::builder().build().tracer("test");
        let subscriber = tracing_subscriber::registry()
            .with(tracing_opentelemetry::layer().with_tracer(tracer))
            .with(super::otel_log_bridge(&logs));

        let sc = tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!("request");
            let _entered = span.enter();
            tracing::info!("in a span");
            tracing::warn!(target: "opentelemetry_sdk", "sdk warning");
            span.context().span().span_context().clone()
        });

        let records = exporter.get_emitted_logs().unwrap();
        let [record] = &records[..] else { panic!("{} records", records.len()) };
        let tc = record.record.trace_context().expect("no trace context");
        assert_eq!(tc.trace_id, sc.trace_id());
        assert_eq!(tc.span_id, sc.span_id());
    }

    /// The camera WebSocket asks the request's one viewer, with the `/api/arcane/*` rule: logged
    /// out 401, without `arcane` 403, auth down 503, all before anything else; a foreign origin
    /// is refused before auth is asked at all.
    #[tokio::test]
    async fn the_camera_websocket_lets_in_only_arcane_users() {
        use crate::json_api::tests::Site;
        use axum::http::{Method, StatusCode};
        let app = || axum::Router::new().route("/ws/arcane", super::arcane_ws_route());
        for (token, status) in [(None, 401), (Some("none"), 403), (Some("down"), 503)] {
            let site = Site::serving(app()).await;
            let mut site = match token {
                Some(token) => site.log_in(token).await,
                None => site,
            };
            assert_eq!(site.get("/ws/arcane").await.status.as_u16(), status, "{token:?}");
        }
        // Allowed: then it is the handshake that is missing (a plain GET), after one introspect.
        let mut site = Site::serving(app()).await.log_in("all").await;
        let reply = site.get("/ws/arcane").await;
        assert!(![401, 403, 503].contains(&reply.status.as_u16()) && reply.status.is_client_error(), "{}", reply.status);
        assert_eq!(site.auth.introspected("all"), 1);
        // A foreign page, and the browser extension too: its exception is the flag's only.
        for origin in ["https://evil.example", "moz-extension://1b2c3d", "chrome-extension://abcdef"] {
            let foreign = site.send(Method::GET, "/ws/arcane", Some(origin), None).await;
            assert_eq!(foreign.status, StatusCode::FORBIDDEN, "{origin}");
        }
        assert_eq!(site.auth.introspected("all"), 1);
    }

    /// OAuth leaves the site, so `?next=` waits in the session: the callback lands on it, on the
    /// invite page when an invite waits, and on `/` when it wasn't one of the site's pages.
    #[tokio::test]
    async fn an_oauth_login_lands_on_the_next_page_it_started_with() {
        use crate::json_api::tests::Site;
        use axum::http::{Method, StatusCode};
        let app = || {
            axum::Router::new()
                .route("/oauth/start/{provider}", axum::routing::get(super::oauth_start))
                .route("/oauth/callback/{provider}", axum::routing::get(super::oauth_callback))
                .merge(crate::json_api::router())
        };
        /// Starts with `query`, comes back from the provider, and says where the login landed.
        async fn round_trip(site: &mut Site, query: &str) -> String {
            let start = format!("/oauth/start/github{query}");
            let reply = site.get(&start).await;
            assert_eq!(reply.status, StatusCode::SEE_OTHER, "{start}");
            // The provider's URL is auth's, untouched: `next` never goes to the provider.
            assert_eq!(reply.headers[axum::http::header::LOCATION], "https://github.test/authorize?state=st");
            let reply = site.get("/oauth/callback/github?code=good&state=st").await;
            assert_eq!(reply.status, StatusCode::SEE_OTHER, "{start}");
            reply.headers[axum::http::header::LOCATION].to_str().unwrap().to_string()
        }

        let mut site = Site::serving(app()).await;
        assert_eq!(round_trip(&mut site, "?next=/asshole").await, "/asshole");
        assert_eq!(round_trip(&mut site, "?next=%2Faccount-deleted").await, "/account-deleted");
        // Logged in now, by the callback.
        assert_eq!(site.get("/api/session").await.body["viewer"]["username"], "user-all");
        // Each start replaces the last one's page.
        assert_eq!(round_trip(&mut site, "").await, "/");
        for hostile in [
            "//evil.com",
            "https://evil.com/",
            "/\\evil.com",
            "%2F%2Fevil.com",
            "/asshole/",
            "/api/session",
            "/Login",
            "javascript:alert(1)",
            "/asshole%0d%0aLocation:%20https://evil.com",
        ] {
            assert_eq!(round_trip(&mut site, &format!("?next={hostile}")).await, "/", "{hostile}");
        }
        drop(site);

        // An invite opened while logged out wins over `next`.
        let mut site = Site::serving(app()).await;
        let opened = site.json(Method::POST, "/api/invites/open", serde_json::json!({"code": "join"})).await;
        assert_eq!(opened.body["kind"], "needs_account");
        assert_eq!(round_trip(&mut site, "?next=/asshole").await, "/invite#join");
        // Once.
        assert_eq!(round_trip(&mut site, "?next=/asshole").await, "/asshole");

        // A callback that fails its state check forgets the page too.
        assert_eq!(site.get("/oauth/start/github?next=/asshole").await.status, StatusCode::SEE_OTHER);
        assert_eq!(site.session_data().await["oauth_next"], "/asshole");
        let forged = site.get("/oauth/callback/github?code=good&state=forged").await;
        assert_eq!(forged.headers[axum::http::header::LOCATION], "/login?error=csrf_mismatch");
        let data = site.session_data().await;
        assert!(!data.contains_key("oauth_next") && !data.contains_key("oauth_csrf_state"), "{data:?}");
        drop(site);

        // A password login redeemed an invite whose page was never taken (no after-login call):
        // an OAuth login later lands on its own `next`, not on that old invite page.
        let mut site = Site::serving(app()).await;
        site.json(Method::POST, "/api/invites/open", serde_json::json!({"code": "join"})).await;
        let login = site.json(Method::POST, "/api/session/login", serde_json::json!({"username": "tester", "password": "right"})).await;
        assert_eq!(login.status, StatusCode::OK);
        assert_eq!(site.session_data().await[api::INVITE_RETURN_KEY], "join");
        assert_eq!(round_trip(&mut site, "?next=/asshole").await, "/asshole");
    }
}
