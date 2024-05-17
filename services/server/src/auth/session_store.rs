use axum::response::IntoResponse;
use axum_login::tower_sessions::Session;
use serde::Deserialize;
use tokio::{signal, task::AbortHandle};

const COUNTER_KEY: &str = "counter";

#[derive(Debug, Deserialize, Default)]
struct Counter(usize);

pub async fn handler(session: Session) -> impl IntoResponse {
    let counter: Counter = session
        .get(COUNTER_KEY)
        .await
        .expect("Could not unwrap counter")
        .unwrap_or_default();

    session
        .insert(COUNTER_KEY, counter.0 + 1)
        .await
        .expect("Failed to insert counter");

    format!("Current count: {}", counter.0);
}

pub async fn shutdown_signal(deletion_task_abot_handle: AbortHandle) {
    let ctrl_c = async {
        signal::ctrl_c().await.expect("Failed to listen for ctrl+C");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("Failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {
            println!("Received SIGINT signal");
            deletion_task_abot_handle.abort()
        }
        _ = terminate => {
            println!("Received SIGTERM signal");
            deletion_task_abot_handle.abort()
        }
    }
}
