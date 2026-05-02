pub mod arcane;
mod core;
mod internal;
mod oauth;
pub mod permissions;
mod protected_route;
mod session_store;
mod user;

use std::{env, panic};

use axum::{Router, routing::get};
use tower_http::trace::{DefaultMakeSpan, DefaultOnResponse, TraceLayer};
use axum_login::{
    AuthManagerLayerBuilder,
    tower_sessions::{
        Expiry, SessionManagerLayer,
        cookie::{SameSite, time::Duration},
        session_store::ExpiredDeletion,
    },
};
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

        Ok(Auth { db, client, g_client })
    }

    pub async fn server(self) -> Result<(), Box<dyn std::error::Error>> {
        let session_store = PostgresStore::new(self.db.clone());
        session_store.migrate().await?;
        let deletion_task = tokio::task::spawn(
            session_store
                .clone()
                .continuously_delete_expired(tokio::time::Duration::from_secs(60)),
        );

        let session_layer = SessionManagerLayer::new(session_store)
            .with_secure(false)
            .with_same_site(SameSite::Lax)
            .with_expiry(Expiry::OnInactivity(Duration::days(7)));

        let g_client_for_internal = self.g_client.clone();
        let backend = Backend::new(self.db.clone(), self.client, self.g_client);
        let auth_layer = AuthManagerLayerBuilder::new(backend, session_layer).build();

        let http_client = oauth2::reqwest::ClientBuilder::new()
            .redirect(oauth2::reqwest::redirect::Policy::none())
            .build()
            .expect("Could not build http_client");

        let internal_state = InternalState {
            db: self.db.clone(),
            jwt_secret: env::var("JWT_SECRET").expect("JWT_SECRET must be set"),
            service_secret: env::var("BFF_SERVICE_SECRET")
                .expect("BFF_SERVICE_SECRET must be set"),
            bff_callback_url: env::var("BFF_CALLBACK_URL")
                .unwrap_or_else(|_| "http://localhost:8080".to_string()),
            g_client: g_client_for_internal,
            http_client,
        };

        let app = Router::new()
            .route("/api", get(handler))
            .merge(internal::router(internal_state))
            .merge(protected_route::router())
            .merge(permissions::router())
            .merge(core::router())
            .merge(oauth::router())
            .layer(auth_layer)
            .layer(
                TraceLayer::new_for_http()
                    .make_span_with(DefaultMakeSpan::new().level(tracing::Level::INFO))
                    .on_response(DefaultOnResponse::new().level(tracing::Level::INFO)),
            );

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
