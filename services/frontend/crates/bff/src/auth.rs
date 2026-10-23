//! The BFF's calls to auth: the phantom token pattern (the session holds an opaque token, auth
//! knows whose it is), logins, the session's viewer, invites kept for after logging in.

use api_types::{ApiError, ErrorCode, Permission};
use serde::{Deserialize, Serialize};

use crate::trace;

fn password_login_error(status: u16, body: &str) -> ApiError {
    // Auth also uses 401 for a bad BFF service secret. That is an outage, not
    // a user's mistyped password, so only its credential rejection is a 4xx.
    if matches!(status, 401 | 403) && body == "Invalid credentials" {
        ApiError::new(ErrorCode::Invalid, "Incorrect username or password.").with_status(status)
    } else {
        login_unavailable()
    }
}

// ---- Session helpers ----

pub(crate) mod session {
    pub fn auth_url() -> String {
        std::env::var("AUTH_SERVICE_URL")
            .unwrap_or_else(|_| "http://localhost:7070".to_string())
    }

    pub fn service_secret() -> String {
        std::env::var("BFF_SERVICE_SECRET").expect("BFF_SERVICE_SECRET must be set")
    }
}

/// Gives the session a new ID on logging in, keeping what's in it (e.g. a pending
/// invite). Otherwise someone who planted a known session ID in a browser (session
/// fixation) would be logged in as whoever logs in there next. Call before storing
/// the login; on failure, don't log in.
#[tracing::instrument(name = "bff.fresh_session_id", skip_all, err)]
pub async fn fresh_session_id(sess: &tower_sessions::Session) -> Result<(), String> {
    sess.cycle_id().await.map_err(|e| {
        tracing::error!(error = %e, "giving the session a new ID on login failed");
        e.to_string()
    })
}

/// The longest a call to auth may take. Auth answers in milliseconds, except the ark
/// commands, which wait for the game server's container.
const AUTH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// The BFF's client for calls to auth: every request gets a CLIENT span and carries
/// the trace context (see `trace::client`).
pub(crate) fn http_client() -> &'static trace::TracedClient {
    static CLIENT: std::sync::OnceLock<trace::TracedClient> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        trace::client(trace::Peer { service: "auth", system: None }, AUTH_TIMEOUT, |b| b)
            .expect("failed to build the HTTP client")
    })
}

/// Builds the client for calls to auth before the server takes requests. Building it loads the
/// system CA certificates (~30 ms of CPU), which otherwise lands in the first request's trace.
pub fn init_http_client() {
    http_client();
}

// ---- OAuth, for main.rs's handlers ----

/// Ask the auth service to begin an OAuth flow. Returns `(auth_url, csrf_state)`.
#[tracing::instrument(name = "bff.start_oauth", skip_all, err, fields(provider = %provider))]
pub async fn start_oauth(provider: &str) -> Result<(String, String), String> {
    use session::{auth_url, service_secret};

    #[derive(Serialize)]
    struct Req<'a> {
        provider: &'a str,
    }
    #[derive(Deserialize)]
    struct Resp {
        auth_url: String,
        state: String,
    }

    let resp = http_client()
        .post(format!("{}/internal/oauth/start", auth_url()))
        .header("x-service-token", service_secret())
        .json(&Req { provider })
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        return Err(format!("oauth start failed: {}", resp.status()));
    }

    let data: Resp = resp.json().await.map_err(|e| e.to_string())?;
    Ok((data.auth_url, data.state))
}

/// Exchange an OAuth authorization code for a BFF opaque token, username and auth's user id.
#[tracing::instrument(name = "bff.exchange_oauth_code", skip_all, fields(provider = %provider))]
pub async fn exchange_oauth_code(
    provider: &str,
    code: &str,
) -> Result<(String, String, Option<i64>), String> {
    use session::{auth_url, service_secret};

    #[derive(Serialize)]
    struct Req<'a> {
        provider: &'a str,
        code: &'a str,
    }
    #[derive(Deserialize)]
    struct Resp {
        token: String,
        username: String,
        #[serde(default)]
        user_id: Option<i64>,
    }

    let resp = http_client()
        .post(format!("{}/internal/oauth/exchange", auth_url()))
        .header("x-service-token", service_secret())
        .json(&Req { provider, code })
        .send()
        .await
        .map_err(|e| {
            trace::failed("auth_unreachable");
            e.to_string()
        })?;

    if !resp.status().is_success() {
        // A 4xx is auth's answer (the provider refused the code, the email is taken).
        if !resp.status().is_client_error() {
            trace::failed("auth_status");
        }
        let body = resp.text().await.unwrap_or_default();
        metrics::counter!("bff_login_attempts_total", "method" => provider.to_string(), "status" => "failure").increment(1);
        return Err(body);
    }

    let data: Resp = resp.json().await.map_err(|e| {
        trace::failed("auth_reply");
        e.to_string()
    })?;
    metrics::counter!("bff_login_attempts_total", "method" => provider.to_string(), "status" => "success").increment(1);
    Ok((data.token, data.username, data.user_id))
}

fn login_unavailable() -> ApiError {
    ApiError::new(ErrorCode::Upstream, "Login is unavailable right now. Try again later.")
}

fn registration_failed() -> ApiError {
    ApiError::new(ErrorCode::Upstream, "Registration failed")
}

/// The session key of the logged-in account's id in auth (absent from sessions made before it
/// was stored, and when auth didn't send it).
pub(crate) const USER_ID_KEY: &str = "user_id";

/// Stores a login (its token, username and auth's user id) in the session.
pub(crate) async fn store_identity(
    sess: &tower_sessions::Session,
    token: &str,
    username: &str,
    user_id: Option<i64>,
) -> Result<(), tower_sessions::session::Error> {
    sess.insert("opaque_token", token).await?;
    sess.insert("username", username).await?;
    if let Some(id) = user_id {
        sess.insert(USER_ID_KEY, id).await?;
    }
    Ok(())
}

/// Stores a login in the session under a new session ID, then redeems a pending invite.
async fn store_login(sess: &tower_sessions::Session, token: &str, username: &str, user_id: Option<i64>) -> Result<(), ApiError> {
    fresh_session_id(sess).await.map_err(|_| ApiError::internal())?;
    store_identity(sess, token, username, user_id).await.map_err(|e| {
        tracing::error!(error = %e, "storing the login in the session failed");
        ApiError::internal()
    })?;
    redeem_pending_invite(sess, token).await;
    Ok(())
}

/// Log in with username + password. The username logged in as.
#[tracing::instrument(name = "bff.login_password", skip_all)]
pub async fn log_in(sess: &tower_sessions::Session, username: String, password: String) -> Result<String, ApiError> {
    trace::rejectable(async move {
        use session::{auth_url, service_secret};

        #[derive(Serialize)]
        struct Req {
            username: String,
            password: String,
        }
        #[derive(Deserialize)]
        struct Resp {
            token: String,
            username: String,
            #[serde(default)]
            user_id: Option<i64>,
        }

        let resp = http_client()
            .post(format!("{}/internal/token/exchange", auth_url()))
            .with_extension(trace::WRONG_TOKEN_OK)
            .header("x-service-token", service_secret())
            .json(&Req { username, password })
            .send()
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "password login failed: reaching auth failed");
                metrics::counter!("bff_login_attempts_total", "method" => "password", "status" => "error").increment(1);
                login_unavailable()
            })?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            let error = password_login_error(status, &body);
            let expected = (400..500).contains(&error.status());
            if expected {
                tracing::warn!("password login failed: invalid credentials");
            } else {
                tracing::error!(status, "password login failed: auth unavailable");
            }
            metrics::counter!("bff_login_attempts_total", "method" => "password", "status" => if expected { "failure" } else { "error" }).increment(1);
            return Err(error);
        }

        let data: Resp = resp.json().await.map_err(|e| {
            tracing::error!(error = %e, "password login failed: auth's reply is unreadable");
            login_unavailable()
        })?;
        store_login(sess, &data.token, &data.username, data.user_id).await?;

        tracing::info!(username = %data.username, "password login succeeded");
        metrics::counter!("bff_login_attempts_total", "method" => "password", "status" => "success").increment(1);
        Ok(data.username)
    })
    .await
}

/// Clear the session.
#[tracing::instrument(name = "bff.logout", skip_all, err)]
pub async fn log_out(sess: &tower_sessions::Session) -> Result<(), ApiError> {
    let username: Option<String> = sess.get("username").await.ok().flatten();
    sess.flush().await.map_err(|e| {
        tracing::error!(error = %e, "logging out failed: the session can't be cleared");
        ApiError::internal()
    })?;
    tracing::info!(username = ?username, "user logged out");
    Ok(())
}

/// Register a new account and log in as it. The username logged in as.
#[tracing::instrument(name = "bff.register_password", skip_all)]
pub async fn register(
    sess: &tower_sessions::Session,
    username: String,
    email: String,
    password: String,
) -> Result<String, ApiError> {
    trace::rejectable(async move {
        use session::{auth_url, service_secret};

        #[derive(Serialize)]
        struct Req {
            username: String,
            email: String,
            password: String,
        }
        #[derive(Deserialize)]
        struct Resp {
            token: String,
            username: String,
            #[serde(default)]
            user_id: Option<i64>,
        }

        let resp = http_client()
            .post(format!("{}/internal/register", auth_url()))
            .header("x-service-token", service_secret())
            .json(&Req { username, email, password })
            .send()
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "registration failed: reaching auth failed");
                registration_failed()
            })?;

        if resp.status() == reqwest::StatusCode::CONFLICT {
            let body = resp.text().await.unwrap_or_default();
            tracing::warn!(reason = %body, "registration conflict");
            metrics::counter!("bff_register_attempts_total", "status" => "conflict").increment(1);
            return Err(ApiError::new(ErrorCode::Conflict, body));
        }

        if resp.status() == reqwest::StatusCode::BAD_REQUEST {
            // Auth says what's wrong ("Invalid email address", password too short);
            // anything else (e.g. a malformed request) gets a generic message.
            let body = resp.text().await.unwrap_or_default();
            metrics::counter!("bff_register_attempts_total", "status" => "invalid").increment(1);
            let known = body == "Invalid email address" || body.starts_with("Password must be at least");
            return Err(ApiError::new(ErrorCode::Invalid, if known { body.as_str() } else { "Registration failed" }));
        }

        if !resp.status().is_success() {
            tracing::error!(status = %resp.status(), "registration failed");
            metrics::counter!("bff_register_attempts_total", "status" => "error").increment(1);
            return Err(registration_failed());
        }

        let data: Resp = resp.json().await.map_err(|e| {
            tracing::error!(error = %e, "registration failed: auth's reply is unreadable");
            registration_failed()
        })?;
        store_login(sess, &data.token, &data.username, data.user_id).await?;

        tracing::info!(username = %data.username, "registration succeeded");
        metrics::counter!("bff_register_attempts_total", "status" => "success").increment(1);
        Ok(data.username)
    })
    .await
}

/// Who a request comes from, by its session.
#[derive(Debug, Clone)]
pub enum Viewer {
    Anonymous,
    User { username: String, token: String, permissions: Vec<Permission> },
    /// Logged in, but auth can't say with which permissions (it is unreachable or answered
    /// something else): shown as logged in, with none.
    Degraded { username: String },
}

/// The session's [`Viewer`], with one call to auth when it is logged in. A session whose
/// token auth no longer knows is cleared. Err when the session can't be read.
pub async fn viewer(sess: &tower_sessions::Session) -> Result<Viewer, String> {
    let username: Option<String> = sess.get("username").await.map_err(|e| e.to_string())?;
    let Some(username) = username else {
        return Ok(Viewer::Anonymous);
    };
    let token: Option<String> = sess.get("opaque_token").await.ok().flatten();
    Ok(match login_viewer(&username, token).await {
        Some(viewer) => viewer,
        None => token_gone(sess, &username).await,
    })
}

/// Who a session logged in as `username` with `token` is, by one call to auth: `None` when the
/// login is over (no token, or auth no longer knows it), so the session must be cleared. Shared
/// by [`viewer`] and the long-lived connections' re-check, which reads a stored session.
pub async fn login_viewer(username: &str, token: Option<String>) -> Option<Viewer> {
    use session::{auth_url, service_secret};

    // Auth drops every token of an account when its password is reset or it is
    // deleted: then this session is over too. Other failures leave it alone.
    let token = token?;
    let username = username.to_string();
    #[derive(Serialize)]
    struct Req<'a> {
        token: &'a str,
    }
    #[derive(Deserialize)]
    struct Resp {
        permissions: Vec<Permission>,
    }
    let resp = http_client()
        .post(format!("{}/internal/token/introspect", auth_url()))
        .with_extension(trace::WRONG_TOKEN_OK)
        .header("x-service-token", service_secret())
        .json(&Req { token: &token })
        .send()
        .await;
    Some(match resp {
        Ok(r) if r.status().is_success() => match r.json::<Resp>().await {
            Ok(data) => Viewer::User { username, token, permissions: data.permissions },
            Err(e) => {
                tracing::warn!(error = %e, "auth's reply to the session's token check is unreadable");
                Viewer::Degraded { username }
            }
        },
        // Only auth's "unknown token" reply; a 401 for a wrong service secret
        // or any other failure must not log everyone out.
        Ok(r) if r.status() == reqwest::StatusCode::UNAUTHORIZED => {
            if r.text().await.is_ok_and(|b| b == "Invalid or expired token") {
                return None;
            }
            Viewer::Degraded { username }
        }
        Ok(r) => {
            tracing::warn!(status = %r.status(), "checking the session's token failed");
            Viewer::Degraded { username }
        }
        Err(e) => {
            tracing::warn!(error = %e, "checking the session's token failed");
            Viewer::Degraded { username }
        }
    })
}

async fn token_gone(sess: &tower_sessions::Session, username: &str) -> Viewer {
    tracing::info!(username = %username, "session's token no longer valid; logging out");
    let _ = sess.flush().await;
    Viewer::Anonymous
}

/// Whether the token's user shares roll pictures for training the dice reader
/// (stored by auth; see `dataset_consent` there). Err when the user lacks
/// `arcane` or auth can't be reached.
#[tracing::instrument(name = "bff.dataset_consent", skip_all, err)]
pub async fn dataset_consent(token: &str) -> Result<bool, String> {
    use session::{auth_url, service_secret};

    #[derive(Serialize)]
    struct Req<'a> {
        token: &'a str,
    }
    #[derive(Deserialize)]
    struct Resp {
        share: bool,
    }
    let resp = http_client()
        .post(format!("{}/internal/dataset/consent", auth_url()))
        .header("x-service-token", service_secret())
        .json(&Req { token })
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("auth returned {}", resp.status()));
    }
    resp.json::<Resp>().await.map(|r| r.share).map_err(|e| e.to_string())
}

/// Record the token's user's choice to share roll pictures, with the version of
/// the consent wording they saw.
#[tracing::instrument(name = "bff.set_dataset_consent", skip_all, err, fields(share))]
pub async fn set_dataset_consent(token: &str, share: bool, consent_version: &str) -> Result<(), String> {
    use session::{auth_url, service_secret};

    #[derive(Serialize)]
    struct Req<'a> {
        token: &'a str,
        share: bool,
        consent_version: &'a str,
    }
    let resp = http_client()
        .post(format!("{}/internal/dataset/consent/set", auth_url()))
        .header("x-service-token", service_secret())
        .json(&Req { token, share, consent_version })
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("auth returned {}", resp.status()));
    }
    Ok(())
}

/// The token's own account, as the profile page shows it.
#[derive(Debug, Clone, Deserialize)]
pub struct AccountProfile {
    /// Permanent id; never shown, used to key the user's profile picture.
    pub user_id: i64,
    pub username: String,
    pub display_name: Option<String>,
    /// `None` for accounts without one (GitHub logins).
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub email_verified: bool,
}

/// POST `body` to auth's `path`: the status and the JSON reply (`Null` when the
/// reply isn't JSON). Auth's email endpoints answer errors as `{"error": code}`.
pub async fn auth_json<T: Serialize>(path: &str, body: &T) -> Result<(u16, serde_json::Value), String> {
    auth_send(http_client().post(format!("{}{path}", session::auth_url())).json(body)).await
}

/// Sends `req` (to auth) with the service token: the status and the JSON reply (`Null` when
/// the reply isn't JSON).
pub(crate) async fn auth_send(req: reqwest_middleware::RequestBuilder) -> Result<(u16, serde_json::Value), String> {
    let resp = req.header("x-service-token", session::service_secret()).send().await.map_err(|e| e.to_string())?;
    let status = resp.status().as_u16();
    let json = resp.json().await.unwrap_or(serde_json::Value::Null);
    Ok((status, json))
}

// ---- Invite links ----
//
// Someone logged out who opens an invite link (`/invite#…`) gets the code kept
// in their session; logging in or signing up by any means then redeems it, and the
// next page is the invite page again, which says they're in.

/// Session key: an invite opened while logged out, redeemed on logging in.
pub const PENDING_INVITE_KEY: &str = "pending_invite";
/// Session key: the code of the invite a login just redeemed; its page (`/invite#<code>`) opens
/// right after logging in.
pub const INVITE_RETURN_KEY: &str = "invite_return";

/// How long an invite opened while logged out waits for a log in. Short, so on a
/// shared computer the next person to log in doesn't get it.
const PENDING_INVITE_SECS: u64 = 3600;

/// An invite opened while logged out, kept in the session.
#[derive(Serialize, Deserialize)]
pub struct PendingInvite {
    code: String,
    /// Unix seconds.
    opened_at: u64,
}

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl PendingInvite {
    pub fn new(code: String) -> Self {
        Self { code, opened_at: unix_now() }
    }

    /// The code, if it was opened recently enough.
    fn fresh_code(self, now: u64) -> Option<String> {
        (now.saturating_sub(self.opened_at) <= PENDING_INVITE_SECS).then_some(self.code)
    }
}

/// Redeems the session's pending invite for the account just logged in as `token`, and keeps
/// its code for the invite page to show next (it also says if the link no longer works).
/// Only this login's: an earlier login's code, never taken, is dropped first.
#[tracing::instrument(name = "bff.redeem_pending_invite", skip_all)]
pub async fn redeem_pending_invite(sess: &tower_sessions::Session, token: &str) {
    let _ = sess.remove::<String>(INVITE_RETURN_KEY).await;
    let Ok(Some(pending)) = sess.remove::<PendingInvite>(PENDING_INVITE_KEY).await else { return };
    let Some(code) = pending.fresh_code(unix_now()) else {
        tracing::info!("pending invite ignored: opened too long ago");
        return;
    };
    match auth_json("/internal/invite/redeem", &serde_json::json!({ "code": code, "token": token })).await {
        Ok((200..=299, reply)) => tracing::info!(role = ?reply.get("role"), "pending invite redeemed"),
        // Not the reply: only its error code, which is auth's own vocabulary.
        Ok((status, reply)) => {
            let code = reply.get("error").and_then(|c| c.as_str()).unwrap_or("");
            tracing::warn!(status, code, "pending invite not redeemed");
        }
        Err(e) => {
            tracing::error!(error = %e, "redeeming a pending invite failed");
            trace::failed("auth_unreachable");
        }
    }
    if let Err(e) = sess.insert(INVITE_RETURN_KEY, &code).await {
        tracing::warn!(error = %e, "saving the invite page to return to failed");
    }
}

/// Why a profile request failed.
#[derive(Debug)]
pub enum ProfileError {
    /// Auth refused the value (e.g. a display name that is too long).
    Invalid,
    /// The session's token is no longer valid.
    LoggedOut,
    Other(String),
}

async fn profile_call<T: Serialize>(path: &str, body: &T) -> Result<AccountProfile, ProfileError> {
    use session::{auth_url, service_secret};

    let resp = http_client()
        .post(format!("{}{path}", auth_url()))
        .header("x-service-token", service_secret())
        .json(body)
        .send()
        .await
        .map_err(|e| ProfileError::Other(e.to_string()));
    let result = match resp {
        Err(e) => Err(e),
        Ok(resp) => match resp.status() {
            s if s.is_success() => resp.json().await.map_err(|e| ProfileError::Other(e.to_string())),
            reqwest::StatusCode::BAD_REQUEST => Err(ProfileError::Invalid),
            reqwest::StatusCode::UNAUTHORIZED => Err(ProfileError::LoggedOut),
            s => Err(ProfileError::Other(format!("auth returned {s}"))),
        },
    };
    // Refusing a value or a stale token is an answer; anything else failed the caller's span.
    if matches!(result, Err(ProfileError::Other(_))) {
        trace::failed("auth");
    }
    result
}

/// The token's account.
#[tracing::instrument(name = "bff.account_profile", skip_all)]
pub async fn account_profile(token: &str) -> Result<AccountProfile, ProfileError> {
    #[derive(Serialize)]
    struct Req<'a> {
        token: &'a str,
    }
    profile_call("/internal/profile", &Req { token }).await
}

/// Set (or with `None`/blank, clear) the token's account's display name.
#[tracing::instrument(name = "bff.set_display_name", skip_all)]
pub async fn set_display_name(token: &str, display_name: Option<&str>) -> Result<AccountProfile, ProfileError> {
    #[derive(Serialize)]
    struct Req<'a> {
        token: &'a str,
        display_name: Option<&'a str>,
    }
    profile_call("/internal/profile/display_name", &Req { token, display_name }).await
}

#[cfg(test)]
mod tests {
    use super::PendingInvite;

    #[test]
    fn password_rejections_distinguish_credentials_from_outages() {
        for status in [401, 403] {
            assert_eq!(super::password_login_error(status, "Invalid credentials").status(), status);
        }
        for (status, body) in [(401, "Invalid service token"), (500, "internal"), (503, ""), (400, "malformed")] {
            assert_eq!(super::password_login_error(status, body).status(), 502);
        }
    }

    #[tokio::test]
    async fn password_login_preserves_rejections_over_http() {
        use opentelemetry::trace::TracerProvider as _;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tracing_subscriber::layer::SubscriberExt as _;

        let exporter = opentelemetry_sdk::trace::InMemorySpanExporter::default();
        let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build();
        crate::trace::several_subscribers();
        let _guard = tracing::subscriber::set_default(
            tracing_subscriber::registry().with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test"))),
        );

        // The variables are the process's: the JSON API's tests point them elsewhere meanwhile.
        let _env = crate::json_api::tests::AUTH_ENV.lock().await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let old_url = std::env::var_os("AUTH_SERVICE_URL");
        let old_secret = std::env::var_os("BFF_SERVICE_SECRET");
        std::env::set_var("AUTH_SERVICE_URL", format!("http://{address}"));
        std::env::set_var("BFF_SERVICE_SECRET", "test-only");

        let sess = tower_sessions::Session::new(None, std::sync::Arc::new(tower_sessions::MemoryStore::default()), None);
        for (status, body, expected) in [
            (401, "Invalid credentials", 401),
            (403, "Invalid credentials", 403),
            (401, "Invalid service token", 502),
            (503, "unavailable", 502),
        ] {
            let request = super::log_in(&sess, "tester".into(), "wrong-password".into());
            let upstream = async {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buf = [0; 4096];
                let _ = socket.read(&mut buf).await.unwrap(); // the request; its content is not checked
                socket.write_all(format!(
                    "HTTP/1.1 {status} Rejected\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()
                ).as_bytes()).await.unwrap();
            };
            let (result, ()) = tokio::join!(request, upstream);
            assert_eq!(result.unwrap_err().status(), expected);

            // In the trace, a wrong password is an answer (the status code is recorded, no
            // span is an error); auth being unusable fails the login's span.
            let spans = exporter.get_finished_spans().unwrap();
            exporter.reset();
            let span = |name: &str| spans.iter().find(|s| s.name == name).unwrap();
            let failed = |name: &str| matches!(span(name).status, opentelemetry::trace::Status::Error { .. });
            let call = span("POST /internal/token/exchange");
            let recorded = call.attributes.iter().find(|kv| kv.key.as_str() == "http.response.status_code");
            assert_eq!(recorded.map(|kv| kv.value.to_string()), Some(status.to_string()));
            assert_eq!(failed("bff.login_password"), expected == 502, "{status} {body}");
            // 401 is the token endpoints' expected answer (so is auth's own 401 for a bad
            // service secret, which the login's span reports instead).
            assert_eq!(failed("POST /internal/token/exchange"), status != 401, "{status} {body}");
        }
        drop(listener);
        let result = super::log_in(&sess, "tester".into(), "wrong-password".into()).await;
        assert_eq!(result.unwrap_err().status(), 502);
        for (name, old) in [("AUTH_SERVICE_URL", old_url), ("BFF_SERVICE_SECRET", old_secret)] {
            match old {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }

    #[test]
    fn pending_invites_wait_an_hour() {
        let p = |opened_at| PendingInvite { code: "c".into(), opened_at };
        assert_eq!(p(1000).fresh_code(1000 + 3600), Some("c".into()));
        assert_eq!(p(1000).fresh_code(1000 + 3601), None);
        // A clock that went backwards doesn't make it last forever or panic.
        assert_eq!(p(5000).fresh_code(1000), Some("c".into()));
    }
}
