mod account_email;
pub mod arcane;
mod core;
mod internal;
mod invites;
mod mail;
pub mod permissions;
mod protected_route;
mod session_store;
pub mod telemetry;
pub mod trace;
mod user;

use std::env;

use axum::{Router, routing::get};
use axum_login::{
    AuthManagerLayerBuilder,
    tower_sessions::{
        Expiry, SessionManagerLayer,
        cookie::{SameSite, time::Duration},
        session_store::ExpiredDeletion,
    },
};
use tracing::Instrument as _;
use axum_prometheus::PrometheusMetricLayer;
use axum_tracing_opentelemetry::middleware::{OtelAxumLayer, OtelInResponseLayer};
use oauth2::{AuthUrl, ClientId, ClientSecret, TokenUrl, basic::BasicClient};
use tower_sessions_sqlx_store::PostgresStore;

use crate::auth::user::BasicClientSet;

use self::{
    internal::InternalState,
    session_store::{handler, shutdown_signal},
    trace::{Peer, TracedStore},
    user::Backend,
};

/// The pool everything queries through: every query gets an OTel client span (see
/// TRACING.md). The raw `sqlx::PgPool` is banned (`clippy.toml`) outside the places that
/// need one: building the pool, sqlx's migrator, tower-sessions' store and the pool gauges.
pub type Db = sqlx_tracing::Pool<sqlx::Postgres>;

/// The docker-control host that runs the game servers (`/ark/..`, `/valheim`).
pub const GAME_HOST: &str = "http://192.168.1.21:9090";

#[allow(clippy::disallowed_types, reason = "owns the pool it builds; `server` wraps it in `Db`")]
pub struct Auth {
    db: sqlx::PgPool,
    session_store: PostgresStore,
    client: BasicClientSet,
    g_client: BasicClientSet,
}

/// Connections are opened at startup and kept: a new one costs ~100 ms of CPU (TLS setup loads
/// the system CA bundle), which sqlx's defaults (closing idle connections after 10 min,
/// every connection after 30) put inside requests. A dead connection is still replaced:
/// the pool pings each one before handing it out.
///
/// An acquire that takes over a millisecond (every connection busy, or a new one being
/// opened) is reported by sqlx as an event, which `trace::PoolAcquireSpans` turns into a
/// `db.pool.acquire` span.
fn pool_options() -> sqlx::postgres::PgPoolOptions {
    sqlx::postgres::PgPoolOptions::new()
        .min_connections(2)
        .idle_timeout(None)
        .max_lifetime(None)
        .acquire_slow_threshold(std::time::Duration::from_millis(1))
        .acquire_slow_level(log::LevelFilter::Info)
}

/// Applies auth's migrations and the session table's, as one span. sqlx's migrator and
/// tower-sessions take the raw pool, so the statements have no spans of their own: the span
/// is a CLIENT span for the whole of it.
#[allow(clippy::disallowed_types, reason = "sqlx's migrator needs the raw pool")]
async fn migrate(db: &sqlx::PgPool, session_store: &PostgresStore) -> Result<(), Box<dyn std::error::Error>> {
    let span = tracing::info_span!(
        parent: None,
        "db.migrate",
        otel.kind = "client",
        db.system.name = "postgresql",
        peer.service = "postgres",
    );
    async {
        // The error names a migration and Postgres's complaint about our own DDL: no user data.
        sqlx::migrate!().run(db).await.inspect_err(|e| tracing::error!(error = %e, "could not apply migrations"))?;
        session_store
            .migrate()
            .await
            .inspect_err(|e| tracing::error!(error = %e, "could not create the session table"))?;
        Ok(())
    }
    .instrument(span)
    .await
}

impl Auth {
    pub async fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let client_id = env::var("CLIENT_ID")
            .map(ClientId::new)
            .expect("CLIENT_ID should be provided");
        let client_secret = env::var("CLIENT_SECRET")
            .map(ClientSecret::new)
            .expect("CLIENT_SECRET should be provided");

        let g_client_id = env::var("G_CLIENT_ID")
            .map(ClientId::new)
            .expect("G_CLIENT_ID should be provided");
        let g_client_secret = env::var("G_CLIENT_SECRET")
            .map(ClientSecret::new)
            .expect("G_CLIENT_SECRET should be provided");

        let auth_url = AuthUrl::new("https://github.com/login/oauth/authorize".to_string())?;
        let token_url = TokenUrl::new("https://github.com/login/oauth/access_token".to_string())?;

        let g_auth_url = AuthUrl::new("https://accounts.google.com/o/oauth2/auth".to_string())?;
        let g_token_url = TokenUrl::new("https://oauth2.googleapis.com/token".to_string())?;

        let client = BasicClient::new(client_id)
            .set_client_secret(client_secret)
            .set_auth_uri(auth_url)
            .set_token_uri(token_url);

        let g_client = BasicClient::new(g_client_id)
            .set_client_secret(g_client_secret)
            .set_auth_uri(g_auth_url)
            .set_token_uri(g_token_url);

        let db_connection = env::var("DATABASE_URL").expect("DATABASE_URL should be provided.");
        let db = pool_options().connect(&db_connection).await?;
        let session_store = PostgresStore::new(db.clone());
        migrate(&db, &session_store).await?;

        Ok(Auth {
            db,
            session_store,
            client,
            g_client,
        })
    }

    pub async fn server(self) -> Result<(), Box<dyn std::error::Error>> {
        // peer.service="postgres" names the node in Tempo's service graph.
        let db: Db = sqlx_tracing::PoolBuilder::from(self.db.clone()).with_name("postgres").build();
        let session_store = TracedStore::new(self.session_store, "postgres");

        // Background loops. Each round is a trace of its own (`trace::spawn`).
        trace::spawn_loop("session expiry", {
            let session_store = session_store.clone();
            async move {
                loop {
                    let session_store = session_store.clone();
                    let round = trace::spawn("session.expire", None, async move {
                        if let Err(e) = session_store.delete_expired().await {
                            tracing::warn!(error = %e, "removing expired sessions failed");
                        }
                    });
                    if let Err(e) = round.await {
                        tracing::error!(error = %e, "removing expired sessions panicked");
                    }
                    tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
                }
            }
        });
        trace::spawn_loop("sessions gauge", {
            let db = db.clone();
            async move {
                loop {
                    if let Err(e) = trace::spawn("sessions.count", None, sync_sessions_gauge(db.clone())).await {
                        tracing::error!(error = %e, "counting sessions panicked");
                    }
                    tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
                }
            }
        });
        trace::spawn_loop("email code cleanup", {
            let db = db.clone();
            async move {
                loop {
                    let round = trace::spawn("email_code.clean_expired", None, account_email::clean_expired(db.clone()));
                    if let Err(e) = round.await {
                        tracing::error!(error = %e, "removing expired email codes panicked");
                    }
                    tokio::time::sleep(tokio::time::Duration::from_secs(3600)).await;
                }
            }
        });
        // No rounds: it reads two counters in memory, no I/O and nothing to trace.
        trace::spawn_loop("pool gauges", poll_pool_metrics(self.db));

        let session_layer = SessionManagerLayer::new(session_store)
            // Defense-in-depth: even though auth is now cluster-internal, require Secure
            // cookies in release builds. Debug builds get plain HTTP for local dev.
            .with_secure(!cfg!(debug_assertions))
            .with_same_site(SameSite::Lax)
            .with_name("milesstorm.auth")
            .with_expiry(Expiry::OnInactivity(Duration::days(7)));

        // One client for the game-server host, shared: building one costs 40–190 ms of CPU
        // (TLS setup and the system certificate store), more than a call. Commands answer
        // when the docker action is done (the host replies `Timeout` itself when one takes
        // too long), hence the long timeout.
        let game_http = trace::client(
            Peer { service: "ark", expected: &[] },
            std::time::Duration::from_secs(60),
            reqwest::redirect::Policy::default(),
        );
        let backend = Backend::new(db.clone(), self.client, self.g_client, game_http.clone());
        let auth_layer = AuthManagerLayerBuilder::new(backend.clone(), session_layer).build();

        let internal_state = InternalState {
            db,
            jwt_secret: env::var("JWT_SECRET").expect("JWT_SECRET must be set"),
            service_secret: env::var("BFF_SERVICE_SECRET").expect("BFF_SERVICE_SECRET must be set"),
            backend,
            mailer: mail::Mailer::from_env(),
            ark_http: game_http,
        };

        let (prometheus_layer, metric_handle) = PrometheusMetricLayer::pair();

        let app = Router::new()
            .route(
                "/metrics",
                get(move || async move { metric_handle.render() }),
            )
            .route("/auth", get(handler))
            .merge(internal::router(internal_state))
            .merge(protected_route::router())
            .merge(permissions::router())
            .merge(core::router())
            .layer(auth_layer)
            .layer(axum::middleware::from_fn(record_trace_id))   // ← now runs after OtelAxumLayer
            .layer(OtelInResponseLayer)
            // Prometheus scrapes every 15s; a trace each would bury the real ones.
            .layer(OtelAxumLayer::default().filter(|path| path != "/metrics"))
            .layer(prometheus_layer);

        let listener = match tokio::net::TcpListener::bind(format!(
            "{}:{}",
            std::env::var("SERVER_IP").unwrap_or("localhost".to_string()),
            std::env::var("SERVER_PORT").unwrap_or("7070".to_string())
        ))
        .await
        {
            Ok(l) => l,
            Err(e) => panic!("Could not start listening with error: {e}"),
        };

        tracing::info!("Listening on: {}", listener.local_addr().unwrap());
        axum::serve(listener, app.into_make_service())
            .with_graceful_shutdown(shutdown_signal())
            .await?;

        // Requests are done; emails still being sent get a moment, so their spans are whole
        // when `main` flushes.
        trace::finish_tasks(std::time::Duration::from_secs(5)).await;

        Ok(())
    }
}

async fn record_trace_id(
    req: axum::http::Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use opentelemetry::trace::TraceContextExt as _;
    use tracing_opentelemetry::OpenTelemetrySpanExt as _;

    let current_span = tracing::Span::current();
    let sc = current_span.context().span().span_context().clone();
    if sc.is_valid() {
        current_span.record("trace_id", sc.trace_id().to_string());
    }
    next.run(req).await
}

#[allow(clippy::disallowed_types, reason = "the pool's size and idle count are only on the raw pool")]
async fn poll_pool_metrics(db: sqlx::PgPool) {
    loop {
        metrics::gauge!("auth_db_pool_size").set(db.size() as f64);
        metrics::gauge!("auth_db_pool_idle").set(db.num_idle() as f64);
        tokio::time::sleep(tokio::time::Duration::from_secs(15)).await;
    }
}

/// One round of the sessions gauge.
async fn sync_sessions_gauge(db: Db) {
    let result = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM bff_tokens WHERE expires_at > NOW()",
    )
    .fetch_one(&db)
    .await;

    match result {
        Ok(count) => metrics::gauge!("auth_sessions_active").set(count as f64),
        Err(e) => tracing::warn!(error = %e, "failed to sync sessions gauge"),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use opentelemetry::trace::{SpanId, SpanKind, Status, TraceContextExt as _};
    use sqlx::Executor as _;
    use tower_sessions::SessionStore as _;
    use tower_sessions::session::{Id, Record};
    use tracing_opentelemetry::OpenTelemetrySpanExt as _;

    use super::trace::testing::{attr, pipeline, postgres};
    use super::*;

    /// Against a real Postgres (Docker): migrations, transactions, a wait for a pool
    /// connection, and the session store each leave the spans TRACING.md describes.
    #[tokio::test]
    async fn database_work_is_spanned() {
        let traced = pipeline();
        let pg = postgres(pool_options().max_connections(2)).await;
        let store = PostgresStore::new(pg.raw.clone());
        let db = pg.db();

        migrate(&pg.raw, &store).await.unwrap();
        let migration = traced.span("db.migrate");
        assert_eq!(migration.span_kind, SpanKind::Client);
        assert_eq!(migration.parent_span_id, SpanId::INVALID, "a trace of its own");
        assert_eq!(attr(&migration, "peer.service").as_deref(), Some("postgres"));
        assert_eq!(migration.status, Status::Unset);

        // A transaction: BEGIN, its queries, COMMIT or ROLLBACK are siblings under the caller.
        let caller = tracing::info_span!("caller");
        let caller_id = caller.context().span().span_context().span_id();
        async {
            let mut tx = trace::begin(&db).await.unwrap();
            (&mut tx.executor()).execute("SELECT 1").await.unwrap();
            trace::commit(tx).await.unwrap();
            trace::rollback(trace::begin(&db).await.unwrap()).await;
        }
        .instrument(caller)
        .await;
        for (name, statement, count) in [("sqlx.begin", "BEGIN", 2), ("sqlx.commit", "COMMIT", 1), ("sqlx.rollback", "ROLLBACK", 1)] {
            let spans = traced.named(name);
            assert_eq!(spans.len(), count, "{name}");
            for span in spans {
                assert_eq!(span.span_kind, SpanKind::Client);
                assert_eq!(span.parent_span_id, caller_id);
                assert_eq!(span.status, Status::Unset);
                assert_eq!(attr(&span, "db.operation.name").as_deref(), Some(statement));
                assert_eq!(attr(&span, "peer.service").as_deref(), Some("postgres"));
            }
        }
        assert_eq!(traced.span("sqlx.execute").parent_span_id, caller_id);

        // Every connection is taken: the query waits for one, which shows as a span in it.
        let (first, second) = (pg.raw.acquire().await.unwrap(), pg.raw.acquire().await.unwrap());
        tokio::join!(
            async {
                tokio::time::sleep(Duration::from_millis(100)).await;
                drop((first, second));
            },
            async { (&db).fetch_one("SELECT 2").await.unwrap() }.instrument(tracing::info_span!("waiter")),
        );
        let query = traced.span("sqlx.fetch_one");
        let waits: Vec<_> = traced
            .named("db.pool.acquire")
            .into_iter()
            .filter(|s| s.parent_span_id == query.span_context.span_id())
            .collect();
        let [wait] = &waits[..] else { panic!("{} acquire spans under the query", waits.len()) };
        assert_eq!(wait.span_kind, SpanKind::Internal);
        assert!(wait.end_time.duration_since(wait.start_time).unwrap() >= Duration::from_millis(80));
        assert!(wait.start_time >= query.start_time - Duration::from_millis(5) && wait.end_time <= query.end_time);
        // sqlx's report of the wait is that span and nothing else: not a log line, not an
        // event on the query's span. And an acquire outside any span makes no trace.
        assert!(!traced.logs().contains("sqlx::pool::acquire"), "{}", traced.logs());
        assert!(traced.spans().iter().all(|s| s.events.is_empty()));
        let (first, second) = (pg.raw.acquire().await.unwrap(), pg.raw.acquire().await.unwrap());
        let (_, third) = tokio::join!(
            async {
                tokio::time::sleep(Duration::from_millis(20)).await;
                drop((first, second));
            },
            pg.raw.acquire(),
        );
        drop(third);
        assert!(traced.named("db.pool.acquire").iter().all(|s| s.parent_span_id != SpanId::INVALID));

        // The session store, through the same wrapper `server` uses.
        let sessions = TracedStore::new(store, "postgres");
        let record = Record {
            id: Id::default(),
            data: Default::default(),
            expiry_date: tower_sessions::cookie::time::OffsetDateTime::now_utc() + Duration::from_secs(60),
        };
        let request = tracing::info_span!("request");
        let request_id = request.context().span().span_context().span_id();
        async {
            sessions.save(&record).await.unwrap();
            assert!(sessions.load(&record.id).await.unwrap().is_some());
            sessions.delete_expired().await.unwrap();
        }
        .instrument(request)
        .await;
        for op in ["session.save", "session.load", "session.delete_expired"] {
            let span = traced.span(&format!("postgres {op}"));
            assert_eq!(span.span_kind, SpanKind::Client);
            assert_eq!(span.parent_span_id, request_id);
        }

        // A failure: status ERROR and the error's type, without its text.
        pg.raw.close().await;
        assert!(trace::begin(&db).await.is_err());
        let failed = traced.named("sqlx.begin").pop().unwrap();
        assert_eq!(failed.status, Status::error(""));
        assert_eq!(attr(&failed, "error.type").as_deref(), Some("pool_closed"));
    }
}
