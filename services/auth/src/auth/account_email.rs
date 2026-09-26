//! Account emails: confirming the address, resetting a forgotten password, and
//! confirming that an account should be deleted.
//!
//! Each email carries a link to the website with a one-time code; the website hands
//! the code back to one of these endpoints. Only the code's SHA-256 is stored
//! (`email_tokens`). A new link replaces any unused one of the same kind, and each
//! kind is limited to one link a minute and `DAILY_LIMIT` a day per account, so the
//! forms can't be used to flood someone's inbox.
//!
//! Endpoints (service token required, like the rest of `/internal`):
//! - `POST /internal/email/verify/send` `{token}`: email the session's user a
//!   confirmation link. 409 `no_email` / `already_verified`, 429 `too_soon`.
//! - `POST /internal/email/verify/confirm` `{code}` → `{username}`. 404 `invalid_code`.
//! - `POST /internal/password/forgot` `{login}` (username or email): always 202, so
//!   it can't be used to learn who has an account; the link is only sent to a password
//!   account's own address.
//! - `POST /internal/password/reset` `{code, password}` → `{username}`; logs the
//!   account out everywhere. 400 `weak_password`, 404 `invalid_code`.
//! - `POST /internal/account/delete/request` `{token}`: email a deletion link.
//!   409 `no_email`, 429 `too_soon`.
//! - `POST /internal/account/delete/check` `{code}` → `{user_id, username}` without
//!   using the code (the website removes the user's other data first).
//! - `POST /internal/account/delete/confirm` `{code}` → `{user_id, username}`: deletes.
//! - `POST /internal/account/delete/direct` `{token}` → `{user_id, username}`: deletes
//!   an account without a confirmed email (GitHub logins, or an address that may be
//!   wrong), which a link might never reach.

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use base64::Engine as _;
use chrono::{DateTime, Utc};
use rand::RngCore as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tokio::task;

use super::internal::InternalState;
use super::mail::{Letter, Mailer, site_url};

/// Most links of one kind per account per day, and most emails to one address per day.
const DAILY_LIMIT: i64 = 10;
/// A new link of the same kind isn't sent sooner than this after the last one.
const COOLDOWN_SECS: i64 = 60;
/// Password length limits, in characters.
pub const PASSWORD_MIN: usize = 8;
const PASSWORD_MAX: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    VerifyEmail,
    ResetPassword,
    DeleteAccount,
}

impl Purpose {
    fn as_str(self) -> &'static str {
        match self {
            Purpose::VerifyEmail => "verify_email",
            Purpose::ResetPassword => "reset_password",
            Purpose::DeleteAccount => "delete_account",
        }
    }

    /// How long the link works.
    fn lifetime_secs(self) -> i64 {
        match self {
            Purpose::VerifyEmail => 48 * 3600,
            Purpose::ResetPassword | Purpose::DeleteAccount => 3600,
        }
    }

    /// The website page the link opens.
    fn page(self) -> &'static str {
        match self {
            Purpose::VerifyEmail => "/verify-email",
            Purpose::ResetPassword => "/reset-password",
            Purpose::DeleteAccount => "/delete-account",
        }
    }
}

pub fn routes() -> Router<InternalState> {
    Router::new()
        .route("/internal/email/verify/send", post(verify_send))
        .route("/internal/email/verify/confirm", post(verify_confirm))
        .route("/internal/password/forgot", post(password_forgot))
        .route("/internal/password/reset", post(password_reset))
        .route("/internal/account/delete/request", post(delete_request))
        .route("/internal/account/delete/check", post(delete_check))
        .route("/internal/account/delete/confirm", post(delete_confirm))
        .route("/internal/account/delete/direct", post(delete_direct))
}

// ---- Codes ----

/// A fresh link code: 32 random bytes, URL-safe base64 (43 characters).
fn new_code() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// What's stored for a code.
fn hash(code: &str) -> String {
    Sha256::digest(code.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// The link for `code`.
fn link(purpose: Purpose, code: &str) -> String {
    format!("{}{}?code={code}", site_url(), purpose.page())
}

#[derive(Debug)]
enum IssueError {
    TooSoon,
    Db(sqlx::Error),
}

impl From<sqlx::Error> for IssueError {
    fn from(e: sqlx::Error) -> Self {
        IssueError::Db(e)
    }
}

/// Creates a link code for the user, replacing any unused one of the same purpose.
async fn issue(db: &PgPool, user_id: i64, purpose: Purpose, email: &str) -> Result<String, IssueError> {
    let mut tx = db.begin().await?;
    // One issuer per account at a time, so the limits below can't be raced.
    sqlx::query("SELECT id FROM users WHERE id = $1 FOR UPDATE")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    let (last, count): (Option<DateTime<Utc>>, i64) = sqlx::query_as(
        "SELECT MAX(created_at), COUNT(*) FROM email_tokens \
         WHERE user_id = $1 AND purpose = $2 AND created_at > NOW() - INTERVAL '1 day'",
    )
    .bind(user_id)
    .bind(purpose.as_str())
    .fetch_one(&mut *tx)
    .await?;
    let too_soon = last.is_some_and(|t| Utc::now() - t < chrono::Duration::seconds(COOLDOWN_SECS));
    // Also per address, over all accounts: signing up again and again with the same
    // address (or its capitalisations) mustn't flood that inbox.
    let (to_address,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM email_tokens WHERE LOWER(email) = LOWER($1) AND created_at > NOW() - INTERVAL '1 day'",
    )
    .bind(email)
    .fetch_one(&mut *tx)
    .await?;
    if too_soon || count >= DAILY_LIMIT || to_address >= DAILY_LIMIT {
        return Err(IssueError::TooSoon);
    }
    sqlx::query(
        "UPDATE email_tokens SET used_at = NOW() WHERE user_id = $1 AND purpose = $2 AND used_at IS NULL",
    )
    .bind(user_id)
    .bind(purpose.as_str())
    .execute(&mut *tx)
    .await?;
    let code = new_code();
    sqlx::query(
        "INSERT INTO email_tokens (token_hash, user_id, purpose, email, expires_at) \
         VALUES ($1, $2, $3, $4, NOW() + make_interval(secs => $5))",
    )
    .bind(hash(&code))
    .bind(user_id)
    .bind(purpose.as_str())
    .bind(email)
    .bind(purpose.lifetime_secs() as f64)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(code)
}

/// Forgets a code whose email couldn't be sent, so asking again works right away.
async fn withdraw(db: &PgPool, code: &str) {
    if let Err(e) = sqlx::query("DELETE FROM email_tokens WHERE token_hash = $1")
        .bind(hash(code))
        .execute(db)
        .await
    {
        tracing::warn!(error = %e, "withdrawing an unsent email code failed");
    }
}

/// The user and address a live code was issued for. With `consume`, the code is
/// used up in the same statement, so it works only once.
async fn redeem<'e>(
    db: impl sqlx::PgExecutor<'e>,
    code: &str,
    purpose: Purpose,
    consume: bool,
) -> Result<Option<(i64, String)>, sqlx::Error> {
    let sql = if consume {
        "UPDATE email_tokens SET used_at = NOW() \
         WHERE token_hash = $1 AND purpose = $2 AND used_at IS NULL AND expires_at > NOW() \
         RETURNING user_id, email"
    } else {
        "SELECT user_id, email FROM email_tokens \
         WHERE token_hash = $1 AND purpose = $2 AND used_at IS NULL AND expires_at > NOW()"
    };
    sqlx::query_as(sql)
        .bind(hash(code))
        .bind(purpose.as_str())
        .fetch_optional(db)
        .await
}

/// Deletes codes that expired over a day ago (kept that long for the daily limit).
pub async fn clean_expired(db: PgPool) {
    loop {
        match sqlx::query("DELETE FROM email_tokens WHERE expires_at < NOW() - INTERVAL '1 day'")
            .execute(&db)
            .await
        {
            Ok(r) if r.rows_affected() > 0 => tracing::debug!(rows = r.rows_affected(), "expired email codes removed"),
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "removing expired email codes failed"),
        }
        tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
    }
}

// ---- Accounts ----

#[derive(sqlx::FromRow)]
struct Account {
    id: i64,
    username: String,
    display_name: Option<String>,
    email: Option<String>,
    email_verified_at: Option<DateTime<Utc>>,
}

impl Account {
    fn name(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.username)
    }
}

const ACCOUNT_COLUMNS: &str = "u.id, u.username, u.display_name, u.email, u.email_verified_at";

/// The account a session token belongs to.
async fn token_account(db: &PgPool, token: &str) -> Result<Option<Account>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {ACCOUNT_COLUMNS} FROM bff_tokens t JOIN users u ON u.id = t.user_id \
         WHERE t.token = $1 AND t.expires_at > NOW()"
    ))
    .bind(token)
    .fetch_optional(db)
    .await
}

async fn account_by_id(db: &PgPool, id: i64) -> Result<Option<Account>, sqlx::Error> {
    sqlx::query_as(&format!("SELECT {ACCOUNT_COLUMNS} FROM users u WHERE u.id = $1"))
        .bind(id)
        .fetch_optional(db)
        .await
}

/// The address as it will be stored: trimmed. `None` when it isn't a plausible
/// address (one `@`, a dot in the domain, no spaces, at most 254 characters).
pub fn clean_email(raw: &str) -> Option<String> {
    let email = raw.trim();
    let (local, domain) = email.split_once('@')?;
    let plausible = email.len() <= 254
        && !local.is_empty()
        && !domain.contains('@')
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !email.chars().any(|c| c.is_whitespace() || c.is_control());
    plausible.then(|| email.to_string())
}

/// The error body the website reads: `{"error": code}`.
fn error(status: StatusCode, code: &str) -> Response {
    (status, Json(serde_json::json!({ "error": code }))).into_response()
}

fn db_error(what: &str, e: sqlx::Error) -> Response {
    tracing::error!(error = %e, "{what} failed");
    error(StatusCode::INTERNAL_SERVER_ERROR, "internal")
}

// ---- The emails ----

fn verify_letter(account: &Account, email: &str, code: &str) -> super::mail::Mail {
    let link = link(Purpose::VerifyEmail, code);
    Letter {
        to: email,
        subject: "Confirm your email for milesstorm.com",
        name: account.name(),
        paragraphs: &["Please confirm that this is your email address, so we can help you if you ever forget your password."],
        button: "Confirm my email",
        link: &link,
        footer: "The link works for 48 hours. If you didn't make an account on milesstorm.com, you can ignore this email.",
    }
    .build()
}

fn reset_letter(account: &Account, email: &str, code: &str) -> super::mail::Mail {
    let link = link(Purpose::ResetPassword, code);
    let paragraph = format!(
        "Someone asked to reset the password for your account, {}. If it was you, choose a new password with the button below.",
        account.username
    );
    Letter {
        to: email,
        subject: "Reset your milesstorm.com password",
        name: account.name(),
        paragraphs: &[&paragraph],
        button: "Choose a new password",
        link: &link,
        footer: "The link works for 1 hour. If you didn't ask for this, you can ignore this email: your password stays the same.",
    }
    .build()
}

fn delete_letter(account: &Account, email: &str, code: &str) -> super::mail::Mail {
    let link = link(Purpose::DeleteAccount, code);
    let paragraph = format!(
        "You asked to delete your account, {}. Deleting removes your profile, your picture and any dice pictures you shared. It can't be undone.",
        account.username
    );
    Letter {
        to: email,
        subject: "Confirm deleting your milesstorm.com account",
        name: account.name(),
        paragraphs: &[&paragraph, "To go ahead, open the link below and confirm once more."],
        button: "Delete my account",
        link: &link,
        footer: "The link works for 1 hour. If you didn't ask for this, someone may be logged in as you: change your password and nothing will be deleted.",
    }
    .build()
}

/// Issues a code and emails it. Errors are ready-made responses.
async fn issue_and_send(
    db: &PgPool,
    mailer: &Mailer,
    account: &Account,
    email: &str,
    purpose: Purpose,
) -> Result<(), Response> {
    let code = match issue(db, account.id, purpose, email).await {
        Ok(c) => c,
        Err(IssueError::TooSoon) => return Err(error(StatusCode::TOO_MANY_REQUESTS, "too_soon")),
        Err(IssueError::Db(e)) => return Err(db_error("issuing an email code", e)),
    };
    let mail = match purpose {
        Purpose::VerifyEmail => verify_letter(account, email, &code),
        Purpose::ResetPassword => reset_letter(account, email, &code),
        Purpose::DeleteAccount => delete_letter(account, email, &code),
    };
    match mailer.send(purpose.as_str(), &mail).await {
        Ok(()) => {
            tracing::info!(user_id = account.id, kind = purpose.as_str(), "account email sent");
            Ok(())
        }
        Err(e) => {
            tracing::error!(user_id = account.id, kind = purpose.as_str(), error = %e, "sending an account email failed");
            withdraw(db, &code).await;
            Err(error(StatusCode::BAD_GATEWAY, "send_failed"))
        }
    }
}

/// After registering: email a confirmation link without holding up the reply.
pub fn send_verification_in_background(state: &InternalState, user_id: i64) {
    let (db, mailer) = (state.db.clone(), state.mailer.clone());
    task::spawn(async move {
        let account = match account_by_id(&db, user_id).await {
            Ok(Some(a)) => a,
            Ok(None) => return,
            Err(e) => {
                tracing::error!(user_id, error = %e, "loading a new account for its confirmation email failed");
                return;
            }
        };
        if let Some(email) = account.email.clone() {
            let _ = issue_and_send(&db, &mailer, &account, &email, Purpose::VerifyEmail).await;
        }
    });
}

// ---- Handlers ----

#[derive(Deserialize)]
struct TokenReq {
    token: String,
}

#[derive(Deserialize)]
struct CodeReq {
    code: String,
}

#[derive(Serialize)]
struct SentResp {
    /// Where the email went.
    email: String,
}

#[derive(Serialize)]
struct UsernameResp {
    username: String,
}

#[derive(Serialize)]
struct DeletedResp {
    user_id: i64,
    username: String,
}

#[tracing::instrument(name = "email.verify_send", skip_all)]
async fn verify_send(State(state): State<InternalState>, Json(req): Json<TokenReq>) -> Response {
    let account = match token_account(&state.db, &req.token).await {
        Ok(Some(a)) => a,
        Ok(None) => return error(StatusCode::UNAUTHORIZED, "logged_out"),
        Err(e) => return db_error("loading the account", e),
    };
    let Some(email) = account.email.clone() else {
        return error(StatusCode::CONFLICT, "no_email");
    };
    if account.email_verified_at.is_some() {
        return error(StatusCode::CONFLICT, "already_verified");
    }
    match issue_and_send(&state.db, &state.mailer, &account, &email, Purpose::VerifyEmail).await {
        Ok(()) => Json(SentResp { email }).into_response(),
        Err(r) => r,
    }
}

#[tracing::instrument(name = "email.verify_confirm", skip_all)]
async fn verify_confirm(State(state): State<InternalState>, Json(req): Json<CodeReq>) -> Response {
    let result: Result<Option<String>, sqlx::Error> = async {
        let mut tx = state.db.begin().await?;
        let Some((user_id, email)) = redeem(&mut *tx, &req.code, Purpose::VerifyEmail, true).await? else {
            return Ok(None);
        };
        // Only while the address is still the one the link was sent to.
        let username: Option<(String,)> = sqlx::query_as(
            "UPDATE users SET email_verified_at = COALESCE(email_verified_at, NOW()) \
             WHERE id = $1 AND email = $2 RETURNING username",
        )
        .bind(user_id)
        .bind(&email)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        if username.is_some() {
            tracing::info!(user_id, "email confirmed");
        }
        Ok(username.map(|(u,)| u))
    }
    .await;
    match result {
        Ok(Some(username)) => Json(UsernameResp { username }).into_response(),
        Ok(None) => error(StatusCode::NOT_FOUND, "invalid_code"),
        Err(e) => db_error("confirming an email", e),
    }
}

#[derive(Deserialize)]
struct ForgotReq {
    /// Username or email address.
    login: String,
}

#[tracing::instrument(name = "password.forgot", skip_all)]
async fn password_forgot(State(state): State<InternalState>, Json(req): Json<ForgotReq>) -> Response {
    let login = req.login.trim().to_string();
    if login.is_empty() || login.len() > 254 {
        return error(StatusCode::BAD_REQUEST, "invalid_login");
    }
    // Everything else happens after replying, so the reply looks and takes the same
    // whether or not the account exists.
    let (db, mailer) = (state.db.clone(), state.mailer.clone());
    task::spawn(async move {
        let by = if login.contains('@') { "LOWER(u.email) = LOWER($1)" } else { "u.username = $1" };
        let account: Result<Option<Account>, _> = sqlx::query_as(&format!(
            "SELECT {ACCOUNT_COLUMNS} FROM users u \
             WHERE {by} AND u.password IS NOT NULL AND u.email IS NOT NULL \
             ORDER BY u.email_verified_at IS NULL, u.id LIMIT 1"
        ))
        .bind(&login)
        .fetch_optional(&db)
        .await;
        match account {
            Ok(Some(account)) => {
                let email = account.email.clone().unwrap_or_default();
                if let Err(r) = issue_and_send(&db, &mailer, &account, &email, Purpose::ResetPassword).await {
                    if r.status() == StatusCode::TOO_MANY_REQUESTS {
                        tracing::info!(user_id = account.id, "password reset not sent: asked again too soon");
                    }
                }
            }
            Ok(None) => tracing::info!("password reset asked for an unknown or password-less account"),
            Err(e) => tracing::error!(error = %e, "looking up an account for a password reset failed"),
        }
    });
    StatusCode::ACCEPTED.into_response()
}

/// Whether `password` is acceptable as a new password.
pub fn password_ok(password: &str) -> bool {
    (PASSWORD_MIN..=PASSWORD_MAX).contains(&password.chars().count())
}

#[derive(Deserialize)]
struct ResetReq {
    code: String,
    password: String,
}

#[tracing::instrument(name = "password.reset", skip_all)]
async fn password_reset(State(state): State<InternalState>, Json(req): Json<ResetReq>) -> Response {
    if !password_ok(&req.password) {
        return error(StatusCode::BAD_REQUEST, "weak_password");
    }
    // Check the code before the slow hashing; it is used up below.
    match redeem(&state.db, &req.code, Purpose::ResetPassword, false).await {
        Ok(Some(_)) => {}
        Ok(None) => return error(StatusCode::NOT_FOUND, "invalid_code"),
        Err(e) => return db_error("checking a reset code", e),
    }
    let password = req.password;
    let hashed = match task::spawn_blocking(move || password_auth::generate_hash(password)).await {
        Ok(h) => h,
        Err(e) => {
            tracing::error!(error = %e, "password hashing failed");
            return error(StatusCode::INTERNAL_SERVER_ERROR, "internal");
        }
    };
    let result: Result<Option<(i64, String)>, sqlx::Error> = async {
        let mut tx = state.db.begin().await?;
        let Some((user_id, email)) = redeem(&mut *tx, &req.code, Purpose::ResetPassword, true).await? else {
            return Ok(None);
        };
        // Resetting through the emailed link also proves the address is theirs.
        let username: Option<(String,)> = sqlx::query_as(
            "UPDATE users SET password = $2, \
               email_verified_at = CASE WHEN email = $3 THEN COALESCE(email_verified_at, NOW()) ELSE email_verified_at END \
             WHERE id = $1 AND password IS NOT NULL RETURNING username",
        )
        .bind(user_id)
        .bind(&hashed)
        .bind(&email)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((username,)) = username else { return Ok(None) };
        // Log out everywhere: whoever knew the old password shouldn't stay in. Their
        // outstanding links (e.g. to delete the account) stop working too.
        sqlx::query("DELETE FROM bff_tokens WHERE user_id = $1")
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE email_tokens SET used_at = NOW() WHERE user_id = $1 AND used_at IS NULL")
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(Some((user_id, username)))
    }
    .await;
    match result {
        Ok(Some((user_id, username))) => {
            tracing::info!(user_id, "password reset by email");
            Json(UsernameResp { username }).into_response()
        }
        Ok(None) => error(StatusCode::NOT_FOUND, "invalid_code"),
        Err(e) => db_error("resetting a password", e),
    }
}

#[tracing::instrument(name = "account.delete_request", skip_all)]
async fn delete_request(State(state): State<InternalState>, Json(req): Json<TokenReq>) -> Response {
    let account = match token_account(&state.db, &req.token).await {
        Ok(Some(a)) => a,
        Ok(None) => return error(StatusCode::UNAUTHORIZED, "logged_out"),
        Err(e) => return db_error("loading the account", e),
    };
    let Some(email) = account.email.clone() else {
        return error(StatusCode::CONFLICT, "no_email");
    };
    match issue_and_send(&state.db, &state.mailer, &account, &email, Purpose::DeleteAccount).await {
        Ok(()) => Json(SentResp { email }).into_response(),
        Err(r) => r,
    }
}

#[tracing::instrument(name = "account.delete_check", skip_all)]
async fn delete_check(State(state): State<InternalState>, Json(req): Json<CodeReq>) -> Response {
    let user_id = match redeem(&state.db, &req.code, Purpose::DeleteAccount, false).await {
        Ok(Some((id, _))) => id,
        Ok(None) => return error(StatusCode::NOT_FOUND, "invalid_code"),
        Err(e) => return db_error("checking a delete code", e),
    };
    match account_by_id(&state.db, user_id).await {
        Ok(Some(a)) => Json(DeletedResp { user_id: a.id, username: a.username }).into_response(),
        Ok(None) => error(StatusCode::NOT_FOUND, "invalid_code"),
        Err(e) => db_error("loading the account", e),
    }
}

#[tracing::instrument(name = "account.delete_confirm", skip_all)]
async fn delete_confirm(State(state): State<InternalState>, Json(req): Json<CodeReq>) -> Response {
    let result: Result<Option<(i64, String)>, sqlx::Error> = async {
        let mut tx = state.db.begin().await?;
        let Some((user_id, email)) = redeem(&mut *tx, &req.code, Purpose::DeleteAccount, true).await? else {
            return Ok(None);
        };
        // Only while the address is still the one the link was sent to.
        let deleted = sqlx::query_as("DELETE FROM users WHERE id = $1 AND email = $2 RETURNING id, username")
            .bind(user_id)
            .bind(&email)
            .fetch_optional(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(deleted)
    }
    .await;
    deleted_reply(result)
}

#[tracing::instrument(name = "account.delete_direct", skip_all)]
async fn delete_direct(State(state): State<InternalState>, Json(req): Json<TokenReq>) -> Response {
    let account = match token_account(&state.db, &req.token).await {
        Ok(Some(a)) => a,
        Ok(None) => return error(StatusCode::UNAUTHORIZED, "logged_out"),
        Err(e) => return db_error("loading the account", e),
    };
    if account.email.is_some() && account.email_verified_at.is_some() {
        // Accounts with a confirmed email confirm by email.
        return error(StatusCode::CONFLICT, "has_email");
    }
    let result = sqlx::query_as(
        "DELETE FROM users WHERE id = $1 AND (email IS NULL OR email_verified_at IS NULL) RETURNING id, username",
    )
        .bind(account.id)
        .fetch_optional(&state.db)
        .await;
    deleted_reply(result)
}

fn deleted_reply(result: Result<Option<(i64, String)>, sqlx::Error>) -> Response {
    match result {
        Ok(Some((user_id, username))) => {
            tracing::info!(user_id, "account deleted");
            super::telemetry::token_operation("delete_account", "success");
            Json(DeletedResp { user_id, username }).into_response()
        }
        Ok(None) => error(StatusCode::NOT_FOUND, "invalid_code"),
        Err(e) => db_error("deleting an account", e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_long_random_and_url_safe() {
        let (a, b) = (new_code(), new_code());
        assert_ne!(a, b);
        assert_eq!(a.len(), 43);
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        assert_eq!(hash(&a).len(), 64);
        assert_eq!(hash(&a), hash(&a));
        assert_ne!(hash(&a), hash(&b));
    }

    #[test]
    fn emails_are_checked_loosely() {
        assert_eq!(clean_email("  ada@example.com "), Some("ada@example.com".into()));
        assert_eq!(clean_email("a.b+c@sub.example.co.uk"), Some("a.b+c@sub.example.co.uk".into()));
        for bad in ["", "ada", "@example.com", "ada@", "ada@example", "a@b@c.com", "ada @example.com", "ada@.com", "ada@example.com."] {
            assert_eq!(clean_email(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn password_length() {
        assert!(!password_ok("1234567"));
        assert!(password_ok("12345678"));
        assert!(password_ok(&"é".repeat(256)));
        assert!(!password_ok(&"a".repeat(257)));
    }
}
