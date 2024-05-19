use axum::{response::IntoResponse, routing::get, Router};

pub fn router() -> Router<()> {
    Router::new().route("/api/login/status", get(self::get::protected))
}

mod get {
    use axum::Json;

    use super::*;
    use crate::auth::{core::ApiResponse, user::AuthSession};

    pub async fn protected(auth_session: AuthSession) -> impl IntoResponse {
        match auth_session.user {
            Some(mut user) => {
                user.password = None;
                user.access_token = None;
                Json(ApiResponse::new("Logged in", Some(user)))
            }
            // None => json!({"username": None, "error": Some("Not logged in")}),
            None => Json(ApiResponse::new("Not logged in", None)),
        }
        .into_response()
    }
}
