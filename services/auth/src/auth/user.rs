use axum::Json;
use axum_login::{AuthUser, AuthnBackend, UserId};
use chrono::{DateTime, Utc};
use oauth2::{
    AuthorizationCode, CsrfToken, EndpointNotSet, EndpointSet, RedirectUrl, Scope, TokenResponse,
    basic::{BasicClient, BasicRequestTokenError},
    http::header::{AUTHORIZATION, USER_AGENT},
    url::Url,
};
use password_auth::verify_password;
use serde::{Deserialize, Serialize};
use sqlx::prelude::FromRow;
use tracing::Instrument;

use super::trace::{self, Peer, TracedClient};

#[derive(Clone, Serialize, Deserialize, FromRow)]
pub struct User {
    pub id: i64,
    pub username: String,
    email: Option<String>,
    password: Option<String>,
    access_token: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ClientUser {
    pub id: i64,
    pub username: String,
    pub email: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct BffToken {
    pub token: String,
    pub user_id: i64,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone, Serialize, Deserialize, FromRow)]
pub struct GoogleUserInfo {
    email: String,
    name: Option<String>,
    picture: Option<String>,
}

impl std::fmt::Debug for ClientUser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientUser")
            .field("id", &self.id)
            .field("username", &self.username)
            .field("email", &self.email)
            .finish()
    }
}

impl From<User> for ClientUser {
    fn from(user: User) -> Self {
        Self {
            id: user.id,
            username: user.username,
            email: user.email,
        }
    }
}

impl std::fmt::Debug for User {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("User")
            .field("id", &self.id)
            .field("username", &self.username)
            .finish()
    }
}

impl AuthUser for User {
    type Id = i64;

    fn id(&self) -> Self::Id {
        self.id
    }

    /// The session auth hash is used to authenticate the session. This is used to verify that the
    /// session is still valid.
    fn session_auth_hash(&self) -> &[u8] {
        if let Some(access_token) = &self.access_token {
            return access_token.as_bytes();
        }

        if let Some(password) = &self.password {
            return password.as_bytes();
        }

        &[]
    }
}

#[derive(Debug, Clone, Deserialize)]
pub enum Credentials {
    Password(PasswordCreds),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum OAuthProvider {
    Github,
    Google,
}

#[derive(Debug)]
pub enum UserError {
    UserAlreadyExists,
    EmailAlreadyInUse,
    DatabaseError(sqlx::Error),
}

impl std::fmt::Display for UserError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UserError::UserAlreadyExists => write!(f, "User already exists"),
            UserError::EmailAlreadyInUse => write!(f, "Email already in use"),
            UserError::DatabaseError(err) => write!(f, "Database error: {}", err),
        }
    }
}

impl axum::response::IntoResponse for UserError {
    fn into_response(self) -> axum::response::Response {
        let (status, error_message) = match self {
            UserError::UserAlreadyExists | UserError::EmailAlreadyInUse => {
                (axum::http::StatusCode::CONFLICT, self.to_string())
            }
            UserError::DatabaseError(_) => (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "Internal server error".to_string(),
            ),
        };

        let body = Json(serde_json::json!({ "error": error_message }));
        (status, body).into_response()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct PasswordCreds {
    pub username: String,
    pub password: String,
    pub next: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SignUpCreds {
    pub username: String,
    pub email: String,
    pub password: String,
}

#[derive(Debug, Clone, Deserialize)]
struct UserInfo {
    login: String,
}

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error(transparent)]
    Sqlx(sqlx::Error),

    #[error("An account with this email already exists and was created with a password")]
    EmailAlreadyInUse,

    #[error(transparent)]
    Reqwest(reqwest_middleware::Error),

    #[error(transparent)]
    OAuth2(BasicRequestTokenError<reqwest_middleware::Error>),

    #[error(transparent)]
    TaskJoin(#[from] tokio::task::JoinError),
}

impl From<sqlx::Error> for BackendError {
    fn from(val: sqlx::Error) -> BackendError {
        BackendError::Sqlx(val)
    }
}

impl BackendError {
    /// `error.type` for a span, or `None` when it isn't a failure (a taken email is an
    /// answer). The error's text stays off spans: it can quote the provider's reply.
    fn error_type(&self) -> Option<std::borrow::Cow<'static, str>> {
        match self {
            BackendError::Sqlx(e) => Some(trace::sqlx_error_type(e)),
            BackendError::EmailAlreadyInUse => None,
            BackendError::Reqwest(_) => Some("provider_request".into()),
            BackendError::OAuth2(_) => Some("token_request".into()),
            BackendError::TaskJoin(_) => Some("task_join".into()),
        }
    }

    /// Marks the current span as failed by this error (see [`trace::fail`]).
    fn fail_span(&self) {
        if let Some(error_type) = self.error_type() {
            trace::fail(&tracing::Span::current(), error_type);
        }
    }
}

/// The client for an OAuth provider's token and user-info endpoints. No redirects: a token
/// request must not be sent on to wherever a reply points (SSRF).
fn provider_client(service: &'static str) -> TracedClient {
    trace::client(
        Peer { service, expected: &[] },
        std::time::Duration::from_secs(10),
        reqwest::redirect::Policy::none(),
    )
}

/// oauth2's HTTP client: `request_async(&|req| oauth_http(client.clone(), req))`. The crate's
/// own reqwest client is off (it would be untraced); this sends the request through a
/// [`TracedClient`], so the token request is a CLIENT span and carries `traceparent`. The
/// client is taken by value (a cheap clone): a future borrowing it isn't `Send` enough for
/// an axum handler.
async fn oauth_http(
    client: TracedClient,
    request: oauth2::HttpRequest,
) -> Result<oauth2::HttpResponse, reqwest_middleware::Error> {
    let (parts, body) = request.into_parts();
    let response = client
        .request(parts.method, parts.uri.to_string())
        .headers(parts.headers)
        .body(body)
        .send()
        .await?;
    let (status, headers) = (response.status(), response.headers().clone());
    let mut reply = oauth2::HttpResponse::new(response.bytes().await?.to_vec());
    *reply.status_mut() = status;
    *reply.headers_mut() = headers;
    Ok(reply)
}

/// The provider's description of the user the access token belongs to.
#[tracing::instrument(name = "oauth.user_info", skip_all)]
async fn user_info<T: serde::de::DeserializeOwned>(
    http: &TracedClient,
    url: &str,
    access_token: &str,
) -> Result<T, BackendError> {
    let info: Result<T, reqwest_middleware::Error> = async {
        let response = http
            .get(url)
            .header(USER_AGENT.as_str(), "milesstorm-auth")
            .header(AUTHORIZATION.as_str(), format!("Bearer {access_token}"))
            .send()
            .await?;
        Ok(response.json().await?)
    }
    .await;
    info.map_err(BackendError::Reqwest).inspect_err(BackendError::fail_span)
}

#[derive(Debug, Clone)]
pub struct Backend {
    pub db: super::Db,
    client: BasicClientSet,
    g_client: BasicClientSet,
    github_http: TracedClient,
    google_http: TracedClient,
    /// For the game-server host (the session routes in `permissions.rs`).
    pub game_http: TracedClient,
}

pub type BasicClientSet =
    BasicClient<EndpointSet, EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointSet>;

impl Backend {
    pub fn new(db: super::Db, client: BasicClientSet, g_client: BasicClientSet, game_http: TracedClient) -> Self {
        let bff_callback_url = std::env::var("BFF_CALLBACK_URL")
            .unwrap_or_else(|_| "http://localhost:8080".to_string());
        let g_client = g_client.set_redirect_uri(
            RedirectUrl::new(format!("{bff_callback_url}/oauth/callback/google"))
                .expect("invalid redirect uri"),
        );
        let client = client.set_redirect_uri(
            RedirectUrl::new(format!("{bff_callback_url}/oauth/callback/github"))
                .expect("invalid redirect uri"),
        );

        Self {
            db,
            client,
            g_client,
            github_http: provider_client("github"),
            google_http: provider_client("google"),
            game_http,
        }
    }

    #[tracing::instrument(name = "oauth.complete", skip_all, fields(provider = ?provider))]
    pub async fn complete_oauth(
        &self,
        provider: OAuthProvider,
        code: String,
    ) -> Result<User, BackendError> {
        self.complete_oauth_steps(provider, code).await.inspect_err(BackendError::fail_span)
    }

    async fn complete_oauth_steps(
        &self,
        provider: OAuthProvider,
        code: String,
    ) -> Result<User, BackendError> {
        match provider {
            OAuthProvider::Github => {
                let token_res = async {
                    self.client
                        .exchange_code(AuthorizationCode::new(code))
                        .request_async(&|req| oauth_http(self.github_http.clone(), req))
                        .await
                        .map_err(BackendError::OAuth2)
                        .inspect_err(BackendError::fail_span)
                }
                .instrument(tracing::info_span!("oauth.code_exchange"))
                .await?;

                let user_info: UserInfo =
                    user_info(&self.github_http, "https://api.github.com/user", token_res.access_token().secret()).await?;

                // The `WHERE users.password IS NULL` guards against account takeover:
                // if a password account already owns this username, the conflict update is
                // skipped, RETURNING yields no row, and we treat that as an account collision.
                let user: Option<User> = sqlx::query_as(
                    r#"
                    insert into users (username, access_token)
                    values ($1, $2)
                    on conflict(username) do update
                    set access_token = excluded.access_token
                    where users.password is null
                    returning *
                    "#,
                )
                .bind(user_info.login)
                .bind(token_res.access_token().secret())
                .fetch_optional(&self.db)
                .await?;

                user.ok_or(BackendError::EmailAlreadyInUse)
            }
            OAuthProvider::Google => {
                let token_res = async {
                    self.g_client
                        .exchange_code(AuthorizationCode::new(code))
                        .request_async(&|req| oauth_http(self.google_http.clone(), req))
                        .await
                        .map_err(BackendError::OAuth2)
                        .inspect_err(BackendError::fail_span)
                }
                .instrument(tracing::info_span!("oauth.code_exchange"))
                .await?;

                let user_info: GoogleUserInfo =
                    user_info(&self.google_http, "https://www.googleapis.com/oauth2/v2/userinfo", token_res.access_token().secret()).await?;

                // Identify Google users by email (unique on Google's side and in our schema).
                // Matching on `username` would let two Googlers with the same display name
                // overwrite each other's row.
                let existing: Option<User> = sqlx::query_as("select * from users where email = $1")
                    .bind(&user_info.email)
                    .fetch_optional(&self.db)
                    .await?;

                if let Some(existing) = existing {
                    if existing.password.is_some() {
                        return Err(BackendError::EmailAlreadyInUse);
                    }
                    // Returning Google user — refresh the access token by id (stable).
                    let user = sqlx::query_as(
                        "update users set access_token = $1 where id = $2 returning *",
                    )
                    .bind(token_res.access_token().secret())
                    .bind(existing.id)
                    .fetch_one(&self.db)
                    .await?;
                    return Ok(user);
                }

                // First-time Google login; Google has confirmed the address. A username collision with another account would
                // surface as a unique-violation Sqlx error rather than silently overwriting.
                let username = user_info.name.clone().unwrap_or_else(|| {
                    user_info
                        .email
                        .split('@')
                        .next()
                        .unwrap_or("user")
                        .to_string()
                });

                let user = sqlx::query_as(
                    r#"
                    insert into users (username, email, access_token, email_verified_at)
                    values ($1, $2, $3, now())
                    returning *
                    "#,
                )
                .bind(&username)
                .bind(&user_info.email)
                .bind(token_res.access_token().secret())
                .fetch_one(&self.db)
                .await?;

                Ok(user)
            }
        }
    }

    pub fn authorize_url(&self) -> (Url, CsrfToken) {
        self.client
            .authorize_url(CsrfToken::new_random)
            .add_scope(Scope::new(String::from("read:user")))
            .add_scope(Scope::new(String::from("user:email")))
            .url()
    }

    pub fn authorize_g_url(&self) -> (Url, CsrfToken) {
        self.g_client
            .authorize_url(CsrfToken::new_random)
            .add_scope(Scope::new(String::from("profile")))
            .add_scope(Scope::new(String::from("email")))
            .add_scope(Scope::new(String::from("openid")))
            .url()
    }

    pub async fn register_user(
        &self,
        username: &str,
        email: &str,
        password: &str,
    ) -> Result<User, UserError> {
        // insert into database and return the new user, if the user already exists,
        // return an error indicating if the username or email already exists

        // password is slow, so spawn off a thread to do the hashing
        let password = password.to_owned();
        let hashed_password = trace::spawn_blocking("password.hash", move || password_auth::generate_hash(password))
            .await
            .expect("password hashing failed");

        let user = sqlx::query_as::<_, User>(
            r#"
            SELECT * FROM insert_user($1, $2, $3);
            "#,
        )
        .bind(username)
        .bind(email)
        .bind(hashed_password)
        .fetch_one(&self.db)
        .await;

        match user {
            Ok(user) => Ok(user),
            Err(e) => match e {
                sqlx::Error::Database(db_err) if db_err.message().contains("UserAlreadyExists") => {
                    Err(UserError::UserAlreadyExists)
                }
                sqlx::Error::Database(db_err) if db_err.message().contains("EmailAlreadyInUse") => {
                    Err(UserError::EmailAlreadyInUse)
                }
                _ => Err(UserError::DatabaseError(e)),
            },
        }
    }
}

impl AuthnBackend for Backend {
    type User = User;
    type Credentials = Credentials;
    type Error = BackendError;

    #[tracing::instrument(name = "user.authenticate", skip_all)]
    async fn authenticate(
        &self,
        creds: Self::Credentials,
    ) -> Result<Option<Self::User>, Self::Error> {
        let Credentials::Password(password_cred) = creds;

        let user: Option<Self::User> =
            sqlx::query_as("select * from users where username = $1 and password is not null")
                .bind(password_cred.username)
                .fetch_optional(&self.db)
                .await
                .inspect_err(trace::db_failed)?;

        // Verifying the password is blocking and potentially slow, so we'll do so via
        // `spawn_blocking`.
        let verified = trace::spawn_blocking("password.verify", move || {
            user.filter(|user| {
                let Some(ref password) = user.password else {
                    return false;
                };
                verify_password(password_cred.password, password).is_ok()
            })
        })
        .await
        .inspect_err(|_| trace::fail(&tracing::Span::current(), "task_join"))?;
        Ok(verified)
    }

    async fn get_user(&self, user_id: &UserId<Self>) -> Result<Option<Self::User>, Self::Error> {
        Ok(sqlx::query_as("select * from users where id = $1")
            .bind(user_id)
            .fetch_optional(&self.db)
            .await?)
    }
}

// type alias for convenience
pub type AuthSession = axum_login::AuthSession<Backend>;

#[cfg(test)]
mod tests {
    use axum::routing::{get, post};
    use oauth2::{AuthUrl, ClientId, TokenUrl};
    use opentelemetry::trace::{SpanKind, Status, TraceContextExt as _};
    use tracing_opentelemetry::OpenTelemetrySpanExt as _;

    use super::super::trace::testing::{attr, echo_traceparent, pipeline, serve, text};
    use super::*;

    /// A token endpoint whose access token is the `traceparent` it was sent.
    async fn token(headers: axum::http::HeaderMap) -> Json<serde_json::Value> {
        Json(serde_json::json!({ "access_token": echo_traceparent(headers).await, "token_type": "bearer" }))
    }

    #[tokio::test]
    async fn token_requests_are_client_spans_and_follow_no_redirect() {
        let traced = pipeline();
        let provider = serve(
            axum::Router::new()
                .route("/token", post(token))
                .route("/moved", post(|| async { axum::response::Redirect::temporary("/token") })),
        )
        .await;
        let http = provider_client("github");
        let oauth = |path: &str| {
            BasicClient::new(ClientId::new("id".into()))
                .set_auth_uri(AuthUrl::new(format!("{provider}/authorize")).unwrap())
                .set_token_uri(TokenUrl::new(format!("{provider}{path}")).unwrap())
        };

        let caller = tracing::info_span!("oauth.code_exchange");
        let caller_sc = caller.context().span().span_context().clone();
        let (token, moved) = async {
            let token = oauth("/token")
                .exchange_code(AuthorizationCode::new("code".into()))
                .request_async(&|req| oauth_http(http.clone(), req))
                .await
                .unwrap();
            let moved = oauth("/moved")
                .exchange_code(AuthorizationCode::new("code".into()))
                .request_async(&|req| oauth_http(http.clone(), req))
                .await;
            (token, moved)
        }
        .instrument(caller)
        .await;

        // One request to /token: the redirect from /moved wasn't followed.
        let span = traced.span("POST /token");
        assert_eq!(span.span_kind, SpanKind::Client);
        assert_eq!(span.parent_span_id, caller_sc.span_id());
        assert_eq!(attr(&span, "peer.service").as_deref(), Some("github"));
        assert_eq!(
            token.access_token().secret(),
            &format!("00-{}-{}-01", caller_sc.trace_id(), span.span_context.span_id())
        );
        assert!(moved.is_err());
        assert_eq!(attr(&traced.span("POST /moved"), "http.response.status_code").as_deref(), Some("307"));
    }

    #[tokio::test]
    async fn user_info_failures_mark_the_span_without_the_reply() {
        let traced = pipeline();
        let provider = serve(
            axum::Router::new()
                .route("/user", get(|headers: axum::http::HeaderMap| async move {
                    assert_eq!(headers["authorization"], "Bearer token");
                    Json(serde_json::json!({ "login": "ada" }))
                }))
                .route("/odd", get(|| async { Json(serde_json::json!({ "login": 5 })) })),
        )
        .await;
        let http = provider_client("github");

        let info: UserInfo = user_info(&http, &format!("{provider}/user"), "token").await.unwrap();
        assert_eq!(info.login, "ada");
        assert!(user_info::<UserInfo>(&http, &format!("{provider}/odd"), "token").await.is_err());

        let [ok, failed] = &traced.named("oauth.user_info")[..] else { panic!("two user-info spans") };
        assert_eq!(ok.status, Status::Unset);
        assert_eq!(traced.span("GET /user").parent_span_id, ok.span_context.span_id());
        assert_eq!(failed.status, Status::error(""));
        assert_eq!(attr(failed, "error.type").as_deref(), Some("provider_request"));
        assert!(!text(failed).contains("invalid type"), "the decode error's text is on the span");
    }
}
