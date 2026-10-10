//! Invite links: opening one gives the user a role, such as `arcane_user` for the
//! dice test group. Someone without an account signs up first; the website keeps
//! the code in their session and redeems it right after.
//!
//! Only the code's SHA-256 is stored (`invites`), so the link is shown once, when
//! it is made. A link stops working when it expires, is revoked, or has been used
//! `max_uses` times. Opening it again as someone who already joined through it
//! still works and doesn't count twice.
//!
//! Endpoints (service token required, like the rest of `/internal`; the website
//! checks the admin's `manage_permissions` before the admin ones):
//! - `POST /internal/admin/invites` `{token, role_id, days, max_uses?, note?}` →
//!   the invite with its `link`. 400 `invalid_invite`, 400 `admin_role` (roles with
//!   `manage_permissions` can't be given by link), 404 `no_role`.
//! - `GET /internal/admin/invites` → the latest invites, newest first.
//! - `DELETE /internal/admin/invites/{id}`: revoke. 404 `no_invite`.
//! - `POST /internal/invite/check` `{code}` → `{role}`. 404 `invalid_code`.
//! - `POST /internal/invite/redeem` `{code, token}` → `{role}`: gives the token's
//!   user the role. 401 `logged_out`, 404 `invalid_code`, 403 `role_removed` (they
//!   joined through it before and an admin has since taken the role away).

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{delete, post},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::account_email::{db_error, error, hash, new_code};
use super::internal::InternalState;
use super::mail::site_url;
use super::telemetry;
use super::trace;

/// Longest a link can work, in days.
const MAX_DAYS: i64 = 365;
/// Most people one link can let in, when limited.
const MAX_USES: i32 = 1000;
/// Longest note, in characters.
const NOTE_MAX: usize = 100;
/// How many invites the admin panel lists.
const LIST_LIMIT: i64 = 200;

pub fn routes() -> Router<InternalState> {
    Router::new()
        .route("/internal/admin/invites", post(create).get(list))
        .route("/internal/admin/invites/{id}", delete(revoke))
        .route("/internal/invite/check", post(check))
        .route("/internal/invite/redeem", post(redeem))
}

/// Whether the role `{role}` gives admin rights: links (which get pasted around) must
/// never hand those out, even if the role gained them after the link was made.
fn is_admin_role(role: &str) -> String {
    format!(
        "EXISTS (SELECT 1 FROM role_permissions rp JOIN permissions p ON p.id = rp.permission_id \
         WHERE rp.role_id = {role} AND p.name = 'manage_permissions')"
    )
}

/// Whether an invite (as `i`) can still let someone new in.
const LIVE: &str = "i.revoked_at IS NULL AND i.expires_at > NOW() AND (i.max_uses IS NULL OR i.uses < i.max_uses) \
     AND NOT EXISTS (SELECT 1 FROM role_permissions rp JOIN permissions p ON p.id = rp.permission_id \
                     WHERE rp.role_id = i.role_id AND p.name = 'manage_permissions')";

/// The link for `code`.
fn link(code: &str) -> String {
    format!("{}/invite#{code}", site_url())
}

/// The note as it will be stored: trimmed, `None` when empty. `Err` when too long
/// or holding control characters.
fn clean_note(raw: Option<&str>) -> Result<Option<String>, ()> {
    let Some(note) = raw.map(str::trim).filter(|n| !n.is_empty()) else {
        return Ok(None);
    };
    if note.chars().count() > NOTE_MAX || note.chars().any(char::is_control) {
        return Err(());
    }
    Ok(Some(note.to_string()))
}

/// Whether the admin's choices for a new invite are in range.
fn limits_ok(days: i64, max_uses: Option<i32>) -> bool {
    (1..=MAX_DAYS).contains(&days) && max_uses.is_none_or(|n| (1..=MAX_USES).contains(&n))
}

// ---- Admin ----

#[derive(Serialize, sqlx::FromRow)]
struct InviteResp {
    id: i32,
    role_id: i32,
    role: String,
    note: Option<String>,
    /// Username of the admin who made it (`None` once that account is gone).
    created_by: Option<String>,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    max_uses: Option<i32>,
    uses: i32,
    revoked: bool,
    /// Whether it can still let someone new in.
    live: bool,
    /// Usernames of who joined through it, in order.
    joined: Vec<String>,
    /// Only when just made: afterwards only the code's hash is left.
    #[sqlx(skip)]
    #[serde(skip_serializing_if = "Option::is_none")]
    link: Option<String>,
}

fn invite_select() -> String {
    format!(
        "SELECT i.id, i.role_id, r.name AS role, i.note, u.username AS created_by, i.created_at, \
         i.expires_at, i.max_uses, i.uses, i.revoked_at IS NOT NULL AS revoked, ({LIVE}) AS live, \
         ARRAY(SELECT ju.username FROM invite_redemptions ir JOIN users ju ON ju.id = ir.user_id \
               WHERE ir.invite_id = i.id ORDER BY ir.redeemed_at) AS joined \
         FROM invites i JOIN roles r ON r.id = i.role_id LEFT JOIN users u ON u.id = i.created_by"
    )
}

#[derive(Deserialize)]
struct CreateReq {
    /// The admin's session token, to record who made it.
    token: String,
    role_id: i32,
    days: i64,
    max_uses: Option<i32>,
    note: Option<String>,
}

#[tracing::instrument(name = "invite.create", skip_all, fields(role_id = req.role_id))]
async fn create(State(state): State<InternalState>, Json(req): Json<CreateReq>) -> Response {
    let Ok(note) = clean_note(req.note.as_deref()) else {
        return error(StatusCode::BAD_REQUEST, "invalid_invite");
    };
    if !limits_ok(req.days, req.max_uses) {
        return error(StatusCode::BAD_REQUEST, "invalid_invite");
    }
    match sqlx::query_as::<_, (bool,)>(&format!("SELECT {}", is_admin_role("$1")))
        .bind(req.role_id)
        .fetch_one(&state.db)
        .await
    {
        Ok((false,)) => {}
        Ok((true,)) => return error(StatusCode::BAD_REQUEST, "admin_role"),
        Err(e) => return db_error("checking an invite's role", e),
    }
    let code = new_code();
    let id: Result<Option<(i32,)>, _> = sqlx::query_as(
        "INSERT INTO invites (code_hash, role_id, note, created_by, expires_at, max_uses) \
         SELECT $1, r.id, $3, (SELECT user_id FROM bff_tokens WHERE token = $4 AND expires_at > NOW()), \
                NOW() + make_interval(days => $5), $6 \
         FROM roles r WHERE r.id = $2 \
         RETURNING id",
    )
    .bind(hash(&code))
    .bind(req.role_id)
    .bind(&note)
    .bind(&req.token)
    .bind(req.days as i32)
    .bind(req.max_uses)
    .fetch_optional(&state.db)
    .await;
    let id = match id {
        Ok(Some((id,))) => id,
        Ok(None) => return error(StatusCode::NOT_FOUND, "no_role"),
        Err(e) => return db_error("making an invite", e),
    };
    let invite: Result<InviteResp, _> = sqlx::query_as(&format!("{} WHERE i.id = $1", invite_select()))
        .bind(id)
        .fetch_one(&state.db)
        .await;
    match invite {
        Ok(mut invite) => {
            tracing::info!(invite_id = id, role = %invite.role, "invite made");
            telemetry::token_operation("invite_create", "success");
            invite.link = Some(link(&code));
            Json(invite).into_response()
        }
        Err(e) => db_error("loading a new invite", e),
    }
}

#[tracing::instrument(name = "invite.list", skip_all)]
async fn list(State(state): State<InternalState>) -> Response {
    let invites: Result<Vec<InviteResp>, _> =
        sqlx::query_as(&format!("{} ORDER BY i.created_at DESC LIMIT $1", invite_select()))
            .bind(LIST_LIMIT)
            .fetch_all(&state.db)
            .await;
    match invites {
        Ok(invites) => Json(invites).into_response(),
        Err(e) => db_error("listing invites", e),
    }
}

#[tracing::instrument(name = "invite.revoke", skip_all, fields(invite_id = id))]
async fn revoke(State(state): State<InternalState>, Path(id): Path<i32>) -> Response {
    let result = sqlx::query("UPDATE invites SET revoked_at = COALESCE(revoked_at, NOW()) WHERE id = $1")
        .bind(id)
        .execute(&state.db)
        .await;
    match result {
        Ok(r) if r.rows_affected() == 0 => error(StatusCode::NOT_FOUND, "no_invite"),
        Ok(_) => {
            tracing::info!(invite_id = id, "invite revoked");
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => db_error("revoking an invite", e),
    }
}

// ---- Invitee ----

#[derive(Deserialize)]
struct CheckReq {
    code: String,
}

#[derive(Serialize)]
struct RoleResp {
    /// The role the link gives, e.g. `arcane_user`.
    role: String,
}

#[tracing::instrument(name = "invite.check", skip_all)]
async fn check(State(state): State<InternalState>, Json(req): Json<CheckReq>) -> Response {
    let role: Result<Option<(String,)>, _> = sqlx::query_as(&format!(
        "SELECT r.name FROM invites i JOIN roles r ON r.id = i.role_id WHERE i.code_hash = $1 AND {LIVE}"
    ))
    .bind(hash(&req.code))
    .fetch_optional(&state.db)
    .await;
    match role {
        Ok(Some((role,))) => Json(RoleResp { role }).into_response(),
        Ok(None) => error(StatusCode::NOT_FOUND, "invalid_code"),
        Err(e) => db_error("checking an invite", e),
    }
}

#[derive(Deserialize)]
struct RedeemReq {
    code: String,
    token: String,
}

#[derive(sqlx::FromRow)]
struct InviteRow {
    id: i32,
    role_id: i32,
    role: String,
    live: bool,
}

enum Redeemed {
    /// The role; `true` when joined just now (not an earlier join opened again).
    Joined(String, bool),
    LoggedOut,
    Invalid,
    /// Joined through this link before; the role was removed since.
    Removed,
}

#[tracing::instrument(name = "invite.redeem", skip_all)]
async fn redeem(State(state): State<InternalState>, Json(req): Json<RedeemReq>) -> Response {
    let result: Result<Redeemed, sqlx::Error> = async {
        let mut tx = trace::begin(&state.db).await?;
        // Locked, so two people can't both take the last use.
        let invite: Option<InviteRow> = sqlx::query_as(&format!(
            "SELECT i.id, i.role_id, r.name AS role, ({LIVE}) AS live \
             FROM invites i JOIN roles r ON r.id = i.role_id WHERE i.code_hash = $1 FOR UPDATE OF i"
        ))
        .bind(hash(&req.code))
        .fetch_optional(&mut tx.executor())
        .await?;
        let Some(invite) = invite else {
            trace::rollback(tx).await;
            return Ok(Redeemed::Invalid);
        };
        let user: Option<(i64,)> =
            sqlx::query_as("SELECT user_id FROM bff_tokens WHERE token = $1 AND expires_at > NOW()")
                .bind(&req.token)
                .fetch_optional(&mut tx.executor())
                .await?;
        let Some((user_id,)) = user else {
            trace::rollback(tx).await;
            return Ok(Redeemed::LoggedOut);
        };
        // Opening the link again after joining (e.g. the page after signing up).
        let (already,): (bool,) = sqlx::query_as(
            "SELECT EXISTS (SELECT 1 FROM invite_redemptions WHERE invite_id = $1 AND user_id = $2)",
        )
        .bind(invite.id)
        .bind(user_id)
        .fetch_one(&mut tx.executor())
        .await?;
        if already {
            // Joined before, but an admin has since taken the role away: that stands.
            let (has_role,): (bool,) = sqlx::query_as(
                "SELECT EXISTS (SELECT 1 FROM user_roles WHERE user_id = $1 AND role_id = $2)",
            )
            .bind(user_id)
            .bind(invite.role_id)
            .fetch_one(&mut tx.executor())
            .await?;
            trace::rollback(tx).await;
            return Ok(if has_role { Redeemed::Joined(invite.role, false) } else { Redeemed::Removed });
        }
        if !invite.live {
            trace::rollback(tx).await;
            return Ok(Redeemed::Invalid);
        }
        let granted = sqlx::query("INSERT INTO user_roles (user_id, role_id) VALUES ($1, $2) ON CONFLICT DO NOTHING")
            .bind(user_id)
            .bind(invite.role_id)
            .execute(&mut tx.executor())
            .await?
            .rows_affected();
        sqlx::query("INSERT INTO invite_redemptions (invite_id, user_id) VALUES ($1, $2)")
            .bind(invite.id)
            .bind(user_id)
            .execute(&mut tx.executor())
            .await?;
        // Someone who already had the role doesn't use up a place.
        if granted > 0 {
            sqlx::query("UPDATE invites SET uses = uses + 1 WHERE id = $1")
                .bind(invite.id)
                .execute(&mut tx.executor())
                .await?;
        }
        trace::commit(tx).await?;
        tracing::info!(user_id, invite_id = invite.id, role = %invite.role, new_role = granted > 0, "invite redeemed");
        Ok(Redeemed::Joined(invite.role, true))
    }
    .await;
    match result {
        Ok(Redeemed::Joined(role, now)) => {
            // Opening it again (e.g. the page after signing up) isn't counted twice.
            if now {
                telemetry::token_operation("invite_redeem", "success");
            }
            Json(RoleResp { role }).into_response()
        }
        Ok(Redeemed::LoggedOut) => error(StatusCode::UNAUTHORIZED, "logged_out"),
        Ok(Redeemed::Removed) => error(StatusCode::FORBIDDEN, "role_removed"),
        Ok(Redeemed::Invalid) => {
            telemetry::token_operation("invite_redeem", "invalid");
            error(StatusCode::NOT_FOUND, "invalid_code")
        }
        Err(e) => db_error("redeeming an invite", e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_are_trimmed_and_limited() {
        assert_eq!(clean_note(Some("  for Sam ")), Ok(Some("for Sam".into())));
        assert_eq!(clean_note(Some("   ")), Ok(None));
        assert_eq!(clean_note(None), Ok(None));
        assert!(clean_note(Some(&"a".repeat(101))).is_err());
        assert!(clean_note(Some("a\nb")).is_err());
    }

    #[test]
    fn invite_limits() {
        assert!(limits_ok(7, None));
        assert!(limits_ok(1, Some(1)));
        assert!(limits_ok(365, Some(1000)));
        assert!(!limits_ok(0, None));
        assert!(!limits_ok(366, None));
        assert!(!limits_ok(7, Some(0)));
        assert!(!limits_ok(7, Some(1001)));
    }

    #[test]
    fn links_open_the_invite_page() {
        let url = reqwest::Url::parse(&link("a_b-c")).unwrap();
        assert_eq!(url.path(), "/invite");
        assert_eq!(url.query(), None);
        assert_eq!(url.fragment(), Some("a_b-c"));
    }
}
