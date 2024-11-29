use std::collections::HashSet;

use axum::Json;
use axum::{async_trait, response::IntoResponse, routing::get, Router};
use axum_login::AuthUser;
use axum_login::{permission_required, AuthzBackend};
use reqwest::StatusCode;
use sqlx::FromRow;
use tracing::info;

use super::user::Backend;

#[derive(Debug, Clone, Eq, PartialEq, Hash, FromRow)]
pub struct Permission {
    pub name: String,
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
        .route("/api/test_perm", get(self::get::test_perm))
        .route_layer(permission_required!(
            Backend,
            login_url = "/api/login/logout",
            Permission::from("test")
        ))
}

mod get {
    use crate::auth::{core::ApiResponse, user::AuthSession};

    use super::*;

    pub async fn test_perm(auth_session: AuthSession) -> impl IntoResponse {
        match auth_session.user {
            Some(user) => {
                tracing::info!("User: {:?}", user);

                Json(ApiResponse {
                    message: "You have permission".to_string(),
                    user: Some(user.into()),
                })
            }
            .into_response(),
            None => StatusCode::UNAUTHORIZED.into_response(),
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

        println!("Permissions: {:?} for user {:?}", permissions, user);
        info!("Permissions: {:?} for user {:?}", permissions, user);

        Ok(dbg!(permissions.into_iter().collect()))
    }
}
