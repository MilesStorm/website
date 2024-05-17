use axum::{http::StatusCode, response::IntoResponse, routing::get, Router};

// #[derive(Template)]
// #[template(path = "protected.html")]
// struct ProtectedTemplate<'a> {
//     username: &'a str,
// }

pub fn router() -> Router<()> {
    Router::new().route("/api/profile", get(self::get::protected))
}

mod get {
    use super::*;
    use crate::auth::user::AuthSession;

    pub async fn protected(auth_session: AuthSession) -> impl IntoResponse {
        match auth_session.user {
            Some(user) => user.username.to_string().into_response(),
            None => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        }
    }
}
