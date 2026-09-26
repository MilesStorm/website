//! Account emails, website side (server functions): confirming the address,
//! resetting a forgotten password, and deleting the account. Auth stores the codes
//! and sends the emails (`services/auth/src/auth/account_email.rs`); the emailed
//! links open the pages in `views/email_links.rs`, which call these.
//!
//! Deleting an account first removes what the website keeps about it elsewhere
//! (SurrealDB: shared roll pictures and the profile picture; Redis: the held and
//! last roll), and only then the account itself. If that cleanup fails, nothing is
//! deleted and the user can try again, so nothing is left behind under a username
//! someone else could later register.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

/// The text of a server function's error, as written by the server.
pub fn server_message(e: ServerFnError) -> String {
    match e {
        ServerFnError::ServerError { message, .. } => message,
        _ => "Couldn't reach the site. Check your connection and try again.".into(),
    }
}

/// What the delete-account link opens on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Deletion {
    pub username: String,
}

/// After resetting a password or deleting an account: whether this browser's
/// session was that account's and has been logged out.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Done {
    pub username: String,
    pub logged_out: bool,
}

#[cfg(not(target_arch = "wasm32"))]
mod server {
    use super::*;
    use dioxus::fullstack::FullstackContext;
    use serde_json::Value;

    /// The request's session and roll hub, after checking the request came from this
    /// site (a second line of defence after the SameSite=Lax cookie).
    pub(super) fn request() -> Result<(Option<tower_sessions::Session>, Option<crate::rolls::RollHub>), ServerFnError> {
        let ctx = FullstackContext::current().ok_or_else(|| ServerFnError::new("no request context"))?;
        let parts = ctx.parts_mut();
        if !crate::rolls::origin_allowed(&parts.headers) {
            return Err(ServerFnError::new("forbidden"));
        }
        Ok((
            parts.extensions.get::<tower_sessions::Session>().cloned(),
            parts.extensions.get::<crate::rolls::RollHub>().cloned(),
        ))
    }

    /// The session's auth token.
    pub(super) async fn token(session: &Option<tower_sessions::Session>) -> Result<String, ServerFnError> {
        let token: Option<String> = match session {
            Some(s) => s.get("opaque_token").await.ok().flatten(),
            None => None,
        };
        token.ok_or_else(|| ServerFnError::new(LOGGED_OUT))
    }

    pub(super) const LOGGED_OUT: &str = "You've been logged out. Log in again.";
    const OFFLINE: &str = "Something went wrong on our side. Try again in a minute.";

    /// Calls auth; `Ok` with the reply on 2xx, otherwise the page's message for the
    /// error code auth gave.
    pub(super) async fn call(path: &str, body: Value) -> Result<Value, ServerFnError> {
        let (status, reply) = api::auth_json(path, &body).await.map_err(|e| {
            tracing::error!(error = %e, path, "reaching auth failed");
            ServerFnError::new(OFFLINE)
        })?;
        if (200..300).contains(&status) {
            return Ok(reply);
        }
        let code = reply.get("error").and_then(Value::as_str).unwrap_or("");
        let message = match code {
            "invalid_code" => "This link has expired or was already used. Ask for a new one.",
            "too_soon" => "An email was just sent. Check your inbox (and spam folder), or wait a minute before asking again.",
            "send_failed" => "The email couldn't be sent right now. Try again later.",
            "already_verified" => "Your email is already confirmed.",
            "no_email" => "Your account has no email address.",
            "weak_password" => "Use at least 8 characters.",
            "logged_out" => LOGGED_OUT,
            _ => {
                tracing::error!(path, status, code, "auth refused an email request");
                OFFLINE
            }
        };
        Err(ServerFnError::new(message))
    }

    pub(super) fn text(reply: &Value, key: &str) -> String {
        reply.get(key).and_then(Value::as_str).unwrap_or_default().to_string()
    }

    /// Logs this browser out if its session belongs to `username`.
    pub(super) async fn log_out_if(session: &Option<tower_sessions::Session>, username: &str) -> bool {
        let Some(session) = session else { return false };
        let current: Option<String> = session.get("username").await.ok().flatten();
        if current.as_deref() != Some(username) {
            return false;
        }
        let _ = session.flush().await;
        true
    }

    /// Removes the account's data outside auth. Must succeed before the account goes.
    pub(super) async fn forget(user_id: i64, username: &str, hub: &Option<crate::rolls::RollHub>) -> Result<(), ServerFnError> {
        let failed = |what: &str, e: &dyn std::fmt::Display| {
            tracing::error!(error = %e, "deleting an account: removing {what} failed");
            ServerFnError::new("Your data couldn't be removed right now, so nothing was deleted. Try again in a few minutes.")
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

}

/// Emails the logged-in user a link to confirm their address; returns the address.
#[server(prefix = "/bff")]
pub async fn send_confirmation_email() -> Result<String, ServerFnError> {
    let (session, _) = server::request()?;
    let token = server::token(&session).await?;
    let reply = server::call("/internal/email/verify/send", serde_json::json!({ "token": token })).await?;
    Ok(server::text(&reply, "email"))
}

/// Confirms an address with the code from the emailed link; returns the username.
#[server(prefix = "/bff")]
pub async fn confirm_email(code: String) -> Result<String, ServerFnError> {
    server::request()?;
    let reply = server::call("/internal/email/verify/confirm", serde_json::json!({ "code": code })).await?;
    Ok(server::text(&reply, "username"))
}

/// Emails a password reset link, if `login` (username or email) is a password
/// account with an address. Succeeds either way, to not tell who has an account.
#[server(prefix = "/bff")]
pub async fn forgot_password(login: String) -> Result<(), ServerFnError> {
    server::request()?;
    if login.trim().is_empty() {
        return Err(ServerFnError::new("Enter your username or email."));
    }
    server::call("/internal/password/forgot", serde_json::json!({ "login": login.trim() })).await?;
    Ok(())
}

/// Sets a new password with the code from the emailed link. The account is logged
/// out everywhere, including here if this browser was logged in as it.
#[server(prefix = "/bff")]
pub async fn reset_password(code: String, password: String) -> Result<Done, ServerFnError> {
    let (session, _) = server::request()?;
    let reply = server::call("/internal/password/reset", serde_json::json!({ "code": code, "password": password })).await?;
    let username = server::text(&reply, "username");
    let logged_out = server::log_out_if(&session, &username).await;
    Ok(Done { username, logged_out })
}

/// Emails the logged-in user a link to delete their account; returns the address.
#[server(prefix = "/bff")]
pub async fn request_account_deletion() -> Result<String, ServerFnError> {
    let (session, _) = server::request()?;
    let token = server::token(&session).await?;
    let reply = server::call("/internal/account/delete/request", serde_json::json!({ "token": token })).await?;
    Ok(server::text(&reply, "email"))
}

/// Which account a delete link is for, without using it.
#[server(prefix = "/bff")]
pub async fn account_to_delete(code: String) -> Result<Deletion, ServerFnError> {
    server::request()?;
    let reply = server::call("/internal/account/delete/check", serde_json::json!({ "code": code })).await?;
    Ok(Deletion { username: server::text(&reply, "username") })
}

/// Deletes the account a delete link is for.
#[server(prefix = "/bff")]
pub async fn delete_account(code: String) -> Result<Done, ServerFnError> {
    let (session, hub) = server::request()?;
    let reply = server::call("/internal/account/delete/check", serde_json::json!({ "code": code })).await?;
    let user_id = reply.get("user_id").and_then(|v| v.as_i64()).ok_or_else(|| ServerFnError::new("unexpected reply"))?;
    let username = server::text(&reply, "username");
    server::forget(user_id, &username, &hub).await?;
    server::call("/internal/account/delete/confirm", serde_json::json!({ "code": code })).await?;
    let logged_out = server::log_out_if(&session, &username).await;
    Ok(Done { username, logged_out })
}

/// Deletes the logged-in account right away. Only for accounts without an email
/// (GitHub logins), which can't get a link; `confirm` must be the username.
#[server(prefix = "/bff")]
pub async fn delete_account_without_email(confirm: String) -> Result<Done, ServerFnError> {
    let (session, hub) = server::request()?;
    let token = server::token(&session).await?;
    let profile = match api::account_profile(&token).await {
        Ok(p) => p,
        Err(api::ProfileError::LoggedOut) => return Err(ServerFnError::new(server::LOGGED_OUT)),
        Err(e) => {
            tracing::error!(error = ?e, "reading the account from auth failed");
            return Err(ServerFnError::new("Something went wrong on our side. Try again in a minute."));
        }
    };
    if profile.email.is_some() {
        return Err(ServerFnError::new("Your account has an email address: use the emailed link."));
    }
    if confirm.trim() != profile.username {
        return Err(ServerFnError::new("Type your username exactly to confirm."));
    }
    server::forget(profile.user_id, &profile.username, &hub).await?;
    server::call("/internal/account/delete/direct", serde_json::json!({ "token": token })).await?;
    let logged_out = server::log_out_if(&session, &profile.username).await;
    Ok(Done { username: profile.username, logged_out })
}
