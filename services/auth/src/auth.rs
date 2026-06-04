pub mod arcane;
mod core;
mod internal;
pub mod permissions;
mod protected_route;
mod session_store;
pub mod telemetry;
mod user;

use std::{env, panic};

use axum::{Router, routing::get};
use axum_login::{
    AuthManagerLayerBuilder,
    tower_sessions::{
        Expiry, SessionManagerLayer,
        cookie::{SameSite, time::Duration},
        session_store::ExpiredDeletion,
    },
};
use axum_prometheus::PrometheusMetricLayer;
use axum_tracing_opentelemetry::middleware::{OtelAxumLayer, OtelInResponseLayer};
use oauth2::{AuthUrl, ClientId, ClientSecret, TokenUrl, basic::BasicClient};
use sqlx::PgPool;
use tower_sessions_sqlx_store::PostgresStore;

use crate::auth::user::BasicClientSet;

use self::{
    internal::InternalState,
    session_store::{handler, shutdown_signal},
    user::Backend,
};

pub struct Auth {
    db: PgPool,
    client: BasicClientSet,
    g_client: BasicClientSet,
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
        let db = PgPool::connect(&db_connection).await?;

        let mig_res = sqlx::migrate!().run(&db).await;
        match mig_res {
            Ok(_) => {}
            Err(e) => panic!("Could not apply migrations: {e}"),
        }

        Ok(Auth {
            db,
            client,
            g_client,
        })
    }

    pub async fn server(self) -> Result<(), Box<dyn std::error::Error>> {
        let session_store = PostgresStore::new(self.db.clone());
        session_store.migrate().await?;
        let deletion_task = tokio::task::spawn(
            session_store
                .clone()
                .continuously_delete_expired(tokio::time::Duration::from_secs(60)),
        );
        tokio::spawn(sync_sessions_gauge(self.db.clone()));
        tokio::spawn(poll_pool_metrics(self.db.clone()));

        let session_layer = SessionManagerLayer::new(session_store)
            // Defense-in-depth: even though auth is now cluster-internal, require Secure
            // cookies in release builds. Debug builds get plain HTTP for local dev.
            .with_secure(!cfg!(debug_assertions))
            .with_same_site(SameSite::Lax)
            .with_name("milesstorm.auth")
            .with_expiry(Expiry::OnInactivity(Duration::days(7)));

        let backend = Backend::new(self.db.clone(), self.client, self.g_client);
        let auth_layer = AuthManagerLayerBuilder::new(backend.clone(), session_layer).build();

        let internal_state = InternalState {
            db: self.db.clone(),
            jwt_secret: env::var("JWT_SECRET").expect("JWT_SECRET must be set"),
            service_secret: env::var("BFF_SERVICE_SECRET").expect("BFF_SERVICE_SECRET must be set"),
            backend,
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
            .layer(OtelAxumLayer::default())
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
            .with_graceful_shutdown(shutdown_signal(deletion_task.abort_handle()))
            .await?;

        deletion_task.await??;

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

async fn poll_pool_metrics(db: PgPool) {
    loop {
        metrics::gauge!("auth_db_pool_size").set(db.size() as f64);
        metrics::gauge!("auth_db_pool_idle").set(db.num_idle() as f64);
        tokio::time::sleep(tokio::time::Duration::from_secs(15)).await;
    }
}

async fn sync_sessions_gauge(db: PgPool) {
    loop {
        let result = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM bff_tokens WHERE expires_at > NOW()",
        )
        .fetch_one(&db)
        .await;

        match result {
            Ok(count) => metrics::gauge!("auth_sessions_active").set(count as f64),
            Err(e) => tracing::warn!(error = %e, "failed to sync sessions gauge"),
        }

        tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
    }
}
