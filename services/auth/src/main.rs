mod auth;

use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // JSON structured logging — one object per line, parsed by Loki / any log aggregator.
    tracing_subscriber::registry()
        .with(EnvFilter::new(std::env::var("RUST_LOG").unwrap_or_else(
            |_| "info,sqlx=warn,tower_sessions=warn".into(),
        )))
        .with(tracing_subscriber::fmt::layer().json())
        .try_init()?;

    match dotenvy::dotenv() {
        Ok(_) => tracing::debug!("loaded .env file"),
        Err(_) if !cfg!(debug_assertions) => {
            tracing::debug!("no .env file found, using environment variables");
        }
        Err(e) => panic!("could not load .env: {e}"),
    }

    tracing::info!("starting auth service");

    auth::Auth::new().await?.server().await
}
