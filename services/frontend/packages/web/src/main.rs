use std::collections::HashMap;

use dioxus::prelude::*;

use api::{check_login_status, get_my_permissions, logout};
use ui::{data_dir::LoginStatus, setup_mode, CookieConsent, Navbar, TAILWIND};
use views::{
    AccountDeleted, AdminPanel, Arcane, Ark, AssholeTimer, DeleteAccount, ForgotPassword, Invite, Landing, Login,
    NotFound, Profile, Register, ResetPassword, VerifyEmail,
};

mod views;
#[cfg(not(target_arch = "wasm32"))]
mod rolls;
#[cfg(not(target_arch = "wasm32"))]
mod capture;
#[cfg(not(target_arch = "wasm32"))]
mod dataset;
mod sharing;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod trace_tests;
mod account;
mod emails;
mod invites;

pub static LOGIN_STATUS: GlobalSignal<LoginStatus> = Signal::global(|| LoginStatus::LoggedOut);
pub static PERMISSIONS: GlobalSignal<HashMap<String, bool>> = Signal::global(HashMap::new);
/// The logged-in user's name and picture (navbar, profile page); `None` until loaded.
pub static ACCOUNT: GlobalSignal<Option<account::AccountInfo>> = Signal::global(|| None);

const FAVICON: Asset = asset!("/assets/favicon.ico");

fn main() {
    #[cfg(not(target_arch = "wasm32"))]
    {
        match dotenvy::dotenv() {
            Ok(_) => {}
            Err(_) if !cfg!(debug_assertions) => {}
            Err(e) => panic!("could not load .env: {e}"),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    server_launch();

    #[cfg(target_arch = "wasm32")]
    dioxus::launch(App);
}

// ---- Server launch ----

/// The exporters' providers, kept to flush them before the process ends.
#[cfg(not(target_arch = "wasm32"))]
static TELEMETRY: std::sync::OnceLock<(opentelemetry_sdk::trace::SdkTracerProvider, opentelemetry_sdk::logs::SdkLoggerProvider)> =
    std::sync::OnceLock::new();

/// Sends the spans and log records still batched. Blocks until the collector has them.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn flush_telemetry() {
    if let Some((traces, logs)) = TELEMETRY.get() {
        let _ = traces.force_flush();
        let _ = logs.force_flush();
    }
}

/// How long a shutdown waits for open connections, camera sessions and detached tasks.
/// Kubernetes kills the pod 30 s after SIGTERM; telemetry is flushed after the wait.
#[cfg(not(target_arch = "wasm32"))]
const SHUTDOWN_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

#[cfg(not(target_arch = "wasm32"))]
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

    // Init subscriber before dioxus::serve — Dioxus's own try_init().ok() will
    // then fail silently and our subscriber (JSON + OTel) wins.
    // The OTel log bridge additionally ships log events via OTLP so Loki entries carry
    // trace_id/span_id, enabling Tempo → Loki correlation.
    // The filter also decides which spans exist (TRACING.md, "Log level"); `dioxus=warn`
    // keeps per-signal spans (dioxus_signals) from becoming root traces of their own.
    tracing_subscriber::registry()
        .with(EnvFilter::new(std::env::var("RUST_LOG").unwrap_or_else(
            |_| "info,dioxus=warn,tower_sessions=warn,opentelemetry=warn".into(),
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

    // `dx serve` hot-patches the running server, which only `dioxus::serve` does. It never
    // returns, so a debug build stops without draining or flushing.
    if cfg!(debug_assertions) {
        dioxus::serve(app)
    }

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

/// What `dioxus::serve` does in a release build (dioxus-server 0.7.10 `launch.rs`: bind the
/// `IP`/`PORT` address, then `axum::serve`), plus a graceful shutdown on SIGTERM, which
/// `dioxus::serve` can't do: it never returns.
#[cfg(not(target_arch = "wasm32"))]
async fn serve<F>(mut app: impl FnMut() -> F, shutdown: tokio_util::sync::CancellationToken)
where
    F: std::future::Future<Output = anyhow::Result<axum::Router>>,
{
    let addr = dioxus::cli_config::fullstack_address_or_localhost();
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
#[cfg(not(target_arch = "wasm32"))]
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
#[cfg(not(target_arch = "wasm32"))]
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

/// The website's router: the Dioxus app and the plain axum routes, with their Redis
/// clients and background loops. `shutdown` ends the long-lived streams.
#[cfg(not(target_arch = "wasm32"))]
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

    let router = Router::new()
        .serve_dioxus_application(ServeConfig::default(), App)
        .route("/oauth/start/{provider}", get(oauth_start))
        .route("/oauth/callback/{provider}", get(oauth_callback))
        .route("/ws/arcane", get(arcane_ws_proxy))
        .route("/api/arcane/me", get(rolls::arcane_me))
        .route("/api/arcane/rolls", get(rolls::arcane_rolls))
        .route("/api/arcane/flag", axum::routing::post(capture::arcane_flag))
        .route(
            "/api/profile/picture",
            get(account::get_picture).post(account::upload_picture).layer(
                // Room for the largest accepted upload; bigger bodies get 413.
                axum::extract::DefaultBodyLimit::max(account::PICTURE_MAX_UPLOAD),
            ),
        )
        .route(
            "/metrics",
            get(move || async move { metric_handle.render() }),
        )
        .layer(axum::middleware::from_fn(render_span))
        .layer(axum::Extension(roll_hub))
        .layer(axum::middleware::from_fn(no_store))
        .layer(layer)
        .layer(axum::middleware::from_fn(api::trace::capture_request_context))
        .layer(axum::middleware::from_fn(name_page_span))
        .layer(axum::middleware::from_fn(api::trace::end_span_with_body))
        .layer(OtelInResponseLayer)
        // Prometheus scrapes every 15s; a trace each would bury the real ones.
        .layer(OtelAxumLayer::default().filter(|path| path != "/metrics"))
        .layer(prometheus_layer);
    Ok(router)
}

/// `service.version` is the commit the image was built from (Dockerfile `GIT_SHA`).
/// `OTEL_RESOURCE_ATTRIBUTES` adds the rest (`deployment.environment.name`).
#[cfg(not(target_arch = "wasm32"))]
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
#[cfg(not(target_arch = "wasm32"))]
fn otel_log_bridge<S>(provider: &opentelemetry_sdk::logs::SdkLoggerProvider) -> impl tracing_subscriber::Layer<S>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    use tracing_subscriber::Layer as _;
    opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge::new(provider)
        .with_filter(tracing_subscriber::filter::filter_fn(|meta| !meta.target().starts_with("opentelemetry")))
}

/// Page requests reach Dioxus's fallback, which has no `MatchedPath`, so `OtelAxumLayer`
/// names their span just `GET`. Names it after the page's `Route` variant instead: a
/// bounded set, unlike raw paths (span names become Tempo span-metrics labels).
/// Server functions are named by their path without Dioxus's hash ([`server_fn_route`]).
#[cfg(not(target_arch = "wasm32"))]
async fn name_page_span(req: axum::extract::Request, next: axum::middleware::Next) -> axum::response::Response {
    let method = req.method();
    let span = tracing::Span::current();
    let matched = req.extensions().get::<axum::extract::MatchedPath>();
    // Only a path that reached a server function: any other is the client's own string,
    // and span names become span-metrics labels.
    if let Some(route) = matched.and_then(|m| server_fn_route(m.as_str())) {
        span.record("otel.name", format!("{method} {route}"));
        span.record("http.route", route);
    } else if is_page(&req) {
        let page = page_name(req.uri().path());
        span.record("otel.name", format!("{method} {page}"));
    }
    next.run(req).await
}

/// Whether a request goes to Dioxus's page handler (the router's fallback): no route of
/// ours or of Dioxus's (server functions, assets) matched it.
#[cfg(not(target_arch = "wasm32"))]
fn is_page(req: &axum::extract::Request) -> bool {
    use axum::http::Method;
    (req.method() == Method::GET || req.method() == Method::HEAD)
        && req.extensions().get::<axum::extract::MatchedPath>().is_none()
}

/// The innermost layer: a page render gets its `ssr.render` span (`api::trace::ssr_render`).
#[cfg(not(target_arch = "wasm32"))]
async fn render_span(req: axum::extract::Request, next: axum::middleware::Next) -> axum::response::Response {
    if is_page(&req) {
        api::trace::ssr_render(req, next).await
    } else {
        next.run(req).await
    }
}

/// `Cache-Control` on server-function responses (`no-store`) and rendered pages (`private,
/// no-cache`), unless the handler chose its own. Neither may be replayed by a shared cache: pages carry the request's
/// `<meta name="traceparent">`, and the browser's clock correction takes its samples from both
/// (TRACING.md, "Browser clock"). Cloudflare caches neither today; this keeps a future rule from
/// changing that.
#[cfg(not(target_arch = "wasm32"))]
async fn no_store(req: axum::extract::Request, next: axum::middleware::Next) -> axum::response::Response {
    use axum::http::header::{HeaderValue, CACHE_CONTROL, CONTENT_TYPE};
    let bff = req.uri().path().starts_with("/bff/");
    let mut res = next.run(req).await;
    let html = res
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("text/html"));
    if (bff || html) && !res.headers().contains_key(CACHE_CONTROL) {
        // Pages may stay in the browser's back/forward cache, which `no-store` turns off.
        let value = if bff { "no-store" } else { "private, no-cache" };
        res.headers_mut().insert(CACHE_CONTROL, HeaderValue::from_static(value));
    }
    res
}

/// `/bff/get_account` for `/bff/get_account12855143162362325647`: Dioxus appends a hash to
/// each server function's path, which the browser's span (assets/trace.js) leaves out, so
/// both sides of a call carry the same name. None for other paths.
#[cfg(not(target_arch = "wasm32"))]
fn server_fn_route(path: &str) -> Option<&str> {
    let name = path.strip_prefix("/bff/")?;
    let base = name.trim_end_matches(|c: char| c.is_ascii_digit());
    // The same rule as trace.js: 6 or more digits after a name.
    let digits = name.len() - base.len();
    (digits >= 6 && !base.is_empty() && !base.ends_with('/')).then(|| &path[..path.len() - digits])
}

/// The `Route` variant `path` renders (`NotFound` for anything unknown).
#[cfg(not(target_arch = "wasm32"))]
fn page_name(path: &str) -> String {
    // The variant name is what Debug prints before the fields.
    let route = path.parse::<Route>().map(|r| format!("{r:?}")).unwrap_or_default();
    match route.split(|c: char| !c.is_alphanumeric()).next() {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => "NotFound".to_string(),
    }
}

// ---- Arcane WebSocket proxy ----

/// Upgrades to WebSocket and bidirectionally proxies to the ai_pipeline inference service.
/// Requires a same-host Origin and a valid session with the `arcane` permission;
/// returns 403/401 otherwise. Settled rolls are published for the user's other
/// devices (see `rolls`).
#[cfg(not(target_arch = "wasm32"))]
async fn arcane_ws_proxy(
    ws: axum::extract::ws::WebSocketUpgrade,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    axum::Extension(hub): axum::Extension<rolls::RollHub>,
    session: tower_sessions::Session,
) -> axum::response::Response {
    use axum::{http::StatusCode, response::IntoResponse};

    if !rolls::origin_allowed(&headers) {
        tracing::warn!(origin = ?headers.get(axum::http::header::ORIGIN), "arcane WebSocket rejected: foreign origin");
        return StatusCode::FORBIDDEN.into_response();
    }

    let user = match rolls::arcane_user(&session).await {
        Ok(u) => u,
        Err(rolls::Denied::LoggedOut) => return StatusCode::UNAUTHORIZED.into_response(),
        Err(rolls::Denied::NoPermission) => {
            tracing::warn!("arcane WebSocket rejected: permission denied");
            return StatusCode::FORBIDDEN.into_response();
        }
    };

    let ai_url = std::env::var("AI_PIPELINE_SERVICE_URL")
        .unwrap_or_else(|_| "ws://localhost:9000".to_string());

    tracing::info!(upstream = %ai_url, "upgrading arcane WebSocket");
    let session_id = session.id();
    let token: String = session.get("opaque_token").await.ok().flatten().unwrap_or_default();
    // The camera session outlives this request (it ends at the 101). This span is its short
    // open span, in the request's trace: it ends once ai_pipeline is connected (whose
    // connection span joins it). What the session does after that is traced per roll.
    let open = api::detached_span!("arcane.ws_session", otel.name = "arcane.ws_session open", user = %user);
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
#[cfg(not(target_arch = "wasm32"))]
const UPSTREAM_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[cfg(not(target_arch = "wasm32"))]
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
                            if rolls_tx.try_send(Carried::with_context(trace.cx.clone(), t.to_string())).is_err() {
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
                                    api::trace::continue_from(&span, trace.cx);
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
#[cfg(not(target_arch = "wasm32"))]
fn query_traceparent(uri: &axum::http::Uri) -> Option<&str> {
    uri.query()?.split('&').find_map(|pair| pair.strip_prefix("traceparent="))
}

/// From an ai_pipeline reply (parsed once): whether it is a roll event, and its
/// `frame_seq`, the number of the forwarded frame it was computed from.
#[cfg(not(target_arch = "wasm32"))]
fn reply_info(text: &str) -> (bool, Option<u64>) {
    if !text.contains("\"frame_seq\"") {
        return (rolls::is_roll(text), None);
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else { return (false, None) };
    let is_roll = v.get("type").and_then(|t| t.as_str()) == Some("roll");
    (is_roll, v.get("frame_seq").and_then(|s| s.as_u64()))
}

// ---- OAuth Axum handlers ----

#[cfg(not(target_arch = "wasm32"))]
const OAUTH_CSRF_KEY: &str = "oauth_csrf_state";
#[cfg(not(target_arch = "wasm32"))]
const OAUTH_PROVIDER_KEY: &str = "oauth_provider";

/// Begin the OAuth flow for `provider`. Asks auth (cluster-internal) for the provider's
/// authorization URL, stashes the CSRF state in the BFF session, and redirects the browser.
#[cfg(not(target_arch = "wasm32"))]
async fn oauth_start(
    axum::extract::Path(provider): axum::extract::Path<String>,
    session: tower_sessions::Session,
) -> axum::response::Response {
    use axum::response::{IntoResponse, Redirect};

    if provider != "github" && provider != "google" {
        return Redirect::to("/login?error=unknown_provider").into_response();
    }

    match api::start_oauth(&provider).await {
        Ok((auth_url, state)) => {
            if let Err(e) = session.insert(OAUTH_CSRF_KEY, &state).await {
                tracing::error!(error = %e, %provider, "oauth_start: failed to write CSRF state");
                return Redirect::to("/login?error=session_failed").into_response();
            }
            if let Err(e) = session.insert(OAUTH_PROVIDER_KEY, &provider).await {
                tracing::error!(error = %e, %provider, "oauth_start: failed to write provider");
                return Redirect::to("/login?error=session_failed").into_response();
            }
            Redirect::to(&auth_url).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, %provider, "oauth_start: api::start_oauth failed");
            Redirect::to("/login?error=start_failed").into_response()
        }
    }
}

/// Provider callback. Validates CSRF state, asks auth to exchange the code, and
/// stores the resulting opaque token + username on the BFF session.
#[cfg(not(target_arch = "wasm32"))]
async fn oauth_callback(
    axum::extract::Path(provider): axum::extract::Path<String>,
    session: tower_sessions::Session,
    axum::extract::Query(params): axum::extract::Query<HashMap<String, String>>,
) -> axum::response::Response {
    use axum::response::{IntoResponse, Redirect};

    let Some(code) = params.get("code").cloned() else {
        return Redirect::to("/login?error=missing_code").into_response();
    };
    let Some(state) = params.get("state").cloned() else {
        return Redirect::to("/login?error=missing_state").into_response();
    };

    let expected_state: Option<String> = session.get(OAUTH_CSRF_KEY).await.ok().flatten();
    let expected_provider: Option<String> = session.get(OAUTH_PROVIDER_KEY).await.ok().flatten();
    let _ = session.remove::<String>(OAUTH_CSRF_KEY).await;
    let _ = session.remove::<String>(OAUTH_PROVIDER_KEY).await;

    if expected_state.as_deref() != Some(&state) || expected_provider.as_deref() != Some(&provider)
    {
        return Redirect::to("/login?error=csrf_mismatch").into_response();
    }

    match api::exchange_oauth_code(&provider, &code).await {
        Ok((token, username)) => {
            if api::fresh_session_id(&session).await.is_err() {
                return Redirect::to("/login?error=session_failed").into_response();
            }
            if let Err(e) = session.insert("opaque_token", &token).await {
                tracing::error!(error = %e, %provider, "oauth_callback: failed to write opaque_token");
                return Redirect::to("/login?error=session_failed").into_response();
            }
            if let Err(e) = session.insert("username", username).await {
                tracing::error!(error = %e, %provider, "oauth_callback: failed to write username");
                return Redirect::to("/login?error=session_failed").into_response();
            }
            // Came from an invite link: back to it (the invite was just redeemed).
            match api::redeem_pending_invite(&session, &token).await {
                Some(page) => {
                    let _ = session.remove::<String>(api::INVITE_RETURN_KEY).await;
                    Redirect::to(&page).into_response()
                }
                None => Redirect::to("/").into_response(),
            }
        }
        Err(e) if e.contains("email_exists") || e.contains("Email already in use") => {
            Redirect::to("/login?error=email_exists").into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, %provider, "oauth_callback: exchange_oauth_code failed");
            Redirect::to("/login?error=exchange_failed").into_response()
        }
    }
}

// ---- Dioxus app ----

// `page_name` (span names) reads the variant name from the derived Debug.
#[derive(Debug, Clone, Routable, PartialEq)]
#[rustfmt::skip]
enum Route {
    #[layout(WebNavbar)]
        #[route("/")]
        Landing {},
        #[route("/login?:error")]
        Login { error: String },
        #[route("/register")]
        Register {},
        #[route("/forgot-password")]
        ForgotPassword {},
        #[route("/reset-password#:code")]
        ResetPassword { code: String },
        #[route("/verify-email#:code")]
        VerifyEmail { code: String },
        #[route("/delete-account#:code")]
        DeleteAccount { code: String },
        #[route("/account-deleted")]
        AccountDeleted {},
        #[route("/invite#:code")]
        Invite { code: String },
        #[route("/profile")]
        Profile {},
        #[route("/ark")]
        Ark {},
        #[route("/arcane")]
        Arcane {},
        #[route("/asshole")]
        AssholeTimer {},
        #[route("/admin")]
        AdminPanel {},
        #[route("/:..segments")]
        NotFound { segments: Vec<String> },
}

#[component]
fn App() -> Element {
    let status = api::trace::use_server_future("check_login_status", check_login_status)?;
    let perms = api::trace::use_server_future("get_my_permissions", get_my_permissions)?;

    use_effect(move || {
        if let Some(Ok(s)) = status.value()() {
            *LOGIN_STATUS.write() = s;
        }
        if let Some(Ok(p)) = perms.value()() {
            let map: HashMap<String, bool> = p.into_iter().map(|n| (n, true)).collect();
            *PERMISSIONS.write() = map;
        }
    });

    // Load the account (display name, picture) whenever someone logs in.
    use_effect(move || match LOGIN_STATUS() {
        LoginStatus::LoggedIn(_) => {
            spawn(async move {
                if let Ok(a) = account::get_account().await {
                    *ACCOUNT.write() = Some(a);
                }
            });
        }
        LoginStatus::LoggedOut => *ACCOUNT.write() = None,
    });

    setup_mode();

    // Browser tracing (TRACING.md, "Browser"): the page request's trace for the browser to
    // continue, then the tracer, as classic scripts that run before the WASM module loads.
    // The meta is rendered on the client too (as `None`, inserting nothing) so the tree
    // hydrates the same. Copied as-is (`with_minify(false)`): the bundles come minified, and
    // dx's esbuild pass turns a file it takes for an ES module into ESM, hiding the
    // top-level `var GrafanaFaroWebSdk` the other scripts need.
    let traceparent = use_hook(api::trace::traceparent);

    rsx! {
        document::Meta { name: "traceparent", content: traceparent }
        document::Script { src: asset!("/assets/vendor/faro-web-sdk.iife.js", AssetOptions::js().with_minify(false)) }
        document::Script { src: asset!("/assets/vendor/faro-web-tracing.iife.js", AssetOptions::js().with_minify(false)) }
        document::Script { src: asset!("/assets/vendor/otel-batch.iife.js", AssetOptions::js().with_minify(false)) }
        document::Script { src: asset!("/assets/trace.js", AssetOptions::js().with_minify(false)) }
        document::Link { rel: "icon", href: FAVICON }
        document::Link { rel: "stylesheet", href: TAILWIND }

        Router::<Route> {}

        CookieConsent {}
    }
}

#[component]
fn WebNavbar() -> Element {
    let logout_handler = move |_: ()| {
        spawn(async move {
            let _ = logout().await;
            *LOGIN_STATUS.write() = LoginStatus::LoggedOut;
            *PERMISSIONS.write() = HashMap::new();
            *ACCOUNT.write() = None;
        });
    };

    let perms = PERMISSIONS.read();
    let account = ACCOUNT();
    rsx! {
        Navbar {
            user: LOGIN_STATUS(),
            name: account.as_ref().map(|a| a.shown_name().to_string()),
            picture: account.as_ref().and_then(|a| a.picture_url()),
            on_logout: logout_handler,
            has_ark: perms.contains_key("llama"),
            has_arcane: perms.contains_key("arcane"),
            has_admin: perms.contains_key("manage_permissions"),
        }
        Outlet::<Route> {}
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
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
    fn server_function_spans_leave_out_the_hash() {
        use super::server_fn_route;
        assert_eq!(server_fn_route("/bff/get_account12855143162362325647"), Some("/bff/get_account"));
        assert_eq!(server_fn_route("/bff/login_password123456"), Some("/bff/login_password"));
        assert_eq!(server_fn_route("/bff/get_account"), None);
        assert_eq!(server_fn_route("/bff/get_account12345"), None);
        assert_eq!(server_fn_route("/bff/12855143162362325647"), None);
        assert_eq!(server_fn_route("/api/profile/picture"), None);
    }

    #[test]
    fn code_pages_start_with_the_same_markup_before_hydration() {
        use dioxus::prelude::*;
        fn render(code: &str) -> String {
            dioxus::ssr::render_element(rsx! {
                super::ResetPassword { code: code.to_string() }
                super::VerifyEmail { code: code.to_string() }
                super::DeleteAccount { code: code.to_string() }
                super::Invite { code: code.to_string() }
            })
        }
        let server = render("");
        let browser_initial = render("a_b-c");
        assert_eq!(server, browser_initial);
        assert!(!server.contains("a_b-c"));
        assert!(server.contains("loading-spinner"));
    }

    #[test]
    fn one_time_code_routes_use_fragments() {
        use super::Route;
        for (path, route) in [
            ("/reset-password", Route::ResetPassword { code: "a_b-c".into() }),
            ("/verify-email", Route::VerifyEmail { code: "a_b-c".into() }),
            ("/delete-account", Route::DeleteAccount { code: "a_b-c".into() }),
            ("/invite", Route::Invite { code: "a_b-c".into() }),
        ] {
            let address = route.to_string();
            assert_eq!(address, format!("{path}#a_b-c"));
            assert_eq!(address.parse::<Route>().unwrap(), route);
            // This is all the browser sends in the initial HTTP request.
            let server_route = path.parse::<Route>().unwrap();
            assert_eq!(server_route.to_string(), path);
        }
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
    async fn bff_responses_and_pages_are_not_stored() {
        use axum::http::header::CACHE_CONTROL;
        use axum::response::{Html, IntoResponse};
        use axum::routing::{get, post};

        let app = axum::Router::new()
            .route("/bff/login_password123456", post(|| async { "{}" }))
            .route("/login", get(|| async { Html("<html></html>") }))
            .route("/assets/app.css", get(|| async { ([(axum::http::header::CONTENT_TYPE, "text/css")], "") }))
            .route("/bff/picture123456", get(|| async { ([(CACHE_CONTROL, "private, max-age=60")], "").into_response() }))
            .layer(axum::middleware::from_fn(super::no_store));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        api::trace::spawn_loop("test: stand-in server", async move { axum::serve(listener, app).await.unwrap() });

        let client = test_client();
        let cache_control = |res: reqwest::Response| {
            res.headers().get(CACHE_CONTROL).map(|v| v.to_str().unwrap().to_string())
        };
        let post = client.post(format!("{base}/bff/login_password123456")).send().await.unwrap();
        assert_eq!(cache_control(post).as_deref(), Some("no-store"));
        let page = client.get(format!("{base}/login")).send().await.unwrap();
        assert_eq!(cache_control(page).as_deref(), Some("private, no-cache"));
        let asset = client.get(format!("{base}/assets/app.css")).send().await.unwrap();
        assert_eq!(cache_control(asset), None);
        let own = client.get(format!("{base}/bff/picture123456")).send().await.unwrap();
        assert_eq!(cache_control(own).as_deref(), Some("private, max-age=60"), "the handler's own is kept");
    }

    #[test]
    fn pages_are_named_by_route() {
        assert_eq!(super::page_name("/"), "Landing");
        assert_eq!(super::page_name("/login"), "Login");
        assert_eq!(super::page_name("/admin"), "AdminPanel");
        assert_eq!(super::page_name("/some/1234/thing"), "NotFound");
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
}
