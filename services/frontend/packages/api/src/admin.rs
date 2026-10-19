//! The admin panel's calls to auth (`/internal/admin/*`), shared by the JSON API and the server
//! functions. Auth trusts the website to have checked the user's `manage_permissions` first:
//! callers must (the API's access layer, the server functions' `admin_token`).

use api_types::{
    AdminInvite, AdminPage, AdminPermission, AdminRole, AdminUser, ApiError, ErrorCode, NewInvite, PagedResult,
};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::session::auth_url;
use crate::{auth_send, http_client, trace};

/// Most rows a page may ask for.
const MAX_PAGE: u32 = 100;

fn failed(what: &str) -> ApiError {
    ApiError::new(ErrorCode::Upstream, format!("{what} Try again later."))
}

/// Sends `req` to auth: the reply on 2xx, else `failed(what)` (or `refusal` for a 4xx it knows).
async fn send(
    req: reqwest_middleware::RequestBuilder,
    what: &str,
    refusal: impl FnOnce(u16, &str) -> Option<ApiError>,
) -> Result<Value, ApiError> {
    let (status, reply) = auth_send(req).await.map_err(|e| {
        tracing::error!(error = %e, "reaching auth failed");
        failed(what)
    })?;
    if (200..300).contains(&status) {
        return Ok(reply);
    }
    let code = reply.get("error").and_then(Value::as_str).unwrap_or("");
    if let Some(e) = (400..500).contains(&status).then(|| refusal(status, code)).flatten() {
        return Err(e);
    }
    tracing::error!(status, code, "an admin request failed");
    Err(failed(what))
}

fn read<T: DeserializeOwned>(reply: Value, what: &str) -> Result<T, ApiError> {
    serde_json::from_value(reply).map_err(|e| {
        tracing::error!(error = %e, "auth's admin reply is unreadable");
        failed(what)
    })
}

/// A page of a listing: `invalid` for a page size out of range.
async fn page<T: DeserializeOwned>(path: &str, page: &AdminPage, what: &str) -> Result<PagedResult<T>, ApiError> {
    if !(1..=MAX_PAGE).contains(&page.limit) {
        return Err(ApiError::new(ErrorCode::Invalid, format!("Ask for 1 to {MAX_PAGE} rows at a time.")));
    }
    let mut url = reqwest::Url::parse(&format!("{}{path}", auth_url())).map_err(|e| {
        tracing::error!(error = %e, "AUTH_SERVICE_URL isn't a URL");
        ApiError::internal()
    })?;
    url.query_pairs_mut()
        .append_pair("page", &page.page.to_string())
        .append_pair("limit", &page.limit.to_string())
        .append_pair("search", &page.search);
    read(send(http_client().get(url), what, |_, _| None).await?, what)
}

const USERS: &str = "Users can't be loaded right now.";
const ROLES: &str = "Roles can't be loaded right now.";
const PERMISSIONS: &str = "Permissions can't be loaded right now.";
const INVITES: &str = "Invite links can't be loaded right now.";
const SAVE: &str = "The change couldn't be saved.";

#[tracing::instrument(name = "bff.admin_list_users", skip_all)]
pub async fn users(p: &AdminPage) -> Result<PagedResult<AdminUser>, ApiError> {
    trace::rejectable(page("/internal/admin/users", p, USERS)).await
}

#[tracing::instrument(name = "bff.admin_list_roles", skip_all)]
pub async fn roles(p: &AdminPage) -> Result<PagedResult<AdminRole>, ApiError> {
    trace::rejectable(page("/internal/admin/roles", p, ROLES)).await
}

/// Every role, without its permissions (for the pickers).
#[tracing::instrument(name = "bff.admin_list_all_roles", skip_all)]
pub async fn all_roles() -> Result<Vec<AdminRole>, ApiError> {
    #[derive(Deserialize)]
    struct Role {
        id: i32,
        name: String,
    }
    trace::rejectable(async {
        let req = http_client().get(format!("{}/internal/admin/roles/all", auth_url()));
        let roles: Vec<Role> = read(send(req, ROLES, |_, _| None).await?, ROLES)?;
        Ok(roles.into_iter().map(|r| AdminRole { id: r.id, name: r.name, permissions: vec![] }).collect())
    })
    .await
}

#[tracing::instrument(name = "bff.admin_list_permissions", skip_all)]
pub async fn permissions() -> Result<Vec<AdminPermission>, ApiError> {
    trace::rejectable(async {
        let req = http_client().get(format!("{}/internal/admin/permissions", auth_url()));
        read(send(req, PERMISSIONS, |_, _| None).await?, PERMISSIONS)
    })
    .await
}

/// `POST` (give) or `DELETE` (take away) on an admin path that answers 204.
async fn change(method: reqwest::Method, path: String) -> Result<(), ApiError> {
    let req = http_client().request(method, format!("{}{path}", auth_url()));
    send(req, SAVE, |_, _| None).await.map(|_| ())
}

#[tracing::instrument(name = "bff.admin_assign_user_role", skip_all, fields(user_id, role_id))]
pub async fn assign_user_role(user_id: i64, role_id: i32) -> Result<(), ApiError> {
    trace::rejectable(change(reqwest::Method::POST, format!("/internal/admin/users/{user_id}/roles/{role_id}"))).await
}

#[tracing::instrument(name = "bff.admin_revoke_user_role", skip_all, fields(user_id, role_id))]
pub async fn revoke_user_role(user_id: i64, role_id: i32) -> Result<(), ApiError> {
    trace::rejectable(change(reqwest::Method::DELETE, format!("/internal/admin/users/{user_id}/roles/{role_id}"))).await
}

#[tracing::instrument(name = "bff.admin_assign_role_permission", skip_all, fields(role_id, permission_id))]
pub async fn assign_role_permission(role_id: i32, permission_id: i32) -> Result<(), ApiError> {
    let path = format!("/internal/admin/roles/{role_id}/permissions/{permission_id}");
    trace::rejectable(change(reqwest::Method::POST, path)).await
}

#[tracing::instrument(name = "bff.admin_revoke_role_permission", skip_all, fields(role_id, permission_id))]
pub async fn revoke_role_permission(role_id: i32, permission_id: i32) -> Result<(), ApiError> {
    let path = format!("/internal/admin/roles/{role_id}/permissions/{permission_id}");
    trace::rejectable(change(reqwest::Method::DELETE, path)).await
}

/// The latest invite links, newest first.
#[tracing::instrument(name = "bff.admin_list_invites", skip_all)]
pub async fn invites() -> Result<Vec<AdminInvite>, ApiError> {
    trace::rejectable(async {
        let req = http_client().get(format!("{}/internal/admin/invites", auth_url()));
        read(send(req, INVITES, |_, _| None).await?, INVITES)
    })
    .await
}

/// Makes an invite link, by the admin of `token`. The reply's `link` is shown only now.
#[tracing::instrument(name = "bff.admin_create_invite", skip_all, fields(role_id = invite.role_id, days = invite.days))]
pub async fn create_invite(token: &str, invite: &NewInvite) -> Result<AdminInvite, ApiError> {
    trace::rejectable(async {
        let body = json!({
            "token": token,
            "role_id": invite.role_id,
            "days": invite.days,
            "max_uses": invite.max_uses,
            "note": invite.note,
        });
        let req = http_client().post(format!("{}/internal/admin/invites", auth_url())).json(&body);
        let reply = send(req, "The invite link couldn't be made.", |status, code| {
            let (error, message) = match (status, code) {
                (400, "admin_role") => (
                    ErrorCode::Invalid,
                    "Links can't give a role with admin rights: give that role in the Users tab instead.",
                ),
                (400, _) => (
                    ErrorCode::Invalid,
                    "Check the fields: 1 to 365 days, 1 to 1000 people, and a note of at most 100 characters.",
                ),
                (404, _) => (ErrorCode::NotFound, "That role no longer exists."),
                _ => return None,
            };
            Some(ApiError::new(error, message))
        })
        .await?;
        read(reply, "The invite link couldn't be made.")
    })
    .await
}

/// Stops an invite link from letting anyone else in. People who already joined keep the role.
#[tracing::instrument(name = "bff.admin_revoke_invite", skip_all, fields(invite_id))]
pub async fn revoke_invite(invite_id: i32) -> Result<(), ApiError> {
    trace::rejectable(async {
        let req = http_client().delete(format!("{}/internal/admin/invites/{invite_id}", auth_url()));
        send(req, SAVE, |status, _| {
            (status == 404).then(|| ApiError::new(ErrorCode::NotFound, "That invite link no longer exists."))
        })
        .await
        .map(|_| ())
    })
    .await
}
