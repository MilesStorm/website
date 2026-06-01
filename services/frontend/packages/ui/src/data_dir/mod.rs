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
