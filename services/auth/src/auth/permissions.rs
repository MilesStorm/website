use std::collections::HashSet;
use std::fmt::Display;

use axum::Json;
use axum::{Router, response::IntoResponse, routing::get};
use axum_login::AuthUser;
use axum_login::{AuthzBackend, permission_required};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use tracing::info;

use super::user::Backend;

#[derive(Debug, Clone, Eq, PartialEq, Hash, FromRow)]
pub struct Permission {
    pub name: String,
}

#[derive(Serialize, Deserialize)]
struct PermissionResponse {
    has_permission: bool,
}

impl From<&str> for Permission {
    fn from(name: &str) -> Self {
        Self {
            name: name.to_string(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub enum CommandResult {
    Stopped,
    AlreadyStopped,
    FailedToStop,
    Started,
    AlreadyRunning,
    FailedToStart,
    Timeout,
    Restarting,
    NumPlayers(i32),
}

impl Display for CommandResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

enum Operation {
    Start,
    Stop,
    Num,
    Restart,
}

impl Display for Operation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Operation::Start => write!(f, "start"),
            Operation::Stop => write!(f, "stop"),
            Operation::Num => write!(f, "num_players"),
            Operation::Restart => write!(f, "restart"),
        }
    }
}

pub fn router() -> Router<()> {
    Router::new()
        .route(
            "/api/permission/valheim_player/restart",
            get(self::get::restart_valheim),
        )
        .route("/api/permission/ark/restart", get(self::get::restart_ark))
        .route("/api/permission/ark/num_players", get(self::get::count_ark))
        .route("/api/permission/ark/start", get(self::get::start_ark))
        .route("/api/permission/ark/stop", get(self::get::stop_ark))
        .route_layer(permission_required!(
            Backend,
            login_url = "/api/login",
            "llama"
        ))
        .route("/api/permission/valheim_player", get(self::get::permission))
        .route("/api/permission/ark", get(self::get::permission))
        .route("/api/permission/llama", get(self::get::permission))
        .route("/api/permission/photoview", get(self::get::permission))
}

mod get {
    use crate::auth::user::AuthSession;

    use super::*;

    #[derive(Serialize, Deserialize)]
    struct DockerRequestResponse {
        restart_result: String,
        command_result: Option<CommandResult>,
    }

    pub async fn permission(auth_session: AuthSession) -> impl IntoResponse {
        match auth_session.user {
            Some(user) => {
                tracing::info!("User: {:?}", user);

                Json(PermissionResponse {
                    has_permission: true,
                })
            }
            .into_response(),
            None => (
                StatusCode::UNAUTHORIZED,
                Json(PermissionResponse {
                    has_permission: false,
                }),
            )
                .into_response(),
        }
    }

    pub async fn restart_valheim(auth_session: AuthSession) -> impl IntoResponse {
        match auth_session.user {
            Some(user) => {
                tracing::info!("{:?} restarted valheim server", user);

                match reqwest::get("http://192.168.1.21:9090/valheim").await {
                    Ok(resp) => {
                        let json: Result<DockerRequestResponse, reqwest::Error> =
                            resp.json::<DockerRequestResponse>().await;

                        match json {
                            Ok(suc) => (
                                StatusCode::OK,
                                Json(DockerRequestResponse {
                                    restart_result: suc.restart_result,
                                    command_result: suc.command_result,
                                }),
                            )
                                .into_response(),
                            Err(ere) => {
                                (StatusCode::INTERNAL_SERVER_ERROR, ere.to_string()).into_response()
                            }
                        }
                    }
                    Err(_) => (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Failed to restart Valheim server",
                    )
                        .into_response(),
                }
            }
            .into_response(),
            None => (
                StatusCode::UNAUTHORIZED,
                Json(PermissionResponse {
                    has_permission: false,
                }),
            )
                .into_response(),
        }
    }

    pub async fn restart_ark(auth_session: AuthSession) -> impl IntoResponse {
        info!("running restart_ark");
        ark_handle(auth_session, Operation::Restart).await
    }

    pub async fn count_ark(auth_session: AuthSession) -> impl IntoResponse {
        info!("running count_ark");
        ark_handle(auth_session, Operation::Num).await
    }

    pub async fn stop_ark(auth_session: AuthSession) -> impl IntoResponse {
        info!("running stop_ark");
        ark_handle(auth_session, Operation::Stop).await
    }

    pub async fn start_ark(auth_session: AuthSession) -> impl IntoResponse {
        ark_handle(auth_session, Operation::Start).await
    }

    async fn ark_handle(
        auth_session: axum_login::AuthSession<Backend>,
        op: Operation,
    ) -> axum::http::Response<axum::body::Body> {
        match auth_session.user {
            Some(user) => {
                tracing::info!("{:?} {}ed ark server", user, op);

                match reqwest::get(format!("http://192.168.1.21:9090/ark/{op}")).await {
                    Ok(resp) => {
                        let json: Result<DockerRequestResponse, reqwest::Error> =
                            resp.json::<DockerRequestResponse>().await;

                        match json {
                            Ok(suc) => (
                                StatusCode::OK,
                                Json(DockerRequestResponse {
                                    restart_result: suc.restart_result,
                                    command_result: suc.command_result,
                                }),
                            )
                                .into_response(),
                            Err(ere) => {
                                (StatusCode::INTERNAL_SERVER_ERROR, ere.to_string()).into_response()
                            }
                        }
                    }
                    Err(_) => (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Failed to {op} ark server",
                    )
                        .into_response(),
                }
            }
            .into_response(),
            None => (
                StatusCode::UNAUTHORIZED,
                Json(PermissionResponse {
                    has_permission: false,
                }),
            )
                .into_response(),
        }
    }
}

impl AuthzBackend for Backend {
    type Permission = Permission;

    async fn get_group_permissions(
        &self,
        user: &Self::User,
    ) -> Result<HashSet<Self::Permission>, Self::Error> {
        info!("Getting permissions for user: {:?}", &user);
        let permissions: Vec<Self::Permission> = sqlx::query_as(
            r#"
            SELECT permissions.name
            FROM users
            JOIN user_roles ON users.id = user_roles.user_id
            JOIN role_permissions ON user_roles.role_id = role_permissions.role_id
            JOIN permissions ON role_permissions.permission_id = permissions.id
            WHERE users.id = $1
            "#,
        )
        .bind(user.id())
        .fetch_all(&self.db)
        .await?;

        info!("Permissions: {:?} for user {:?}", &permissions, &user);

        Ok(permissions.into_iter().collect())
    }
}
