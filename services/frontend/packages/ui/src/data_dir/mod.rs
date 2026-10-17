use serde::{Deserialize, Serialize};
use std::fmt::Display;

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub enum LoginStatus {
    LoggedOut,
    LoggedIn(String),
}

impl LoginStatus {
    pub fn username(&self) -> Option<&str> {
        match self {
            LoginStatus::LoggedIn(u) => Some(u),
            LoginStatus::LoggedOut => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Theme {
    Dark,
    Light,
    Dracula,
    Synthwave,
    Retro,
    Dim,
    Corporate,
    #[default]
    Preferred,
}

impl Display for Theme {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Theme::Dark => "dark",
                Theme::Light => "light",
                Theme::Dracula => "dracula",
                Theme::Synthwave => "synthwave",
                Theme::Retro => "retro",
                Theme::Dim => "dim",
                Theme::Corporate => "corporate",
                Theme::Preferred => "system",
            }
        )
    }
}

impl Theme {
    pub fn from_str_theme(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "dark" => Theme::Dark,
            "light" => Theme::Light,
            "dracula" => Theme::Dracula,
            "synthwave" => Theme::Synthwave,
            "retro" => Theme::Retro,
            "dim" => Theme::Dim,
            "corporate" => Theme::Corporate,
            _ => Theme::Preferred,
        }
    }

    pub fn all() -> &'static [Theme] {
        &[
            Theme::Dark,
            Theme::Light,
            Theme::Dracula,
            Theme::Synthwave,
            Theme::Retro,
            Theme::Dim,
            Theme::Corporate,
            Theme::Preferred,
        ]
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum CommandResult {
    Stopped,
    AlreadyStopped,
    FailedToStop,
    Started,
    AlreadyRunning,
    FailedToStart,
    Timeout,
    Restarting,
    NumPlayers(i32),
}

impl Display for CommandResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

/// The Enshrouded server as the panel (`/enshrouded`) shows it; auth's `StatusResp`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EnshroudedPlayers {
    pub online: u8,
    pub max: u8,
    /// How long each player has been connected, in seconds (the game sends no names).
    pub sessions: Vec<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EnshroudedAction {
    /// `None` when the account was deleted.
    pub name: Option<String>,
    /// `restart`, `update` or `update_now`.
    pub action: String,
    /// Unix seconds.
    pub at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientUser {
    pub id: i64,
    pub username: String,
    pub email: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AdminPermission {
    pub id: i32,
    pub name: String,
}

/// A role as returned in the roles listing (includes its permissions).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AdminRole {
    pub id: i32,
    pub name: String,
    pub permissions: Vec<AdminPermission>,
}

/// A role reference as returned in user listings (no permissions attached).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AdminUserRole {
    pub id: i32,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AdminUser {
    pub id: i64,
    pub username: String,
    pub email: Option<String>,
    pub roles: Vec<AdminUserRole>,
}

/// An invite link as the admin panel lists it (services/auth/src/auth/invites.rs).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PagedResult<T> {
    pub items: Vec<T>,
    pub total: i64,
}
