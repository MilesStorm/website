use axum::{async_trait, Json};
use axum_login::{AuthUser, AuthnBackend, UserId};
use oauth2::{
    basic::{BasicClient, BasicRequestTokenError},
    http::header::{AUTHORIZATION, USER_AGENT},
    reqwest::{async_http_client, AsyncHttpClientError},
    AuthorizationCode, CsrfToken, RedirectUrl, Scope, TokenResponse,
};
use password_auth::verify_password;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use sqlx::prelude::FromRow;
use tokio::task;

#[derive(Clone, Serialize, Deserialize, FromRow)]
pub struct User {
    id: i64,
    username: String,
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
    AccessToken(OAuthCreds),
    GoogleToken(OAuthCreds),
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
pub struct OAuthCreds {
    pub code: String,
    pub old_state: CsrfToken,
    pub new_state: CsrfToken,
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
    Reqwest(reqwest::Error),

    #[error(transparent)]
    OAuth2(BasicRequestTokenError<AsyncHttpClientError>),

    #[error(transparent)]
    TaskJoin(#[from] tokio::task::JoinError),
}

impl From<sqlx::Error> for BackendError {
    fn from(val: sqlx::Error) -> Self {
        BackendError::Sqlx(val)
    }
}

#[derive(Debug, Clone)]
pub struct Backend {
    pub db: sqlx::PgPool,
    client: BasicClient,
    g_client: BasicClient,
}

impl Backend {
    pub fn new(db: sqlx::PgPool, client: BasicClient, g_client: BasicClient) -> Self {
        let g_client = g_client.set_redirect_uri(
            RedirectUrl::new(String::from(if cfg!(debug_assertions) {
                "http://localhost:8080/api/login/google/callback"
            } else {
                "https://yousofmersal.com/api/login/google/callback"
            }))
            .expect("invalid redirect uri"),
        );
        Self {
            db,
            client,
            g_client,
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
        let hashed_password = task::spawn_blocking(move || password_auth::generate_hash(password))
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

#[async_trait]
impl AuthnBackend for Backend {
    type User = User;
    type Credentials = Credentials;
    type Error = BackendError;

    async fn authenticate(
        &self,
        creds: Self::Credentials,
    ) -> Result<Option<Self::User>, Self::Error> {
        match creds {
            Credentials::Password(password_cred) => {
                let user: Option<Self::User> = sqlx::query_as(
                    "select * from users where username = $1 and password is not null",
                )
                .bind(password_cred.username)
                .fetch_optional(&self.db)
                .await?;

                // Verifying the password is blocking and potentially slow, so we'll do so via
                // `spawn_blocking`.
                task::spawn_blocking(|| {
                    // We're using password-based authentication: this works by comparing our form
                    // input with an argon2 password hash.
                    Ok(user.filter(|user| {
                        let Some(ref password) = user.password else {
                            return false;
                        };
                        verify_password(password_cred.password, password).is_ok()
                    }))
                })
                .await?
            }
            Credentials::AccessToken(oauth_cred) => {
                tracing::debug!("oauth_cred: {:?}", oauth_cred);
                if oauth_cred.old_state.secret() != oauth_cred.new_state.secret() {
                    return Ok(None);
                }

                let token_res = self
                    .client
                    .exchange_code(AuthorizationCode::new(oauth_cred.code))
                    .request_async(async_http_client)
                    .await
                    .map_err(BackendError::OAuth2)?;

                let user_info = reqwest::Client::new()
                    .get("https://api.github.com/user")
                    .header(USER_AGENT.as_str(), "axum-login") // See: https://docs.github.com/en/rest/overview/resources-in-the-rest-api?apiVersion=2022-11-28#user-agent-required
                    .header(
                        AUTHORIZATION.as_str(),
                        format!("Bearer {}", token_res.access_token().secret()),
                    )
                    .send()
                    .await
                    .map_err(Self::Error::Reqwest)?
                    .json::<UserInfo>()
                    .await
                    .map_err(Self::Error::Reqwest)?;

                let user = sqlx::query_as(
                    r#"
                    insert into users (username, access_token)
                    values ($1, $2)
                    on conflict(username) do update
                    set access_token = excluded.access_token
                    returning *
                    "#,
                )
                .bind(user_info.login)
                .bind(token_res.access_token().secret())
                .fetch_one(&self.db)
                .await?;

                Ok(Some(user))
            }
            Credentials::GoogleToken(oauth_cred) => {
                tracing::info!("Google token: {:?}", oauth_cred);
                if oauth_cred.old_state.secret() != oauth_cred.new_state.secret() {
                    return Ok(None);
                }

                let token_res = self
                    .g_client
                    .exchange_code(AuthorizationCode::new(oauth_cred.code))
                    .request_async(async_http_client)
                    .await
                    .map_err(BackendError::OAuth2)?;

                tracing::info!("token_res: {:?}", token_res);

                let user_info = reqwest::Client::new()
                    .get("https://www.googleapis.com/oauth2/v2/userinfo")
                    .header(USER_AGENT.as_str(), "axum-login")
                    .header(
                        AUTHORIZATION.as_str(),
                        format!("Bearer {}", token_res.access_token().secret()),
                    )
                    .send()
                    .await
                    .map_err(Self::Error::Reqwest)?
                    .json::<GoogleUserInfo>()
                    .await
                    .map_err(Self::Error::Reqwest)?;

                let existing_user_test: Option<User> = sqlx::query_as(
                    r#"
                    select * from users where email = $1
                    "#,
                )
                .bind(&user_info.email)
                .fetch_optional(&self.db)
                .await?;
                tracing::info!("existing_user_test: {:?}", existing_user_test);

                if let Some(user) = existing_user_test {
                    if user.password.is_some() {
                        tracing::error!("An account with this email already exists!");
                        return Err(BackendError::EmailAlreadyInUse);
                    }
                }

                let user = sqlx::query_as(
                    r#"
                    insert into users (username, email, access_token)
                    values ($1, $2, $3)
                    on conflict(username) do update
                    set email = excluded.email,
                        access_token = excluded.access_token
                    returning *
                    "#,
                )
                .bind(&user_info.name)
                .bind(&user_info.email)
                .bind(token_res.access_token().secret())
                .fetch_one(&self.db)
                .await?;

                Ok(Some(user))
            }
        }
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
