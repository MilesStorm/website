mod auth;

use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    match dotenvy::dotenv() {
        Ok(_) => {
            tracing::debug!("Loaded .env file");
        }
        Err(e) => {
            tracing::debug!("Loaded .env file");
            tracing::debug!("assuming environment variables are set");
            if cfg!(debug_assertions) {
                panic!("could not load env: {e}");
            }
        }
    }

    tracing_subscriber::registry()
        .with(EnvFilter::new(std::env::var("RUST_LOG").unwrap_or_else(
            |_| "axum_login=debug,tower_sessions=debug,sqlx=warn,tower_http=debug".into(),
        )))
        .with(tracing_subscriber::fmt::layer())
        .try_init()?;

    println!("started server");

    auth::Auth::new().await?.server().await
}
