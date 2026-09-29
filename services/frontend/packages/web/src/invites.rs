//! Invite links, website side (server functions). An admin makes a link in the admin
//! panel; opening it (`views/invite.rs`) gives the visitor the link's role, such as
//! `arcane_user` for the dice test group. Auth stores the invites
//! (`services/auth/src/auth/invites.rs`).
//!
//! Someone logged out gets the code kept in their session; logging in or signing up
//! next (password, GitHub or Google) redeems it (`api::redeem_pending_invite`) and
//! brings them back to the invite page, which then says they're in.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

/// What opening an invite link did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Opened {
    /// The logged-in user now has `role` (or already had it through this link).
    Joined { role: String },
    /// Logged out: the invite is kept until they log in or sign up.
    NeedsAccount { role: String },
}

/// The name people see for a role.
pub fn group_name(role: &str) -> String {
    match role {
        "arcane_user" => "the Arcane dice test group".into(),
        _ => format!("the “{role}” group"),
    }
}

/// Where to go after joining `role`: (page, button text).
pub fn after_join(role: &str) -> (&'static str, &'static str) {
    match role {
        "arcane_user" => ("/arcane", "Open Arcane dice"),
        _ => ("/", "Go to the site"),
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod server {
    use super::*;
    use serde_json::Value;

    pub(super) const INVALID: &str =
        "This invite link has expired, been used up, or been turned off. Ask whoever sent it for a new one.";
    use crate::emails::server::OFFLINE;

    /// Whether `code` looks like a link code (URL-safe base64), before asking auth.
    /// It is later put into a page address, so nothing else may get through.
    pub(super) fn code_ok(code: &str) -> bool {
        (1..=64).contains(&code.len()) && code.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    }

    pub(super) enum Refused {
        Invalid,
        LoggedOut,
        /// Joined through it before; an admin has since taken the role away.
        Removed,
        Other(ServerFnError),
    }

    /// Calls auth; the reply's `role` on 2xx. By status rather than error code (unlike
    /// `emails::server::call`) because `open_invite` must tell "logged out" apart.
    pub(super) async fn call(path: &str, body: Value) -> Result<String, Refused> {
        let (status, reply) = api::auth_json(path, &body).await.map_err(|e| {
            tracing::error!(error = %e, path, "reaching auth failed");
            Refused::Other(ServerFnError::new(OFFLINE))
        })?;
        match status {
            200..=299 => Ok(reply.get("role").and_then(Value::as_str).unwrap_or_default().to_string()),
            404 => Err(Refused::Invalid),
            401 => Err(Refused::LoggedOut),
            403 => Err(Refused::Removed),
            _ => {
                tracing::error!(path, status, reply = %reply, "auth refused an invite request");
                Err(Refused::Other(ServerFnError::new(OFFLINE)))
            }
        }
    }

    pub(super) fn message(r: Refused) -> ServerFnError {
        match r {
            Refused::Invalid => ServerFnError::new(INVALID),
            Refused::LoggedOut => ServerFnError::new(crate::emails::server::LOGGED_OUT),
            Refused::Removed => ServerFnError::new(
                "You joined through this link before, but that access has since been removed. Ask whoever sent it.",
            ),
            Refused::Other(e) => e,
        }
    }
}

/// Opens an invite link: joins right away when logged in, otherwise keeps the
/// invite in the session for after logging in or signing up.
#[server(prefix = "/bff")]
pub async fn open_invite(code: String) -> Result<Opened, ServerFnError> {
    use server::{call, code_ok, message, Refused};

    let (session, _) = crate::emails::server::request()?;
    if !code_ok(&code) {
        return Err(ServerFnError::new(server::INVALID));
    }
    if let Ok(token) = crate::emails::server::token(&session).await {
        match call("/internal/invite/redeem", serde_json::json!({ "code": code, "token": token })).await {
            Ok(role) => return Ok(Opened::Joined { role }),
            // The session ran out: treat them as logged out.
            Err(Refused::LoggedOut) => {}
            Err(r) => return Err(message(r)),
        }
    }
    let role = call("/internal/invite/check", serde_json::json!({ "code": code })).await.map_err(message)?;
    let session = session.ok_or_else(|| ServerFnError::new("no session context"))?;
    session
        .insert(api::PENDING_INVITE_KEY, api::PendingInvite::new(code))
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;
    Ok(Opened::NeedsAccount { role })
}

/// Where to go right after logging in or signing up: back to the invite page if
/// that's where they came from, otherwise the start page.
#[server(prefix = "/bff")]
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
    use super::server::code_ok;
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
