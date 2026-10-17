//! The Enshrouded server panel (/enshrouded on the website), for users with the
//! `enshrouded` permission: how the server is doing, and restart / force update.
//!
//! The server runs in the cluster (homelab `enshrouded/`), under supervisord. Its state and
//! the commands go through supervisord's XML-RPC port (`enshrouded-control`, only reachable
//! from the auth namespace); the player count is Steam's A2S query on the game port.
//! Every command is stored in `enshrouded_actions`, which the panel lists and which spaces
//! commands at least `COOLDOWN_SECS` apart.
//!
//! Endpoints (service token required, like the rest of `/internal`):
//! - `POST /internal/enshrouded/status` `{token}` → [`StatusResp`].
//! - `POST /internal/enshrouded/command` `{token, cmd}` → `{}`. `restart` (players are
//!   disconnected), `update` (now if nobody is online, else once everyone has left: the image's
//!   30-minute check installs it) or `update_now` (players are disconnected).
//!   403 `forbidden`, 400 `invalid_command`, 503 `not_configured` (no control password),
//!   502 `unreachable`, 409 `busy` (a restart or update is under way), 429 `too_soon`.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use serde::{Deserialize, Serialize};
use tokio::net::UdpSocket;

use super::Db;
use super::account_email::{db_error, error};
use super::internal::InternalState;
use super::telemetry;
use super::trace::{self, Peer, TracedClient};

/// Least time between two commands, from anyone.
const COOLDOWN_SECS: i64 = 120;
/// How many past commands the panel lists.
const RECENT_LIMIT: i64 = 5;
/// How long to wait for one A2S answer. The server answers in a millisecond when it is up.
const A2S_TIMEOUT: Duration = Duration::from_millis(1500);
/// How long Steam's latest build is reused before asking again.
const STEAM_CACHE: Duration = Duration::from_secs(600);
/// How long the installed build is reused before asking supervisord again.
const INSTALLED_CACHE: Duration = Duration::from_secs(60);
const STEAM_APP_ID: &str = "2278520";

/// The image's supervisor programs (homelab `enshrouded/manifests`).
const SERVER: &str = "enshrouded-server";
/// Restarts the server whether or not players are online.
const RESTART: &str = "enshrouded-restart";
/// Reinstalls the latest build if nobody is online; otherwise it clears the recorded version,
/// so the next 30-minute check installs it once the server is empty.
const FORCE_UPDATE: &str = "enshrouded-force-update";
/// Stops the server first, so the update happens now (homelab `control.yaml`).
const FORCE_UPDATE_NOW: &str = "website-force-update";
const UPDATE_PROGRAMS: [&str; 3] = ["enshrouded-updater", FORCE_UPDATE, FORCE_UPDATE_NOW];
/// Prints `build=<installed Steam build>` to its log (homelab `control.yaml`).
const VERSION: &str = "website-version";

/// Where the server is and how to reach it; `Auth::server` builds it once.
pub struct Control {
    rpc: TracedClient,
    rpc_url: String,
    /// `None`: commands are off (the status still shows players, from A2S).
    rpc_password: Option<String>,
    query_addr: String,
    steam: TracedClient,
    /// Steam's latest public build, and when it was fetched.
    latest_build: Mutex<Option<(Instant, Option<SteamBuild>)>>,
    /// The installed Steam build, and when it was read.
    installed_build: Mutex<Option<(Instant, Option<String>)>>,
}

/// A build on Steam's public branch.
#[derive(Clone, Debug, PartialEq)]
struct SteamBuild {
    id: String,
    /// When it was published (unix seconds).
    at: i64,
}

impl Control {
    pub fn from_env() -> Arc<Self> {
        let env = |name: &str, default: &str| std::env::var(name).unwrap_or_else(|_| default.to_string());
        let rpc_password = std::env::var("ENSHROUDED_RPC_PASSWORD").ok().filter(|p| !p.is_empty());
        if rpc_password.is_none() {
            tracing::warn!("ENSHROUDED_RPC_PASSWORD is not set: the Enshrouded panel can't restart or update");
        }
        Arc::new(Control {
            rpc: trace::client(
                Peer { service: "enshrouded", expected: &[] },
                Duration::from_secs(10),
                reqwest::redirect::Policy::none(),
            ),
            rpc_url: env("ENSHROUDED_RPC_URL", "http://enshrouded-control.enshrouded.svc.cluster.local:9001/RPC2"),
            rpc_password,
            query_addr: env("ENSHROUDED_QUERY_ADDR", "enshrouded.enshrouded.svc.cluster.local:15637"),
            steam: trace::client(
                Peer { service: "steamcmd", expected: &[] },
                Duration::from_secs(5),
                reqwest::redirect::Policy::default(),
            ),
            latest_build: Mutex::new(None),
            installed_build: Mutex::new(None),
        })
    }

    /// One XML-RPC call; the response body, or why there is none.
    async fn rpc(&self, method: &str, params: &str) -> Result<String, RpcError> {
        let password = self.rpc_password.as_deref().ok_or(RpcError::NotConfigured)?;
        let body = format!(
            "<?xml version=\"1.0\"?><methodCall><methodName>{method}</methodName><params>{params}</params></methodCall>"
        );
        let resp = self
            .rpc
            .post(&self.rpc_url)
            .basic_auth("website", Some(password))
            .header("content-type", "text/xml")
            .body(body)
            .send()
            .await
            .map_err(|_| RpcError::Unreachable)?;
        if !resp.status().is_success() {
            return Err(RpcError::Unreachable);
        }
        let text = resp.text().await.map_err(|_| RpcError::Unreachable)?;
        if text.contains("<fault>") {
            return Err(RpcError::Fault);
        }
        Ok(text)
    }

    async fn processes(&self) -> Result<Vec<Process>, RpcError> {
        self.rpc("supervisor.getAllProcessInfo", "").await.map(|xml| parse_processes(&xml))
    }

    /// Starts a supervisor program without waiting for it to finish.
    async fn start(&self, program: &str) -> Result<(), RpcError> {
        let params = format!(
            "<param><value><string>{program}</string></value></param><param><value><boolean>0</boolean></value></param>"
        );
        self.rpc("supervisor.startProcess", &params).await.map(|_| ())
    }

    /// Steam's public branch's latest build, cached for `STEAM_CACHE`.
    async fn latest_build(&self) -> Option<SteamBuild> {
        if let Some((at, value)) = &*self.latest_build.lock().unwrap()
            && at.elapsed() < STEAM_CACHE
        {
            return value.clone();
        }
        let value = self.fetch_latest_build().await;
        *self.latest_build.lock().unwrap() = Some((Instant::now(), value.clone()));
        value
    }

    async fn fetch_latest_build(&self) -> Option<SteamBuild> {
        let resp = self
            .steam
            .get(format!("https://api.steamcmd.net/v1/info/{STEAM_APP_ID}"))
            .send()
            .await
            .ok()?;
        let body: serde_json::Value = resp.json().await.ok()?;
        let public = &body["data"][STEAM_APP_ID]["depots"]["branches"]["public"];
        Some(SteamBuild {
            id: public["buildid"].as_str()?.to_string(),
            at: public["timebuildupdated"].as_str()?.parse().ok()?,
        })
    }

    /// The installed Steam build, cached for `INSTALLED_CACHE`. `None` while an update is under
    /// way, and until the pod has started with the `website-version` program.
    async fn installed_build(&self) -> Option<String> {
        if let Some((at, value)) = &*self.installed_build.lock().unwrap()
            && at.elapsed() < INSTALLED_CACHE
        {
            return value.clone();
        }
        // The previous run's answer, then a new run for the next time: reading right after
        // starting it could miss its line.
        let params = format!(
            "<param><value><string>{VERSION}</string></value></param><param><value><int>0</int></value></param><param><value><int>256</int></value></param>"
        );
        let value = match self.rpc("supervisor.tailProcessStdoutLog", &params).await {
            Ok(xml) => last_build(&xml),
            Err(RpcError::Fault) => None,
            // Not configured or down: nothing to start either.
            Err(_) => return None,
        };
        let _ = self.start(VERSION).await;
        *self.installed_build.lock().unwrap() = Some((Instant::now(), value.clone()));
        value
    }
}

/// The build in the last `build=` line of `website-version`'s log tail.
fn last_build(xml: &str) -> Option<String> {
    let line = between(xml, "<string>", "</string>")?.lines().rev().find(|l| l.starts_with("build="))?;
    let build = line.trim_start_matches("build=").trim();
    (!build.is_empty()).then(|| build.to_string())
}

#[derive(Debug, PartialEq)]
enum RpcError {
    NotConfigured,
    Unreachable,
    /// supervisord refused (e.g. the program is already running).
    Fault,
}

pub fn routes() -> Router<InternalState> {
    Router::new()
        .route("/internal/enshrouded/status", post(status))
        .route("/internal/enshrouded/command", post(command))
}

/// The token's user, if the token is valid and the user holds `enshrouded`.
#[tracing::instrument(name = "enshrouded.authorize", skip_all)]
async fn resolve_user(db: &Db, token: &str) -> Result<Option<i64>, sqlx::Error> {
    let row: Option<(i64,)> = sqlx::query_as(
        r#"
        SELECT t.user_id FROM bff_tokens t
        JOIN user_roles ur ON ur.user_id = t.user_id
        JOIN role_permissions rp ON rp.role_id = ur.role_id
        JOIN permissions p ON p.id = rp.permission_id
        WHERE t.token = $1 AND t.expires_at > NOW() AND p.name = 'enshrouded'
        LIMIT 1
        "#,
    )
    .bind(token)
    .fetch_optional(db)
    .await
    .inspect_err(trace::db_failed)?;
    Ok(row.map(|(id,)| id))
}

// ---- Status ----

/// What the server is doing, as the panel shows it.
#[derive(Serialize, Debug, PartialEq, Clone, Copy)]
enum ServerState {
    Online,
    /// The process runs but the world isn't loaded yet (no A2S answer).
    Starting,
    Restarting,
    Updating,
    Stopping,
    Stopped,
    /// supervisord gave up starting it.
    Crashed,
    /// Neither supervisord nor the game answers: the pod is down or being replaced.
    Unreachable,
}

#[derive(Serialize)]
struct Players {
    online: u8,
    max: u8,
    /// How long each player has been connected, in seconds (Enshrouded sends no names).
    sessions: Vec<u32>,
}

#[derive(Serialize, sqlx::FromRow)]
struct Action {
    /// `None` when the account was deleted.
    name: Option<String>,
    action: String,
    /// Unix seconds.
    at: i64,
}

#[derive(Serialize)]
struct StatusResp {
    state: ServerState,
    /// `None` when the server doesn't answer the query.
    players: Option<Players>,
    /// The game's own version (its SVN revision), from the query.
    version: Option<String>,
    /// The installed Steam build, when known.
    build: Option<String>,
    /// Steam's latest public build.
    latest_build: Option<String>,
    /// Seconds the server process has been running.
    uptime_secs: Option<i64>,
    /// When Steam last published a build (unix seconds).
    latest_build_at: Option<i64>,
    /// Steam has a build the server doesn't run: it updates itself when nobody is online.
    update_pending: bool,
    /// Someone asked for an update that hasn't happened yet (players were online).
    update_queued: bool,
    /// Whether restart / update can be sent (password set and supervisord answers).
    can_control: bool,
    /// Latest commands, newest first.
    recent: Vec<Action>,
    /// When the next command may be sent (unix seconds), if that is in the future.
    next_command_at: Option<i64>,
}

#[derive(Deserialize)]
struct TokenReq {
    token: String,
}

#[tracing::instrument(name = "enshrouded.status", skip_all)]
async fn status(State(state): State<InternalState>, Json(req): Json<TokenReq>) -> Response {
    match resolve_user(&state.db, &req.token).await {
        Ok(Some(_)) => {}
        Ok(None) => return error(StatusCode::FORBIDDEN, "forbidden"),
        Err(e) => return db_error("checking the enshrouded permission", e),
    }
    let control = &state.enshrouded;
    let (processes, query, latest, installed, recent) = tokio::join!(
        control.processes(),
        a2s_query(&control.query_addr),
        control.latest_build(),
        control.installed_build(),
        recent_actions(&state.db),
    );
    let recent = match recent {
        Ok(r) => r,
        Err(e) => return db_error("reading enshrouded commands", e),
    };
    let processes = processes.ok();
    let server = processes.as_deref().and_then(|ps| ps.iter().find(|p| p.name == SERVER));
    let running_since = server.filter(|p| p.statename == "RUNNING").map(|p| p.start);
    let next_command_at = recent.first().map(|a| a.at + COOLDOWN_SECS).filter(|&at| at > now_secs());

    Json(StatusResp {
        state: server_state(processes.as_deref(), query.is_some()),
        version: query.as_ref().and_then(|q| q.version.clone()),
        players: query.map(|q| Players { online: q.players, max: q.max_players, sessions: q.sessions }),
        uptime_secs: server.filter(|p| p.statename == "RUNNING").map(|p| p.now - p.start),
        update_pending: update_pending(latest.as_ref(), installed.as_deref(), running_since),
        latest_build_at: latest.as_ref().map(|b| b.at),
        latest_build: latest.map(|b| b.id),
        build: installed,
        update_queued: update_queued(&recent, running_since),
        can_control: processes.is_some(),
        recent,
        next_command_at,
    })
    .into_response()
}

async fn recent_actions(db: &Db) -> Result<Vec<Action>, sqlx::Error> {
    sqlx::query_as(
        "SELECT COALESCE(u.display_name, u.username) AS name, a.action, \
                EXTRACT(EPOCH FROM a.created_at)::BIGINT AS at \
         FROM enshrouded_actions a LEFT JOIN users u ON u.id = a.user_id \
         ORDER BY a.created_at DESC LIMIT $1",
    )
    .bind(RECENT_LIMIT)
    .fetch_all(db)
    .await
    .inspect_err(trace::db_failed)
}

/// Whether Steam has a build the server doesn't run. Exact when the installed build is known;
/// otherwise, whether Steam published one after the server started.
fn update_pending(latest: Option<&SteamBuild>, installed: Option<&str>, running_since: Option<i64>) -> bool {
    match (latest, installed, running_since) {
        (Some(latest), Some(installed), _) => latest.id != installed,
        (Some(latest), None, Some(started)) => latest.at > started,
        _ => false,
    }
}

/// Whether an `update` was asked for after the server last started: an update restarts the
/// server, so it hasn't happened yet. Only while the server runs (else the state says it all).
fn update_queued(recent: &[Action], running_since: Option<i64>) -> bool {
    let Some(started) = running_since else { return false };
    recent.iter().any(|a| a.action == "update" && a.at > started)
}

fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

/// `processes` is `None` when supervisord didn't answer; `answers` is whether the game did.
fn server_state(processes: Option<&[Process]>, answers: bool) -> ServerState {
    let Some(processes) = processes else {
        return if answers { ServerState::Online } else { ServerState::Unreachable };
    };
    let active = |name: &str| {
        processes
            .iter()
            .any(|p| p.name == name && matches!(p.statename.as_str(), "STARTING" | "RUNNING" | "STOPPING"))
    };
    if UPDATE_PROGRAMS.into_iter().any(active) {
        return ServerState::Updating;
    }
    if active(RESTART) {
        return ServerState::Restarting;
    }
    // The container is starting: the bootstrap starts the updater, which starts the server.
    if active("enshrouded-bootstrap") {
        return ServerState::Starting;
    }
    match processes.iter().find(|p| p.name == SERVER).map(|p| p.statename.as_str()) {
        Some("RUNNING") if answers => ServerState::Online,
        Some("RUNNING" | "STARTING" | "BACKOFF") => ServerState::Starting,
        Some("STOPPING") => ServerState::Stopping,
        Some("FATAL") => ServerState::Crashed,
        _ => ServerState::Stopped,
    }
}

// ---- Commands ----

#[derive(Deserialize)]
struct CommandReq {
    token: String,
    cmd: String,
}

#[tracing::instrument(name = "enshrouded.command", skip_all, fields(cmd = %req.cmd))]
async fn command(State(state): State<InternalState>, Json(req): Json<CommandReq>) -> Response {
    let user_id = match resolve_user(&state.db, &req.token).await {
        Ok(Some(id)) => id,
        Ok(None) => {
            telemetry::enshrouded_command(&req.cmd, "denied");
            return error(StatusCode::FORBIDDEN, "forbidden");
        }
        Err(e) => return db_error("checking the enshrouded permission", e),
    };
    let (cmd, program) = match req.cmd.as_str() {
        "restart" => ("restart", RESTART),
        "update" => ("update", FORCE_UPDATE),
        "update_now" => ("update_now", FORCE_UPDATE_NOW),
        _ => {
            telemetry::enshrouded_command(&req.cmd, "invalid");
            return error(StatusCode::BAD_REQUEST, "invalid_command");
        }
    };
    let result = run_command(&state.db, &state.enshrouded, user_id, cmd, program).await;
    let (status, code) = match result {
        Ok(()) => {
            tracing::info!(user_id, cmd, "enshrouded command issued");
            telemetry::enshrouded_command(cmd, "success");
            return Json(serde_json::json!({})).into_response();
        }
        Err(CommandError::Db(e)) => return db_error("running an enshrouded command", e),
        Err(CommandError::TooSoon) => (StatusCode::TOO_MANY_REQUESTS, "too_soon"),
        Err(CommandError::Busy) => (StatusCode::CONFLICT, "busy"),
        Err(CommandError::Rpc(RpcError::NotConfigured)) => (StatusCode::SERVICE_UNAVAILABLE, "not_configured"),
        // A fault on start: the program is already running, i.e. busy.
        Err(CommandError::Rpc(RpcError::Fault)) => (StatusCode::CONFLICT, "busy"),
        Err(CommandError::Rpc(RpcError::Unreachable)) => {
            trace::fail(&tracing::Span::current(), "unreachable");
            (StatusCode::BAD_GATEWAY, "unreachable")
        }
    };
    tracing::warn!(user_id, cmd, code, "enshrouded command refused");
    telemetry::enshrouded_command(cmd, code);
    error(status, code)
}

enum CommandError {
    Db(sqlx::Error),
    TooSoon,
    Busy,
    Rpc(RpcError),
}

impl From<sqlx::Error> for CommandError {
    fn from(e: sqlx::Error) -> Self {
        CommandError::Db(e)
    }
}

/// Checks the cooldown and that nothing is under way, starts `program` and records it. One
/// command at a time across auth's replicas: the transaction holds an advisory lock.
async fn run_command(db: &Db, control: &Control, user_id: i64, cmd: &str, program: &str) -> Result<(), CommandError> {
    let mut tx = trace::begin(db).await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('enshrouded_actions'))")
        .execute(&mut tx.executor())
        .await?;
    let (recent,): (bool,) = sqlx::query_as(
        "SELECT EXISTS (SELECT 1 FROM enshrouded_actions WHERE created_at > NOW() - make_interval(secs => $1))",
    )
    .bind(COOLDOWN_SECS as f64)
    .fetch_one(&mut tx.executor())
    .await?;
    let started = async {
        if recent {
            return Err(CommandError::TooSoon);
        }
        let processes = control.processes().await.map_err(CommandError::Rpc)?;
        if matches!(
            server_state(Some(&processes), true),
            ServerState::Updating | ServerState::Restarting | ServerState::Stopping
        ) {
            return Err(CommandError::Busy);
        }
        control.start(program).await.map_err(CommandError::Rpc)
    }
    .await;
    if let Err(e) = started {
        trace::rollback(tx).await;
        return Err(e);
    }
    sqlx::query("INSERT INTO enshrouded_actions (user_id, action) VALUES ($1, $2)")
        .bind(user_id)
        .bind(cmd)
        .execute(&mut tx.executor())
        .await?;
    trace::commit(tx).await?;
    Ok(())
}

// ---- supervisord's XML-RPC ----

/// The fields of `supervisor.getProcessInfo` the panel uses.
#[derive(Debug, Default, PartialEq)]
struct Process {
    name: String,
    statename: String,
    /// Unix seconds, on the server's clock.
    start: i64,
    now: i64,
}

/// The processes in a `getAllProcessInfo` response. supervisord (Python's xmlrpc) writes one
/// `<member><name>..</name><value><type>..</type></value></member>` per field.
fn parse_processes(xml: &str) -> Vec<Process> {
    xml.split("<struct>")
        .skip(1)
        .map(|item| {
            let mut p = Process::default();
            for member in item.split("<member>").skip(1) {
                let (Some(name), Some(value)) = (between(member, "<name>", "</name>"), member_value(member)) else {
                    continue;
                };
                match name {
                    "name" => p.name = value.to_string(),
                    "statename" => p.statename = value.to_string(),
                    "start" => p.start = value.parse().unwrap_or(0),
                    "now" => p.now = value.parse().unwrap_or(0),
                    _ => {}
                }
            }
            p
        })
        .collect()
}

/// The text inside a member's `<value><type>…</type></value>`.
fn member_value(member: &str) -> Option<&str> {
    let value = between(member, "<value>", "</value>")?;
    let inner = value.strip_prefix('<')?;
    let tag_end = inner.find('>')?;
    let tag = &inner[..tag_end];
    between(value, &format!("<{tag}>"), &format!("</{tag}>"))
}

fn between<'a>(s: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = s.find(open)? + open.len();
    let len = s[start..].find(close)?;
    Some(&s[start..start + len])
}

// ---- Steam's A2S query ----

#[derive(Debug, PartialEq)]
struct Query {
    players: u8,
    max_players: u8,
    /// The game's SVN revision, which its log calls "Game Version".
    version: Option<String>,
    sessions: Vec<u32>,
}

/// A2S_INFO and A2S_PLAYER on the game's query port; `None` when it doesn't answer.
#[tracing::instrument(name = "a2s.query", skip_all, fields(otel.kind = "client", peer.service = "enshrouded"))]
async fn a2s_query(addr: &str) -> Option<Query> {
    let socket = UdpSocket::bind("0.0.0.0:0").await.ok()?;
    socket.connect(addr).await.ok()?;
    let info = a2s_request(&socket, b"TSource Engine Query\0").await?;
    let (players, max_players, version) = parse_info(&info)?;
    let sessions = a2s_request(&socket, &[0x55, 0xFF, 0xFF, 0xFF, 0xFF])
        .await
        .and_then(|p| parse_players(&p))
        .unwrap_or_default();
    Some(Query { players, max_players, version, sessions })
}

/// Sends one A2S request, answering a challenge if the server sends one. The reply
/// without its 4-byte header.
async fn a2s_request(socket: &UdpSocket, payload: &[u8]) -> Option<Vec<u8>> {
    let mut packet = [&[0xFF; 4][..], payload].concat();
    let mut buf = vec![0; 1400];
    for _ in 0..2 {
        socket.send(&packet).await.ok()?;
        let n = tokio::time::timeout(A2S_TIMEOUT, socket.recv(&mut buf)).await.ok()?.ok()?;
        let reply = buf.get(4..n)?;
        match reply.first()? {
            // S2C_CHALLENGE: the same request again with the challenge (A2S_PLAYER replaces its
            // 0xFFFFFFFF with it, A2S_INFO appends it).
            0x41 => {
                let challenge = reply.get(1..5)?;
                packet = if payload[0] == 0x55 {
                    [&[0xFF; 4][..], &[0x55], challenge].concat()
                } else {
                    [&[0xFF; 4][..], payload, challenge].concat()
                };
            }
            _ => return Some(reply.to_vec()),
        }
    }
    None
}

/// Players, slots and the game's version from an A2S_INFO reply (`I`, then the Source format).
/// The version field is a fixed "0.0.15.0"; the real one is the second of the keywords
/// (`3;1269579;…`), in the extra data.
fn parse_info(reply: &[u8]) -> Option<(u8, u8, Option<String>)> {
    let mut r = Reader { buf: reply.strip_prefix(b"I")? };
    r.byte()?; // protocol
    for _ in 0..4 {
        r.cstr()?; // name, map, folder, game
    }
    r.take(2)?; // app id
    let players = r.byte()?;
    let max_players = r.byte()?;
    r.take(5)?; // bots, server type, environment, visibility, VAC
    r.cstr()?; // version
    Some((players, max_players, game_version(&mut r)))
}

/// The second keyword, from the extra data flags (EDF) and the fields they announce.
fn game_version(r: &mut Reader) -> Option<String> {
    let edf = r.byte()?;
    if edf & 0x80 != 0 {
        r.take(2)?; // game port
    }
    if edf & 0x10 != 0 {
        r.take(8)?; // steam id
    }
    if edf & 0x40 != 0 {
        r.take(2)?; // spectator port
        r.cstr()?; // spectator name
    }
    if edf & 0x20 == 0 {
        return None;
    }
    let keywords = r.cstr()?;
    let version = keywords.split(';').nth(1)?;
    (!version.is_empty() && version.bytes().all(|b| b.is_ascii_digit())).then(|| version.to_string())
}

/// Each player's connected time from an A2S_PLAYER reply (`D`, count, then the players).
fn parse_players(reply: &[u8]) -> Option<Vec<u32>> {
    let mut r = Reader { buf: reply.strip_prefix(b"D")? };
    let count = r.byte()?;
    (0..count)
        .map(|_| {
            r.byte()?; // index
            r.cstr()?; // name
            r.take(4)?; // score
            let secs = f32::from_le_bytes(r.take(4)?.try_into().ok()?);
            Some(secs.max(0.0) as u32)
        })
        .collect()
}

struct Reader<'a> {
    buf: &'a [u8],
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let (head, rest) = self.buf.split_at_checked(n)?;
        self.buf = rest;
        Some(head)
    }

    fn byte(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    fn cstr(&mut self) -> Option<String> {
        let end = self.buf.iter().position(|&b| b == 0)?;
        let s = String::from_utf8_lossy(&self.buf[..end]).into_owned();
        self.buf = &self.buf[end + 1..];
        Some(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The server's real answers (Chronicle, 2026-10-17), without the 0xFFFFFFFF header.
    const INFO: &[u8] = b"I\x11Chronicle\x00\x00Enshrouded\x00Enshrouded\x00\x00\x00\x01\x10\x00dl\x01\x000.0.15.0\x00\xb1\x15=\x19X\x15_\xda\xca@\x013;1269579;1;0;1;0;1;0;0\x00\xa4]\x12\x00\x00\x00\x00\x00";
    const PLAYERS: &[u8] = b"D\x01\x00\x00\x00\x00\x00\x00\xf8\x0cxC";

    #[test]
    fn reads_the_servers_a2s_answers() {
        assert_eq!(parse_info(INFO), Some((1, 16, Some("1269579".to_string()))));
        assert_eq!(parse_players(PLAYERS), Some(vec![248]));
        assert_eq!(parse_players(b"D\x00"), Some(vec![]));
    }

    #[test]
    fn short_a2s_answers_are_none() {
        assert_eq!(parse_info(&INFO[..20]), None);
        // Cut inside the extra data: the counts still read, the version doesn't.
        assert_eq!(parse_info(&INFO[..62]), Some((1, 16, None)));
        assert_eq!(parse_players(&PLAYERS[..8]), None);
        assert_eq!(parse_info(b"X"), None);
    }

    /// Trimmed from the server's `getAllProcessInfo` answer.
    const PROCESSES: &str = "<?xml version='1.0'?>
<methodResponse>
<params>
<param>
<value><array><data>
<value><struct>
<member>
<name>name</name>
<value><string>enshrouded-server</string></value>
</member>
<member>
<name>start</name>
<value><int>1792165000</int></value>
</member>
<member>
<name>now</name>
<value><int>1792237762</int></value>
</member>
<member>
<name>statename</name>
<value><string>RUNNING</string></value>
</member>
<member>
<name>spawnerr</name>
<value><string></string></value>
</member>
</struct></value>
<value><struct>
<member>
<name>name</name>
<value><string>enshrouded-updater</string></value>
</member>
<member>
<name>statename</name>
<value><string>EXITED</string></value>
</member>
</struct></value>
</data></array></value>
</param>
</params>
</methodResponse>";

    fn process(name: &str, statename: &str) -> Process {
        Process { name: name.into(), statename: statename.into(), ..Default::default() }
    }

    #[test]
    fn reads_supervisords_process_list() {
        assert_eq!(
            parse_processes(PROCESSES),
            vec![
                Process { name: SERVER.into(), statename: "RUNNING".into(), start: 1792165000, now: 1792237762 },
                process("enshrouded-updater", "EXITED"),
            ]
        );
    }

    /// `tailProcessStdoutLog` of `website-version` (the 1.8.0 image), the second run during an
    /// update.
    const VERSION_LOG: &str = "<?xml version='1.0'?>
<methodResponse>
<params>
<param>
<value><array><data>
<value><string>build=25988414
build=
</string></value>
<value><int>22</int></value>
<value><boolean>0</boolean></value>
</data></array></value>
</param>
</params>
</methodResponse>";

    #[test]
    fn reads_the_installed_build() {
        assert_eq!(last_build(VERSION_LOG), None);
        assert_eq!(last_build(&VERSION_LOG.replace("build=\n</string>", "</string>")), Some("25988414".into()));
        assert_eq!(last_build("<value><string></string></value>"), None);
    }

    #[test]
    fn an_update_is_pending_when_steam_has_another_build() {
        let latest = SteamBuild { id: "2".into(), at: 200 };
        assert!(update_pending(Some(&latest), Some("1"), Some(300)));
        assert!(!update_pending(Some(&latest), Some("2"), Some(100)));
        // The installed build unknown: by time.
        assert!(update_pending(Some(&latest), None, Some(100)));
        assert!(!update_pending(Some(&latest), None, Some(300)));
        assert!(!update_pending(Some(&latest), None, None));
        assert!(!update_pending(None, Some("1"), Some(100)));
    }

    #[test]
    fn an_update_is_queued_until_the_server_restarts() {
        let action = |action: &str, at| Action { name: None, action: action.into(), at };
        let recent = [action("restart", 300), action("update", 200)];
        assert!(update_queued(&recent, Some(100)));
        assert!(!update_queued(&recent, Some(250)));
        assert!(!update_queued(&recent, None));
        assert!(!update_queued(&[action("update_now", 200)], Some(100)));
    }

    #[test]
    fn server_states() {
        use ServerState::*;
        let state = |ps: &[Process], answers| server_state(Some(ps), answers);
        let server = |s| process(SERVER, s);
        assert_eq!(state(&[server("RUNNING")], true), Online);
        assert_eq!(state(&[server("RUNNING")], false), Starting);
        assert_eq!(state(&[server("BACKOFF")], false), Starting);
        assert_eq!(state(&[server("STOPPING")], false), Stopping);
        assert_eq!(state(&[server("FATAL")], false), Crashed);
        assert_eq!(state(&[server("STOPPED")], false), Stopped);
        assert_eq!(state(&[server("STOPPING"), process(FORCE_UPDATE_NOW, "RUNNING")], true), Updating);
        assert_eq!(state(&[server("RUNNING"), process(FORCE_UPDATE, "RUNNING")], true), Updating);
        assert_eq!(state(&[server("STOPPED"), process("enshrouded-updater", "RUNNING")], false), Updating);
        assert_eq!(state(&[server("STOPPING"), process(RESTART, "RUNNING")], true), Restarting);
        assert_eq!(state(&[server("STOPPED"), process("enshrouded-bootstrap", "RUNNING")], false), Starting);
        assert_eq!(state(&[server("RUNNING"), process("enshrouded-updater", "EXITED")], true), Online);
        assert_eq!(server_state(None, true), Online);
        assert_eq!(server_state(None, false), Unreachable);
    }

    /// A stand-in supervisord: answers `getAllProcessInfo` with the updater in `updater`'s state,
    /// and records the programs started.
    #[derive(Clone, Default)]
    struct FakeSupervisor {
        updater: Arc<Mutex<&'static str>>,
        started: Arc<Mutex<Vec<String>>>,
    }

    async fn fake_supervisor(fake: FakeSupervisor) -> String {
        use super::super::trace::testing::serve;
        let rpc = move |headers: axum::http::HeaderMap, body: String| {
            let fake = fake.clone();
            async move {
                // website:pw
                assert_eq!(headers["authorization"], "Basic d2Vic2l0ZTpwdw==");
                if body.contains("supervisor.startProcess") {
                    fake.started.lock().unwrap().push(between(&body, "<string>", "</string>").unwrap().to_string());
                    return "<methodResponse><params><param><value><boolean>1</boolean></value></param></params></methodResponse>".to_string();
                }
                if body.contains("supervisor.tailProcessStdoutLog") {
                    return VERSION_LOG.to_string();
                }
                let updater = *fake.updater.lock().unwrap();
                PROCESSES.replace("EXITED", updater)
            }
        };
        serve(axum::Router::new().route("/RPC2", axum::routing::post(rpc))).await
    }

    fn control(url: &str, password: Option<&str>) -> Control {
        let client = || trace::client(Peer { service: "test", expected: &[] }, Duration::from_secs(5), reqwest::redirect::Policy::none());
        Control {
            rpc: client(),
            rpc_url: format!("{url}/RPC2"),
            rpc_password: password.map(String::from),
            query_addr: "127.0.0.1:9".into(),
            steam: client(),
            latest_build: Mutex::new(None),
            installed_build: Mutex::new(None),
        }
    }

    /// Against a real Postgres (Docker): a command starts its program and is recorded; the
    /// next one within the cooldown, one during an update, and any without a password are
    /// refused without starting anything.
    #[tokio::test]
    async fn commands_are_spaced_and_wait_for_updates() {
        use super::super::trace::testing::postgres;
        let pg = postgres(super::super::pool_options()).await;
        super::super::migrate(&pg.raw, &tower_sessions_sqlx_store::PostgresStore::new(pg.raw.clone())).await.unwrap();
        let db = pg.db();
        let (user_id,): (i64,) = sqlx::query_as("INSERT INTO users (username) VALUES ('ada') RETURNING id")
            .fetch_one(&db)
            .await
            .unwrap();
        let fake = FakeSupervisor { updater: Arc::new(Mutex::new("EXITED")), ..Default::default() };
        let url = fake_supervisor(fake.clone()).await;
        let ctl = control(&url, Some("pw"));

        assert!(run_command(&db, &ctl, user_id, "restart", RESTART).await.is_ok());
        assert_eq!(*fake.started.lock().unwrap(), vec![RESTART.to_string()]);
        let recent = recent_actions(&db).await.unwrap();
        assert_eq!((recent.len(), recent[0].name.as_deref(), recent[0].action.as_str()), (1, Some("ada"), "restart"));

        assert!(matches!(run_command(&db, &ctl, user_id, "update", FORCE_UPDATE).await, Err(CommandError::TooSoon)));

        sqlx::query("UPDATE enshrouded_actions SET created_at = NOW() - INTERVAL '3 minutes'").execute(&db).await.unwrap();
        *fake.updater.lock().unwrap() = "RUNNING";
        assert!(matches!(run_command(&db, &ctl, user_id, "update", FORCE_UPDATE).await, Err(CommandError::Busy)));
        assert!(matches!(
            run_command(&db, &control(&url, None), user_id, "update", FORCE_UPDATE).await,
            Err(CommandError::Rpc(RpcError::NotConfigured))
        ));

        *fake.updater.lock().unwrap() = "EXITED";
        assert!(run_command(&db, &ctl, user_id, "update", FORCE_UPDATE).await.is_ok());
        assert_eq!(*fake.started.lock().unwrap(), vec![RESTART.to_string(), FORCE_UPDATE.to_string()]);
        assert_eq!(recent_actions(&db).await.unwrap().len(), 2);
    }

    /// The installed build comes from the last run's log, and each read (at most once a minute)
    /// starts another run.
    #[tokio::test]
    async fn the_installed_build_is_read_from_the_last_run() {
        let fake = FakeSupervisor::default();
        let ctl = control(&fake_supervisor(fake.clone()).await, Some("pw"));
        // The last run was during an update.
        assert_eq!(ctl.installed_build().await, None);
        assert_eq!(*fake.started.lock().unwrap(), vec![VERSION.to_string()]);
        assert_eq!(ctl.installed_build().await, None);
        assert_eq!(fake.started.lock().unwrap().len(), 1, "cached");

        assert_eq!(control("http://127.0.0.1:9", None).installed_build().await, None);
    }

    /// Against the real server on the LAN: `ENSHROUDED_QUERY_ADDR=192.168.1.129:15637 cargo
    /// test a2s_against_the_server -- --ignored`.
    #[tokio::test]
    #[ignore = "needs the game server"]
    async fn a2s_against_the_server() {
        let addr = std::env::var("ENSHROUDED_QUERY_ADDR").unwrap();
        let q = a2s_query(&addr).await.expect("an answer");
        assert_eq!(q.max_players, 16);
        assert_eq!(q.sessions.len(), q.players as usize);
        assert!(q.version.is_some(), "the game's version");
    }

    /// Against a supervisord with homelab's `control.yaml` (e.g. the image run locally with it
    /// mounted): `ENSHROUDED_RPC_URL=http://127.0.0.1:19001/RPC2 ENSHROUDED_RPC_PASSWORD=pw cargo
    /// test installed_build_against_supervisord -- --ignored`.
    #[tokio::test]
    #[ignore = "needs supervisord"]
    async fn installed_build_against_supervisord() {
        let ctl = Control::from_env();
        ctl.installed_build().await;
        tokio::time::sleep(Duration::from_secs(1)).await;
        *ctl.installed_build.lock().unwrap() = None;
        let build = ctl.installed_build().await.expect("a build");
        assert!(build.bytes().all(|b| b.is_ascii_digit()), "{build}");
    }
}
