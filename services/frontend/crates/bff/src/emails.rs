//! Account emails, website side (the JSON API in json_api.rs): confirming the address,
//! resetting a forgotten password, and deleting the account. Auth stores the codes
//! and sends the emails (`services/auth/src/auth/account_email.rs`); the emailed
//! links open the site's pages, which call these.
//!
//! Deleting an account first removes what the website keeps about it elsewhere
//! (SurrealDB: shared roll pictures and the profile picture; Redis: the held and
//! last roll), and only then the account itself. If that cleanup fails, nothing is
//! deleted and the user can try again, so nothing is left behind under a username
//! someone else could later register.

pub use api_types::{Deletion, Done};
use api_types::{ApiError, ErrorCode};
use serde_json::{json, Value};
use tower_sessions::Session;

use crate::rolls::RollHub;

pub(crate) const OFFLINE: &str = "Something went wrong on our side. Try again in a minute.";

/// Whether `code` looks like a link code (auth makes them URL-safe base64, 43 characters),
/// before asking auth. An invite's is later put into a page address, so nothing else may
/// get through.
pub(crate) fn code_ok(code: &str) -> bool {
    (1..=64).contains(&code.len()) && code.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// An emailed link's code that auth doesn't know (or that can't be one): auth's 404.
fn expired() -> ApiError {
    ApiError::new(ErrorCode::Gone, "This link has expired or was already used. Ask for a new one.").with_status(404)
}

/// `expired` unless `code` can be a link code.
fn checked(code: &str) -> Result<&str, ApiError> {
    if code_ok(code) {
        Ok(code)
    } else {
        Err(expired())
    }
}

pub(crate) fn offline() -> ApiError {
    ApiError::new(ErrorCode::Upstream, OFFLINE)
}

/// Calls auth; `Ok` with the reply on 2xx, otherwise the page's message for the error code
/// auth gave, with auth's status when it is a 4xx (a refusal is an answer, also one this
/// doesn't know); anything else is a gateway error.
async fn call(path: &str, body: Value) -> Result<Value, ApiError> {
    let (status, reply) = crate::auth::auth_json(path, &body).await.map_err(|e| {
        tracing::error!(error = %e, path, "reaching auth failed");
        offline()
    })?;
    if (200..300).contains(&status) {
        return Ok(reply);
    }
    let code = reply.get("error").and_then(Value::as_str).unwrap_or("");
    let refused = (400..500).contains(&status);
    let (error, message) = match code {
        "invalid_code" => (ErrorCode::Gone, "This link has expired or was already used. Ask for a new one."),
        "invalid_login" => (ErrorCode::Invalid, "Enter your username or email."),
        "has_email" => (ErrorCode::Conflict, "Your email is confirmed: use the emailed link."),
        "too_soon" => (
            ErrorCode::RateLimited,
            "An email was just sent. Check your inbox (and spam folder), or wait a minute before asking again.",
        ),
        "send_failed" => (ErrorCode::Upstream, "The email couldn't be sent right now. Try again later."),
        "already_verified" => (ErrorCode::Conflict, "Your email is already confirmed."),
        "no_email" => (ErrorCode::Invalid, "Your account has no email address."),
        "weak_password" => (ErrorCode::Invalid, "Use at least 8 characters."),
        "logged_out" => return Err(ApiError::logged_out()),
        _ if refused => {
            tracing::warn!(path, status, code, "auth refused an email request");
            let error = match status {
                403 => ErrorCode::Forbidden,
                404 => ErrorCode::NotFound,
                409 => ErrorCode::Conflict,
                429 => ErrorCode::RateLimited,
                _ => ErrorCode::Invalid,
            };
            (error, OFFLINE)
        }
        _ => {
            tracing::error!(path, status, code, "auth failed an email request");
            return Err(offline());
        }
    };
    Err(ApiError::new(error, message).with_status(if refused { status } else { 502 }))
}

fn text(reply: &Value, key: &str) -> String {
    reply.get(key).and_then(Value::as_str).unwrap_or_default().to_string()
}

/// Logs this browser out if its session belongs to `username`.
async fn log_out_if(session: Option<&Session>, username: &str) -> bool {
    let Some(session) = session else { return false };
    let current: Option<String> = session.get("username").await.ok().flatten();
    if current.as_deref() != Some(username) {
        return false;
    }
    let _ = session.flush().await;
    true
}

/// Removes the account's data outside auth. Must succeed before the account goes.
async fn forget(user_id: i64, username: &str, hub: Option<&RollHub>) -> Result<(), ApiError> {
    let failed = |what: &str, e: &dyn std::fmt::Display| {
        tracing::error!(error = %e, "deleting an account: removing {what} failed");
        ApiError::new(
            ErrorCode::Upstream,
            "Your data couldn't be removed right now, so nothing was deleted. Try again in a few minutes.",
        )
    };
    // Not configured only in local development: there is nothing to remove.
    if let Some(ds) = crate::dataset::configured() {
        let rolls = ds.delete_user_data(username).await.map_err(|e| failed("shared rolls", &e))?;
        ds.delete_profile_picture(user_id).await.map_err(|e| failed("the profile picture", &e))?;
        tracing::info!(user_id, rolls, "deleting an account: pictures removed");
    }
    if let Some(hub) = hub {
        hub.forget_user(username).await.map_err(|e| failed("held rolls", &e))?;
    }
    Ok(())
}

/// Emails the token's user a link to confirm their address; returns the address.
#[tracing::instrument(name = "bff.send_confirmation_email", skip_all)]
pub async fn send_confirmation(token: &str) -> Result<String, ApiError> {
    crate::trace::rejectable(async {
        let reply = call("/internal/email/verify/send", json!({ "token": token })).await?;
        Ok(text(&reply, "email"))
    })
    .await
}

/// Confirms an address with the code from the emailed link; returns the username.
#[tracing::instrument(name = "bff.confirm_email", skip_all)]
pub async fn confirm(code: &str) -> Result<String, ApiError> {
    crate::trace::rejectable(async {
        let reply = call("/internal/email/verify/confirm", json!({ "code": checked(code)? })).await?;
        Ok(text(&reply, "username"))
    })
    .await
}

/// Emails a password reset link, if `login` (username or email) is a password
/// account with an address. Succeeds either way, to not tell who has an account.
#[tracing::instrument(name = "bff.forgot_password", skip_all)]
pub async fn forgot(login: &str) -> Result<(), ApiError> {
    crate::trace::rejectable(async {
        // Auth's own limit: the longest email address.
        if login.trim().is_empty() || login.trim().len() > 254 {
            return Err(ApiError::new(ErrorCode::Invalid, "Enter your username or email."));
        }
        call("/internal/password/forgot", json!({ "login": login.trim() })).await?;
        Ok(())
    })
    .await
}

/// Sets a new password with the code from the emailed link. The account is logged
/// out everywhere, including here if this browser was logged in as it.
#[tracing::instrument(name = "bff.reset_password", skip_all)]
pub async fn reset(session: Option<&Session>, code: &str, password: &str) -> Result<Done, ApiError> {
    crate::trace::rejectable(async {
        let reply = call("/internal/password/reset", json!({ "code": checked(code)?, "password": password })).await?;
        let username = text(&reply, "username");
        let logged_out = log_out_if(session, &username).await;
        Ok(Done { username, logged_out })
    })
    .await
}

/// Emails the token's user a link to delete their account; returns the address.
#[tracing::instrument(name = "bff.request_account_deletion", skip_all)]
pub async fn request_deletion(token: &str) -> Result<String, ApiError> {
    crate::trace::rejectable(async {
        let reply = call("/internal/account/delete/request", json!({ "token": token })).await?;
        Ok(text(&reply, "email"))
    })
    .await
}

/// Which account a delete link is for, without using it.
#[tracing::instrument(name = "bff.account_to_delete", skip_all)]
pub async fn deletion(code: &str) -> Result<Deletion, ApiError> {
    crate::trace::rejectable(async {
        let reply = call("/internal/account/delete/check", json!({ "code": checked(code)? })).await?;
        Ok(Deletion { username: text(&reply, "username") })
    })
    .await
}

/// Deletes the account a delete link is for.
#[tracing::instrument(name = "bff.delete_account", skip_all)]
pub async fn delete(session: Option<&Session>, hub: Option<&RollHub>, code: &str) -> Result<Done, ApiError> {
    crate::trace::rejectable(async {
        let reply = call("/internal/account/delete/check", json!({ "code": checked(code)? })).await?;
        let user_id = reply.get("user_id").and_then(Value::as_i64).ok_or_else(|| {
            tracing::error!("deleting an account: auth's reply has no user_id");
            offline()
        })?;
        let username = text(&reply, "username");
        forget(user_id, &username, hub).await?;
        // Rare: the link ran out or was replaced in the moment since the check.
        call("/internal/account/delete/confirm", json!({ "code": code })).await.map_err(|e| {
            tracing::warn!(user_id, "deleting an account: data removed but the account wasn't");
            let message = format!(
                "The data you shared was removed, but the account itself wasn't deleted: {}",
                e.message
            );
            ApiError::new(e.error, message).with_status(e.status())
        })?;
        let logged_out = log_out_if(session, &username).await;
        Ok(Done { username, logged_out })
    })
    .await
}

/// Deletes the token's account right away. Only for accounts without a confirmed
/// email (GitHub logins, or an address that may be wrong), which a link might never
/// reach; `confirm` must be the username.
#[tracing::instrument(name = "bff.delete_account_without_email", skip_all)]
pub async fn delete_direct(
    session: Option<&Session>,
    hub: Option<&RollHub>,
    token: &str,
    confirm: &str,
) -> Result<Done, ApiError> {
    crate::trace::rejectable(async {
        let profile = match crate::auth::account_profile(token).await {
            Ok(p) => p,
            Err(crate::auth::ProfileError::LoggedOut) => return Err(ApiError::logged_out()),
            Err(e) => {
                tracing::error!(error = ?e, "reading the account from auth failed");
                return Err(offline());
            }
        };
        if profile.email.is_some() && profile.email_verified {
            return Err(ApiError::new(ErrorCode::Conflict, "Your email is confirmed: use the emailed link."));
        }
        if confirm.trim() != profile.username {
            return Err(ApiError::new(ErrorCode::Invalid, "Type your username exactly to confirm."));
        }
        forget(profile.user_id, &profile.username, hub).await?;
        call("/internal/account/delete/direct", json!({ "token": token })).await?;
        let logged_out = log_out_if(session, &profile.username).await;
        Ok(Done { username: profile.username, logged_out })
    })
    .await
}
