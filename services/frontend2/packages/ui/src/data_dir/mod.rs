#[derive(Debug, Clone, Eq, PartialEq)]
pub enum LoginStatus {
    LoggedOut,
    LoggedIn(String),
}
