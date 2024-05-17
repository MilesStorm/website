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
}

impl Auth {
    pub async fn new() -> Result<Self, Box<dyn std::error::Error>> {
        dotenvy::dotenv()?;

        let client_id = env::var("CLIENT_ID")
            .map(ClientId::new)
            .expect("CLIENT_ID should be provided");
        let client_secret = env::var("CLIENT_SECRET")
            .map(ClientSecret::new)
            .expect("CLIENT_SECRET should be provided");

        let auth_url = AuthUrl::new("https://github.com/login/oauth/authorize".to_string())?;
        let token_url = TokenUrl::new("https://github.com/login/oauth/access_token".to_string())?;

        let client = BasicClient::new(client_id, Some(client_secret), auth_url, Some(token_url));

        let db = PgPool::connect(&env::var("DATABASE_URL")?).await?;
        sqlx::migrate!().run(&db).await?;

        Ok(Auth { db, client })
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
            .with_same_site(SameSite::Strict)
            .with_expiry(Expiry::OnInactivity(Duration::days(2)));

        // Auth Service
        let backend = Backend::new(self.db, self.client);
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
            std::env::var("SERVER_PORT").expect("Server port has to be provided")
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
