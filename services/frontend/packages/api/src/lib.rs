//! BFF server functions — phantom token pattern delegating to auth service.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

pub use ui::data_dir::{CommandResult, LoginStatus};

// ---- Session helpers (server-only) ----

#[cfg(feature = "server")]
mod session {
    use dioxus::fullstack::FullstackContext;
    use tower_sessions::Session;

    pub fn get_session() -> Option<Session> {
        let ctx = FullstackContext::current()?;
        let parts = ctx.parts_mut();
        parts.extensions.get::<Session>().cloned()
    }

    pub fn auth_url() -> String {
        std::env::var("AUTH_SERVICE_URL")
            .unwrap_or_else(|_| "http://localhost:7070".to_string())
    }

    pub fn service_secret() -> String {
        std::env::var("BFF_SERVICE_SECRET").expect("BFF_SERVICE_SECRET must be set")
    }
}

// ---- Plain async helpers for Axum OAuth handlers in the web crate ----

#[cfg(feature = "server")]
pub async fn exchange_handoff_code(code: &str) -> Result<(String, String), String> {
    use session::{auth_url, service_secret};

    #[derive(Serialize)]
    struct Req<'a> {
        code: &'a str,
    }
    #[derive(Deserialize)]
    struct Resp {
        token: String,
        username: String,
    }

    let resp = reqwest::Client::new()
        .post(format!("{}/internal/token/exchange/code", auth_url()))
        .header("x-service-token", service_secret())
        .json(&Req { code })
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        return Err(format!("exchange failed: {}", resp.status()));
    }

    let data: Resp = resp.json().await.map_err(|e| e.to_string())?;
    Ok((data.token, data.username))
}

#[cfg(feature = "server")]
pub async fn exchange_google_auth_code(code: &str) -> Result<(String, String), String> {
    use session::{auth_url, service_secret};

    #[derive(Serialize)]
    struct Req<'a> {
        code: &'a str,
    }
    #[derive(Deserialize)]
    struct Resp {
        token: String,
        username: String,
    }

    let resp = reqwest::Client::new()
        .post(format!("{}/internal/oauth/exchange/google", auth_url()))
        .header("x-service-token", service_secret())
        .json(&Req { code })
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(body);
    }

    let data: Resp = resp.json().await.map_err(|e| e.to_string())?;
    Ok((data.token, data.username))
}

// ---- Shared response types ----

#[derive(Serialize, Deserialize, Debug)]
pub struct DockerRequestResponse {
    pub restart_result: String,
    pub command_result: Option<CommandResult>,
}

// ---- Server functions ----

/// Log in with username + password.
#[server]
pub async fn login_password(
    username: String,
    password: String,
) -> Result<LoginStatus, ServerFnError> {
    use session::*;

    #[derive(Serialize)]
    struct Req {
        username: String,
        password: String,
    }
    #[derive(Deserialize)]
    struct Resp {
        token: String,
        username: String,
    }

    let resp = reqwest::Client::new()
        .post(format!("{}/internal/token/exchange", auth_url()))
        .header("x-service-token", service_secret())
        .json(&Req { username: username.clone(), password })
        .send()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;

    if !resp.status().is_success() {
        tracing::warn!(username = %username, "password login failed: invalid credentials");
        return Err(ServerFnError::new("Invalid credentials"));
    }

    let data: Resp = resp
        .json()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;

    let sess = get_session().ok_or_else(|| ServerFnError::new("no session context"))?;
    sess.insert("opaque_token", data.token)
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;
    sess.insert("username", data.username.clone())
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;

    tracing::info!(username = %data.username, "password login succeeded");
    Ok(LoginStatus::LoggedIn(data.username))
}

/// Clear the current session.
#[server]
pub async fn logout() -> Result<(), ServerFnError> {
    use session::*;

    if let Some(sess) = get_session() {
        let username: Option<String> = sess.get("username").await.ok().flatten();
        sess.flush()
            .await
            .map_err(|e| ServerFnError::new(e.to_string()))?;
        tracing::info!(username = ?username, "user logged out");
    }
    Ok(())
}

/// Check the current login status from the BFF session.
#[server]
pub async fn check_login_status() -> Result<LoginStatus, ServerFnError> {
    use session::*;

    let sess = match get_session() {
        Some(s) => s,
        None => return Ok(LoginStatus::LoggedOut),
    };

    let username: Option<String> = sess
        .get("username")
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;

    Ok(match username {
        Some(u) => LoginStatus::LoggedIn(u),
        None => LoginStatus::LoggedOut,
    })
}

/// Register a new account and auto-login on success.
#[server]
pub async fn register_password(
    username: String,
    email: String,
    password: String,
) -> Result<LoginStatus, ServerFnError> {
    use session::*;

    #[derive(Serialize)]
    struct Req {
        username: String,
        email: String,
        password: String,
    }
    #[derive(Deserialize)]
    struct Resp {
        token: String,
        username: String,
    }

    let resp = reqwest::Client::new()
        .post(format!("{}/internal/register", auth_url()))
        .header("x-service-token", service_secret())
        .json(&Req { username: username.clone(), email, password })
        .send()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;

    if resp.status() == reqwest::StatusCode::CONFLICT {
        let body = resp.text().await.unwrap_or_default();
        tracing::warn!(username = %username, reason = %body, "registration conflict");
        return Err(ServerFnError::new(body));
    }

    if !resp.status().is_success() {
        tracing::error!(username = %username, status = %resp.status(), "registration failed");
        return Err(ServerFnError::new("Registration failed"));
    }

    let data: Resp = resp
        .json()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;

    let sess = get_session().ok_or_else(|| ServerFnError::new("no session context"))?;
    sess.insert("opaque_token", data.token)
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;
    sess.insert("username", data.username.clone())
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;

    tracing::info!(username = %data.username, "registration succeeded");
    Ok(LoginStatus::LoggedIn(data.username))
}

/// Returns the auth service URL for a given OAuth provider init endpoint.
/// Uses PUBLIC_AUTH_URL (browser-facing base) rather than AUTH_SERVICE_URL (internal cluster URL)
/// so the browser can actually reach it. Empty string → relative path, which Istio routes to auth.
#[server]
pub async fn get_oauth_init_url(provider: String) -> Result<String, ServerFnError> {
    let base = std::env::var("PUBLIC_AUTH_URL")
        .unwrap_or_else(|_| "http://localhost:7070".to_string());
    Ok(format!("{}/api/login/{}/init", base, provider))
}

/// Returns the list of permission names held by the current session's user.
#[server]
pub async fn get_my_permissions() -> Result<Vec<String>, ServerFnError> {
    use session::*;

    let sess = match get_session() {
        Some(s) => s,
        None => return Ok(vec![]),
    };

    let token: Option<String> = sess
        .get("opaque_token")
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;

    let token = match token {
        Some(t) => t,
        None => return Ok(vec![]),
    };

    #[derive(Serialize)]
    struct Req {
        token: String,
    }
    #[derive(Deserialize)]
    struct Resp {
        permissions: Vec<String>,
    }

    let resp = reqwest::Client::new()
        .post(format!("{}/internal/token/introspect", auth_url()))
        .header("x-service-token", service_secret())
        .json(&Req { token })
        .send()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;

    if !resp.status().is_success() {
        return Ok(vec![]);
    }

    let data: Resp = resp
        .json()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;

    Ok(data.permissions)
}

/// Check whether the current user holds a specific permission.
#[server]
pub async fn check_permission(name: String) -> Result<bool, ServerFnError> {
    let result = get_my_permissions().await?.contains(&name);
    tracing::debug!(permission = %name, granted = result, "permission check");
    Ok(result)
}

/// Get the number of active players on the Ark server.
#[server]
pub async fn ark_player_count() -> Result<i32, ServerFnError> {
    use session::*;

    let sess = get_session().ok_or_else(|| ServerFnError::new("Not authenticated"))?;
    let token: Option<String> = sess
        .get("opaque_token")
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;
    let token = token.ok_or_else(|| ServerFnError::new("Not authenticated"))?;

    #[derive(Serialize)]
    struct Req {
        token: String,
    }

    let resp = reqwest::Client::new()
        .post(format!("{}/internal/ark/num_players", auth_url()))
        .header("x-service-token", service_secret())
        .json(&Req { token })
        .send()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;

    if !resp.status().is_success() {
        return Err(ServerFnError::new("Ark request failed"));
    }

    let body: DockerRequestResponse = resp
        .json()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;

    Ok(match body.command_result {
        Some(CommandResult::NumPlayers(n)) => n,
        _ => -1,
    })
}

/// Execute an Ark server command (start | stop | restart).
#[server]
pub async fn ark_command(cmd: String) -> Result<CommandResult, ServerFnError> {
    use session::*;

    let sess = get_session().ok_or_else(|| ServerFnError::new("Not authenticated"))?;
    let username: Option<String> = sess.get("username").await.ok().flatten();
    let token: Option<String> = sess
        .get("opaque_token")
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;
    let token = token.ok_or_else(|| ServerFnError::new("Not authenticated"))?;
    tracing::info!(username = ?username, cmd = %cmd, "ark command requested");

    #[derive(Serialize)]
    struct Req {
        token: String,
        cmd: String,
    }

    let resp = reqwest::Client::new()
        .post(format!("{}/internal/ark/command", auth_url()))
        .header("x-service-token", service_secret())
        .json(&Req { token, cmd })
        .send()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;

    if !resp.status().is_success() {
        return Err(ServerFnError::new("Ark command failed"));
    }

    let body: DockerRequestResponse = resp
        .json()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;

    body.command_result
        .ok_or_else(|| ServerFnError::new("No command result"))
}
