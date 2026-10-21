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

    /// Logged in, without the permission a route needs.
    pub fn forbidden() -> Self {
        Self::new(ErrorCode::Forbidden, "You don't have access to this.")
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

impl std::fmt::Display for Permission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// Who may use a route. Serialized for the `x-pages` list of the OpenAPI document: `"public"`,
/// `"guest_only"`, `"logged_in"` or `{"perm": "arcane"}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    Public,
    /// A page for the logged out (logging in, signing up): anyone may use it, and a page sends
    /// the logged in to `/` instead.
    GuestOnly,
    LoggedIn,
    Perm(KnownPermission),
}

// ---- Pages ----

/// A page of the site: its path, the file of the Astro build (`web/dist/`) it is, and who may
/// see it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Page {
    pub path: PagePath,
    pub file: &'static str,
    pub access: Access,
}

/// Declares the pages once: the [`PagePath`] enum (a string enum in the OpenAPI document, so
/// TypeScript gets a literal union) and the [`PAGES`] table the server routes and guards.
macro_rules! pages {
    ($($variant:ident => $path:literal, $file:literal, $access:expr;)*) => {
        /// A page's path. Deserializing anything else (another site, an API path, a page that
        /// doesn't exist) fails.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
        pub enum PagePath {
            $(#[serde(rename = $path)] $variant,)*
        }

        impl PagePath {
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $path,)*
                }
            }
        }

        /// The page at exactly `s`, as deserializing reads it: anything else is `Err`.
        impl std::str::FromStr for PagePath {
            type Err = ();

            fn from_str(s: &str) -> Result<Self, ()> {
                match s {
                    $($path => Ok(Self::$variant),)*
                    _ => Err(()),
                }
            }
        }

        /// Every page the server answers with Astro's HTML. A file in the build that is
        /// neither here nor in [`STATUS_PAGES`] stops the server from starting.
        pub const PAGES: &[Page] = &[$(Page { path: PagePath::$variant, file: $file, access: $access },)*];
    };
}

// One enum and one table, so a page's path, file and access can't disagree. (No doc comments
// on the variants: schemars would split the string enum into one schema per documented value.)
pages! {
    Home => "/", "index.html", Access::Public;
    Asshole => "/asshole", "asshole.html", Access::Public;
    AccountDeleted => "/account-deleted", "account-deleted.html", Access::Public;
    Login => "/login", "login.html", Access::GuestOnly;
    Register => "/register", "register.html", Access::GuestOnly;
    ForgotPassword => "/forgot-password", "forgot-password.html", Access::GuestOnly;
    // An emailed link's code alone authorizes these (it is in the fragment, never sent to the
    // server in the URL), so they work logged out or logged in as anyone.
    ResetPassword => "/reset-password", "reset-password.html", Access::Public;
    VerifyEmail => "/verify-email", "verify-email.html", Access::Public;
    DeleteAccount => "/delete-account", "delete-account.html", Access::Public;
    // Joins when logged in, else keeps the invite for after logging in.
    Invite => "/invite", "invite.html", Access::Public;
    Profile => "/profile", "profile.html", Access::LoggedIn;
    Ark => "/ark", "ark.html", Access::Perm(KnownPermission::Llama);
    Enshrouded => "/enshrouded", "enshrouded.html", Access::Perm(KnownPermission::Enshrouded);
    Admin => "/admin", "admin.html", Access::Perm(KnownPermission::ManagePermissions);
}

/// Pages without a path of their own, answered with a status: an unknown path (404), a page the
/// viewer may not see (403), and auth being down (503). They are spliced like every page.
pub const STATUS_PAGES: &[&str] = &["404.html", "403.html", "503.html"];

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

/// `POST /api/session/after-login`: the page the login page was opened for (its `?next=`), if
/// it is one of the site's.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AfterLogin {
    pub next: Option<PagePath>,
}

/// Where to go after logging in: the invite page a login redeemed (its code as the fragment),
/// else the page asked for, else `/`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NextPage {
    pub path: PagePath,
    /// Without the `#`.
    pub fragment: Option<String>,
}

impl NextPage {
    /// The address to navigate to.
    pub fn href(&self) -> String {
        match &self.fragment {
            Some(fragment) => format!("{}#{fragment}", self.path.as_str()),
            None => self.path.as_str().to_owned(),
        }
    }
}

/// Why an OAuth login came back to `/login?error=<code>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OAuthError {
    UnknownProvider,
    SessionFailed,
    StartFailed,
    MissingCode,
    MissingState,
    CsrfMismatch,
    EmailExists,
    ExchangeFailed,
}

impl OAuthError {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnknownProvider => "unknown_provider",
            Self::SessionFailed => "session_failed",
            Self::StartFailed => "start_failed",
            Self::MissingCode => "missing_code",
            Self::MissingState => "missing_state",
            Self::CsrfMismatch => "csrf_mismatch",
            Self::EmailExists => "email_exists",
            Self::ExchangeFailed => "exchange_failed",
        }
    }

    /// The login page that says so.
    pub fn login_page(self) -> String {
        format!("/login?error={}", self.as_str())
    }
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

// ---- Ark ----

/// What the Ark server can be told to do: the commands auth accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArkCommand {
    Start,
    Stop,
    Restart,
}

impl ArkCommand {
    /// The name auth takes.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
        }
    }
}

/// `POST /api/ark/command`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ArkCommandRequest {
    pub cmd: ArkCommand,
}

/// What an Ark command did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArkOutcome {
    Stopped,
    AlreadyStopped,
    FailedToStop,
    Started,
    AlreadyRunning,
    FailedToStart,
    Timeout,
    Restarting,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ArkResult {
    pub outcome: ArkOutcome,
}

/// `GET /api/ark/players`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ArkPlayers {
    /// `None` when the game server doesn't say (it is offline).
    pub count: Option<u32>,
}

// ---- Enshrouded ----

/// The Enshrouded server as the panel (`/enshrouded`) shows it: auth's `StatusResp`, passed on.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct EnshroudedStatus {
    pub state: EnshroudedState,
    /// `None` when the server doesn't answer the query.
    pub players: Option<EnshroudedPlayers>,
    /// The game's build number.
    pub version: Option<String>,
    /// Seconds the server process has been running.
    pub uptime_secs: Option<i64>,
    /// When Steam last published a build (unix seconds).
    pub latest_build_at: Option<i64>,
    /// Steam has a build the server doesn't run.
    pub update_pending: bool,
    /// Someone asked for an update that waits for the server to be empty.
    #[serde(default)]
    pub update_queued: bool,
    /// Whether Restart / Force update can be sent right now.
    pub can_control: bool,
    /// Latest commands, newest first.
    pub recent: Vec<EnshroudedAction>,
    /// When the next command may be sent (unix seconds), while that is in the future.
    pub next_command_at: Option<i64>,
}

/// Auth's names, unchanged.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
pub enum EnshroudedState {
    Online,
    /// The process runs but the world isn't loaded yet.
    Starting,
    Restarting,
    Updating,
    Stopping,
    Stopped,
    Crashed,
    /// Neither the server nor its control port answers.
    Unreachable,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct EnshroudedPlayers {
    pub online: u8,
    pub max: u8,
    /// How long each player has been connected, in seconds (the game sends no names).
    pub sessions: Vec<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct EnshroudedAction {
    /// `None` when the account was deleted.
    pub name: Option<String>,
    pub action: EnshroudedActionName,
    /// Unix seconds.
    pub at: i64,
}

/// A command as auth stored it: one this site sends, or a name it doesn't know (from an older
/// or newer auth), which still deserializes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum EnshroudedActionName {
    Known(EnshroudedCommand),
    Other(String),
}

/// Restart, or force an update: `update` waits until nobody is online, `update_now`
/// disconnects the players.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EnshroudedCommand {
    Restart,
    Update,
    UpdateNow,
}

impl EnshroudedCommand {
    /// The name auth takes.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Restart => "restart",
            Self::Update => "update",
            Self::UpdateNow => "update_now",
        }
    }
}

/// `POST /api/enshrouded/command`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EnshroudedCommandRequest {
    pub cmd: EnshroudedCommand,
}

// ---- Admin ----

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct AdminPermission {
    pub id: i32,
    /// Also the ones the site doesn't know yet.
    pub name: Permission,
}

/// A role as the roles listing has it, with its permissions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct AdminRole {
    pub id: i32,
    pub name: String,
    pub permissions: Vec<AdminPermission>,
}

/// A role without its permissions: `GET /api/admin/roles/all`, for the pickers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct AdminRoleRef {
    pub id: i32,
    pub name: String,
}

/// A role as the user listing has it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct AdminUserRole {
    pub id: i32,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct AdminUser {
    pub id: i64,
    pub username: String,
    pub email: Option<String>,
    pub roles: Vec<AdminUserRole>,
}

/// An invite link as the admin panel lists it (services/auth/src/auth/invites.rs).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct AdminInvite {
    pub id: i32,
    pub role_id: i32,
    pub role: String,
    pub note: Option<String>,
    /// Username of the admin who made it.
    pub created_by: Option<String>,
    /// RFC 3339 timestamps.
    pub created_at: String,
    pub expires_at: String,
    /// `None` = any number of people.
    pub max_uses: Option<i32>,
    pub uses: i32,
    pub revoked: bool,
    /// Whether it can still let someone new in.
    pub live: bool,
    /// Usernames of who joined through it, in order.
    #[serde(default)]
    pub joined: Vec<String>,
    /// The link itself: only right after making it.
    #[serde(default)]
    pub link: Option<String>,
}

/// One page of a listing, and how many there are in all.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
#[schemars(rename = "{T}Page")]
pub struct PagedResult<T> {
    pub items: Vec<T>,
    pub total: i64,
}

/// The query of `GET /api/admin/users` and `GET /api/admin/roles`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AdminPage {
    /// From 0.
    pub page: u32,
    pub limit: u32,
    /// Part of a name; empty or absent for all.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub search: String,
}

/// `POST /api/admin/invites`: a link that gives `role_id`, working for `days` days and for up
/// to `max_uses` people (`None`: any number).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct NewInvite {
    pub role_id: i32,
    pub days: i64,
    pub max_uses: Option<i32>,
    pub note: Option<String>,
}

/// `/api/admin/users/{user_id}/roles/{role_id}`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
pub struct UserRolePath {
    pub user_id: i64,
    pub role_id: i32,
}

/// `/api/admin/roles/{role_id}/permissions/{permission_id}`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
pub struct RolePermissionPath {
    pub role_id: i32,
    pub permission_id: i32,
}

/// `/api/admin/invites/{invite_id}`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
pub struct InvitePath {
    pub invite_id: i32,
}

// ---- Arcane ----

/// `GET /api/arcane/me`: always answered, so the extension can tell "logged out" from
/// "server down".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ArcaneMe {
    pub logged_in: bool,
    pub username: Option<String>,
    pub has_arcane: bool,
}

/// `POST /api/arcane/flag`: the latest roll read wrong, with the user's corrections (one per
/// die, left to right; `null` for one left empty).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FlagRequest {
    pub roll_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<Option<String>>,
}

/// A flagged roll was saved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Flagged {
    pub ok: bool,
}

// What ai_pipeline sends the camera page over `/ws/arcane` (services/ai_pipeline/src/serve.rs),
// and the rolls `/api/arcane/rolls` streams. The server passes them on without reading them
// into these types: the golden fixtures in contracts/arcane/ keep both sides to them.

/// A message from `/ws/arcane`, or an event's data on `/api/arcane/rolls` (always a roll).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ArcaneServerMsg {
    /// What was seen in one camera frame.
    Frame(Frame),
    /// The dice in view have settled into a new roll (or one re-read).
    Roll(Roll),
    /// A frame couldn't be read.
    Error { error: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Frame {
    pub detections: Vec<Detection>,
    /// How long reading the frame took.
    pub frame_ms: u64,
    /// The number of the camera frame this answers (1, 2, … per connection).
    pub frame_seq: u64,
}

/// One die found in a frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Detection {
    /// The box, as fractions of the frame.
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
    pub yolo_conf: f32,
    pub yolo_class: u32,
    pub dice_class: u32,
    pub dice_conf: f32,
    /// The face it shows ("0" is the d10 zero).
    pub value: String,
    /// When false, `value` isn't a reading.
    pub confident: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Roll {
    /// Kept when the same dice are read again.
    pub roll_id: String,
    /// Left to right.
    pub dice: Vec<RolledDie>,
    /// The sum (a d10's "0" counts 10); `None` unless every die was read.
    pub total: Option<u32>,
    pub complete: bool,
    /// Unix milliseconds.
    pub ts: u64,
    /// The model that read it.
    pub model: String,
    /// The camera frame it settled on.
    pub frame_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RolledDie {
    /// "1".."20" ("0" is the d10 zero); `None` when it couldn't be read confidently.
    pub value: Option<String>,
    pub conf: f32,
    /// As fractions of the frame: [x1, y1, x2, y2].
    #[serde(rename = "box")]
    pub bbox: [f32; 4],
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

    /// ai_pipeline's test checks it writes these same files (contracts/arcane/README.md).
    #[test]
    fn arcane_fixtures_are_the_documented_messages() {
        let read = |text: &str| -> ArcaneServerMsg {
            let msg = serde_json::from_str(text).unwrap();
            // Nothing in the fixture is lost on the way through the type.
            assert_eq!(serde_json::to_value(&msg).unwrap(), serde_json::from_str::<serde_json::Value>(text).unwrap());
            msg
        };
        let ArcaneServerMsg::Frame(frame) = read(include_str!("../../../../../contracts/arcane/frame.json")) else {
            panic!("not a frame")
        };
        assert_eq!((frame.frame_seq, frame.detections[0].value.as_str()), (6, "16"));
        let ArcaneServerMsg::Roll(roll) = read(include_str!("../../../../../contracts/arcane/roll.json")) else {
            panic!("not a roll")
        };
        assert_eq!((roll.total, roll.dice[0].value.as_deref()), (Some(16), Some("16")));
        let ArcaneServerMsg::Roll(roll) = read(include_str!("../../../../../contracts/arcane/roll_unread.json")) else {
            panic!("not a roll")
        };
        assert_eq!((roll.total, roll.dice[1].value.as_deref()), (None, None));
        let error = read(include_str!("../../../../../contracts/arcane/error.json"));
        assert!(matches!(error, ArcaneServerMsg::Error { .. }));
    }

    #[test]
    fn arcane_messages_with_unknown_fields_are_refused() {
        let mut roll: serde_json::Value = serde_json::from_str(include_str!("../../../../../contracts/arcane/roll.json")).unwrap();
        roll["dice"][0]["colour"] = json!("red");
        assert!(serde_json::from_value::<ArcaneServerMsg>(roll.clone()).is_err(), "a die's unknown field");
        roll["dice"][0].as_object_mut().unwrap().remove("colour");
        roll["mood"] = json!("lucky");
        assert!(serde_json::from_value::<ArcaneServerMsg>(roll).is_err(), "a roll's unknown field");
        assert!(serde_json::from_value::<ArcaneServerMsg>(json!({"type": "error", "error": "x", "more": 1})).is_err());
        assert!(serde_json::from_value::<ArcaneServerMsg>(json!({"type": "dance"})).is_err());
    }

    #[test]
    fn commands_are_the_names_auth_takes() {
        for c in [ArkCommand::Start, ArkCommand::Stop, ArkCommand::Restart] {
            assert_eq!(serde_json::to_value(c).unwrap(), json!(c.as_str()));
        }
        for c in [EnshroudedCommand::Restart, EnshroudedCommand::Update, EnshroudedCommand::UpdateNow] {
            assert_eq!(serde_json::to_value(c).unwrap(), json!(c.as_str()));
        }
        assert!(serde_json::from_value::<ArkCommandRequest>(json!({"cmd": "explode"})).is_err());
    }

    #[test]
    fn enshrouded_actions_keep_auths_names() {
        let actions = json!([
            {"name": null, "action": "update_now", "at": 1},
            {"name": "ann", "action": "reinstall", "at": 2},
        ]);
        let read: Vec<EnshroudedAction> = serde_json::from_value(actions.clone()).unwrap();
        assert_eq!(read[0].action, EnshroudedActionName::Known(EnshroudedCommand::UpdateNow));
        assert_eq!(read[1].action, EnshroudedActionName::Other("reinstall".into()));
        assert_eq!(serde_json::to_value(&read).unwrap(), actions);
    }

    #[test]
    fn a_flag_without_values_has_none() {
        let flag: FlagRequest = serde_json::from_value(json!({"roll_id": "r"})).unwrap();
        assert!(flag.values.is_empty());
    }

    #[test]
    fn page_paths_are_only_the_sites_pages() {
        for page in PAGES {
            assert_eq!(serde_json::to_value(page.path).unwrap(), json!(page.path.as_str()));
            assert_eq!(serde_json::from_value::<PagePath>(json!(page.path.as_str())).unwrap(), page.path);
            assert_eq!(page.path.as_str().parse::<PagePath>(), Ok(page.path));
        }
        for bad in [
            "//evil.com",
            "/\\evil.com",
            "https://milesstorm.com/",
            "/api/session",
            "/nope",
            "",
            "/asshole/",
            " /asshole",
            "/ASSHOLE",
            "/asshole?x=1",
            "/asshole#x",
            "%2Fasshole",
            "/./asshole",
            "javascript:alert(1)",
        ] {
            assert!(serde_json::from_value::<PagePath>(json!(bad)).is_err(), "{bad}");
            assert!(bad.parse::<PagePath>().is_err(), "{bad}");
        }
    }

    #[test]
    fn pages_are_listed_once_with_html_files() {
        let mut paths: Vec<_> = PAGES.iter().map(|p| p.path.as_str()).collect();
        let mut files: Vec<_> = PAGES.iter().map(|p| p.file).chain(STATUS_PAGES.iter().copied()).collect();
        paths.sort_unstable();
        paths.dedup();
        files.sort_unstable();
        files.dedup();
        assert_eq!(paths.len(), PAGES.len());
        assert_eq!(files.len(), PAGES.len() + STATUS_PAGES.len());
        assert!(files.iter().all(|f| f.ends_with(".html") && !f.contains('/')));
        // A path goes into `/login?next=` as it is: nothing in it needs escaping in a query.
        for page in PAGES {
            let path = page.path.as_str();
            assert!(path.starts_with('/') && !path.starts_with("//"), "{path}");
            assert!(path.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'/' || b == b'-'), "{path}");
        }
        assert_eq!(serde_json::to_value(Access::Perm(KnownPermission::Arcane)).unwrap(), json!({"perm": "arcane"}));
        assert_eq!(serde_json::to_value(Access::LoggedIn).unwrap(), json!("logged_in"));
        assert_eq!(serde_json::to_value(Access::GuestOnly).unwrap(), json!("guest_only"));
    }

    #[test]
    fn oauth_errors_are_their_query_values() {
        for e in [
            OAuthError::UnknownProvider,
            OAuthError::SessionFailed,
            OAuthError::StartFailed,
            OAuthError::MissingCode,
            OAuthError::MissingState,
            OAuthError::CsrfMismatch,
            OAuthError::EmailExists,
            OAuthError::ExchangeFailed,
        ] {
            assert_eq!(serde_json::to_value(e).unwrap(), json!(e.as_str()));
        }
        assert_eq!(OAuthError::EmailExists.login_page(), "/login?error=email_exists");
    }

    #[test]
    fn a_next_page_is_its_path_and_fragment() {
        assert_eq!(NextPage { path: PagePath::Invite, fragment: Some("abc".into()) }.href(), "/invite#abc");
        assert_eq!(NextPage { path: PagePath::Home, fragment: None }.href(), "/");
        let read: AfterLogin = serde_json::from_value(json!({"next": "/asshole"})).unwrap();
        assert_eq!(read.next, Some(PagePath::Asshole));
        assert!(serde_json::from_value::<AfterLogin>(json!({"next": "https://evil.com"})).is_err());
    }

    #[test]
    fn permission_names_are_auths() {
        for p in [KnownPermission::Llama, KnownPermission::Enshrouded, KnownPermission::Arcane, KnownPermission::ManagePermissions] {
            assert_eq!(serde_json::to_value(p).unwrap(), json!(p.as_str()));
        }
    }
}
