mod core;
mod oauth;
mod protected_route;
mod session_store;
mod user;

use std::env;

use axum::routing::get;
use axum_login::{
    login_required,
    tower_sessions::{
        cookie::{time::Duration, SameSite},
        session_store::ExpiredDeletion,
        Expiry, SessionManagerLayer,
    },
    AuthManagerLayerBuilder,
};
use oauth2::{basic::BasicClient, AuthUrl, ClientId, ClientSecret, TokenUrl};
use sqlx::PgPool;
use tower_sessions_sqlx_store::PostgresStore;

use self::{
    session_store::{handler, shutdown_signal},
    user::Backend,
};

pub struct Auth {
    db: PgPool,
    client: BasicClient,
    g_client: BasicClient,
}

impl Auth {
    pub async fn new() -> Result<Self, Box<dyn std::error::Error>> {
        match dotenvy::dotenv() {
            Ok(_) => {
                tracing::debug!("Loaded .env file");
            }
            Err(_) => {
                tracing::debug!("Loaded .env file");
                tracing::debug!("assuming environment variables are set");
            }
        }

        let client_id = env::var("CLIENT_ID")
            .map(ClientId::new)
            .expect("CLIENT_ID should be provided");
        tracing::trace!("client_id: {:?}", client_id);
        let client_secret = env::var("CLIENT_SECRET")
            .map(ClientSecret::new)
            .expect("CLIENT_SECRET should be provided");

        let g_client_id = env::var("G_CLIENT_ID")
            .map(ClientId::new)
            .expect("CLIENT_ID should be provided");
        tracing::trace!("google client_id: {:?}", client_id);

        let g_client_secret = env::var("G_CLIENT_SECRET")
            .map(ClientSecret::new)
            .expect("CLIENT_SECRET should be provided");
        tracing::trace!("google client_id: {:?}", client_id);

        let auth_url = AuthUrl::new("https://github.com/login/oauth/authorize".to_string())?;
        tracing::trace!("auth_url: {:?}", auth_url);
        let token_url = TokenUrl::new("https://github.com/login/oauth/access_token".to_string())?;
        tracing::trace!("token_url: {:?}", token_url);

        let g_auth_url = AuthUrl::new("https://google.com/login/oauth/authorize".to_string())?;
        tracing::trace!("auth_url: {:?}", auth_url);
        let g_token_url = TokenUrl::new("https://google.com/login/oauth/access_token".to_string())?;
        tracing::trace!("token_url: {:?}", token_url);

        let client = BasicClient::new(client_id, Some(client_secret), auth_url, Some(token_url));
        tracing::trace!("client: {:?}", client);

        let g_client = BasicClient::new(
            g_client_id,
            Some(g_client_secret),
            g_auth_url,
            Some(g_token_url),
        );
        tracing::trace!("client: {:?}", client);

        let db_connection = env::var("DATABASE_URL").expect("DATABASE_URL should be provided");
        tracing::trace!("db_connection: {:?}", db_connection);
        let db = PgPool::connect(&db_connection).await?;
        tracing::trace!("db: {:?}", db);
        sqlx::migrate!().run(&db).await?;

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

        let session_layer = SessionManagerLayer::new(session_store)
            .with_secure(false)
            .with_same_site(SameSite::Lax)
            .with_expiry(Expiry::OnInactivity(Duration::days(2)));

        // Auth Service
        let backend = Backend::new(self.db, self.client, self.g_client);
        let auth_layer = AuthManagerLayerBuilder::new(backend, session_layer).build();

        let app = protected_route::router()
            .route("/api", get(handler))
            // .route("/", get(handler))
            .route_layer(login_required!(Backend, login_url = "/api/login"))
            .merge(core::router())
            .merge(oauth::router())
            .layer(auth_layer);

        let listener = tokio::net::TcpListener::bind(format!(
            "{}:{}",
            std::env::var("SERVER_IP").unwrap_or("localhost".to_string()),
            std::env::var("SERVER_PORT").unwrap_or("7070".to_string())
        ))
        .await
        .unwrap();

        axum::serve(listener, app.into_make_service())
            .with_graceful_shutdown(shutdown_signal(deletion_task.abort_handle()))
            .await?;

        deletion_task.await??;

        Ok(())
    }
}
