use std::collections::HashMap;

use dioxus::prelude::*;

use api::{check_login_status, get_my_permissions, logout};
use ui::{data_dir::LoginStatus, setup_mode, CookieConsent, Navbar, TAILWIND};
use views::{AdminPanel, Arcane, Ark, AssholeTimer, Landing, Login, NotFound, Profile, Register};

mod views;

pub static LOGIN_STATUS: GlobalSignal<LoginStatus> = Signal::global(|| LoginStatus::LoggedOut);
pub static PERMISSIONS: GlobalSignal<HashMap<String, bool>> = Signal::global(HashMap::new);

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

#[cfg(not(target_arch = "wasm32"))]
fn server_launch() -> ! {
    use axum::{routing::get, Router};
    use axum_prometheus::PrometheusMetricLayer;
    use axum_tracing_opentelemetry::middleware::{OtelAxumLayer, OtelInResponseLayer};
    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry::KeyValue;
    use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
    use opentelemetry_otlp::WithExportConfig;
    use opentelemetry_sdk::{
        logs::LoggerProvider as SdkLoggerProvider, runtime::Tokio as OtelTokio,
        trace::TracerProvider as SdkTracerProvider, Resource,
    };
    use tower_sessions::cookie::time::Duration;
    use tower_sessions::cookie::SameSite;
    use tower_sessions::{Expiry, SessionManagerLayer};
    use tower_sessions_redis_store::fred::prelude::*;
    use tower_sessions_redis_store::RedisStore;
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

    // Dedicated runtime for the OTel batch exporter. Lives for the process lifetime
    // because server_launch() is `-> !` and never returns, so _otel_rt is never dropped.
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
                        .with_batch_exporter(exporter, OtelTokio)
                        .with_resource(Resource::new([KeyValue::new("service.name", "frontend")]))
                        .build();

                    let tracer = provider.tracer("frontend");
                    opentelemetry::global::set_tracer_provider(provider);

                    let log_exporter = opentelemetry_otlp::LogExporter::builder()
                        .with_tonic()
                        .with_endpoint(endpoint)
                        .build()
                        .expect("failed to build OTLP log exporter");

                    let log_provider = SdkLoggerProvider::builder()
                        .with_batch_exporter(log_exporter, OtelTokio)
                        .with_resource(Resource::new([KeyValue::new("service.name", "frontend")]))
                        .build();

                    (
                        Some(tracing_opentelemetry::layer().with_tracer(tracer)),
                        Some(log_provider),
                    )
                }
                Err(_) => (None, None),
            }
        });

    let otel_log_layer = _otel_log_provider
        .as_ref()
        .map(|p| OpenTelemetryTracingBridge::new(p));

    // Init subscriber before dioxus::serve — Dioxus's own try_init().ok() will
    // then fail silently and our subscriber (JSON + OTel) wins.
    // The OTel log bridge additionally ships log events via OTLP so Loki entries carry
    // trace_id/span_id, enabling Tempo → Loki correlation.
    tracing_subscriber::registry()
        .with(EnvFilter::new(std::env::var("RUST_LOG").unwrap_or_else(
            |_| "info,dioxus=warn,tower_sessions=warn".into(),
        )))
        .with(tracing_subscriber::fmt::layer().json())
        .with(otel_layer)
        .with(otel_log_layer)
        .init();

    dioxus::serve(move || {
        let redis_url = redis_url.clone();
        async move {
            use tower_sessions_redis_store::fred::socket2::TcpKeepalive;

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
            let pool = Pool::new(
                config,
                None,
                Some(con_conf),
                Some(ReconnectPolicy::new_exponential(0, 100, 30_000, 2)),
                6,
            )
            .expect("failed to build Redis pool");
            pool.connect();
            pool.wait_for_connect()
                .await
                .expect("failed to connect to Redis");
            let session_store = RedisStore::new(pool);

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
                .route(
                    "/metrics",
                    get(move || async move { metric_handle.render() }),
                )
                .layer(layer)
                .layer(axum::middleware::from_fn(capture_traceparent))
                .layer(OtelInResponseLayer)
                .layer(OtelAxumLayer::default())
                .layer(prometheus_layer);
            Ok(router)
        }
    })
}

// ---- Arcane WebSocket proxy ----

/// Upgrades to WebSocket and bidirectionally proxies to the ai_pipeline inference service.
/// Requires a valid session with the `arcane` permission; returns 401/403 otherwise.
#[cfg(not(target_arch = "wasm32"))]
async fn arcane_ws_proxy(
    ws: axum::extract::ws::WebSocketUpgrade,
    session: tower_sessions::Session,
) -> axum::response::Response {
    use axum::{http::StatusCode, response::IntoResponse};

    let token: Option<String> = session.get("opaque_token").await.ok().flatten();
    let Some(token) = token else {
        return StatusCode::UNAUTHORIZED.into_response();
    };

    if !api::has_arcane_permission(&token).await {
        tracing::warn!("arcane WebSocket rejected: permission denied");
        return StatusCode::FORBIDDEN.into_response();
    }

    let ai_url = std::env::var("AI_PIPELINE_SERVICE_URL")
        .unwrap_or_else(|_| "ws://localhost:9000".to_string());

    tracing::info!(upstream = %ai_url, "upgrading arcane WebSocket");
    ws.on_upgrade(move |socket| proxy_ws(socket, ai_url))
}

#[cfg(not(target_arch = "wasm32"))]
async fn proxy_ws(client: axum::extract::ws::WebSocket, upstream_url: String) {
    use axum::extract::ws::Message as AxMsg;
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message as TngMsg;

    let (upstream, _) = match tokio_tungstenite::connect_async(&upstream_url).await {
        Ok(conn) => conn,
        Err(e) => {
            tracing::error!(error = %e, upstream = %upstream_url, "ai_pipeline connect failed");
            return;
        }
    };

    let (mut upstream_tx, mut upstream_rx) = upstream.split();
    let (mut client_tx, mut client_rx) = client.split();

    tokio::select! {
        // Browser sends binary camera frames → forward to ai_pipeline.
        _ = async {
            while let Some(Ok(msg)) = client_rx.next().await {
                match msg {
                    AxMsg::Binary(b) => {
                        if upstream_tx.send(TngMsg::Binary(b)).await.is_err() { break; }
                    }
                    AxMsg::Close(_) => break,
                    _ => {}
                }
            }
        } => {}
        // ai_pipeline sends JSON detection results → forward to browser.
        _ = async {
            while let Some(Ok(msg)) = upstream_rx.next().await {
                match msg {
                    TngMsg::Text(t) => {
                        if client_tx.send(AxMsg::Text(t.to_string().into())).await.is_err() { break; }
                    }
                    TngMsg::Close(_) => break,
                    _ => {}
                }
            }
        } => {}
    }
}

// ---- Trace context capture middleware ----

/// Runs after `OtelAxumLayer` has created the request span. Reads back the current
/// span's OTel context and stores the serialised `traceparent` as a request extension.
/// BFF server functions read this extension to forward the trace context to auth even
/// when Dioxus's SSR dispatcher spawns them into a new task (dropping thread-local state).
#[cfg(not(target_arch = "wasm32"))]
async fn capture_traceparent(
    mut req: axum::http::Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use opentelemetry::propagation::TextMapPropagator as _;
    use opentelemetry_sdk::propagation::TraceContextPropagator;
    use tracing_opentelemetry::OpenTelemetrySpanExt as _;

    let cx = tracing::Span::current().context();
    let propagator = TraceContextPropagator::new();
    let mut carrier = std::collections::HashMap::new();
    propagator.inject_context(&cx, &mut carrier);
    if let Some(tp) = carrier.remove("traceparent") {
        req.extensions_mut().insert(api::IncomingTraceparent(tp));
    }
    next.run(req).await
}

// ---- OAuth Axum handlers ----

const OAUTH_CSRF_KEY: &str = "oauth_csrf_state";
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
            if let Err(e) = session.insert("opaque_token", token).await {
                tracing::error!(error = %e, %provider, "oauth_callback: failed to write opaque_token");
                return Redirect::to("/login?error=session_failed").into_response();
            }
            if let Err(e) = session.insert("username", username).await {
                tracing::error!(error = %e, %provider, "oauth_callback: failed to write username");
                return Redirect::to("/login?error=session_failed").into_response();
            }
            Redirect::to("/").into_response()
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
    let status = use_server_future(check_login_status)?;
    let perms = use_server_future(get_my_permissions)?;

    use_effect(move || {
        if let Some(Ok(s)) = status.value()() {
            *LOGIN_STATUS.write() = s;
        }
        if let Some(Ok(p)) = perms.value()() {
            let map: HashMap<String, bool> = p.into_iter().map(|n| (n, true)).collect();
            *PERMISSIONS.write() = map;
        }
    });

    setup_mode();

    rsx! {
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
        });
    };

    let perms = PERMISSIONS.read();
    rsx! {
        Navbar {
            user: LOGIN_STATUS(),
            on_logout: logout_handler,
            has_ark: perms.contains_key("llama"),
            has_arcane: perms.contains_key("arcane"),
            has_admin: perms.contains_key("manage_permissions"),
        }
        Outlet::<Route> {}
    }
}
