use crate::auth::{
    core::NEXT_URL_KEY,
    user::{OAuthCreds, User},
};
use axum::{
    extract::Query,
    http::StatusCode,
    response::{IntoResponse, Redirect},
    routing::get,
    Router,
};
use axum_login::tower_sessions::Session;
use oauth2::CsrfToken;
use serde::Deserialize;

pub const CSRF_STATE_KEY: &str = "oauth.csrf-state";

#[derive(Clone, Deserialize, Debug)]
pub struct AuthzResp {
    code: String,
    state: CsrfToken,
}

use crate::auth::user::{AuthSession, Credentials};

pub fn router() -> Router<()> {
    Router::new().route("/api/login/oauth/callback", get(self::get::callback))
}

mod get {
    use super::*;

    pub async fn callback(
        mut auth_session: AuthSession,
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
                return (StatusCode::UNAUTHORIZED, "Invalid CSRF state.".to_string())
                    .into_response()
            }
            Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        };

        if auth_session.login(&user).await.is_err() {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }

        if let Ok(Some(next)) = session.remove::<String>(NEXT_URL_KEY).await {
            Redirect::to(&next).into_response()
        } else {
            Redirect::to("/").into_response()
        }
    }
}
