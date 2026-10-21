//! Invite links, website side (`POST /api/invites/open`). An admin makes a link in the admin
//! panel; opening it (the `/invite` page) gives the visitor the link's role, such as
//! `arcane_user` for the dice test group. Auth stores the invites
//! (`services/auth/src/auth/invites.rs`).
//!
//! Someone logged out gets the code kept in their session; logging in or signing up
//! next (password, GitHub or Google) redeems it (`crate::auth::redeem_pending_invite`) and
//! brings them back to the invite page, which then says they're in.

pub use api_types::Opened;
use api_types::{ApiError, ErrorCode};
use serde_json::{json, Value};
use tower_sessions::Session;

/// Where to go after joining `role`.
fn after_join(role: &str) -> api_types::PagePath {
    match role {
        "arcane_user" => api_types::PagePath::Arcane,
        _ => api_types::PagePath::Home,
    }
}

/// The name people see for a role (`Opened::group_label`).
fn group_name(role: &str) -> String {
    match role {
        "arcane_user" => "the Arcane dice test group".into(),
        _ => format!("the “{role}” group"),
    }
}

const INVALID: &str =
    "This invite link has expired, been used up, or been turned off. Ask whoever sent it for a new one.";

enum Refused {
    Invalid,
    LoggedOut,
    /// Joined through it before; an admin has since taken the role away.
    Removed,
    Other(ApiError),
}

/// Calls auth; the reply's `role` on 2xx. By status rather than error code (unlike
/// `emails::call`) because `open` must tell "logged out" apart.
async fn call(path: &str, body: Value) -> Result<String, Refused> {
    let (status, reply) = crate::auth::auth_json(path, &body).await.map_err(|e| {
        tracing::error!(error = %e, path, "reaching auth failed");
        Refused::Other(crate::emails::offline())
    })?;
    match status {
        200..=299 => Ok(reply.get("role").and_then(Value::as_str).unwrap_or_default().to_string()),
        404 => Err(Refused::Invalid),
        401 => Err(Refused::LoggedOut),
        403 => Err(Refused::Removed),
        _ => {
            tracing::error!(path, status, error = ?reply.get("error"), "auth refused an invite request");
            Err(Refused::Other(crate::emails::offline()))
        }
    }
}

fn message(r: Refused) -> ApiError {
    match r {
        Refused::Invalid => ApiError::new(ErrorCode::NotFound, INVALID),
        Refused::LoggedOut => ApiError::logged_out(),
        Refused::Removed => ApiError::new(
            ErrorCode::Forbidden,
            "You joined through this link before, but that access has since been removed. Ask whoever sent it.",
        ),
        Refused::Other(e) => e,
    }
}

/// Opens an invite link: joins right away when the session is logged in, otherwise
/// keeps the invite in the session for after logging in or signing up.
#[tracing::instrument(name = "bff.open_invite", skip_all)]
pub async fn open(session: &Session, code: &str) -> Result<Opened, ApiError> {
    crate::trace::rejectable(async {
        if !crate::emails::code_ok(code) {
            return Err(ApiError::new(ErrorCode::Invalid, INVALID));
        }
        let token: Option<String> = session.get("opaque_token").await.ok().flatten();
        if let Some(token) = token {
            match call("/internal/invite/redeem", json!({ "code": code, "token": token })).await {
                Ok(role) => {
                    return Ok(Opened::Joined { group_label: group_name(&role), next: after_join(&role), role })
                }
                // The session ran out: treat them as logged out.
                Err(Refused::LoggedOut) => {}
                Err(r) => return Err(message(r)),
            }
        }
        let role = call("/internal/invite/check", json!({ "code": code })).await.map_err(message)?;
        session.insert(crate::auth::PENDING_INVITE_KEY, crate::auth::PendingInvite::new(code.to_string())).await.map_err(|e| {
            tracing::error!(error = %e, "keeping the invite in the session failed");
            ApiError::internal()
        })?;
        Ok(Opened::NeedsAccount { group_label: group_name(&role), role })
    })
    .await
}

#[cfg(test)]
mod tests {
    use crate::emails::code_ok;
    use super::*;

    #[test]
    fn codes_are_checked_before_asking_auth() {
        assert!(code_ok("AbC-_09"));
        assert!(code_ok(&"a".repeat(43)));
        assert!(!code_ok(""));
        assert!(!code_ok(&"a".repeat(65)));
        assert!(!code_ok("a b"));
        assert!(!code_ok("a&next=/evil"));
    }

    #[test]
    fn arcane_invites_have_a_friendly_name() {
        assert_eq!(group_name("arcane_user"), "the Arcane dice test group");
        assert_eq!(after_join("arcane_user"), api_types::PagePath::Arcane);
        assert_eq!(after_join("other"), api_types::PagePath::Home);
    }
}
