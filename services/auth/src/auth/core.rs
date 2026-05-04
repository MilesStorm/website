use crate::auth::user::{Backend, Credentials, PasswordCreds};
use axum::{
    Form, Router,
    response::IntoResponse,
    routing::{get, post},
};
use axum_login::login_required;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

use crate::auth::user::AuthSession;

use super::user::ClientUser;

#[derive(Serialize)]
pub struct ApiResponse {
    pub message: String,
    pub user: Option<ClientUser>, // Optionally include user info if registration succeeds
}

impl ApiResponse {
    pub fn new(message: &str, user: Option<ClientUser>) -> Self {
        Self {
            message: message.to_string(),
            user,
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct NextUrl {
    next: Option<String>,
}

pub fn router() -> Router<()> {
    Router::new()
        .route("/auth/logout", get(self::get::logout))
        .route_layer(login_required!(Backend, login_url = "/auth/login"))
        // Registration is intentionally outside login_required
        .route(
            "/auth/register/password",
            post(self::post::register::password),
        )
        .route("/auth/login/password", post(self::post::login::password))
        .route("/auth/login", get(self::get::login))
}

mod post {
    use super::*;

    pub(super) mod register {
        use axum::Json;

        use crate::auth::user::{SignUpCreds, UserError};

        use super::*;

        pub async fn password(
            auth_session: AuthSession,
            Form(creds): Form<SignUpCreds>,
        ) -> impl IntoResponse {
            let result = auth_session
                .backend
                .register_user(&creds.username, &creds.email, &creds.password)
                .await;

            match result {
                Ok(user) => (
                    axum::http::StatusCode::OK,
                    Json(ApiResponse {
                        message: "Registration successful".to_string(),
                        user: Some(user.into()),
                    }),
                ),
                Err(UserError::UserAlreadyExists) => (
                    axum::http::StatusCode::CONFLICT,
                    Json(ApiResponse {
                        message: "User already exists".to_string(),
                        user: None,
                    }),
                ),
                Err(UserError::EmailAlreadyInUse) => (
                    axum::http::StatusCode::CONFLICT,
                    Json(ApiResponse {
                        message: "Email already in use".to_string(),
                        user: None,
                    }),
                ),
                Err(_) => (
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ApiResponse {
                        message: "An unexpected error occurred".to_string(),
                        user: None,
                    }),
                ),
            }
            .into_response()
        }
    }

    pub(super) mod login {

        use axum::Json;

        use crate::auth::user::ClientUser;

        use super::*;

        pub async fn password(
            mut auth_session: AuthSession,
            Form(creds): Form<PasswordCreds>,
        ) -> impl IntoResponse {
            let user = match auth_session
                .authenticate(Credentials::Password(creds.clone()))
                .await
            {
                Ok(Some(user)) => user,
                Ok(None) => {
                    tracing::info!("Invalid password");
                    return (StatusCode::UNAUTHORIZED, "invalid password".to_string())
                        .into_response();
                }
                Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            };

            if auth_session.login(&user).await.is_err() {
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }

            let client_user = ClientUser::from(user);
            tracing::info!(user_id = client_user.id, username = %client_user.username, "legacy password login succeeded");

            if let Some(ref next) = creds.next {
                // Redirect::to(next).into_response();
                Json(NextUrl {
                    next: Some(next.clone()),
                })
                .into_response()
            } else {
                // Redirect::to("/").into_response()
                Json(ApiResponse {
                    message: "Login successful".to_string(),
                    user: Some(client_user),
                })
                .into_response()
            }
        }
    }
}

mod get {
    use super::*;
    use axum::Json;

    pub async fn login(auth_session: AuthSession) -> impl IntoResponse {
        if let Some(user) = auth_session.user {
            Json(ApiResponse {
                message: "Already logged in".to_string(),
                user: Some(user.into()),
            })
            .into_response()
        } else {
            Json(ApiResponse {
                message: "Not logged in".to_string(),
                user: None,
            })
            .into_response()
        }
    }

    pub async fn logout(mut auth_session: AuthSession) -> impl IntoResponse {
        match auth_session.logout().await {
            Ok(user) => {
                tracing::info!("User logged out: {:?}", user);

                StatusCode::RESET_CONTENT.into_response()
            }
            Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        }
    }
}
