//! BFF server functions — phantom token pattern delegating to auth service.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

pub use ui::data_dir::{AdminPermission, AdminRole, AdminUser, AdminUserRole, CommandResult, LoginStatus, PagedResult};

/// Request extension that carries the serialised W3C `traceparent` captured
/// before Dioxus's SSR dispatcher spawns server-function tasks.
/// Set by the `capture_traceparent` middleware in the `web` crate.
#[cfg(feature = "server")]
#[derive(Clone)]
pub struct IncomingTraceparent(pub String);

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

    /// Returns the W3C `traceparent` captured before any Dioxus spawn, or `None`
    /// if not available (in which case `TracingMiddleware` handles propagation).
    pub fn traceparent() -> Option<String> {
        let ctx = FullstackContext::current()?;
        let parts = ctx.parts_mut();
        parts.extensions.get::<crate::IncomingTraceparent>().map(|t| t.0.clone())
    }
}

#[cfg(feature = "server")]
fn http_client() -> &'static reqwest_middleware::ClientWithMiddleware {
    use std::sync::OnceLock;
    use reqwest_middleware::ClientBuilder;
    static CLIENT: OnceLock<reqwest_middleware::ClientWithMiddleware> = OnceLock::new();
    CLIENT.get_or_init(|| {
        ClientBuilder::new(reqwest::Client::new())
            .with(PropagateTraceContext)
            .build()
    })
}

/// Injects the W3C `traceparent` header into every outbound BFF→auth request.
///
/// Client-triggered path: Dioxus instruments the handler task with the current
/// span, so `Span::current()` has a valid OTel context — extract and inject it.
///
/// SSR path: Dioxus spawns the render without tracing context, so
/// `Span::current()` is a no-op span. Fall back to the traceparent captured
/// synchronously before the spawn by the `capture_traceparent` Axum middleware.
#[cfg(feature = "server")]
struct PropagateTraceContext;

#[cfg(feature = "server")]
#[async_trait::async_trait]
impl reqwest_middleware::Middleware for PropagateTraceContext {
    async fn handle(
        &self,
        mut req: reqwest::Request,
        extensions: &mut http::Extensions,
        next: reqwest_middleware::Next<'_>,
    ) -> reqwest_middleware::Result<reqwest::Response> {
        use opentelemetry::trace::TraceContextExt as _;
        use tracing_opentelemetry::OpenTelemetrySpanExt as _;

        let cx = tracing::Span::current().context();
        let traceparent = if cx.span().span_context().is_valid() {
            let mut carrier = std::collections::HashMap::new();
            opentelemetry::global::get_text_map_propagator(|p| p.inject_context(&cx, &mut carrier));
            carrier.remove("traceparent")
        } else {
            session::traceparent()
        };

        if let Some(tp) = traceparent {
            if let Ok(val) = reqwest::header::HeaderValue::from_str(&tp) {
                req.headers_mut().insert("traceparent", val);
            }
        }

        next.run(req, extensions).await
    }
}

// ---- Plain async helpers for Axum OAuth handlers in the web crate ----

/// Ask the auth service to begin an OAuth flow. Returns `(auth_url, csrf_state)`.
#[cfg(feature = "server")]
#[tracing::instrument(name = "bff.start_oauth", skip_all, fields(provider = %provider))]
pub async fn start_oauth(provider: &str) -> Result<(String, String), String> {
    use session::{auth_url, service_secret};

    #[derive(Serialize)]
    struct Req<'a> {
        provider: &'a str,
    }
    #[derive(Deserialize)]
    struct Resp {
        auth_url: String,
        state: String,
    }

    let resp = http_client()
        .post(format!("{}/internal/oauth/start", auth_url()))
        .header("x-service-token", service_secret())
        .json(&Req { provider })
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        return Err(format!("oauth start failed: {}", resp.status()));
    }

    let data: Resp = resp.json().await.map_err(|e| e.to_string())?;
    Ok((data.auth_url, data.state))
}

/// Exchange an OAuth authorization code for a BFF opaque token + username.
#[cfg(feature = "server")]
#[tracing::instrument(name = "bff.exchange_oauth_code", skip_all, fields(provider = %provider))]
pub async fn exchange_oauth_code(
    provider: &str,
    code: &str,
) -> Result<(String, String), String> {
    use session::{auth_url, service_secret};

    #[derive(Serialize)]
    struct Req<'a> {
        provider: &'a str,
        code: &'a str,
    }
    #[derive(Deserialize)]
    struct Resp {
        token: String,
        username: String,
    }

    let resp = http_client()
        .post(format!("{}/internal/oauth/exchange", auth_url()))
        .header("x-service-token", service_secret())
        .json(&Req { provider, code })
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        metrics::counter!("bff_login_attempts_total", "method" => provider.to_string(), "status" => "failure").increment(1);
        return Err(body);
    }

    let data: Resp = resp.json().await.map_err(|e| e.to_string())?;
    metrics::counter!("bff_login_attempts_total", "method" => provider.to_string(), "status" => "success").increment(1);
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
#[server(prefix = "/bff")]
#[tracing::instrument(name = "bff.login_password", skip_all, fields(username = %username))]
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

    let resp = http_client()
        .post(format!("{}/internal/token/exchange", auth_url()))
        .header("x-service-token", service_secret())
        .json(&Req { username: username.clone(), password })
        .send()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;

    if !resp.status().is_success() {
        tracing::warn!(username = %username, "password login failed: invalid credentials");
        metrics::counter!("bff_login_attempts_total", "method" => "password", "status" => "failure").increment(1);
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
    metrics::counter!("bff_login_attempts_total", "method" => "password", "status" => "success").increment(1);
    Ok(LoginStatus::LoggedIn(data.username))
}

/// Clear the current session.
#[server(prefix = "/bff")]
#[tracing::instrument(name = "bff.logout", skip_all)]
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
#[server(prefix = "/bff")]
#[tracing::instrument(name = "bff.check_login_status", skip_all)]
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
#[server(prefix = "/bff")]
#[tracing::instrument(name = "bff.register_password", skip_all, fields(username = %username))]
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

    let resp = http_client()
        .post(format!("{}/internal/register", auth_url()))
        .header("x-service-token", service_secret())
        .json(&Req { username: username.clone(), email, password })
        .send()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;

    if resp.status() == reqwest::StatusCode::CONFLICT {
        let body = resp.text().await.unwrap_or_default();
        tracing::warn!(username = %username, reason = %body, "registration conflict");
        metrics::counter!("bff_register_attempts_total", "status" => "conflict").increment(1);
        return Err(ServerFnError::new(body));
    }

    if !resp.status().is_success() {
        tracing::error!(username = %username, status = %resp.status(), "registration failed");
        metrics::counter!("bff_register_attempts_total", "status" => "error").increment(1);
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
    metrics::counter!("bff_register_attempts_total", "status" => "success").increment(1);
    Ok(LoginStatus::LoggedIn(data.username))
}

/// Returns the list of permission names held by the current session's user.
#[server(prefix = "/bff")]
#[tracing::instrument(name = "bff.get_my_permissions", skip_all)]
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

    let resp = http_client()
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

/// Check whether a raw opaque token grants the `arcane` permission.
/// Used by Axum handlers that have direct session access (e.g. the WebSocket proxy)
/// but are outside the Dioxus server function context.
#[cfg(feature = "server")]
#[tracing::instrument(name = "bff.has_arcane_permission", skip_all)]
pub async fn has_arcane_permission(token: &str) -> bool {
    use session::{auth_url, service_secret};

    #[derive(Serialize)]
    struct Req<'a> {
        token: &'a str,
    }
    #[derive(Deserialize)]
    struct Resp {
        permissions: Vec<String>,
    }

    let result = http_client()
        .post(format!("{}/internal/token/introspect", auth_url()))
        .header("x-service-token", service_secret())
        .json(&Req { token })
        .send()
        .await;

    match result {
        Ok(r) if r.status().is_success() => r
            .json::<Resp>()
            .await
            .map(|data| data.permissions.iter().any(|p| p == "arcane"))
            .unwrap_or(false),
        Ok(r) => {
            tracing::warn!(status = %r.status(), "arcane permission introspect returned non-success");
            false
        }
        Err(e) => {
            tracing::error!(error = %e, "arcane permission introspect request failed");
            false
        }
    }
}

/// Check whether the current user holds a specific permission.
#[server(prefix = "/bff")]
#[tracing::instrument(name = "bff.check_permission", skip_all, fields(permission = %name))]
pub async fn check_permission(name: String) -> Result<bool, ServerFnError> {
    let result = get_my_permissions().await?.contains(&name);
    tracing::debug!(permission = %name, granted = result, "permission check");
    Ok(result)
}

/// Get the number of active players on the Ark server.
#[server(prefix = "/bff")]
#[tracing::instrument(name = "bff.ark_player_count", skip_all)]
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

    let resp = http_client()
        .post(format!("{}/internal/ark/num_players", auth_url()))
        .header("x-service-token", service_secret())
        .json(&Req { token })
        .send()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;

    if !resp.status().is_success() {
        metrics::counter!("bff_ark_commands_total", "cmd" => "num_players", "status" => "error").increment(1);
        return Err(ServerFnError::new("Ark request failed"));
    }

    let body: DockerRequestResponse = resp
        .json()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;

    metrics::counter!("bff_ark_commands_total", "cmd" => "num_players", "status" => "success").increment(1);
    Ok(match body.command_result {
        Some(CommandResult::NumPlayers(n)) => n,
        _ => -1,
    })
}

/// Execute an Ark server command (start | stop | restart).
#[server(prefix = "/bff")]
#[tracing::instrument(name = "bff.ark_command", skip_all, fields(cmd = %cmd))]
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

    let cmd_label = cmd.clone();
    let resp = http_client()
        .post(format!("{}/internal/ark/command", auth_url()))
        .header("x-service-token", service_secret())
        .json(&Req { token, cmd })
        .send()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;

    if !resp.status().is_success() {
        metrics::counter!("bff_ark_commands_total", "cmd" => cmd_label, "status" => "error").increment(1);
        return Err(ServerFnError::new("Ark command failed"));
    }

    let body: DockerRequestResponse = resp
        .json()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;

    metrics::counter!("bff_ark_commands_total", "cmd" => cmd_label, "status" => "success").increment(1);
    body.command_result
        .ok_or_else(|| ServerFnError::new("No command result"))
}

// ---- Admin RBAC server functions ----

#[server(prefix = "/bff")]
#[tracing::instrument(name = "bff.admin_list_users", skip_all)]
pub async fn admin_list_users(page: u32, limit: u32, search: String) -> Result<PagedResult<AdminUser>, ServerFnError> {
    use session::*;

    if !check_permission("manage_permissions".to_string()).await? {
        return Err(ServerFnError::new("Forbidden"));
    }

    #[derive(Deserialize)]
    struct RoleRef { id: i32, name: String }
    #[derive(Deserialize)]
    struct UserResp { id: i64, username: String, email: Option<String>, roles: Vec<RoleRef> }
    #[derive(Deserialize)]
    struct Paged { items: Vec<UserResp>, total: i64 }

    let mut url = reqwest::Url::parse(&format!("{}/internal/admin/users", auth_url()))
        .map_err(|e| ServerFnError::new(e.to_string()))?;
    url.query_pairs_mut()
        .append_pair("page", &page.to_string())
        .append_pair("limit", &limit.to_string())
        .append_pair("search", &search);

    let resp = http_client()
        .get(url)
        .header("x-service-token", service_secret())
        .send()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;

    if !resp.status().is_success() {
        return Err(ServerFnError::new("Failed to fetch users"));
    }

    let data: Paged = resp.json().await.map_err(|e| ServerFnError::new(e.to_string()))?;
    Ok(PagedResult {
        total: data.total,
        items: data.items.into_iter().map(|u| AdminUser {
            id: u.id,
            username: u.username,
            email: u.email,
            roles: u.roles.into_iter().map(|r| AdminUserRole { id: r.id, name: r.name }).collect(),
        }).collect(),
    })
}

#[server(prefix = "/bff")]
#[tracing::instrument(name = "bff.admin_list_roles", skip_all)]
pub async fn admin_list_roles(page: u32, limit: u32, search: String) -> Result<PagedResult<AdminRole>, ServerFnError> {
    use session::*;

    if !check_permission("manage_permissions".to_string()).await? {
        return Err(ServerFnError::new("Forbidden"));
    }

    #[derive(Deserialize)]
    struct PermRef { id: i32, name: String }
    #[derive(Deserialize)]
    struct RoleResp { id: i32, name: String, permissions: Vec<PermRef> }
    #[derive(Deserialize)]
    struct Paged { items: Vec<RoleResp>, total: i64 }

    let mut url = reqwest::Url::parse(&format!("{}/internal/admin/roles", auth_url()))
        .map_err(|e| ServerFnError::new(e.to_string()))?;
    url.query_pairs_mut()
        .append_pair("page", &page.to_string())
        .append_pair("limit", &limit.to_string())
        .append_pair("search", &search);

    let resp = http_client()
        .get(url)
        .header("x-service-token", service_secret())
        .send()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;

    if !resp.status().is_success() {
        return Err(ServerFnError::new("Failed to fetch roles"));
    }

    let data: Paged = resp.json().await.map_err(|e| ServerFnError::new(e.to_string()))?;
    Ok(PagedResult {
        total: data.total,
        items: data.items.into_iter().map(|r| AdminRole {
            id: r.id,
            name: r.name,
            permissions: r.permissions.into_iter().map(|p| AdminPermission { id: p.id, name: p.name }).collect(),
        }).collect(),
    })
}

/// Returns all roles (names only, no permissions) for use in assignment dropdowns.
#[server(prefix = "/bff")]
#[tracing::instrument(name = "bff.admin_list_all_roles", skip_all)]
pub async fn admin_list_all_roles() -> Result<Vec<AdminRole>, ServerFnError> {
    use session::*;

    if !check_permission("manage_permissions".to_string()).await? {
        return Err(ServerFnError::new("Forbidden"));
    }

    #[derive(Deserialize)]
    struct RoleResp { id: i32, name: String, permissions: Vec<serde_json::Value> }

    let resp = http_client()
        .get(format!("{}/internal/admin/roles/all", auth_url()))
        .header("x-service-token", service_secret())
        .send()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;

    if !resp.status().is_success() {
        return Err(ServerFnError::new("Failed to fetch roles"));
    }

    let data: Vec<RoleResp> = resp.json().await.map_err(|e| ServerFnError::new(e.to_string()))?;
    Ok(data.into_iter().map(|r| AdminRole { id: r.id, name: r.name, permissions: vec![] }).collect())
}

#[server(prefix = "/bff")]
#[tracing::instrument(name = "bff.admin_list_permissions", skip_all)]
pub async fn admin_list_permissions() -> Result<Vec<AdminPermission>, ServerFnError> {
    use session::*;

    if !check_permission("manage_permissions".to_string()).await? {
        return Err(ServerFnError::new("Forbidden"));
    }

    #[derive(Deserialize)]
    struct PermResp {
        id: i32,
        name: String,
    }

    let resp = http_client()
        .get(format!("{}/internal/admin/permissions", auth_url()))
        .header("x-service-token", service_secret())
        .send()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;

    if !resp.status().is_success() {
        return Err(ServerFnError::new("Failed to fetch permissions"));
    }

    let data: Vec<PermResp> = resp.json().await.map_err(|e| ServerFnError::new(e.to_string()))?;
    Ok(data.into_iter().map(|p| AdminPermission { id: p.id, name: p.name }).collect())
}

#[server(prefix = "/bff")]
#[tracing::instrument(name = "bff.admin_assign_user_role", skip_all, fields(user_id, role_id))]
pub async fn admin_assign_user_role(user_id: i64, role_id: i32) -> Result<(), ServerFnError> {
    use session::*;

    if !check_permission("manage_permissions".to_string()).await? {
        return Err(ServerFnError::new("Forbidden"));
    }

    let resp = http_client()
        .post(format!("{}/internal/admin/users/{user_id}/roles/{role_id}", auth_url()))
        .header("x-service-token", service_secret())
        .send()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;

    if !resp.status().is_success() {
        return Err(ServerFnError::new("Failed to assign role"));
    }
    Ok(())
}

#[server(prefix = "/bff")]
#[tracing::instrument(name = "bff.admin_revoke_user_role", skip_all, fields(user_id, role_id))]
pub async fn admin_revoke_user_role(user_id: i64, role_id: i32) -> Result<(), ServerFnError> {
    use session::*;

    if !check_permission("manage_permissions".to_string()).await? {
        return Err(ServerFnError::new("Forbidden"));
    }

    let resp = http_client()
        .delete(format!("{}/internal/admin/users/{user_id}/roles/{role_id}", auth_url()))
        .header("x-service-token", service_secret())
        .send()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;

    if !resp.status().is_success() {
        return Err(ServerFnError::new("Failed to revoke role"));
    }
    Ok(())
}

#[server(prefix = "/bff")]
#[tracing::instrument(name = "bff.admin_assign_role_permission", skip_all, fields(role_id, permission_id))]
pub async fn admin_assign_role_permission(role_id: i32, permission_id: i32) -> Result<(), ServerFnError> {
    use session::*;

    if !check_permission("manage_permissions".to_string()).await? {
        return Err(ServerFnError::new("Forbidden"));
    }

    let resp = http_client()
        .post(format!("{}/internal/admin/roles/{role_id}/permissions/{permission_id}", auth_url()))
        .header("x-service-token", service_secret())
        .send()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;

    if !resp.status().is_success() {
        return Err(ServerFnError::new("Failed to assign permission"));
    }
    Ok(())
}

#[server(prefix = "/bff")]
#[tracing::instrument(name = "bff.admin_revoke_role_permission", skip_all, fields(role_id, permission_id))]
pub async fn admin_revoke_role_permission(role_id: i32, permission_id: i32) -> Result<(), ServerFnError> {
    use session::*;

    if !check_permission("manage_permissions".to_string()).await? {
        return Err(ServerFnError::new("Forbidden"));
    }

    let resp = http_client()
        .delete(format!("{}/internal/admin/roles/{role_id}/permissions/{permission_id}", auth_url()))
        .header("x-service-token", service_secret())
        .send()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;

    if !resp.status().is_success() {
        return Err(ServerFnError::new("Failed to revoke permission"));
    }
    Ok(())
}
