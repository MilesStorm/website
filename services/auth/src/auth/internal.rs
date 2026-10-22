use axum::{
    Json, Router,
    body::Body,
    extract::{Path, Query, State},
    http::{Request, StatusCode},
    middleware::{self, Next},
    response::IntoResponse,
    routing::{get, post},
};
use jsonwebtoken::{EncodingKey, Header, encode};
use password_auth::verify_password;
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use super::account_email;
use super::invites;
use super::mail::Mailer;
use super::telemetry;
use super::trace::{self, TracedClient};
use super::{Db, GAME_HOST};
use super::user::{Backend, BackendError, BffToken, OAuthProvider};

#[derive(Clone)]
pub struct InternalState {
    pub db: Db,
    pub jwt_secret: String,
    pub service_secret: String,
    pub backend: Backend,
    pub mailer: Mailer,
    /// Shared client for the ark host (`Auth::server` builds it once).
    pub ark_http: TracedClient,
    /// The Enshrouded server's control port and query (`Auth::server` builds it once).
    pub enshrouded: std::sync::Arc<super::enshrouded::Control>,
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
        .route("/internal/dataset/consent", post(dataset_consent_get))
        .route("/internal/dataset/consent/set", post(dataset_consent_set))
        .route("/internal/profile", post(profile_get))
        .route("/internal/profile/display_name", post(profile_set_display_name))
        // Admin RBAC management
        .route("/internal/admin/users", get(admin_list_users))
        .route("/internal/admin/users/{user_id}/roles/{role_id}", post(admin_assign_user_role).delete(admin_revoke_user_role))
        .route("/internal/admin/roles", get(admin_list_roles))
        .route("/internal/admin/roles/all", get(admin_list_all_roles))
        .route("/internal/admin/permissions", get(admin_list_permissions))
        .route("/internal/admin/roles/{role_id}/permissions/{permission_id}", post(admin_assign_role_permission).delete(admin_revoke_role_permission))
        // Confirm email, reset password, delete account
        .merge(account_email::routes())
        // Invite links that give a role
        .merge(invites::routes())
        // The Enshrouded server panel
        .merge(super::enshrouded::routes())
        .layer(middleware::from_fn_with_state(
            state.clone(),
            verify_service_token,
        ))
        .with_state(state)
}

#[tracing::instrument(name = "token.create", skip_all)]
async fn create_bff_token(db: &Db, user_id: i64) -> Result<BffToken, sqlx::Error> {
    sqlx::query_as("INSERT INTO bff_tokens (token, user_id) VALUES ($1, $2) RETURNING *")
        .bind(Ulid::new().to_string())
        .bind(user_id)
        .fetch_one(db)
        .await
        .inspect_err(trace::db_failed)
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

#[tracing::instrument(name = "token.exchange.password", skip_all)]
async fn exchange_password(
    State(state): State<InternalState>,
    Json(req): Json<ExchangePasswordReq>,
) -> impl IntoResponse {
    let user: Option<UserRow> = match sqlx::query_as(
        "SELECT id, username, password FROM users WHERE username = $1 AND password IS NOT NULL",
    )
    .bind(&req.username)
    .fetch_optional(&state.db)
    .await {
        Ok(user) => user,
        Err(e) => {
            tracing::error!(error = %e, "password login failed: database unavailable");
            telemetry::login_attempt("password", "error");
            telemetry::token_operation("exchange", "error");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let Some(user) = user else {
        return (StatusCode::UNAUTHORIZED, "Invalid credentials").into_response();
    };

    let password_hash = user.password.clone().unwrap_or_default();
    let input = req.password.clone();

    let valid = match trace::spawn_blocking("password.verify", move || verify_password(input, &password_hash).is_ok()).await {
        Ok(valid) => valid,
        Err(e) => {
            tracing::error!(error = %e, "password verification failed");
            telemetry::login_attempt("password", "error");
            telemetry::token_operation("exchange", "error");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    if !valid {
        tracing::warn!(username = %req.username, "password login failed: invalid credentials");
        telemetry::login_attempt("password", "failure");
        telemetry::token_operation("exchange", "failure");
        return (StatusCode::UNAUTHORIZED, "Invalid credentials").into_response();
    }

    match create_bff_token(&state.db, user.id).await {
        Ok(bff_token) => {
            tracing::info!(user_id = user.id, username = %user.username, "password login succeeded");
            telemetry::login_attempt("password", "success");
            telemetry::token_operation("exchange", "success");
            Json(TokenResp {
                token: bff_token.token,
                username: user.username,
            })
            .into_response()
        }
        Err(e) => {
            tracing::error!(user_id = user.id, error = %e, "failed to insert bff_token");
            telemetry::token_operation("exchange", "error");
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

#[tracing::instrument(name = "auth.oauth_start", skip_all, fields(provider = ?req.provider))]
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

#[tracing::instrument(name = "token.exchange.oauth", skip_all, fields(provider = ?req.provider))]
async fn oauth_exchange(
    State(state): State<InternalState>,
    Json(req): Json<OAuthExchangeReq>,
) -> impl IntoResponse {
    let provider_str = format!("{:?}", req.provider).to_lowercase();

    let user = match state.backend.complete_oauth(req.provider, req.code).await {
        Ok(u) => u,
        Err(BackendError::EmailAlreadyInUse) => {
            tracing::warn!("oauth exchange: email already in use by a password account");
            telemetry::login_attempt(&provider_str, "conflict");
            telemetry::token_operation("exchange", "conflict");
            return (
                StatusCode::CONFLICT,
                "Email already in use by a password account",
            )
                .into_response();
        }
        Err(e) => {
            tracing::error!(error = ?e, "oauth exchange failed");
            telemetry::login_attempt(&provider_str, "error");
            telemetry::token_operation("exchange", "error");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let bff_token = match create_bff_token(&state.db, user.id).await {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(user_id = user.id, error = %e, "failed to insert bff_token after oauth exchange");
            telemetry::token_operation("exchange", "error");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    tracing::info!(user_id = user.id, username = %user.username, "oauth exchange succeeded");
    telemetry::login_attempt(&provider_str, "success");
    telemetry::token_operation("exchange", "success");
    Json(TokenResp {
        token: bff_token.token,
        username: user.username,
    })
    .into_response()
}

// ---- Token introspection → JWT ----

/// Introspection's reply for a token that doesn't exist or has expired.
const INVALID_TOKEN: &str = "Invalid or expired token";

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

#[tracing::instrument(name = "token.introspect", skip_all)]
async fn introspect(
    State(state): State<InternalState>,
    Json(req): Json<IntrospectReq>,
) -> impl IntoResponse {
    let row: Option<TokenRow> = match sqlx::query_as(
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
    {
        Ok(row) => row,
        // Not "invalid token": the website logs a session out only on that reply.
        Err(e) => {
            tracing::error!(error = %e, "token introspection query failed");
            telemetry::token_operation("introspect", "error");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let Some(row) = row else {
        telemetry::token_operation("introspect", "invalid");
        // The website logs a session out on exactly this reply (`INVALID_TOKEN`).
        return (StatusCode::UNAUTHORIZED, INVALID_TOKEN).into_response();
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
    telemetry::token_operation("introspect", "success");
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

#[tracing::instrument(name = "auth.register", skip_all)]
async fn register(
    State(state): State<InternalState>,
    Json(req): Json<RegisterReq>,
) -> impl IntoResponse {
    let Some(email) = account_email::clean_email(&req.email) else {
        return (StatusCode::BAD_REQUEST, "Invalid email address").into_response();
    };
    if !account_email::password_ok(&req.password) {
        return (
            StatusCode::BAD_REQUEST,
            format!("Password must be at least {} characters", account_email::PASSWORD_MIN),
        )
            .into_response();
    }
    // The unique index is case-sensitive; addresses aren't.
    match sqlx::query_scalar::<_, bool>("SELECT EXISTS (SELECT 1 FROM users WHERE LOWER(email) = LOWER($1))")
        .bind(&email)
        .fetch_one(&state.db)
        .await
    {
        Ok(false) => {}
        Ok(true) => {
            tracing::warn!("registration failed: email already in use");
            telemetry::token_operation("register", "conflict");
            return (StatusCode::CONFLICT, "Email already in use").into_response();
        }
        Err(e) => {
            tracing::error!(error = %e, "registration failed: checking the email");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }
    let password = req.password.clone();
    let hashed = trace::spawn_blocking("password.hash", move || password_auth::generate_hash(password))
        .await
        .expect("password hashing failed");

    let user: Result<UserRow, sqlx::Error> = sqlx::query_as(
        "INSERT INTO users (username, email, password) VALUES ($1, $2, $3) RETURNING id, username, password",
    )
    .bind(&req.username)
    .bind(&email)
    .bind(&hashed)
    .fetch_one(&state.db)
    .await;

    match user {
        Ok(u) => match create_bff_token(&state.db, u.id).await {
            Ok(bff_token) => {
                tracing::info!(user_id = u.id, username = %u.username, "registration succeeded");
                telemetry::token_operation("register", "success");
                account_email::send_verification_in_background(&state, u.id);
                Json(TokenResp {
                    token: bff_token.token,
                    username: u.username,
                })
                .into_response()
            }
            Err(e) => {
                tracing::error!(user_id = u.id, error = %e, "failed to insert bff_token after register");
                telemetry::token_operation("register", "error");
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        },
        Err(sqlx::Error::Database(db_err)) => match db_err.constraint() {
            Some("users_username_key") => {
                tracing::warn!(username = %req.username, "registration failed: username already exists");
                telemetry::token_operation("register", "conflict");
                (StatusCode::CONFLICT, "User already exists").into_response()
            }
            Some("users_email_key") => {
                tracing::warn!("registration failed: email already in use");
                telemetry::token_operation("register", "conflict");
                (StatusCode::CONFLICT, "Email already in use").into_response()
            }
            _ => {
                tracing::error!(username = %req.username, error = %db_err, "registration failed");
                telemetry::token_operation("register", "error");
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        },
        Err(e) => {
            tracing::error!(username = %req.username, error = %e, "registration failed");
            telemetry::token_operation("register", "error");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// GET `<base>/ark/<path>` on the game-server host (`GAME_HOST`). The client makes the CLIENT
/// span and sends `traceparent`. The outer error is "unreachable", the inner one a bad body.
async fn ark_get(
    http: &TracedClient,
    base: &str,
    path: &str,
) -> Result<Result<DockerRequestResponse, reqwest::Error>, reqwest_middleware::Error> {
    let resp = http.get(format!("{base}/ark/{path}")).send().await?;
    Ok(resp.json::<DockerRequestResponse>().await)
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

#[tracing::instrument(name = "ark.authorize", skip_all)]
async fn resolve_ark_user(db: &Db, token: &str) -> Option<i64> {
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

// ---- Profile ----
//
// What a logged-in user can see and change about their own account on the website's
// profile page. The username can't be changed: GitHub logins find their account by it.

#[derive(Serialize, sqlx::FromRow)]
struct ProfileResp {
    user_id: i64,
    username: String,
    display_name: Option<String>,
    /// `None` for accounts without one (GitHub logins).
    email: Option<String>,
    email_verified: bool,
}

const PROFILE_COLUMNS: &str =
    "u.id AS user_id, u.username, u.display_name, u.email, u.email_verified_at IS NOT NULL AS email_verified";

/// The token's user (id, username, display name, email), for any valid token.
#[tracing::instrument(name = "profile.resolve", skip_all)]
async fn resolve_profile(db: &Db, token: &str) -> Result<Option<ProfileResp>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {PROFILE_COLUMNS} FROM bff_tokens t JOIN users u ON u.id = t.user_id \
         WHERE t.token = $1 AND t.expires_at > NOW()"
    ))
    .bind(token)
    .fetch_optional(db)
    .await
    .inspect_err(trace::db_failed)
}

#[tracing::instrument(name = "profile.get", skip_all)]
async fn profile_get(
    State(state): State<InternalState>,
    Json(req): Json<ArkTokenReq>,
) -> impl IntoResponse {
    match resolve_profile(&state.db, &req.token).await {
        Ok(Some(p)) => Json(p).into_response(),
        Ok(None) => (StatusCode::UNAUTHORIZED, "Invalid or expired token").into_response(),
        Err(e) => {
            tracing::error!(error = %e, "reading profile failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// Longest display name, in characters.
const DISPLAY_NAME_MAX: usize = 40;

/// The name as it will be stored: trimmed, `None` when empty. `Err` when it is too
/// long or holds control or text-direction characters (which could disguise it).
fn clean_display_name(raw: Option<&str>) -> Result<Option<String>, ()> {
    let Some(name) = raw.map(str::trim).filter(|n| !n.is_empty()) else {
        return Ok(None);
    };
    let bad = |c: char| {
        c.is_control() || matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}')
    };
    if name.chars().count() > DISPLAY_NAME_MAX || name.chars().any(bad) {
        return Err(());
    }
    Ok(Some(name.to_string()))
}

#[derive(Deserialize)]
struct DisplayNameReq {
    token: String,
    /// `None` or blank clears it.
    display_name: Option<String>,
}

#[tracing::instrument(name = "profile.set_display_name", skip_all)]
async fn profile_set_display_name(
    State(state): State<InternalState>,
    Json(req): Json<DisplayNameReq>,
) -> impl IntoResponse {
    let Ok(name) = clean_display_name(req.display_name.as_deref()) else {
        return (StatusCode::BAD_REQUEST, "Invalid display name").into_response();
    };
    let row: Result<Option<ProfileResp>, _> = sqlx::query_as(&format!(
        "UPDATE users u SET display_name = $2 FROM bff_tokens t \
         WHERE t.token = $1 AND t.expires_at > NOW() AND u.id = t.user_id \
         RETURNING {PROFILE_COLUMNS}"
    ))
    .bind(&req.token)
    .bind(&name)
    .fetch_optional(&state.db)
    .await;
    match row {
        Ok(Some(profile)) => {
            tracing::info!(user_id = profile.user_id, "display name changed");
            Json(profile).into_response()
        }
        Ok(None) => (StatusCode::UNAUTHORIZED, "Invalid or expired token").into_response(),
        Err(e) => {
            tracing::error!(error = %e, "saving display name failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

// ---- Dice training-data consent ----
//
// Whether the user shares roll pictures to train the dice reader (the website's
// profile switch). Only users with the `arcane` permission (the dice roller) have
// a choice; the pictures themselves are stored by the website in SurrealDB.

/// The token's user, if the token is valid and the user holds `arcane`.
#[tracing::instrument(name = "dataset.authorize", skip_all)]
async fn resolve_arcane_user(db: &Db, token: &str) -> Option<i64> {
    let row: Option<(i64,)> = sqlx::query_as(
        r#"
        SELECT t.user_id FROM bff_tokens t
        JOIN user_roles ur ON ur.user_id = t.user_id
        JOIN role_permissions rp ON rp.role_id = ur.role_id
        JOIN permissions p ON p.id = rp.permission_id
        WHERE t.token = $1 AND t.expires_at > NOW() AND p.name = 'arcane'
        LIMIT 1
        "#,
    )
    .bind(token)
    .fetch_optional(db)
    .await
    .unwrap_or(None);

    row.map(|(id,)| id)
}

#[derive(Serialize)]
struct DatasetConsentResp {
    share: bool,
}

#[tracing::instrument(name = "dataset.consent_get", skip_all)]
async fn dataset_consent_get(
    State(state): State<InternalState>,
    Json(req): Json<ArkTokenReq>,
) -> impl IntoResponse {
    let Some(user_id) = resolve_arcane_user(&state.db, &req.token).await else {
        return (StatusCode::FORBIDDEN, "No arcane permission").into_response();
    };
    let share: Result<Option<(bool,)>, _> =
        sqlx::query_as("SELECT share FROM dataset_consent WHERE user_id = $1")
            .bind(user_id)
            .fetch_optional(&state.db)
            .await;
    match share {
        // Never asked means not sharing.
        Ok(row) => Json(DatasetConsentResp { share: row.is_some_and(|(s,)| s) }).into_response(),
        Err(e) => {
            tracing::error!(user_id, error = %e, "reading dataset consent failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[derive(Deserialize)]
struct DatasetConsentSetReq {
    token: String,
    share: bool,
    consent_version: String,
}

#[tracing::instrument(name = "dataset.consent_set", skip_all, fields(share = req.share))]
async fn dataset_consent_set(
    State(state): State<InternalState>,
    Json(req): Json<DatasetConsentSetReq>,
) -> impl IntoResponse {
    let Some(user_id) = resolve_arcane_user(&state.db, &req.token).await else {
        return (StatusCode::FORBIDDEN, "No arcane permission").into_response();
    };
    if req.consent_version.is_empty() || req.consent_version.len() > 32 {
        return (StatusCode::BAD_REQUEST, "Invalid consent_version").into_response();
    }
    let res = sqlx::query(
        r#"
        INSERT INTO dataset_consent (user_id, share, consent_version) VALUES ($1, $2, $3)
        ON CONFLICT (user_id) DO UPDATE
            SET share = EXCLUDED.share, consent_version = EXCLUDED.consent_version, updated_at = NOW()
        "#,
    )
    .bind(user_id)
    .bind(req.share)
    .bind(&req.consent_version)
    .execute(&state.db)
    .await;
    match res {
        Ok(_) => {
            tracing::info!(user_id, share = req.share, "dataset consent changed");
            Json(DatasetConsentResp { share: req.share }).into_response()
        }
        Err(e) => {
            tracing::error!(user_id, error = %e, "saving dataset consent failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[tracing::instrument(name = "ark.num_players", skip_all)]
async fn ark_num_players(
    State(state): State<InternalState>,
    Json(req): Json<ArkTokenReq>,
) -> impl IntoResponse {
    let Some(user_id) = resolve_ark_user(&state.db, &req.token).await else {
        tracing::warn!("ark num_players denied: missing llama permission");
        telemetry::ark_command("num_players", "denied");
        return (StatusCode::FORBIDDEN, "No ark permission").into_response();
    };
    tracing::debug!(user_id, "ark num_players request");

    match ark_get(&state.ark_http, GAME_HOST, "num_players").await {
        Ok(body) => match body {
            Ok(body) => {
                telemetry::ark_command("num_players", "success");
                Json(body).into_response()
            }
            Err(e) => {
                telemetry::ark_command("num_players", "error");
                trace::fail(&tracing::Span::current(), "bad_body");
                (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
            }
        },
        Err(_) => {
            telemetry::ark_command("num_players", "unreachable");
            trace::fail(&tracing::Span::current(), "unreachable");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Could not reach ark host",
            )
                .into_response()
        }
    }
}

#[tracing::instrument(name = "ark.command", skip_all, fields(cmd = %req.cmd))]
async fn ark_command(
    State(state): State<InternalState>,
    Json(req): Json<ArkCommandReq>,
) -> impl IntoResponse {
    let Some(user_id) = resolve_ark_user(&state.db, &req.token).await else {
        tracing::warn!(cmd = %req.cmd, "ark command denied: missing llama permission");
        telemetry::ark_command(&req.cmd, "denied");
        return (StatusCode::FORBIDDEN, "No ark permission").into_response();
    };

    let cmd = match req.cmd.as_str() {
        "start" | "stop" | "restart" => req.cmd.clone(),
        _ => {
            tracing::warn!(user_id, cmd = %req.cmd, "ark command rejected: unknown command");
            telemetry::ark_command(&req.cmd, "invalid");
            return (StatusCode::BAD_REQUEST, "Unknown command").into_response();
        }
    };
    tracing::info!(user_id, cmd = %cmd, "ark command issued");

    match ark_get(&state.ark_http, GAME_HOST, &cmd).await {
        Ok(body) => match body {
            Ok(body) => {
                telemetry::ark_command(&cmd, "success");
                Json(body).into_response()
            }
            Err(e) => {
                telemetry::ark_command(&cmd, "error");
                trace::fail(&tracing::Span::current(), "bad_body");
                (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
            }
        },
        Err(_) => {
            telemetry::ark_command(&cmd, "unreachable");
            trace::fail(&tracing::Span::current(), "unreachable");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Could not reach ark host",
            )
                .into_response()
        }
    }
}

// ---- Admin RBAC endpoints ----

#[derive(serde::Deserialize, Default)]
struct PageQuery {
    #[serde(default)]
    page: u32,
    #[serde(default = "default_page_limit")]
    limit: u32,
    #[serde(default)]
    search: String,
}

fn default_page_limit() -> u32 { 25 }

#[derive(Serialize)]
struct PagedResp<T: Serialize> {
    items: Vec<T>,
    total: i64,
}

#[derive(Serialize)]
struct AdminUserResp {
    id: i64,
    username: String,
    email: Option<String>,
    roles: Vec<RoleResp>,
}

#[derive(Serialize, Clone)]
struct RoleResp {
    id: i32,
    name: String,
}

#[derive(Serialize)]
struct PermissionResp {
    id: i32,
    name: String,
}

#[derive(Serialize)]
struct AdminRoleResp {
    id: i32,
    name: String,
    permissions: Vec<PermissionResp>,
}

#[derive(sqlx::FromRow)]
struct UserRow2 {
    id: i64,
    username: String,
    email: Option<String>,
}

#[derive(sqlx::FromRow)]
struct UserRoleRow {
    user_id: i32, // user_roles.user_id is INT (not BIGINT), matching the schema
    role_id: i32,
    role_name: String,
}

#[derive(sqlx::FromRow)]
struct RoleRow {
    id: i32,
    name: String,
}

#[derive(sqlx::FromRow)]
struct RolePermRow {
    role_id: i32,
    permission_id: i32,
    permission_name: String,
}

#[derive(sqlx::FromRow)]
struct PermRow {
    id: i32,
    name: String,
}

#[tracing::instrument(name = "admin.list_users", skip_all)]
async fn admin_list_users(
    State(state): State<InternalState>,
    Query(q): Query<PageQuery>,
) -> impl IntoResponse {
    let search = format!("%{}%", q.search.to_lowercase());
    let offset = (q.page * q.limit) as i64;
    let limit = q.limit as i64;

    let total: (i64,) = match sqlx::query_as(
        "SELECT COUNT(*) FROM users \
         WHERE LOWER(username) LIKE $1 OR LOWER(COALESCE(email, '')) LIKE $1",
    )
    .bind(&search)
    .fetch_one(&state.db)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "admin_list_users: count error");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let users: Vec<UserRow2> = match sqlx::query_as(
        "SELECT id, username, email FROM users \
         WHERE LOWER(username) LIKE $1 OR LOWER(COALESCE(email, '')) LIKE $1 \
         ORDER BY username LIMIT $2 OFFSET $3",
    )
    .bind(&search)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "admin_list_users: db error");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    // Only fetch roles for users on this page.
    let user_ids: Vec<i32> = users.iter().map(|u| u.id as i32).collect();
    let user_roles: Vec<UserRoleRow> = if user_ids.is_empty() {
        vec![]
    } else {
        match sqlx::query_as(
            "SELECT ur.user_id, r.id as role_id, r.name as role_name \
             FROM user_roles ur JOIN roles r ON r.id = ur.role_id \
             WHERE ur.user_id = ANY($1)",
        )
        .bind(&user_ids)
        .fetch_all(&state.db)
        .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(error = %e, "admin_list_users: roles db error");
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
        }
    };

    let items: Vec<AdminUserResp> = users
        .into_iter()
        .map(|u| {
            let roles = user_roles
                .iter()
                .filter(|ur| ur.user_id as i64 == u.id)
                .map(|ur| RoleResp { id: ur.role_id, name: ur.role_name.clone() })
                .collect();
            AdminUserResp { id: u.id, username: u.username, email: u.email, roles }
        })
        .collect();

    Json(PagedResp { items, total: total.0 }).into_response()
}

#[tracing::instrument(name = "admin.list_roles", skip_all)]
async fn admin_list_roles(
    State(state): State<InternalState>,
    Query(q): Query<PageQuery>,
) -> impl IntoResponse {
    let search = format!("%{}%", q.search.to_lowercase());
    let offset = (q.page * q.limit) as i64;
    let limit = q.limit as i64;

    let total: (i64,) = match sqlx::query_as(
        "SELECT COUNT(*) FROM roles WHERE LOWER(name) LIKE $1",
    )
    .bind(&search)
    .fetch_one(&state.db)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "admin_list_roles: count error");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let roles: Vec<RoleRow> = match sqlx::query_as(
        "SELECT id, name FROM roles WHERE LOWER(name) LIKE $1 ORDER BY name LIMIT $2 OFFSET $3",
    )
    .bind(&search)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "admin_list_roles: db error");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    // Only fetch permissions for roles on this page.
    let role_ids: Vec<i32> = roles.iter().map(|r| r.id).collect();
    let role_perms: Vec<RolePermRow> = if role_ids.is_empty() {
        vec![]
    } else {
        match sqlx::query_as(
            "SELECT rp.role_id, p.id as permission_id, p.name as permission_name \
             FROM role_permissions rp JOIN permissions p ON p.id = rp.permission_id \
             WHERE rp.role_id = ANY($1)",
        )
        .bind(&role_ids)
        .fetch_all(&state.db)
        .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(error = %e, "admin_list_roles: perms db error");
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
        }
    };

    let items: Vec<AdminRoleResp> = roles
        .into_iter()
        .map(|r| {
            let permissions = role_perms
                .iter()
                .filter(|rp| rp.role_id == r.id)
                .map(|rp| PermissionResp { id: rp.permission_id, name: rp.permission_name.clone() })
                .collect();
            AdminRoleResp { id: r.id, name: r.name, permissions }
        })
        .collect();

    Json(PagedResp { items, total: total.0 }).into_response()
}

/// Returns all roles (without permissions) for use in assignment dropdowns.
/// Capped at 1000 — roles are admin-defined so this is not expected to be hit.
#[tracing::instrument(name = "admin.list_all_roles", skip_all)]
async fn admin_list_all_roles(State(state): State<InternalState>) -> impl IntoResponse {
    match sqlx::query_as::<_, RoleRow>("SELECT id, name FROM roles ORDER BY name LIMIT 1000")
        .fetch_all(&state.db)
        .await
    {
        Ok(rows) => Json(
            rows.into_iter()
                .map(|r| AdminRoleResp { id: r.id, name: r.name, permissions: vec![] })
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "admin_list_all_roles: db error");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[tracing::instrument(name = "admin.list_permissions", skip_all)]
async fn admin_list_permissions(State(state): State<InternalState>) -> impl IntoResponse {
    match sqlx::query_as::<_, PermRow>("SELECT id, name FROM permissions ORDER BY name")
        .fetch_all(&state.db)
        .await
    {
        Ok(rows) => Json(
            rows.into_iter()
                .map(|p| PermissionResp { id: p.id, name: p.name })
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "admin_list_permissions: db error");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[tracing::instrument(name = "admin.assign_user_role", skip_all, fields(user_id, role_id))]
async fn admin_assign_user_role(
    State(state): State<InternalState>,
    Path((user_id, role_id)): Path<(i64, i32)>,
) -> impl IntoResponse {
    match sqlx::query(
        "INSERT INTO user_roles (user_id, role_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
    )
    .bind(user_id)
    .bind(role_id)
    .execute(&state.db)
    .await
    {
        Ok(_) => {
            tracing::info!(user_id, role_id, "assigned role to user");
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, user_id, role_id, "admin_assign_user_role: db error");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

// ---- The last admin ----

/// Who holds `manage_permissions` (through any of their roles).
const ADMIN_USERS: &str = "SELECT DISTINCT ur.user_id FROM user_roles ur \
     JOIN role_permissions rp ON rp.role_id = ur.role_id JOIN permissions p ON p.id = rp.permission_id \
     WHERE p.name = 'manage_permissions'";

/// Key of the transaction lock that every change able to take `manage_permissions` away takes
/// first ("admins" in ASCII), so two of them can't each count the other's admin as staying.
const ADMINS_LOCK: i64 = 0x6164_6d69_6e73;

/// Takes the admins lock (held until the transaction ends) and counts who holds
/// `manage_permissions`. Called before and after such a change: one that takes the count from
/// some to none is undone, with 409 `last_admin`, so the site can't be left without an admin.
pub(super) async fn admins(tx: &mut trace::Tx<'_>) -> Result<i64, sqlx::Error> {
    sqlx::query("SELECT pg_advisory_xact_lock($1)").bind(ADMINS_LOCK).execute(&mut tx.executor()).await?;
    let (n,): (i64,) = sqlx::query_as(&format!("SELECT COUNT(*) FROM ({ADMIN_USERS}) a"))
        .fetch_one(&mut tx.executor())
        .await?;
    Ok(n)
}

/// Whether `user_id` is the only one who holds `manage_permissions`. Only to refuse early
/// (before emailing a delete link, or before the website removes the account's other data):
/// the deletion itself checks with [`admins`].
pub(super) async fn only_admin(db: &Db, user_id: i64) -> Result<bool, sqlx::Error> {
    let (only,): (bool,) =
        sqlx::query_as(&format!("SELECT COALESCE(bool_and(a.user_id = $1), false) FROM ({ADMIN_USERS}) a"))
            .bind(user_id)
            .fetch_one(db)
            .await
            .inspect_err(trace::db_failed)?;
    Ok(only)
}

/// Runs `change` (taking a role from a user, or a permission from a role) unless that leaves
/// nobody with `manage_permissions`: `Ok(false)` then, with nothing changed.
async fn unless_last_admin(
    db: &Db,
    change: sqlx::query::Query<'_, sqlx::Postgres, sqlx::postgres::PgArguments>,
) -> Result<bool, sqlx::Error> {
    let mut tx = trace::begin(db).await?;
    let before = admins(&mut tx).await?;
    change.execute(&mut tx.executor()).await?;
    if before > 0 && admins(&mut tx).await? == 0 {
        trace::rollback(tx).await;
        return Ok(false);
    }
    trace::commit(tx).await?;
    Ok(true)
}

/// 409 `last_admin`: the change would leave nobody able to use the admin panel.
pub(super) fn last_admin() -> axum::response::Response {
    account_email::error(StatusCode::CONFLICT, "last_admin")
}

#[tracing::instrument(name = "admin.revoke_user_role", skip_all, fields(user_id, role_id))]
async fn admin_revoke_user_role(
    State(state): State<InternalState>,
    Path((user_id, role_id)): Path<(i64, i32)>,
) -> impl IntoResponse {
    let change = sqlx::query("DELETE FROM user_roles WHERE user_id = $1 AND role_id = $2").bind(user_id).bind(role_id);
    match unless_last_admin(&state.db, change).await {
        Ok(true) => {
            tracing::info!(user_id, role_id, "revoked role from user");
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => {
            tracing::warn!(user_id, role_id, "refused to take the last admin's rights");
            last_admin()
        }
        Err(e) => {
            trace::db_failed(&e);
            tracing::error!(error = %e, user_id, role_id, "admin_revoke_user_role: db error");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[tracing::instrument(name = "admin.assign_role_permission", skip_all, fields(role_id, permission_id))]
async fn admin_assign_role_permission(
    State(state): State<InternalState>,
    Path((role_id, permission_id)): Path<(i32, i32)>,
) -> impl IntoResponse {
    match sqlx::query(
        "INSERT INTO role_permissions (role_id, permission_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
    )
    .bind(role_id)
    .bind(permission_id)
    .execute(&state.db)
    .await
    {
        Ok(_) => {
            tracing::info!(role_id, permission_id, "assigned permission to role");
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, role_id, permission_id, "admin_assign_role_permission: db error");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[tracing::instrument(name = "admin.revoke_role_permission", skip_all, fields(role_id, permission_id))]
async fn admin_revoke_role_permission(
    State(state): State<InternalState>,
    Path((role_id, permission_id)): Path<(i32, i32)>,
) -> impl IntoResponse {
    let change = sqlx::query("DELETE FROM role_permissions WHERE role_id = $1 AND permission_id = $2")
        .bind(role_id)
        .bind(permission_id);
    match unless_last_admin(&state.db, change).await {
        Ok(true) => {
            tracing::info!(role_id, permission_id, "revoked permission from role");
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => {
            tracing::warn!(role_id, permission_id, "refused to take the last admin's rights");
            last_admin()
        }
        Err(e) => {
            trace::db_failed(&e);
            tracing::error!(error = %e, role_id, permission_id, "admin_revoke_role_permission: db error");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::clean_display_name;

    /// `ark_get` makes a CLIENT span, and the callee's `OtelAxumLayer` continues the trace
    /// under it. A mismatched OTel matrix still builds but breaks this (TRACING.md,
    /// "Upgrading OpenTelemetry").
    #[tokio::test]
    async fn traceparent_continues_the_trace_across_a_hop() {
        use axum_tracing_opentelemetry::middleware::OtelAxumLayer;
        use opentelemetry::trace::{SpanKind, TraceContextExt as _};
        use tracing::Instrument as _;
        use tracing_opentelemetry::OpenTelemetrySpanExt as _;

        use super::super::trace::{self, Peer, testing::{attr, pipeline, serve}};
        use super::CommandResult;

        let traced = pipeline();
        let ark = serve(
            axum::Router::new()
                .route(
                    "/ark/num_players",
                    axum::routing::get(|| async {
                        axum::Json(serde_json::json!({ "restart_result": "ok", "command_result": { "NumPlayers": 3 } }))
                    }),
                )
                .layer(OtelAxumLayer::default()),
        )
        .await;
        let http = trace::client(
            Peer { service: "ark", expected: &[] },
            std::time::Duration::from_secs(5),
            reqwest::redirect::Policy::default(),
        );

        let caller = tracing::info_span!("caller");
        let caller_sc = caller.context().span().span_context().clone();
        let body = super::ark_get(&http, &ark, "num_players").instrument(caller).await.unwrap().unwrap();
        assert!(matches!(body.command_result, Some(CommandResult::NumPlayers(3))));

        // Both sides name their span after the request.
        let [server, client] = &traced.named("GET /ark/num_players")[..] else { panic!("a CLIENT and a SERVER span") };
        assert_eq!(client.span_kind, SpanKind::Client);
        assert_eq!(client.parent_span_id, caller_sc.span_id());
        assert_eq!(attr(client, "peer.service").as_deref(), Some("ark"));
        // The callee's SERVER span is the CLIENT span's child, in the caller's trace.
        assert_eq!(server.span_kind, SpanKind::Server);
        assert_eq!(server.span_context.trace_id(), caller_sc.trace_id());
        assert_eq!(server.parent_span_id, client.span_context.span_id());
    }

    #[test]
    fn display_name_is_trimmed_and_blank_clears_it() {
        assert_eq!(clean_display_name(Some("  Miles  ")), Ok(Some("Miles".into())));
        assert_eq!(clean_display_name(Some("   ")), Ok(None));
        assert_eq!(clean_display_name(None), Ok(None));
    }

    #[test]
    fn display_name_limits() {
        assert_eq!(clean_display_name(Some(&"é".repeat(40))), Ok(Some("é".repeat(40))));
        assert!(clean_display_name(Some(&"a".repeat(41))).is_err());
        assert!(clean_display_name(Some("a\nb")).is_err());
        assert!(clean_display_name(Some("admin\u{202E}nimda")).is_err());
        assert!(clean_display_name(Some("zero\u{200B}width")).is_err());
    }

    /// `/internal` against a real Postgres (Docker), as the website calls it.
    struct Internal {
        url: String,
        http: super::TracedClient,
        db: super::Db,
        _pg: super::super::trace::testing::TestDb,
    }

    impl Internal {
        async fn start() -> Self {
            use oauth2::{AuthUrl, ClientId, TokenUrl, basic::BasicClient};
            use super::super::trace::{self, Peer, testing::{postgres, serve}};

            let pg = postgres(super::super::pool_options()).await;
            super::super::migrate(&pg.raw, &tower_sessions_sqlx_store::PostgresStore::new(pg.raw.clone())).await.unwrap();
            let db = pg.db();
            let oauth = || {
                BasicClient::new(ClientId::new("test".into()))
                    .set_auth_uri(AuthUrl::new("https://oauth.test/authorize".into()).unwrap())
                    .set_token_uri(TokenUrl::new("https://oauth.test/token".into()).unwrap())
            };
            let http = trace::client(Peer { service: "test", expected: &[] }, std::time::Duration::from_secs(10), reqwest::redirect::Policy::none());
            let state = super::InternalState {
                db: db.clone(),
                jwt_secret: "test".into(),
                service_secret: "service".into(),
                backend: super::Backend::new(db.clone(), oauth(), oauth(), http.clone()),
                mailer: super::Mailer::Off,
                ark_http: http.clone(),
                enshrouded: super::super::enshrouded::Control::from_env(),
            };
            let url = serve(super::router(state)).await;
            Internal { url, http, db, _pg: pg }
        }

        /// A user (with `email` confirmed when given) and their session token, named after them.
        async fn user(&self, name: &str, email: Option<&str>) -> i64 {
            let (id,): (i64,) = sqlx::query_as(
                "INSERT INTO users (username, email, email_verified_at) \
                 VALUES ($1, $2, CASE WHEN $2 IS NULL THEN NULL ELSE NOW() END) RETURNING id",
            )
            .bind(name)
            .bind(email)
            .fetch_one(&self.db)
            .await
            .unwrap();
            sqlx::query("INSERT INTO bff_tokens (token, user_id) VALUES ($1, $2)").bind(name).bind(id).execute(&self.db).await.unwrap();
            id
        }

        async fn id(&self, table: &str, name: &str) -> i32 {
            let (id,): (i32,) = sqlx::query_as(&format!("SELECT id FROM {table} WHERE name = $1 ORDER BY id LIMIT 1"))
                .bind(name)
                .fetch_one(&self.db)
                .await
                .unwrap();
            id
        }

        /// The status and the `error` code of a call.
        async fn call(&self, method: reqwest::Method, path: &str, body: Option<serde_json::Value>) -> (u16, String) {
            let mut req = self.http.request(method, format!("{}{path}", self.url)).header("x-service-token", "service");
            if let Some(body) = body {
                req = req.json(&body);
            }
            let reply = req.send().await.unwrap();
            let status = reply.status().as_u16();
            let body: serde_json::Value = reply.json().await.unwrap_or_default();
            (status, body["error"].as_str().unwrap_or_default().to_string())
        }

        async fn admins(&self) -> i64 {
            let (n,): (i64,) = sqlx::query_as(&format!("SELECT COUNT(*) FROM ({}) a", super::ADMIN_USERS))
                .fetch_one(&self.db)
                .await
                .unwrap();
            n
        }
    }

    /// Nothing leaves the site without someone holding `manage_permissions`: not taking a role
    /// from a user, not taking the permission from a role, not deleting an account, and not two
    /// such changes at once.
    #[tokio::test]
    async fn the_last_admin_keeps_their_rights() {
        use reqwest::Method;
        use serde_json::json;

        let auth = Internal::start().await;
        let admin = auth.id("roles", "admin").await;
        let manage = auth.id("permissions", "manage_permissions").await;
        let (ada, bob) = (auth.user("ada", None).await, auth.user("bob", Some("bob@example.test")).await);
        for user in [ada, bob] {
            assert_eq!(auth.call(Method::POST, &format!("/internal/admin/users/{user}/roles/{admin}"), None).await.0, 204);
        }

        // Two admins each lose the role at once: only one of them can.
        let (ada_path, bob_path) =
            (format!("/internal/admin/users/{ada}/roles/{admin}"), format!("/internal/admin/users/{bob}/roles/{admin}"));
        let (a, b) = tokio::join!(
            auth.call(Method::DELETE, &ada_path, None),
            auth.call(Method::DELETE, &bob_path, None),
        );
        let mut answers = [a, b];
        answers.sort();
        assert_eq!(answers, [(204, String::new()), (409, "last_admin".into())]);
        assert_eq!(auth.admins().await, 1);

        // Whoever is left: their role, the role's permission and their account all stay.
        let (left, gone) = {
            let (left,): (i64,) = sqlx::query_as(&format!("SELECT user_id::BIGINT FROM ({}) a", super::ADMIN_USERS))
                .fetch_one(&auth.db)
                .await
                .unwrap();
            if left == ada { (ada, bob) } else { (bob, ada) }
        };
        let last_admin = (409, "last_admin".to_string());
        assert_eq!(auth.call(Method::DELETE, &format!("/internal/admin/users/{left}/roles/{admin}"), None).await, last_admin);
        assert_eq!(auth.call(Method::DELETE, &format!("/internal/admin/roles/{admin}/permissions/{manage}"), None).await, last_admin);
        if left == ada {
            // No confirmed email: deleted directly, asked first.
            let body = || Some(json!({"token": "ada", "check": true}));
            assert_eq!(auth.call(Method::POST, "/internal/account/delete/direct", body()).await, last_admin);
            assert_eq!(auth.call(Method::POST, "/internal/account/delete/direct", Some(json!({"token": "ada"}))).await, last_admin);
        } else {
            // A confirmed email: no link is sent, and a link sent earlier doesn't delete it.
            assert_eq!(auth.call(Method::POST, "/internal/account/delete/request", Some(json!({"token": "bob"}))).await, last_admin);
            let code = "a-delete-code";
            sqlx::query(
                "INSERT INTO email_tokens (token_hash, user_id, purpose, email, expires_at) \
                 VALUES ($1, $2, 'delete_account', 'bob@example.test', NOW() + INTERVAL '1 hour')",
            )
            .bind(super::super::account_email::hash(code))
            .bind(bob)
            .execute(&auth.db)
            .await
            .unwrap();
            assert_eq!(auth.call(Method::POST, "/internal/account/delete/check", Some(json!({"code": code}))).await, last_admin);
            assert_eq!(auth.call(Method::POST, "/internal/account/delete/confirm", Some(json!({"code": code}))).await, last_admin);
        }
        assert_eq!(auth.admins().await, 1);

        // With another admin, each of those works.
        assert_eq!(auth.call(Method::POST, &format!("/internal/admin/users/{gone}/roles/{admin}"), None).await.0, 204);
        assert_eq!(auth.call(Method::DELETE, &format!("/internal/admin/users/{left}/roles/{admin}"), None).await.0, 204);
        assert_eq!(auth.call(Method::POST, &format!("/internal/admin/users/{left}/roles/{admin}"), None).await.0, 204);
        let token = if left == ada { "ada" } else { "bob" };
        if left == ada {
            assert_eq!(auth.call(Method::POST, "/internal/account/delete/direct", Some(json!({"token": token, "check": true}))).await.0, 200);
            assert_eq!(auth.call(Method::POST, "/internal/account/delete/direct", Some(json!({"token": token}))).await.0, 200);
        } else {
            assert_eq!(auth.call(Method::POST, "/internal/account/delete/confirm", Some(json!({"code": "a-delete-code"}))).await.0, 200);
        }
        assert_eq!(auth.admins().await, 1);

        // A site with no admin at all (a fresh one) still lets accounts go.
        let user = auth.id("roles", "user").await;
        let cy = auth.user("cy", None).await;
        assert_eq!(auth.call(Method::POST, &format!("/internal/admin/users/{cy}/roles/{user}"), None).await.0, 204);
        sqlx::query("DELETE FROM user_roles WHERE role_id = $1").bind(admin).execute(&auth.db).await.unwrap();
        assert_eq!(auth.call(Method::DELETE, &format!("/internal/admin/users/{cy}/roles/{user}"), None).await.0, 204);
        assert_eq!(auth.call(Method::POST, "/internal/account/delete/direct", Some(json!({"token": "cy"}))).await.0, 200);
    }

    /// An invite link never gives admin rights: not when made, and not when opened after its
    /// role has gained them.
    #[tokio::test]
    async fn invites_for_a_role_that_became_admin_are_refused() {
        use reqwest::Method;
        use serde_json::json;

        let auth = Internal::start().await;
        let (admin, user) = (auth.id("roles", "admin").await, auth.id("roles", "user").await);
        let manage = auth.id("permissions", "manage_permissions").await;
        auth.user("ada", None).await;
        let dee = auth.user("dee", None).await;
        let made = |role| auth.call(Method::POST, "/internal/admin/invites", Some(json!({"token": "ada", "role_id": role, "days": 7})));
        assert_eq!(made(admin).await, (400, "admin_role".into()));

        // A link for `user`, made while that gives no admin rights; then it does.
        let reply = auth
            .http
            .post(format!("{}/internal/admin/invites", auth.url))
            .header("x-service-token", "service")
            .json(&json!({"token": "ada", "role_id": user, "days": 7}))
            .send()
            .await
            .unwrap();
        assert_eq!(reply.status().as_u16(), 200);
        let link: serde_json::Value = reply.json().await.unwrap();
        let code = link["link"].as_str().unwrap().rsplit_once('#').unwrap().1.to_string();
        assert_eq!(auth.call(Method::POST, "/internal/invite/check", Some(json!({"code": code}))).await, (200, String::new()));
        assert_eq!(auth.call(Method::POST, &format!("/internal/admin/roles/{user}/permissions/{manage}"), None).await.0, 204);

        let admin_role = (403, "admin_role".to_string());
        assert_eq!(auth.call(Method::POST, "/internal/invite/check", Some(json!({"code": code}))).await, admin_role);
        assert_eq!(auth.call(Method::POST, "/internal/invite/redeem", Some(json!({"code": code, "token": "dee"}))).await, admin_role);
        let (has,): (bool,) = sqlx::query_as("SELECT EXISTS (SELECT 1 FROM user_roles WHERE user_id = $1)")
            .bind(dee)
            .fetch_one(&auth.db)
            .await
            .unwrap();
        assert!(!has, "the redeem gave no role");

        // Without admin rights again, the link works.
        assert_eq!(auth.call(Method::DELETE, &format!("/internal/admin/roles/{user}/permissions/{manage}"), None).await.0, 204);
        assert_eq!(auth.call(Method::POST, "/internal/invite/redeem", Some(json!({"code": code, "token": "dee"}))).await, (200, String::new()));
    }
}
