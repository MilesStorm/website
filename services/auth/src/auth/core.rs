use crate::auth::user::{Credentials, PasswordCreds};
use axum::{
    response::{IntoResponse, Redirect},
    routing::{get, post},
    Form, Router,
};
use axum_login::tower_sessions::Session;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

pub const NEXT_URL_KEY: &str = "auth.next-url";

use crate::auth::{oauth::CSRF_STATE_KEY, user::AuthSession};

use super::user::User;

#[derive(Serialize)]
pub struct ApiResponse {
    message: String,
    user: Option<User>, // Optionally include user info if registration succeeds
}

impl ApiResponse {
    pub fn new(message: &str, user: Option<User>) -> Self {
        Self {
            message: message.to_string(),
            user,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct NextUrl {
    next: Option<String>,
}

pub fn router() -> Router<()> {
    Router::new()
        .route(
            "/api/register/password",
            post(self::post::register::password),
        )
        .route("/api/login/password", post(self::post::login::password))
        .route("/api/login/oauth", post(self::post::login::oauth))
        .route("/api/login", get(self::get::login))
        .route("/api/logout", get(self::get::logout))
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
                Ok(mut user) => {
                    user.password = None;
                    (
                        axum::http::StatusCode::OK,
                        Json(ApiResponse {
                            message: "Registration successful".to_string(),
                            user: Some(user),
                        }),
                    )
                }
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

        use super::*;

        pub async fn password(
            mut auth_session: AuthSession,
            Form(creds): Form<PasswordCreds>,
        ) -> impl IntoResponse {
            let mut user = match auth_session
                .authenticate(Credentials::Password(creds.clone()))
                .await
            {
                Ok(Some(user)) => user,
                Ok(None) => {
                    return (StatusCode::UNAUTHORIZED, "invalid password".to_string())
                        .into_response()
                }
                Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            };

            if auth_session.login(&user).await.is_err() {
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }

            user.password = None;
            dbg!(&user);

            if let Some(ref next) = creds.next {
                Redirect::to(next).into_response()
            } else {
                // Redirect::to("/").into_response()
                Json(ApiResponse {
                    message: "Login successful".to_string(),
                    user: Some(user),
                })
                .into_response()
            }
        }

        pub async fn oauth(
            auth_session: AuthSession,
            session: Session,
            Form(NextUrl { next }): Form<NextUrl>,
        ) -> impl IntoResponse {
            let (auth_url, csrf_state) = auth_session.backend.authorize_url();

            session
                .insert(CSRF_STATE_KEY, csrf_state.secret())
                .await
                .expect("Serialization should not fail.");

            session
                .insert(NEXT_URL_KEY, next)
                .await
                .expect("Serialization should not fail.");

            Redirect::to(auth_url.as_str()).into_response()
        }
    }
}

mod get {
    use super::*;
    use axum::Json;

    pub async fn login(auth_session: AuthSession) -> impl IntoResponse {
        if let Some(mut user) = auth_session.user {
            user.password = None;
            Json(ApiResponse {
                message: "Already logged in".to_string(),
                user: Some(user),
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
            Ok(_) => Redirect::to("/login").into_response(),
            Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        }
    }
}
