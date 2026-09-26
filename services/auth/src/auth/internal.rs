use axum::{
    Json, Router,
    body::Body,
    extract::{Path, Query, State},
    http::{Request, StatusCode},
    middleware::{self, Next},
    response::IntoResponse,
    routing::{delete, get, post},
};
use jsonwebtoken::{EncodingKey, Header, encode};
use password_auth::verify_password;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tokio::task;
use ulid::Ulid;

use super::account_email;
use super::mail::Mailer;
use super::telemetry;
use super::user::{Backend, BackendError, BffToken, OAuthProvider};

#[derive(Clone)]
pub struct InternalState {
    pub db: PgPool,
    pub jwt_secret: String,
    pub service_secret: String,
    pub backend: Backend,
    pub mailer: Mailer,
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

#[tracing::instrument(name = "token.exchange.password", skip_all)]
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
        telemetry::token_operation("introspect", "invalid");
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
    let password = req.password.clone();
    let hashed = task::spawn_blocking(move || password_auth::generate_hash(password))
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

// Extracts the W3C traceparent header value from the current OTel span context
// so it can be injected into outbound ark requests without pulling in a
// separate HTTP middleware crate (which would conflict with oauth2's reqwest).
fn traceparent() -> Option<String> {
    use opentelemetry::propagation::TextMapPropagator as _;
    let propagator = opentelemetry_sdk::propagation::TraceContextPropagator::new();
    let cx = opentelemetry::Context::current();
    let mut carrier = std::collections::HashMap::<String, String>::new();
    propagator.inject_context(&cx, &mut carrier);
    carrier.remove("traceparent")
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
async fn resolve_profile(db: &PgPool, token: &str) -> Result<Option<ProfileResp>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {PROFILE_COLUMNS} FROM bff_tokens t JOIN users u ON u.id = t.user_id \
         WHERE t.token = $1 AND t.expires_at > NOW()"
    ))
    .bind(token)
    .fetch_optional(db)
    .await
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
async fn resolve_arcane_user(db: &PgPool, token: &str) -> Option<i64> {
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

    let mut builder = reqwest::Client::new().get("http://192.168.1.21:9090/ark/num_players");
    if let Some(tp) = traceparent() {
        builder = builder.header("traceparent", tp);
    }
    match builder.send().await {
        Ok(resp) => match resp.json::<DockerRequestResponse>().await {
            Ok(body) => {
                telemetry::ark_command("num_players", "success");
                Json(body).into_response()
            }
            Err(e) => {
                telemetry::ark_command("num_players", "error");
                (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
            }
        },
        Err(_) => {
            telemetry::ark_command("num_players", "unreachable");
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

    let mut builder = reqwest::Client::new().get(format!("http://192.168.1.21:9090/ark/{cmd}"));
    if let Some(tp) = traceparent() {
        builder = builder.header("traceparent", tp);
    }
    match builder.send().await {
        Ok(resp) => match resp.json::<DockerRequestResponse>().await {
            Ok(body) => {
                telemetry::ark_command(&cmd, "success");
                Json(body).into_response()
            }
            Err(e) => {
                telemetry::ark_command(&cmd, "error");
                (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
            }
        },
        Err(_) => {
            telemetry::ark_command(&cmd, "unreachable");
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

#[tracing::instrument(name = "admin.revoke_user_role", skip_all, fields(user_id, role_id))]
async fn admin_revoke_user_role(
    State(state): State<InternalState>,
    Path((user_id, role_id)): Path<(i64, i32)>,
) -> impl IntoResponse {
    match sqlx::query("DELETE FROM user_roles WHERE user_id = $1 AND role_id = $2")
        .bind(user_id)
        .bind(role_id)
        .execute(&state.db)
        .await
    {
        Ok(_) => {
            tracing::info!(user_id, role_id, "revoked role from user");
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => {
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
    match sqlx::query(
        "DELETE FROM role_permissions WHERE role_id = $1 AND permission_id = $2",
    )
    .bind(role_id)
    .bind(permission_id)
    .execute(&state.db)
    .await
    {
        Ok(_) => {
            tracing::info!(role_id, permission_id, "revoked permission from role");
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, role_id, permission_id, "admin_revoke_role_permission: db error");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::clean_display_name;

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
}
