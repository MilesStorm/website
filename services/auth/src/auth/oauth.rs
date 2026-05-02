use crate::auth::{
    core::NEXT_URL_KEY,
    user::{OAuthCreds, User},
};
use axum::{
    Router,
    extract::Query,
    http::StatusCode,
    response::{IntoResponse, Redirect},
    routing::get,
};
use axum_login::tower_sessions::Session;
use oauth2::CsrfToken;
use serde::Deserialize;
use ulid::Ulid;

pub const CSRF_STATE_KEY: &str = "oauth.csrf-state";

#[derive(Clone, Deserialize, Debug)]
pub struct AuthzResp {
    code: String,
    state: CsrfToken,
}

#[derive(Deserialize)]
pub struct NextQuery {
    next: Option<String>,
}

use crate::auth::user::{AuthSession, Credentials};

pub fn router() -> Router<()> {
    Router::new()
        // Browser-navigable init endpoints (set CSRF session cookie, redirect to provider)
        .route("/api/login/github/init", get(self::get::github_init))
        .route("/api/login/google/init", get(self::get::google_init))
        // OAuth provider callbacks
        .route(
            "/api/login/github/callback",
            get(self::get::github_callback),
        )
        .route(
            "/api/login/google/callback",
            get(self::get::google_callback),
        )
}

mod get {
    use crate::auth::user::BackendError;
    use axum_login::AuthUser as _;

    use super::*;

    // ---- OAuth init: browser navigates here directly to start the OAuth flow ----

    pub async fn github_init(
        auth_session: AuthSession,
        session: Session,
        Query(NextQuery { next }): Query<NextQuery>,
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

    pub async fn google_init(
        auth_session: AuthSession,
        session: Session,
        Query(NextQuery { next }): Query<NextQuery>,
    ) -> impl IntoResponse {
        let (auth_url, csrf_state) = auth_session.backend.authorize_g_url();

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

    // ---- OAuth callbacks: authenticate user, create handoff code, redirect to BFF ----

    #[axum::debug_handler]
    pub async fn github_callback(
        auth_session: AuthSession,
        session: Session,
        Query(AuthzResp {
            code,
            state: new_state,
        }): Query<AuthzResp>,
    ) -> impl IntoResponse {
        let Ok(Some(old_state)) = session.get(CSRF_STATE_KEY).await else {
            return StatusCode::BAD_REQUEST.into_response();
        };

        let creds = Credentials::AccessToken(OAuthCreds {
            code,
            old_state,
            new_state,
        });

        let user: User = match auth_session.authenticate(creds).await {
            Ok(Some(user)) => user,
            Ok(None) => {
                tracing::warn!("github oauth: invalid CSRF state");
                return (StatusCode::UNAUTHORIZED, "Invalid CSRF state.").into_response();
            }
            Err(e) => {
                tracing::error!(error = ?e, "github oauth: authentication error");
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
        };

        tracing::info!(user_id = user.id(), "github oauth login succeeded");
        redirect_to_bff_with_handoff(&auth_session.backend.db, user.id()).await
    }

    pub async fn google_callback(
        auth_session: AuthSession,
        session: Session,
        Query(AuthzResp {
            code,
            state: new_state,
        }): Query<AuthzResp>,
    ) -> impl IntoResponse {
        let Ok(Some(old_state)) = session.get(CSRF_STATE_KEY).await else {
            return StatusCode::BAD_REQUEST.into_response();
        };

        match auth_session
            .authenticate(Credentials::GoogleToken(OAuthCreds {
                code,
                old_state,
                new_state,
            }))
            .await
        {
            Ok(Some(user)) => {
                tracing::info!(user_id = user.id(), "google oauth login succeeded");
                redirect_to_bff_with_handoff(&auth_session.backend.db, user.id()).await
            }
            Ok(None) => {
                tracing::warn!("google oauth: invalid CSRF state");
                (StatusCode::UNAUTHORIZED, "Invalid CSRF state.").into_response()
            }
            Err(e) => match e {
                axum_login::Error::Backend(be) => match be {
                    BackendError::EmailAlreadyInUse => {
                        tracing::warn!("google oauth: email already in use by a password account");
                        let bff_url = bff_base();
                        Redirect::to(&format!("{}/login?error=email_exists", bff_url))
                            .into_response()
                    }
                    _ => {
                        tracing::error!(error = ?be, "google oauth: backend error");
                        StatusCode::INTERNAL_SERVER_ERROR.into_response()
                    }
                },
                _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            },
        }
    }

    fn bff_base() -> String {
        std::env::var("BFF_CALLBACK_URL").unwrap_or_else(|_| "http://localhost:8080".to_string())
    }

    async fn redirect_to_bff_with_handoff(
        db: &sqlx::PgPool,
        user_id: i64,
    ) -> axum::response::Response {
        let code = Ulid::new().to_string();
        if let Err(e) =
            sqlx::query("INSERT INTO oauth_handoff_codes (code, user_id) VALUES ($1, $2)")
                .bind(&code)
                .bind(user_id)
                .execute(db)
                .await
        {
            tracing::error!("Failed to insert handoff code: {e}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }

        Redirect::to(&format!("{}/oauth/callback?code={}", bff_base(), code)).into_response()
    }
}
