use std::collections::HashSet;

use axum::Json;
use axum::{async_trait, response::IntoResponse, routing::get, Router};
use axum_login::AuthUser;
use axum_login::{permission_required, AuthzBackend};
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

pub fn router() -> Router<()> {
    Router::new()
        .route(
            "/api/permission/:valheim_player",
            get(self::get::permission),
        )
        .route_layer(permission_required!(
            Backend,
            login_url = "/api/login",
            "restart_valheim"
        ))
}

mod get {
    use axum::{extract::Path, http::request};

    use crate::auth::user::AuthSession;

    use super::*;

    pub async fn permission(
        auth_session: AuthSession,
        Path(permission): Path<String>,
    ) -> impl IntoResponse {
        println!("Permissions: {:?}", permission);

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

                // request::Request // Here send a request to 192.168.1.21 to restart the valheim server
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
}

#[async_trait]
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

        info!("Permissions: {:?} for user {:?}", permissions, user);

        Ok(permissions.into_iter().collect())
    }
}
