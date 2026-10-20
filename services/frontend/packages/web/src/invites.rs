//! Invite links, website side (`POST /api/invites/open` and the server function over the same
//! function). An admin makes a link in the admin
//! panel; opening it (`views/invite.rs`) gives the visitor the link's role, such as
//! `arcane_user` for the dice test group. Auth stores the invites
//! (`services/auth/src/auth/invites.rs`).
//!
//! Someone logged out gets the code kept in their session; logging in or signing up
//! next (password, GitHub or Google) redeems it (`api::redeem_pending_invite`) and
//! brings them back to the invite page, which then says they're in.

use dioxus::prelude::*;

pub use api_types::Opened;

/// Where to go after joining `role`: (page, button text).
pub fn after_join(role: &str) -> (&'static str, &'static str) {
    match role {
        "arcane_user" => ("/arcane", "Open Arcane dice"),
        _ => ("/", "Go to the site"),
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod server {
    use super::*;
    use api_types::{ApiError, ErrorCode};
    use serde_json::{json, Value};
    use tower_sessions::Session;

    /// The name people see for a role (`Opened::group_label`).
    pub(super) fn group_name(role: &str) -> String {
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
    /// `emails::server::call`) because `open` must tell "logged out" apart.
    async fn call(path: &str, body: Value) -> Result<String, Refused> {
        let (status, reply) = api::auth_json(path, &body).await.map_err(|e| {
            tracing::error!(error = %e, path, "reaching auth failed");
            Refused::Other(crate::emails::server::offline())
        })?;
        match status {
            200..=299 => Ok(reply.get("role").and_then(Value::as_str).unwrap_or_default().to_string()),
            404 => Err(Refused::Invalid),
            401 => Err(Refused::LoggedOut),
            403 => Err(Refused::Removed),
            _ => {
                tracing::error!(path, status, error = ?reply.get("error"), "auth refused an invite request");
                Err(Refused::Other(crate::emails::server::offline()))
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
        api::trace::rejectable(async {
            if !crate::emails::server::code_ok(code) {
                return Err(ApiError::new(ErrorCode::Invalid, INVALID));
            }
            let token: Option<String> = session.get("opaque_token").await.ok().flatten();
            if let Some(token) = token {
                match call("/internal/invite/redeem", json!({ "code": code, "token": token })).await {
                    Ok(role) => return Ok(Opened::Joined { group_label: group_name(&role), role }),
                    // The session ran out: treat them as logged out.
                    Err(Refused::LoggedOut) => {}
                    Err(r) => return Err(message(r)),
                }
            }
            let role = call("/internal/invite/check", json!({ "code": code })).await.map_err(message)?;
            session.insert(api::PENDING_INVITE_KEY, api::PendingInvite::new(code.to_string())).await.map_err(|e| {
                tracing::error!(error = %e, "keeping the invite in the session failed");
                ApiError::internal()
            })?;
            Ok(Opened::NeedsAccount { group_label: group_name(&role), role })
        })
        .await
    }
}

/// Opens an invite link: joins right away when logged in, otherwise keeps the
/// invite in the session for after logging in or signing up.
#[server(prefix = "/bff")]
pub async fn open_invite(code: String) -> Result<Opened, ServerFnError> {
    let (session, _) = crate::emails::server::request()?;
    let session = session.ok_or_else(|| ServerFnError::new("no session context"))?;
    server::open(&session, &code).await.map_err(api::server_error)
}

/// Where to go right after logging in or signing up: back to the invite page if
/// that's where they came from, otherwise the start page.
#[server(prefix = "/bff")]
#[tracing::instrument(name = "bff.after_login_path", skip_all, err)]
pub async fn after_login_path() -> Result<String, ServerFnError> {
    let (session, _) = crate::emails::server::request()?;
    let page: Option<String> = match session {
        Some(s) => s.remove(api::INVITE_RETURN_KEY).await.ok().flatten(),
        None => None,
    };
    Ok(page.unwrap_or_else(|| "/".into()))
}

#[cfg(test)]
mod tests {
    use super::server::group_name;
    use crate::emails::server::code_ok;
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
        assert_eq!(after_join("arcane_user").0, "/arcane");
        assert_eq!(after_join("other").0, "/");
    }
}
