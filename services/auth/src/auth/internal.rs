use axum::{
    Json, Router,
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    middleware::{self, Next},
    response::IntoResponse,
    routing::post,
};
use jsonwebtoken::{EncodingKey, Header, encode};
use password_auth::verify_password;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tokio::task;
use ulid::Ulid;

use super::user::{Backend, BackendError, BffToken, OAuthProvider};

#[derive(Clone)]
pub struct InternalState {
    pub db: PgPool,
    pub jwt_secret: String,
    pub service_secret: String,
    pub backend: Backend,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JwtClaims {
    pub sub: String,
    pub preferred_username: String,
    pub permissions: Vec<String>,
    pub exp: usize,
    pub iat: usize,
    pub iss: String,
}

pub fn router(state: InternalState) -> Router<()> {
    Router::new()
        .route("/internal/token/exchange", post(exchange_password))
        .route("/internal/token/introspect", post(introspect))
        .route("/internal/register", post(register))
        .route("/internal/oauth/start", post(oauth_start))
        .route("/internal/oauth/exchange", post(oauth_exchange))
        .route("/internal/ark/num_players", post(ark_num_players))
        .route("/internal/ark/command", post(ark_command))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            verify_service_token,
        ))
        .with_state(state)
}

async fn create_bff_token(db: &PgPool, user_id: i64) -> Result<BffToken, sqlx::Error> {
    sqlx::query_as("INSERT INTO bff_tokens (token, user_id) VALUES ($1, $2) RETURNING *")
        .bind(Ulid::new().to_string())
        .bind(user_id)
        .fetch_one(db)
        .await
}

async fn verify_service_token(
    State(state): State<InternalState>,
    req: Request<Body>,
    next: Next,
) -> impl IntoResponse {
    let token = req
        .headers()
        .get("x-service-token")
        .and_then(|v| v.to_str().ok());

    if token != Some(state.service_secret.as_str()) {
        return (StatusCode::UNAUTHORIZED, "Invalid service token").into_response();
    }

    next.run(req).await
}

// ---- Token exchange (password credentials) ----

#[derive(Deserialize)]
struct ExchangePasswordReq {
    username: String,
    password: String,
}

#[derive(Serialize)]
struct TokenResp {
    token: String,
    username: String,
}

#[derive(sqlx::FromRow)]
struct UserRow {
    id: i64,
    username: String,
    password: Option<String>,
}

async fn exchange_password(
    State(state): State<InternalState>,
    Json(req): Json<ExchangePasswordReq>,
) -> impl IntoResponse {
    let user: Option<UserRow> = sqlx::query_as(
        "SELECT id, username, password FROM users WHERE username = $1 AND password IS NOT NULL",
    )
    .bind(&req.username)
    .fetch_optional(&state.db)
    .await
    .unwrap_or(None);

    let Some(user) = user else {
        return (StatusCode::UNAUTHORIZED, "Invalid credentials").into_response();
    };

    let password_hash = user.password.clone().unwrap_or_default();
    let input = req.password.clone();

    let valid = task::spawn_blocking(move || verify_password(input, &password_hash).is_ok())
        .await
        .unwrap_or(false);

    if !valid {
        tracing::warn!(username = %req.username, "password login failed: invalid credentials");
        return (StatusCode::UNAUTHORIZED, "Invalid credentials").into_response();
    }

    match create_bff_token(&state.db, user.id).await {
        Ok(bff_token) => {
            tracing::info!(user_id = user.id, username = %user.username, "password login succeeded");
            Json(TokenResp { token: bff_token.token, username: user.username }).into_response()
        }
        Err(e) => {
            tracing::error!(user_id = user.id, error = %e, "failed to insert bff_token");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

// ---- OAuth start / exchange (BFF-mediated) ----

#[derive(Deserialize)]
struct OAuthStartReq {
    provider: OAuthProvider,
}

#[derive(Serialize)]
struct OAuthStartResp {
    auth_url: String,
    state: String,
}

#[derive(Deserialize)]
struct OAuthExchangeReq {
    provider: OAuthProvider,
    code: String,
}

async fn oauth_start(
    State(state): State<InternalState>,
    Json(req): Json<OAuthStartReq>,
) -> impl IntoResponse {
    let (url, csrf) = match req.provider {
        OAuthProvider::Github => state.backend.authorize_url(),
        OAuthProvider::Google => state.backend.authorize_g_url(),
    };
    Json(OAuthStartResp {
        auth_url: url.to_string(),
        state: csrf.secret().to_string(),
    })
    .into_response()
}

async fn oauth_exchange(
    State(state): State<InternalState>,
    Json(req): Json<OAuthExchangeReq>,
) -> impl IntoResponse {
    let user = match state.backend.complete_oauth(req.provider, req.code).await {
        Ok(u) => u,
        Err(BackendError::EmailAlreadyInUse) => {
            tracing::warn!("oauth exchange: email already in use by a password account");
            return (
                StatusCode::CONFLICT,
                "Email already in use by a password account",
            )
                .into_response();
        }
        Err(e) => {
            tracing::error!(error = ?e, "oauth exchange failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let bff_token = match create_bff_token(&state.db, user.id).await {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(user_id = user.id, error = %e, "failed to insert bff_token after oauth exchange");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    tracing::info!(user_id = user.id, username = %user.username, "oauth exchange succeeded");
    Json(TokenResp {
        token: bff_token.token,
        username: user.username,
    })
    .into_response()
}

// ---- Token introspection → JWT ----

#[derive(Deserialize)]
struct IntrospectReq {
    token: String,
}

#[derive(Serialize)]
struct IntrospectResp {
    jwt: String,
    username: String,
    permissions: Vec<String>,
}

#[derive(sqlx::FromRow)]
struct TokenRow {
    user_id: i64,
    username: String,
}

async fn introspect(
    State(state): State<InternalState>,
    Json(req): Json<IntrospectReq>,
) -> impl IntoResponse {
    let row: Option<TokenRow> = sqlx::query_as(
        r#"
        SELECT t.user_id, u.username
        FROM bff_tokens t
        JOIN users u ON u.id = t.user_id
        WHERE t.token = $1 AND t.expires_at > NOW()
        "#,
    )
    .bind(&req.token)
    .fetch_optional(&state.db)
    .await
    .unwrap_or(None);

    let Some(row) = row else {
        return (StatusCode::UNAUTHORIZED, "Invalid or expired token").into_response();
    };

    let permissions: Vec<String> = sqlx::query_scalar(
        r#"
        SELECT permissions.name
        FROM user_roles
        JOIN role_permissions ON user_roles.role_id = role_permissions.role_id
        JOIN permissions ON role_permissions.permission_id = permissions.id
        WHERE user_roles.user_id = $1
        "#,
    )
    .bind(row.user_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as usize;

    let claims = JwtClaims {
        sub: row.user_id.to_string(),
        preferred_username: row.username.clone(),
        permissions: permissions.clone(),
        exp: now + 900, // 15 minute JWT lifetime
        iat: now,
        iss: "milesstorm-auth".to_string(),
    };

    let jwt = match encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(state.jwt_secret.as_bytes()),
    ) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(user_id = row.user_id, error = %e, "JWT encoding failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    tracing::debug!(user_id = row.user_id, username = %row.username, permissions = ?permissions, "token introspected");
    Json(IntrospectResp {
        jwt,
        username: row.username,
        permissions,
    })
    .into_response()
}

// ---- Internal registration ----

#[derive(Deserialize)]
struct RegisterReq {
    username: String,
    email: String,
    password: String,
}

async fn register(
    State(state): State<InternalState>,
    Json(req): Json<RegisterReq>,
) -> impl IntoResponse {
    let password = req.password.clone();
    let hashed = task::spawn_blocking(move || password_auth::generate_hash(password))
        .await
        .expect("password hashing failed");

    let user: Result<UserRow, sqlx::Error> = sqlx::query_as(
        "INSERT INTO users (username, email, password) VALUES ($1, $2, $3) RETURNING id, username, password",
    )
    .bind(&req.username)
    .bind(&req.email)
    .bind(&hashed)
    .fetch_one(&state.db)
    .await;

    match user {
        Ok(u) => {
            match create_bff_token(&state.db, u.id).await {
                Ok(bff_token) => {
                    tracing::info!(user_id = u.id, username = %u.username, "registration succeeded");
                    Json(TokenResp { token: bff_token.token, username: u.username }).into_response()
                }
                Err(e) => {
                    tracing::error!(user_id = u.id, error = %e, "failed to insert bff_token after register");
                    StatusCode::INTERNAL_SERVER_ERROR.into_response()
                }
            }
        }
        Err(sqlx::Error::Database(db_err)) => {
            match db_err.constraint() {
                Some("users_username_key") => {
                    tracing::warn!(username = %req.username, "registration failed: username already exists");
                    (StatusCode::CONFLICT, "User already exists").into_response()
                }
                Some("users_email_key") => {
                    tracing::warn!(email = %req.email, "registration failed: email already in use");
                    (StatusCode::CONFLICT, "Email already in use").into_response()
                }
                _ => {
                    tracing::error!(username = %req.username, error = %db_err, "registration failed");
                    StatusCode::INTERNAL_SERVER_ERROR.into_response()
                }
            }
        }
        Err(e) => {
            tracing::error!(username = %req.username, error = %e, "registration failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

// ---- Internal Ark operations ----

#[derive(Deserialize)]
struct ArkTokenReq {
    token: String,
}

#[derive(Deserialize)]
struct ArkCommandReq {
    token: String,
    cmd: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub enum CommandResult {
    Stopped,
    AlreadyStopped,
    FailedToStop,
    Started,
    AlreadyRunning,
    FailedToStart,
    Timeout,
    Restarting,
    NumPlayers(i32),
}

#[derive(Serialize, Deserialize)]
struct DockerRequestResponse {
    restart_result: String,
    command_result: Option<CommandResult>,
}

async fn resolve_ark_user(db: &PgPool, token: &str) -> Option<i64> {
    let row: Option<(i64,)> = sqlx::query_as(
        r#"
        SELECT t.user_id FROM bff_tokens t
        JOIN user_roles ur ON ur.user_id = t.user_id
        JOIN role_permissions rp ON rp.role_id = ur.role_id
        JOIN permissions p ON p.id = rp.permission_id
        WHERE t.token = $1 AND t.expires_at > NOW() AND p.name = 'llama'
        "#,
    )
    .bind(token)
    .fetch_optional(db)
    .await
    .unwrap_or(None);

    row.map(|(id,)| id)
}

async fn ark_num_players(
    State(state): State<InternalState>,
    Json(req): Json<ArkTokenReq>,
) -> impl IntoResponse {
    let Some(user_id) = resolve_ark_user(&state.db, &req.token).await else {
        tracing::warn!("ark num_players denied: missing llama permission");
        return (StatusCode::FORBIDDEN, "No ark permission").into_response();
    };
    tracing::debug!(user_id, "ark num_players request");

    match reqwest::get("http://192.168.1.21:9090/ark/num_players").await {
        Ok(resp) => match resp.json::<DockerRequestResponse>().await {
            Ok(body) => Json(body).into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        },
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Could not reach ark host").into_response(),
    }
}

async fn ark_command(
    State(state): State<InternalState>,
    Json(req): Json<ArkCommandReq>,
) -> impl IntoResponse {
    let Some(user_id) = resolve_ark_user(&state.db, &req.token).await else {
        tracing::warn!(cmd = %req.cmd, "ark command denied: missing llama permission");
        return (StatusCode::FORBIDDEN, "No ark permission").into_response();
    };

    let cmd = match req.cmd.as_str() {
        "start" | "stop" | "restart" => req.cmd.clone(),
        _ => {
            tracing::warn!(user_id, cmd = %req.cmd, "ark command rejected: unknown command");
            return (StatusCode::BAD_REQUEST, "Unknown command").into_response();
        }
    };
    tracing::info!(user_id, cmd = %cmd, "ark command issued");

    match reqwest::get(format!("http://192.168.1.21:9090/ark/{cmd}")).await {
        Ok(resp) => match resp.json::<DockerRequestResponse>().await {
            Ok(body) => Json(body).into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        },
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Could not reach ark host").into_response(),
    }
}
