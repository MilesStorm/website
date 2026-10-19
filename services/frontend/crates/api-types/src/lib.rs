//! What the JSON API under `/api/*` takes and returns (the browser's contract). The server's
//! handlers return these, so the OpenAPI document (`server openapi`) and the TypeScript types
//! generated from it (`web/src/api/`) follow them.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ---- Errors ----

/// What kind of failure an [`ApiError`] is: for the code paths that branch on it. The rest is
/// in the message. Clients send the user to the login page on `logged_out` only, never on a
/// 401 alone: a wrong password is a 401 too (`invalid`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    LoggedOut,
    Forbidden,
    Invalid,
    Conflict,
    NotFound,
    Gone,
    RateLimited,
    Unavailable,
    Upstream,
    Internal,
}

impl ErrorCode {
    /// The HTTP status an error of this kind is answered with, unless it sets its own.
    pub fn status(self) -> u16 {
        match self {
            Self::LoggedOut => 401,
            Self::Forbidden => 403,
            Self::Invalid => 400,
            Self::Conflict => 409,
            Self::NotFound => 404,
            Self::Gone => 410,
            Self::RateLimited => 429,
            Self::Unavailable => 503,
            Self::Upstream => 502,
            Self::Internal => 500,
        }
    }
}

/// Every error the API answers: `{"error": "logged_out", "message": "..."}` with a 4xx or 5xx
/// status. `message` is for the user. A 5xx message never carries the internal error, which
/// goes only to the log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ApiError {
    pub error: ErrorCode,
    pub message: String,
    /// The status when it isn't the code's own: auth's 401/403 for wrong credentials, 413
    /// for an upload over the limit. Not part of the body.
    #[serde(skip)]
    status: Option<u16>,
}

impl ApiError {
    pub fn new(error: ErrorCode, message: impl Into<String>) -> Self {
        Self { error, message: message.into(), status: None }
    }

    /// The same error, answered with `status` instead of its code's.
    pub fn with_status(mut self, status: u16) -> Self {
        self.status = Some(status);
        self
    }

    pub fn status(&self) -> u16 {
        self.status.unwrap_or(self.error.status())
    }

    pub fn logged_out() -> Self {
        Self::new(ErrorCode::LoggedOut, "You've been logged out. Log in again.")
    }

    /// Auth can't be asked who is logged in.
    pub fn login_unavailable() -> Self {
        Self::new(ErrorCode::Unavailable, "Your login can't be checked right now. Try again later.")
    }

    /// A failure of ours, logged where it happened.
    pub fn internal() -> Self {
        Self::new(ErrorCode::Internal, "Something went wrong. Try again later.")
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {:?}: {}", self.status(), self.error, self.message)
    }
}

impl std::error::Error for ApiError {}

// ---- Permissions ----

/// The permissions the site knows. Every permission check uses this closed set, so TypeScript
/// gets a literal union.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum KnownPermission {
    Llama,
    Enshrouded,
    Arcane,
    ManagePermissions,
}

impl KnownPermission {
    /// The name auth uses.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Llama => "llama",
            Self::Enshrouded => "enshrouded",
            Self::Arcane => "arcane",
            Self::ManagePermissions => "manage_permissions",
        }
    }
}

/// A permission name from auth, which may have permissions the site doesn't know yet: those
/// deserialize as `Other` instead of failing. Only for showing names; checks use
/// [`KnownPermission`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Permission {
    Known(KnownPermission),
    Other(String),
}

impl Permission {
    pub fn known(&self) -> Option<KnownPermission> {
        match self {
            Self::Known(p) => Some(*p),
            Self::Other(_) => None,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Self::Known(p) => p.as_str(),
            Self::Other(name) => name,
        }
    }
}

/// Who may use a route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Public,
    LoggedIn,
    Perm(KnownPermission),
}

// ---- Session ----

/// Who is looking at the page: what `GET /api/session` answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Bootstrap {
    /// `None` when logged out.
    pub viewer: Option<Viewer>,
}

/// The logged-in user, as the page shows them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Viewer {
    pub username: String,
    /// The display name, or the username when there is none.
    pub shown_name: String,
    pub picture_url: Option<String>,
    /// Only the ones the site knows; empty when auth can't be reached.
    pub permissions: Vec<KnownPermission>,
}

/// `POST /api/session/login`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Login {
    pub username: String,
    pub password: String,
}

/// `POST /api/session/register`: a new account, logged in at once.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Registration {
    pub username: String,
    pub email: String,
    pub password: String,
}

/// A login or registration that succeeded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LoggedIn {
    pub username: String,
}

// ---- Account ----

/// The logged-in user's account settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AccountInfo {
    /// Login name; can't be changed.
    pub username: String,
    pub display_name: Option<String>,
    /// The display name, or the username when there is none.
    pub shown_name: String,
    /// Version of the profile picture (the `?v=` of `picture_url`), `None` when there is none.
    pub picture_version: Option<i64>,
    pub picture_url: Option<String>,
    /// False when the picture store isn't reachable or configured.
    pub pictures_available: bool,
    /// `None` for accounts without one (GitHub logins).
    pub email: Option<String>,
    pub email_verified: bool,
}

/// `PUT /api/account/display-name`. Blank clears it.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DisplayName {
    pub name: String,
}

/// A profile picture just stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PictureVersion {
    pub version: i64,
}

/// Where `username`'s picture of `version` is. A new picture is a new URL, so it can be cached
/// for good; the username keeps two accounts used in one browser apart in its cache (the
/// server answers with the session's own picture and ignores both).
pub fn picture_url(username: &str, version: i64) -> String {
    let user: String = username
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => char::from(b).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect();
    format!("/api/account/picture?u={user}&v={version}")
}

// ---- Account emails ----
//
// One-time codes from emailed links travel only in request bodies, never in a URL, so they
// stay out of access logs and traces.

/// A one-time code from an emailed link or an invite link.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Code {
    pub code: String,
}

/// Where an email was sent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EmailSent {
    pub email: String,
}

/// An address confirmed with an emailed code: whose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Confirmed {
    pub username: String,
}

/// `POST /api/password/forgot`: a username or an email address.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ForgotPassword {
    pub login: String,
}

/// `POST /api/password/reset`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PasswordReset {
    pub code: String,
    pub password: String,
}

/// What a delete-account link is for, before it is used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Deletion {
    pub username: String,
}

/// `POST /api/account/deletion/direct`: `confirm` must be the username.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DirectDeletion {
    pub confirm: String,
}

/// After resetting a password or deleting an account: whether this browser's session was that
/// account's and has been logged out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Done {
    pub username: String,
    pub logged_out: bool,
}

// ---- Invites ----

/// What opening an invite link did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Opened {
    /// The logged-in user now has `role` (or already had it through this link).
    Joined { role: String, group_label: String },
    /// Logged out: the invite is kept until they log in or sign up.
    NeedsAccount { role: String, group_label: String },
}

// ---- Sharing roll pictures ----

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SharingState {
    /// False when the training-data store isn't configured on this server.
    pub available: bool,
    pub share: bool,
}

/// `PUT /api/account/sharing`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Sharing {
    pub share: bool,
}

/// How many shared rolls were deleted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Deleted {
    pub deleted: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn errors_are_the_envelope_and_keep_their_status_out_of_it() {
        let e = ApiError::new(ErrorCode::Invalid, "Incorrect username or password.").with_status(401);
        assert_eq!(e.status(), 401);
        assert_eq!(
            serde_json::to_value(&e).unwrap(),
            json!({"error": "invalid", "message": "Incorrect username or password."})
        );
        assert_eq!(ApiError::logged_out().status(), 401);
        assert_eq!(ApiError::internal().status(), 500);
    }

    #[test]
    fn unknown_permissions_deserialize_and_are_never_known() {
        let ps: Vec<Permission> = serde_json::from_value(json!(["arcane", "manage_permissions", "brand_new"])).unwrap();
        assert_eq!(
            ps,
            [
                Permission::Known(KnownPermission::Arcane),
                Permission::Known(KnownPermission::ManagePermissions),
                Permission::Other("brand_new".into()),
            ]
        );
        assert_eq!(ps.iter().filter_map(Permission::known).count(), 2);
        assert_eq!(ps.iter().map(Permission::name).collect::<Vec<_>>(), ["arcane", "manage_permissions", "brand_new"]);
    }

    #[test]
    fn picture_urls_differ_by_user_and_are_query_safe() {
        assert_eq!(picture_url("miles", 7), "/api/account/picture?u=miles&v=7");
        assert_eq!(picture_url("a b&v=1", 7), "/api/account/picture?u=a%20b%26v%3D1&v=7");
        assert_ne!(picture_url("ann", 7), picture_url("bob", 7));
    }

    #[test]
    fn opened_invites_are_tagged_by_kind() {
        let joined = Opened::Joined { role: "arcane_user".into(), group_label: "the group".into() };
        assert_eq!(
            serde_json::to_value(joined).unwrap(),
            json!({"kind": "joined", "role": "arcane_user", "group_label": "the group"})
        );
        let kept = Opened::NeedsAccount { role: "r".into(), group_label: "g".into() };
        assert_eq!(serde_json::to_value(kept).unwrap()["kind"], "needs_account");
    }

    #[test]
    fn permission_names_are_auths() {
        for p in [KnownPermission::Llama, KnownPermission::Enshrouded, KnownPermission::Arcane, KnownPermission::ManagePermissions] {
            assert_eq!(serde_json::to_value(p).unwrap(), json!(p.as_str()));
        }
    }
}
