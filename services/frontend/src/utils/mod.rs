use std::fmt::Display;

#[derive(Debug, Eq, PartialEq)]
pub enum LogInStatus {
    LoggedOut,
    LoggedIn(String),
}

impl Display for LogInStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LogInStatus::LoggedOut => write!(f, "LoggedOut"),
            LogInStatus::LoggedIn(name) => write!(f, "LoggedIn({})", name),
        }
    }
}
