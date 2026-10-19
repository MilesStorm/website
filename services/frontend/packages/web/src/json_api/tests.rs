//! Contract tests of the JSON API: statuses, the error envelope, access by login, the origin
//! check and one introspect call to auth per request; and the Dioxus server functions over the
//! same functions. Auth is a stand-in that answers by token: `all` (every permission, plus one
//! the site doesn't know), `none`, `lost` (an account auth can't load), `dead` (a token auth
//! dropped), `down` (auth failing) and `github` (an account without an email). Emailed codes:
//! `good` works, anything else has expired; invite code `join` is for `arcane_user`. The same
//! stand-in plays SurrealDB for pictures and shared rolls, and the Ark, Enshrouded and admin
//! endpoints, which fail while [`Auth::failing`] is set.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use axum::routing::post;
use serde_json::{json, Value};
use tower::ServiceExt as _;
use tower_sessions::{MemoryStore, Session, SessionManagerLayer};

/// Held by every test that points `AUTH_SERVICE_URL` at a stand-in: the variable is the
/// process's, and tests run in parallel.
pub(crate) static AUTH_ENV: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const HOST: &str = "milesstorm.test";
const SITE: &str = "https://milesstorm.test";

/// Introspect calls, by token, and the stored picture (base64 JPEG, version).
#[derive(Default)]
struct Auth {
    introspected: Mutex<HashMap<String, usize>>,
    picture: Mutex<Option<(String, i64)>>,
    /// The SurrealDB statements run, and `delete/confirm` calls to auth, in order.
    queries: Mutex<Vec<String>>,
    /// The Ark, Enshrouded and admin calls to auth, as `METHOD path?query`, in order.
    calls: Mutex<Vec<String>>,
    /// The game hosts and auth's database are down: those endpoints fail.
    failing: std::sync::atomic::AtomicBool,
}

impl Auth {
    fn introspected(&self, token: &str) -> usize {
        self.introspected.lock().unwrap().get(token).copied().unwrap_or(0)
    }
}

fn profile(token: &str, display_name: Option<&str>) -> Value {
    json!({"user_id": 7, "username": format!("user-{token}"), "display_name": display_name, "email": "a@b.c", "email_verified": true})
}

async fn stand_in_auth(auth: Arc<Auth>) -> String {
    use axum::response::IntoResponse;

    let auth_panels = auth.clone();
    let pictures = auth.clone();
    let confirmed = auth.clone();
    let introspect = move |body: axum::Json<Value>| async move {
        let token = body["token"].as_str().unwrap_or_default().to_string();
        *auth.introspected.lock().unwrap().entry(token.clone()).or_default() += 1;
        match token.as_str() {
            "all" => axum::Json(json!({"permissions": ["llama", "enshrouded", "arcane", "manage_permissions", "brand_new"]})).into_response(),
            "none" | "lost" | "github" => axum::Json(json!({"permissions": []})).into_response(),
            "dead" => (StatusCode::UNAUTHORIZED, "Invalid or expired token").into_response(),
            _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        }
    };
    let profile_of = |body: axum::Json<Value>| async move {
        match body["token"].as_str() {
            Some(t @ ("all" | "none")) => axum::Json(profile(t, Some("Shown"))).into_response(),
            Some("github") => axum::Json(json!({"user_id": 9, "username": "user-github", "display_name": null})).into_response(),
            Some("dead") => StatusCode::UNAUTHORIZED.into_response(),
            _ => StatusCode::BAD_GATEWAY.into_response(),
        }
    };
    let display_name = |body: axum::Json<Value>| async move {
        let name = body["display_name"].as_str().unwrap_or_default();
        if name.chars().count() > 40 {
            return StatusCode::BAD_REQUEST.into_response();
        }
        axum::Json(profile(body["token"].as_str().unwrap_or_default(), Some(name))).into_response()
    };
    let exchange = |body: axum::Json<Value>| async move {
        match (body["username"].as_str(), body["password"].as_str()) {
            (Some("tester"), Some("right")) => axum::Json(json!({"token": "all", "username": "tester"})).into_response(),
            (Some("outage"), _) => (StatusCode::UNAUTHORIZED, "Invalid service token").into_response(),
            _ => (StatusCode::UNAUTHORIZED, "Invalid credentials").into_response(),
        }
    };
    let register = |body: axum::Json<Value>| async move {
        match body["username"].as_str() {
            Some("taken") => (StatusCode::CONFLICT, "Username already taken").into_response(),
            Some(u) => axum::Json(json!({"token": "none", "username": u})).into_response(),
            None => StatusCode::BAD_REQUEST.into_response(),
        }
    };
    // SurrealDB's `/rpc`, for the picture queries (dataset.rs).
    let rpc = move |body: axum::Json<Value>| async move {
        let sql = body["params"][0].as_str().unwrap_or_default();
        pictures.queries.lock().unwrap().push(sql.to_string());
        let mut stored = pictures.picture.lock().unwrap();
        let result = if sql.starts_with("DELETE roll_sample") && body["params"][1]["user"] == "user-stuck" {
            return axum::Json(json!({"id": 1, "error": {"code": -32000, "message": "the store is down"}}));
        } else if sql.starts_with("DELETE roll_sample") {
            json!([{"id": 1}, {"id": 2}])
        } else if sql.starts_with("UPSERT") {
            *stored = Some((body["params"][1]["jpeg"].as_str().unwrap().to_string(), 1700));
            json!([1700])
        } else if sql.starts_with("DELETE") {
            *stored = None;
            Value::Null
        } else if sql.starts_with("SELECT VALUE") {
            stored.as_ref().map_or(Value::Null, |(_, v)| json!(v))
        } else {
            stored.as_ref().map_or(Value::Null, |(jpeg, v)| json!({"jpeg": jpeg, "version": v}))
        };
        axum::Json(json!({"id": 1, "result": [{"status": "OK", "result": result}]}))
    };
    let refuse = |status: StatusCode, code: &str| (status, axum::Json(json!({"error": code}))).into_response();
    let by_code = move |ok: Value| {
        move |body: axum::Json<Value>| {
            let ok = ok.clone();
            async move {
                match body["code"].as_str() {
                    Some("good") if body["password"].as_str() == Some("short") => refuse(StatusCode::BAD_REQUEST, "weak_password"),
                    Some("good") => axum::Json(ok).into_response(),
                    _ => refuse(StatusCode::NOT_FOUND, "invalid_code"),
                }
            }
        }
    };
    let by_token = move |ok: Value| {
        move |body: axum::Json<Value>| {
            let ok = ok.clone();
            async move {
                match body["token"].as_str() {
                    Some("all" | "github") => axum::Json(ok).into_response(),
                    Some("none") => refuse(StatusCode::TOO_MANY_REQUESTS, "too_soon"),
                    _ => refuse(StatusCode::UNAUTHORIZED, "logged_out"),
                }
            }
        }
    };
    // `stuck`: an account whose shared rolls can't be deleted (the store fails for it).
    let delete_check = move |body: axum::Json<Value>| async move {
        match body["code"].as_str() {
            Some("good") => axum::Json(json!({"username": "user-all", "user_id": 7})).into_response(),
            Some("stuck") => axum::Json(json!({"username": "user-stuck", "user_id": 8})).into_response(),
            _ => refuse(StatusCode::NOT_FOUND, "invalid_code"),
        }
    };
    let delete_confirm = move |body: axum::Json<Value>| async move {
        confirmed.queries.lock().unwrap().push(format!("auth delete/confirm {}", body["code"]));
        axum::Json(json!({}))
    };
    // Auth's own checks of a login, and a refusal this site doesn't know.
    let forgot = move |body: axum::Json<Value>| async move {
        match body["login"].as_str() {
            Some("brand-new-refusal") => refuse(StatusCode::UNPROCESSABLE_ENTITY, "something_new"),
            Some("auth-broke") => refuse(StatusCode::INTERNAL_SERVER_ERROR, "boom"),
            _ => axum::Json(json!({})).into_response(),
        }
    };
    let invite = |body: axum::Json<Value>| async move {
        match (body["code"].as_str(), body["token"].as_str()) {
            (_, Some("dead")) => StatusCode::UNAUTHORIZED.into_response(),
            (Some("join"), _) => axum::Json(json!({"role": "arcane_user"})).into_response(),
            _ => StatusCode::NOT_FOUND.into_response(),
        }
    };
    let app = axum::Router::new()
        .merge(stand_in_panels(auth_panels))
        .route("/internal/email/verify/send", post(by_token(json!({"email": "a@b.c"}))))
        .route("/internal/email/verify/confirm", post(by_code(json!({"username": "user-all"}))))
        .route("/internal/password/forgot", post(forgot))
        .route("/internal/password/reset", post(by_code(json!({"username": "user-all"}))))
        .route("/internal/account/delete/request", post(by_token(json!({"email": "a@b.c"}))))
        .route("/internal/account/delete/check", post(delete_check))
        .route("/internal/account/delete/confirm", post(delete_confirm))
        .route("/internal/account/delete/direct", post(by_token(json!({}))))
        .route("/internal/invite/redeem", post(invite))
        .route("/internal/invite/check", post(invite))
        .route("/internal/dataset/consent", post(|| async { axum::Json(json!({"share": true})) }))
        .route("/internal/dataset/consent/set", post(|| async { axum::Json(json!({})) }))
        .route("/signin", post(|| async { json!({"code": 200, "token": "token-1"}).to_string() }))
        .route("/rpc", post(rpc))
        .route("/internal/token/introspect", post(introspect))
        .route("/internal/profile", post(profile_of))
        .route("/internal/profile/display_name", post(display_name))
        .route("/internal/token/exchange", post(exchange))
        .route("/internal/register", post(register));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    api::trace::spawn_loop("test: stand-in auth", async move { axum::serve(listener, app).await.unwrap() });
    base
}

/// Auth's Ark, Enshrouded and admin endpoints. Ark: `start` starts, `stop` finds it stopped,
/// `restart` gets no result. Enshrouded: `restart` works, `update` is too soon, `update_now`
/// finds it busy. Admin: role 1 has admin rights, role 999 and invite 999 don't exist.
fn stand_in_panels(auth: Arc<Auth>) -> axum::Router {
    use axum::extract::OriginalUri;
    use axum::response::IntoResponse;
    use std::sync::atomic::Ordering;

    let ark_reply = |result: Value| axum::Json(json!({"restart_result": "ok", "command_result": result})).into_response();
    let refuse = |status: StatusCode, code: &str| (status, axum::Json(json!({"error": code}))).into_response();
    let ark = {
        let auth = auth.clone();
        move |uri: OriginalUri, body: axum::Json<Value>| {
            let auth = auth.clone();
            async move {
                auth.calls.lock().unwrap().push(format!("POST {} {}", uri.path(), body["cmd"]));
                if auth.failing.load(Ordering::SeqCst) {
                    return (StatusCode::INTERNAL_SERVER_ERROR, "Could not reach ark host").into_response();
                }
                if body["token"] != "all" {
                    return (StatusCode::FORBIDDEN, "No ark permission").into_response();
                }
                match body["cmd"].as_str() {
                    None => ark_reply(json!({"NumPlayers": 3})),
                    Some("start") => ark_reply(json!("Started")),
                    Some("stop") => ark_reply(json!("AlreadyStopped")),
                    _ => ark_reply(Value::Null),
                }
            }
        }
    };
    let enshrouded = {
        let auth = auth.clone();
        move |uri: OriginalUri, body: axum::Json<Value>| {
            let auth = auth.clone();
            async move {
                auth.calls.lock().unwrap().push(format!("POST {} {}", uri.path(), body["cmd"]));
                if auth.failing.load(Ordering::SeqCst) {
                    return refuse(StatusCode::SERVICE_UNAVAILABLE, "not_configured");
                }
                if body["token"] != "all" {
                    return refuse(StatusCode::FORBIDDEN, "forbidden");
                }
                match body["cmd"].as_str() {
                    None => axum::Json(json!({
                        "state": "Online", "players": {"online": 1, "max": 16, "sessions": [60]},
                        "version": "123", "uptime_secs": 3600, "latest_build_at": null, "update_pending": false,
                        "update_queued": false, "can_control": true,
                        "recent": [{"name": "user-all", "action": "restart", "at": 1000}], "next_command_at": null,
                    }))
                    .into_response(),
                    Some("restart") => axum::Json(json!({})).into_response(),
                    Some("update") => refuse(StatusCode::TOO_MANY_REQUESTS, "too_soon"),
                    _ => refuse(StatusCode::CONFLICT, "busy"),
                }
            }
        }
    };
    // Every admin endpoint: logs the call, then answers `reply` unless failing.
    let admin = move |reply: fn(&str, Option<&Value>) -> axum::response::Response| {
        let auth = auth.clone();
        move |method: axum::http::Method, uri: OriginalUri, body: Option<axum::Json<Value>>| {
            let auth = auth.clone();
            async move {
                auth.calls.lock().unwrap().push(format!("{method} {}", uri.0));
                if auth.failing.load(Ordering::SeqCst) {
                    return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                }
                reply(uri.path(), body.as_ref().map(|b| &b.0))
            }
        }
    };
    let users = |_: &str, _: Option<&Value>| {
        let user = json!({"id": 5, "username": "ann", "email": null, "roles": [{"id": 2, "name": "arcane_user"}]});
        axum::Json(json!({"items": [user], "total": 1})).into_response()
    };
    let roles = |_: &str, _: Option<&Value>| {
        let permissions = [json!({"id": 3, "name": "arcane"}), json!({"id": 9, "name": "brand_new"})];
        axum::Json(json!({"items": [{"id": 2, "name": "arcane_user", "permissions": permissions}], "total": 1}))
            .into_response()
    };
    let all_roles = |_: &str, _: Option<&Value>| axum::Json(json!([{"id": 2, "name": "arcane_user"}])).into_response();
    let permissions =
        |_: &str, _: Option<&Value>| axum::Json(json!([{"id": 3, "name": "arcane"}, {"id": 9, "name": "brand_new"}])).into_response();
    // User 404 doesn't exist; user 400 is refused for a reason this site doesn't know.
    let done = |path: &str, _: Option<&Value>| match path {
        p if p.contains("/users/404/") => (StatusCode::NOT_FOUND, axum::Json(json!({"error": "no_user"}))).into_response(),
        p if p.contains("/users/400/") => (StatusCode::BAD_REQUEST, axum::Json(json!({"error": "something_new"}))).into_response(),
        _ => StatusCode::NO_CONTENT.into_response(),
    };
    let invites = |_: &str, body: Option<&Value>| {
        let invite = json!({
            "id": 4, "role_id": 2, "role": "arcane_user", "note": null, "created_by": "user-all",
            "created_at": "2026-10-19T00:00:00Z", "expires_at": "2026-10-26T00:00:00Z", "max_uses": null,
            "uses": 0, "revoked": false, "live": true, "joined": [],
        });
        let Some(body) = body else { return axum::Json(json!([invite])).into_response() };
        let refuse = |status: StatusCode, code: &str| (status, axum::Json(json!({"error": code}))).into_response();
        match (body["token"].as_str(), body["role_id"].as_i64(), body["days"].as_i64()) {
            (Some("all"), Some(1), _) => refuse(StatusCode::BAD_REQUEST, "admin_role"),
            (Some("all"), Some(999), _) => refuse(StatusCode::NOT_FOUND, "no_role"),
            (Some("all"), _, Some(1..=365)) => {
                let mut made = invite;
                made["link"] = json!("https://milesstorm.test/invite#code");
                axum::Json(made).into_response()
            }
            (Some("all"), _, _) => refuse(StatusCode::BAD_REQUEST, "invalid_invite"),
            _ => StatusCode::UNAUTHORIZED.into_response(),
        }
    };
    let revoke = |path: &str, _: Option<&Value>| {
        if path.ends_with("/999") {
            (StatusCode::NOT_FOUND, axum::Json(json!({"error": "no_invite"}))).into_response()
        } else {
            StatusCode::NO_CONTENT.into_response()
        }
    };
    axum::Router::new()
        .route("/internal/ark/num_players", post(ark.clone()))
        .route("/internal/ark/command", post(ark))
        .route("/internal/enshrouded/status", post(enshrouded.clone()))
        .route("/internal/enshrouded/command", post(enshrouded))
        .route("/internal/admin/users", axum::routing::get(admin(users)))
        .route("/internal/admin/users/{user_id}/roles/{role_id}", post(admin(done)).delete(admin(done)))
        .route("/internal/admin/roles", axum::routing::get(admin(roles)))
        .route("/internal/admin/roles/all", axum::routing::get(admin(all_roles)))
        .route("/internal/admin/roles/{role_id}/permissions/{permission_id}", post(admin(done)).delete(admin(done)))
        .route("/internal/admin/permissions", axum::routing::get(admin(permissions)))
        .route("/internal/admin/invites", axum::routing::get(admin(invites)).post(admin(invites)))
        .route("/internal/admin/invites/{invite_id}", axum::routing::delete(admin(revoke)))
}

/// The API as the server mounts it, behind an in-memory session layer, and a browser with
/// a cookie jar of one. A `Site` holds [`AUTH_ENV`] until dropped: drop one before making
/// the next in the same test, or the test waits for itself.
struct Site {
    app: axum::Router,
    auth: Arc<Auth>,
    cookie: Option<String>,
    _env: tokio::sync::MutexGuard<'static, ()>,
}

struct Reply {
    status: StatusCode,
    content_type: Option<String>,
    bytes: Vec<u8>,
    body: Value,
}

impl Site {
    async fn new() -> Self {
        Self::serving(super::router()).await
    }

    /// The site with `app` instead of the API as the server mounts it.
    async fn serving(app: axum::Router) -> Self {
        let env = AUTH_ENV.lock().await;
        let auth = Arc::new(Auth::default());
        let url = stand_in_auth(auth.clone()).await;
        std::env::set_var("AUTH_SERVICE_URL", &url);
        std::env::set_var("BFF_SERVICE_SECRET", "test-only");
        crate::dataset::install_for_tests(None);
        // A session logged in with the token in the body, as a login leaves it.
        let seed = |session: Session, token: String| async move {
            session.insert("opaque_token", &token).await.unwrap();
            session.insert("username", format!("user-{token}")).await.unwrap();
        };
        let app = app
            .route("/test/seed", post(seed))
            .layer(SessionManagerLayer::new(MemoryStore::default()).with_secure(false));
        Self { app, auth, cookie: None, _env: env }
    }

    async fn logged_in(token: &str) -> Self {
        Self::new().await.log_in(token).await
    }

    async fn log_in(mut self, token: &str) -> Self {
        assert_eq!(self.send(Method::POST, "/test/seed", Some(SITE), Some(("text/plain", token.into()))).await.status, StatusCode::OK);
        self
    }

    async fn send(&mut self, method: Method, path: &str, origin: Option<&str>, body: Option<(&str, Vec<u8>)>) -> Reply {
        let mut req = Request::builder().method(method).uri(path).header(header::HOST, HOST);
        if let Some(origin) = origin {
            req = req.header(header::ORIGIN, origin);
        }
        if let Some(cookie) = &self.cookie {
            req = req.header(header::COOKIE, cookie);
        }
        let req = match body {
            Some((content_type, bytes)) => req.header(header::CONTENT_TYPE, content_type).body(Body::from(bytes)),
            None => req.body(Body::empty()),
        };
        let res = self.app.clone().oneshot(req.unwrap()).await.unwrap();
        if let Some(set) = res.headers().get(header::SET_COOKIE) {
            self.cookie = set.to_str().unwrap().split(';').next().map(str::to_string);
        }
        let status = res.status();
        let content_type = res.headers().get(header::CONTENT_TYPE).map(|v| v.to_str().unwrap().to_string());
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap().to_vec();
        let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        Reply { status, content_type, bytes, body }
    }

    /// Pictures on, stored by the stand-in.
    fn with_pictures(self) -> Self {
        let url = std::env::var("AUTH_SERVICE_URL").unwrap();
        crate::dataset::install_for_tests(Some(crate::dataset::Dataset::for_tests(url, "pw")));
        self
    }

    async fn get(&mut self, path: &str) -> Reply {
        self.send(Method::GET, path, None, None).await
    }

    async fn json(&mut self, method: Method, path: &str, body: Value) -> Reply {
        self.send(method, path, Some(SITE), Some(("application/json", body.to_string().into_bytes()))).await
    }
}

fn envelope(reply: &Reply, status: u16, error: &str) {
    assert_eq!(reply.status.as_u16(), status, "{}", reply.body);
    assert_eq!(reply.body["error"], error, "{}", reply.body);
    assert!(reply.body["message"].as_str().is_some_and(|m| !m.is_empty()), "{}", reply.body);
    assert_eq!(reply.body.as_object().unwrap().len(), 2, "only error and message: {}", reply.body);
}

#[tokio::test]
async fn a_logged_out_session_is_no_viewer_and_costs_no_call_to_auth() {
    let mut site = Site::new().await;
    let reply = site.get("/api/session").await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body, json!({"viewer": null}));
    assert!(site.auth.introspected.lock().unwrap().is_empty());
}

#[tokio::test]
async fn the_viewer_has_their_shown_name_and_only_known_permissions() {
    let mut site = Site::logged_in("all").await;
    let reply = site.get("/api/session").await;
    assert_eq!(reply.status, StatusCode::OK);
    let viewer = &reply.body["viewer"];
    assert_eq!(viewer["username"], "user-all");
    assert_eq!(viewer["shown_name"], "Shown");
    assert_eq!(viewer["permissions"], json!(["llama", "enshrouded", "arcane", "manage_permissions"]));
    assert_eq!(site.auth.introspected("all"), 1);
}

#[tokio::test]
async fn a_token_auth_dropped_logs_the_session_out_and_auth_down_keeps_it() {
    let mut site = Site::logged_in("dead").await;
    assert_eq!(site.get("/api/session").await.body, json!({"viewer": null}));
    // The session is cleared: no more calls to auth.
    assert_eq!(site.get("/api/session").await.body, json!({"viewer": null}));
    assert_eq!(site.auth.introspected("dead"), 1);
    drop(site); // it holds AUTH_ENV

    let mut site = Site::logged_in("down").await;
    let reply = site.get("/api/session").await;
    assert_eq!(
        reply.body["viewer"],
        json!({"username": "user-down", "shown_name": "user-down", "picture_url": null, "permissions": []})
    );
    let reply = site.get("/api/session").await;
    assert_eq!(reply.body["viewer"]["username"], "user-down", "auth being down logs no one out");
}

#[tokio::test]
async fn account_routes_need_a_login() {
    let mut site = Site::new().await;
    envelope(&site.get("/api/account").await, 401, "logged_out");
    envelope(&site.json(Method::PUT, "/api/account/display-name", json!({"name": "x"})).await, 401, "logged_out");
    envelope(&site.send(Method::DELETE, "/api/account/picture", Some(SITE), None).await, 401, "logged_out");
    envelope(&site.get("/api/account/picture").await, 401, "logged_out");
    envelope(&site.get("/api/profile/picture").await, 401, "logged_out");
    envelope(&site.send(Method::POST, "/api/profile/picture", Some(SITE), Some(("image/png", vec![1]))).await, 401, "logged_out");

    // Auth down: the login can't be checked.
    drop(site); // it holds AUTH_ENV
    let mut site = Site::logged_in("down").await;
    envelope(&site.get("/api/account").await, 503, "unavailable");
}

#[tokio::test]
async fn a_logged_in_account_request_asks_auth_once_who_is_logged_in() {
    let (exporter, _guard) = crate::trace_tests::exporting();
    let mut site = Site::logged_in("none").await;
    let reply = site.get("/api/account").await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.body["username"], "user-none");
    assert_eq!(reply.body["shown_name"], "Shown");
    // The access layer and the handler share one resolution.
    assert_eq!(site.auth.introspected("none"), 1);
    // The same span as the server function's.
    crate::trace_tests::span(&exporter, "bff.get_account").await;
}

#[tokio::test]
async fn the_display_name_is_set_or_refused_with_a_message() {
    let mut site = Site::logged_in("all").await;
    let reply = site.json(Method::PUT, "/api/account/display-name", json!({"name": "Miles"})).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.body["shown_name"], "Miles");

    let reply = site.json(Method::PUT, "/api/account/display-name", json!({"name": "x".repeat(41)})).await;
    envelope(&reply, 400, "invalid");
    assert_eq!(reply.body["message"], "Use at most 40 characters, without line breaks or hidden characters.");
}

#[tokio::test]
async fn bad_request_bodies_are_invalid_envelopes() {
    let mut site = Site::logged_in("all").await;
    let reply = site.json(Method::PUT, "/api/account/display-name", json!({"nom": "x"})).await;
    envelope(&reply, 422, "invalid");
    // serde's text (field names, types) stays in the log.
    assert_eq!(reply.body["message"], "The request body isn't valid.");
    let not_json = Some(("application/json", b"{".to_vec()));
    envelope(&site.send(Method::PUT, "/api/account/display-name", Some(SITE), not_json).await, 400, "invalid");
    let no_type = Some(("text/plain", b"{}".to_vec()));
    envelope(&site.send(Method::PUT, "/api/account/display-name", Some(SITE), no_type).await, 415, "invalid");

    // Over the upload limit: 413, which the Dioxus profile page shows as "too large".
    let big = Some(("image/png", vec![0; crate::account::PICTURE_MAX_UPLOAD + 1]));
    envelope(&site.send(Method::POST, "/api/account/picture", Some(SITE), big).await, 413, "invalid");
}

#[tokio::test]
async fn logging_in_answers_wrong_passwords_and_outages_apart() {
    let (exporter, _guard) = crate::trace_tests::exporting();
    let mut site = Site::new().await;
    let reply = site.json(Method::POST, "/api/session/login", json!({"username": "tester", "password": "wrong"})).await;
    envelope(&reply, 401, "invalid");
    assert_eq!(reply.body["message"], "Incorrect username or password.");
    let reply = site.json(Method::POST, "/api/session/login", json!({"username": "outage", "password": "x"})).await;
    envelope(&reply, 502, "upstream");

    let reply = site.json(Method::POST, "/api/session/login", json!({"username": "tester", "password": "right"})).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.body, json!({"username": "tester"}));
    assert_eq!(site.get("/api/session").await.body["viewer"]["username"], "tester");
    crate::trace_tests::span(&exporter, "bff.login_password").await;

    let reply = site.send(Method::DELETE, "/api/session", Some(SITE), None).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    assert_eq!(site.get("/api/session").await.body, json!({"viewer": null}));
    crate::trace_tests::span(&exporter, "bff.logout").await;
}

#[tokio::test]
async fn registering_logs_in_or_says_what_is_taken() {
    let mut site = Site::new().await;
    let taken = json!({"username": "taken", "email": "a@b.c", "password": "long enough"});
    let reply = site.json(Method::POST, "/api/session/register", taken).await;
    envelope(&reply, 409, "conflict");
    assert_eq!(reply.body["message"], "Username already taken");

    let new = json!({"username": "newcomer", "email": "a@b.c", "password": "long enough"});
    let reply = site.json(Method::POST, "/api/session/register", new).await;
    assert_eq!(reply.body, json!({"username": "newcomer"}));
}

#[tokio::test]
async fn changes_from_another_site_are_refused() {
    let mut site = Site::logged_in("all").await;
    let evil = Some("https://evil.example");
    let login = json!({"username": "tester", "password": "right"}).to_string().into_bytes();
    envelope(&site.send(Method::POST, "/api/session/login", evil, Some(("application/json", login.clone()))).await, 403, "forbidden");
    envelope(&site.send(Method::DELETE, "/api/session", evil, None).await, 403, "forbidden");
    envelope(&site.send(Method::DELETE, "/api/account/picture", evil, None).await, 403, "forbidden");
    envelope(&site.send(Method::POST, "/api/profile/picture", evil, Some(("image/png", vec![1]))).await, 403, "forbidden");
    envelope(&site.send(Method::DELETE, "/api/account/sharing/data", evil, None).await, 403, "forbidden");
    for (path, body) in [
        ("/api/deletion/confirm", json!({"code": "good"})),
        ("/api/password/reset", json!({"code": "good", "password": "long enough"})),
        ("/api/invites/open", json!({"code": "join"})),
        ("/api/account/deletion/direct", json!({"confirm": "user-all"})),
    ] {
        let body = Some(("application/json", body.to_string().into_bytes()));
        envelope(&site.send(Method::POST, path, evil, body).await, 403, "forbidden");
    }
    // Still logged in, and nothing was deleted.
    assert!(site.auth.queries.lock().unwrap().is_empty());
    assert_eq!(site.get("/api/session").await.body["viewer"]["username"], "user-all");
    // Reads are fine from anywhere (the browser keeps the answer from the other site), and
    // requests without an Origin aren't from a browser.
    assert_eq!(site.send(Method::GET, "/api/session", evil, None).await.status, StatusCode::OK);
    let no_origin = site.send(Method::POST, "/api/session/login", None, Some(("application/json", login))).await;
    assert_eq!(no_origin.status, StatusCode::OK, "{}", no_origin.body);
}

#[test]
fn unreadable_addresses_are_ours_when_the_route_is_wrong() {
    let ours = super::rejected_address(StatusCode::INTERNAL_SERVER_ERROR).0;
    assert_eq!((ours.status(), ours.error), (500, api_types::ErrorCode::Internal));
    let theirs = super::rejected_address(StatusCode::BAD_REQUEST).0;
    assert_eq!((theirs.status(), theirs.error), (400, api_types::ErrorCode::Invalid));
}

#[test]
fn errors_answer_their_status_with_the_envelope() {
    use axum::response::IntoResponse;
    let error = api_types::ApiError::new(api_types::ErrorCode::Invalid, "too big").with_status(413);
    assert_eq!(super::ApiError(error).into_response().status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(super::ApiError(api_types::ApiError::internal()).into_response().status(), StatusCode::INTERNAL_SERVER_ERROR);
}

/// Dioxus routes each file of its public dir (as the image has it: dx's bundle). Nothing else
/// reads DIOXUS_PUBLIC_PATH; it is put back, and the directory removed, either way. Tests that
/// make one hold [`PUBLIC_ENV`].
struct Public(std::path::PathBuf, Option<std::ffi::OsString>, #[allow(dead_code)] std::sync::MutexGuard<'static, ()>);

static PUBLIC_ENV: Mutex<()> = Mutex::new(());

impl Public {
    fn new(name: &str) -> Self {
        let guard = PUBLIC_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let public = Public(
            std::env::temp_dir().join(format!("web-public-{name}-{}", std::process::id())),
            std::env::var_os("DIOXUS_PUBLIC_PATH"),
            guard,
        );
        std::fs::create_dir_all(public.0.join("assets")).unwrap();
        std::fs::write(public.0.join("assets/web.js"), "").unwrap();
        std::fs::write(public.0.join("favicon.ico"), "").unwrap();
        std::env::set_var("DIOXUS_PUBLIC_PATH", &public.0);
        public
    }
}

impl Drop for Public {
    fn drop(&mut self) {
        match self.1.take() {
            Some(old) => std::env::set_var("DIOXUS_PUBLIC_PATH", old),
            None => std::env::remove_var("DIOXUS_PUBLIC_PATH"),
        }
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn the_api_mounts_beside_the_dioxus_app() {
    use dioxus::prelude::*;
    let _public = Public::new("mount");
    // axum panics on overlapping routes when the router is built.
    let _ = axum::Router::new()
        .serve_dioxus_application(ServeConfig::default(), crate::App)
        .route("/ws/arcane", axum::routing::get(|| async {}))
        .merge(super::router());
}

/// The Dioxus pages' calls over HTTP, as the browser makes them: the command enums go over the
/// wire as auth's names and come back.
#[tokio::test]
async fn panel_server_functions_over_http() {
    use dioxus::prelude::*;
    let public = Public::new("bff");
    let app = axum::Router::new().serve_dioxus_application(ServeConfig::default(), crate::App);
    drop(public);
    let mut site = Site::serving(app).await.log_in("all").await;
    let path = |name: &str| {
        let fns = ServerFunction::collect();
        let found = fns.iter().find(|f| f.path().strip_prefix("/bff/").and_then(|p| p.strip_prefix(name)).is_some_and(|hash| hash.bytes().all(|b| b.is_ascii_digit())));
        found.map(|f| f.path()).unwrap_or_else(|| panic!("no server function {name}"))
    };

    let reply = site.json(Method::POST, path("ark_command"), json!({"cmd": "start"})).await;
    assert_eq!((reply.status, &reply.body), (StatusCode::OK, &json!("started")));
    let reply = site.json(Method::POST, path("enshrouded_command"), json!({"cmd": "update_now"})).await;
    assert_eq!(reply.status, StatusCode::CONFLICT, "{}", reply.body);
    let reply = site.json(Method::POST, path("enshrouded_command"), json!({"cmd": "restart"})).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(
        site.calls(),
        [
            "POST /internal/ark/command \"start\"",
            "POST /internal/enshrouded/command \"update_now\"",
            "POST /internal/enshrouded/command \"restart\"",
        ]
    );
    // A name that isn't a command doesn't reach auth (Dioxus answers arguments it can't read
    // with a 500).
    let reply = site.json(Method::POST, path("ark_command"), json!({"cmd": "explode"})).await;
    assert!(!reply.status.is_success(), "{}", reply.status);
    assert_eq!(site.calls().len(), 3);
}

#[test]
fn the_document_has_every_route_and_leaves_the_old_picture_path_out() {
    let doc: Value = serde_json::from_str(&super::openapi_json()).unwrap();
    let mut routes: Vec<String> = Vec::new();
    for (path, item) in doc["paths"].as_object().unwrap() {
        for method in item.as_object().unwrap().keys() {
            routes.push(format!("{} {path}", method.to_uppercase()));
        }
    }
    routes.sort();
    assert_eq!(
        routes,
        [
            "DELETE /api/account/picture",
            "DELETE /api/account/sharing/data",
            "DELETE /api/admin/invites/{invite_id}",
            "DELETE /api/admin/roles/{role_id}/permissions/{permission_id}",
            "DELETE /api/admin/users/{user_id}/roles/{role_id}",
            "DELETE /api/session",
            "GET /api/account",
            "GET /api/account/picture",
            "GET /api/account/sharing",
            "GET /api/admin/invites",
            "GET /api/admin/permissions",
            "GET /api/admin/roles",
            "GET /api/admin/roles/all",
            "GET /api/admin/users",
            "GET /api/arcane/me",
            "GET /api/arcane/rolls",
            "GET /api/ark/players",
            "GET /api/enshrouded",
            "GET /api/session",
            "POST /api/account/deletion/direct",
            "POST /api/account/deletion/request",
            "POST /api/account/email/send-confirmation",
            "POST /api/account/picture",
            "POST /api/admin/invites",
            "POST /api/arcane/flag",
            "POST /api/ark/command",
            "POST /api/deletion/check",
            "POST /api/deletion/confirm",
            "POST /api/email/confirm",
            "POST /api/enshrouded/command",
            "POST /api/invites/open",
            "POST /api/password/forgot",
            "POST /api/password/reset",
            "POST /api/session/login",
            "POST /api/session/register",
            "PUT /api/account/display-name",
            "PUT /api/account/sharing",
            "PUT /api/admin/roles/{role_id}/permissions/{permission_id}",
            "PUT /api/admin/users/{user_id}/roles/{role_id}",
        ]
    );
    // Parameters: the admin routes' ids (named like the path's segments, from the path
    // structs) and the listings' paging. One-time codes go in bodies, never the URL.
    let mut parameters: Vec<String> = Vec::new();
    for (path, item) in doc["paths"].as_object().unwrap() {
        for (method, op) in item.as_object().unwrap() {
            for p in op["parameters"].as_array().into_iter().flatten() {
                let name = p["name"].as_str().unwrap();
                if p["in"] == "path" {
                    assert!(path.contains(&format!("{{{name}}}")), "{method} {path}: {name}");
                }
                parameters.push(format!("{} {path} {}:{name}", method.to_uppercase(), p["in"].as_str().unwrap()));
            }
        }
    }
    parameters.sort();
    assert_eq!(
        parameters,
        [
            "DELETE /api/admin/invites/{invite_id} path:invite_id",
            "DELETE /api/admin/roles/{role_id}/permissions/{permission_id} path:permission_id",
            "DELETE /api/admin/roles/{role_id}/permissions/{permission_id} path:role_id",
            "DELETE /api/admin/users/{user_id}/roles/{role_id} path:role_id",
            "DELETE /api/admin/users/{user_id}/roles/{role_id} path:user_id",
            "GET /api/admin/roles query:limit",
            "GET /api/admin/roles query:page",
            "GET /api/admin/roles query:search",
            "GET /api/admin/users query:limit",
            "GET /api/admin/users query:page",
            "GET /api/admin/users query:search",
            "PUT /api/admin/roles/{role_id}/permissions/{permission_id} path:permission_id",
            "PUT /api/admin/roles/{role_id}/permissions/{permission_id} path:role_id",
            "PUT /api/admin/users/{user_id}/roles/{role_id} path:role_id",
            "PUT /api/admin/users/{user_id}/roles/{role_id} path:user_id",
        ]
    );
    let schemas = &doc["components"]["schemas"];
    // Literal unions for TypeScript.
    assert_eq!(schemas["KnownPermission"]["enum"], json!(["llama", "enshrouded", "arcane", "manage_permissions"]));
    assert_eq!(schemas["Bootstrap"]["required"], json!(["viewer"]));
    assert_eq!(schemas["ArkCommand"]["enum"], json!(["start", "stop", "restart"]));
    assert_eq!(schemas["EnshroudedCommand"]["enum"], json!(["restart", "update", "update_now"]));
    // The camera socket's and the roll stream's messages, tagged by `type`.
    let tags: Vec<&Value> = schemas["ArcaneServerMsg"]["oneOf"].as_array().unwrap().iter().map(|v| &v["properties"]["type"]["const"]).collect();
    assert_eq!(tags, [&json!("frame"), &json!("roll"), &json!("error")]);
    for name in ["Roll", "RolledDie", "Frame", "Detection", "ArcaneMe", "FlagRequest"] {
        assert!(schemas[name].is_object(), "{name}");
    }
    let stream = &doc["paths"]["/api/arcane/rolls"]["get"]["responses"]["200"]["content"]["text/event-stream"];
    assert_eq!(stream["schema"]["$ref"], "#/components/schemas/ArcaneServerMsg");
}

#[tokio::test]
async fn without_the_account_the_viewer_is_shown_by_username() {
    let mut site = Site::logged_in("lost").await;
    let reply = site.get("/api/session").await;
    assert_eq!(
        reply.body["viewer"],
        json!({"username": "user-lost", "shown_name": "user-lost", "picture_url": null, "permissions": []})
    );
}

#[tokio::test]
async fn login_spans_fail_on_outages_not_on_wrong_passwords() {
    use opentelemetry::trace::Status;
    let (exporter, _guard) = crate::trace_tests::exporting();
    let mut site = Site::new().await;
    site.json(Method::POST, "/api/session/login", json!({"username": "tester", "password": "wrong"})).await;
    assert_eq!(crate::trace_tests::span(&exporter, "bff.login_password").await.status, Status::Unset);
    exporter.reset();
    site.json(Method::POST, "/api/session/login", json!({"username": "outage", "password": "x"})).await;
    let span = crate::trace_tests::span(&exporter, "bff.login_password").await;
    assert!(matches!(span.status, Status::Error { .. }), "{:?}", span.status);
}

#[tokio::test]
async fn a_picture_is_uploaded_served_and_removed() {
    let mut site = Site::logged_in("all").await.with_pictures();
    let mut png = std::io::Cursor::new(Vec::new());
    image::RgbaImage::from_pixel(40, 30, image::Rgba([200, 0, 0, 255])).write_to(&mut png, image::ImageFormat::Png).unwrap();

    let reply = site.send(Method::POST, "/api/account/picture", Some(SITE), Some(("image/png", png.into_inner()))).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.body, json!({"version": 1700}));

    let account = site.get("/api/account").await.body;
    assert_eq!(account["pictures_available"], true);
    assert_eq!(account["picture_url"], "/api/account/picture?u=user-all&v=1700");
    assert_eq!(site.get("/api/session").await.body["viewer"]["picture_url"], account["picture_url"]);

    for path in ["/api/account/picture?u=user-all&v=1700", "/api/profile/picture"] {
        let reply = site.get(path).await;
        assert_eq!(reply.status, StatusCode::OK, "{path}");
        assert_eq!(reply.content_type.as_deref(), Some("image/jpeg"), "{path}");
        assert_eq!(image::load_from_memory(&reply.bytes).unwrap().width(), 256, "{path}");
    }

    let reply = site.send(Method::DELETE, "/api/account/picture", Some(SITE), None).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.body["picture_version"], Value::Null);
    envelope(&site.get("/api/account/picture").await, 404, "not_found");
}

#[tokio::test]
async fn pictures_need_auth_to_answer() {
    let mut site = Site::logged_in("down").await.with_pictures();
    envelope(&site.get("/api/account/picture").await, 503, "unavailable");
    envelope(&site.send(Method::POST, "/api/account/picture", Some(SITE), Some(("image/png", vec![1]))).await, 503, "unavailable");
}

#[tokio::test]
async fn origins_let_in_for_arcane_are_not_let_into_the_api() {
    let mut site = Site::serving(super::router_with("https://dev.example")).await.log_in("all").await;
    let reply = site.send(Method::DELETE, "/api/session", Some("https://dev.example"), None).await;
    envelope(&reply, 403, "forbidden");
}

#[test]
fn dev_proxy_origins_count_in_debug_builds_only() {
    let headers = |origin: &str| {
        let mut h = axum::http::HeaderMap::new();
        h.insert(header::HOST, HOST.parse().unwrap());
        h.insert(header::ORIGIN, origin.parse().unwrap());
        h
    };
    let dev = "http://localhost:4321, http://localhost:8080";
    // Release: only this host, over HTTPS; the dev list is ignored.
    assert!(super::origin_allowed(&headers(SITE), dev, false));
    assert!(!super::origin_allowed(&headers("http://localhost:4321"), dev, false));
    assert!(!super::origin_allowed(&headers("http://milesstorm.test"), dev, false));
    // Debug: the dev proxies too, and plain HTTP for this host.
    assert!(super::origin_allowed(&headers("http://localhost:4321"), dev, true));
    assert!(super::origin_allowed(&headers("http://localhost:8080"), dev, true));
    assert!(super::origin_allowed(&headers("http://milesstorm.test"), dev, true));
    assert!(!super::origin_allowed(&headers("https://evil.example"), dev, true));
}

#[tokio::test]
async fn unknown_api_paths_and_methods_get_the_envelope_and_plain_routes_still_match() {
    let mut site = Site::new().await;
    site.app = site.app.clone().route("/api/test/plain", axum::routing::get(|| async { "plain" }));
    envelope(&site.get("/api/nope").await, 404, "not_found");
    envelope(&site.send(Method::POST, "/api/account/nope", Some(SITE), None).await, 404, "not_found");
    envelope(&site.send(Method::PATCH, "/api/session", Some(SITE), None).await, 405, "invalid");
    let plain = site.get("/api/test/plain").await;
    assert_eq!((plain.status, plain.bytes.as_slice()), (StatusCode::OK, &b"plain"[..]));
}

#[test]
fn access_by_login_and_permission() {
    use api_types::{Access, ErrorCode, KnownPermission, Permission};
    let user = |permissions| super::Viewer(api::Viewer::User { username: "u".into(), token: "t".into(), permissions });
    let status = |access, viewer: &super::Viewer| super::allowed(access, viewer).map_err(|e| (e.0.status(), e.0.error));

    let anonymous = super::Viewer(api::Viewer::Anonymous);
    let degraded = super::Viewer(api::Viewer::Degraded { username: "u".into() });
    let arcane = Access::Perm(KnownPermission::Arcane);
    for access in [Access::LoggedIn, arcane] {
        assert_eq!(status(access, &anonymous), Err((401, ErrorCode::LoggedOut)));
        assert_eq!(status(access, &degraded), Err((503, ErrorCode::Unavailable)));
    }
    let plain = user(vec![Permission::Other("arcane2".into())]);
    assert_eq!(status(Access::LoggedIn, &plain), Ok(()));
    assert_eq!(status(arcane, &plain), Err((403, ErrorCode::Forbidden)));
    assert_eq!(status(arcane, &user(vec![Permission::Known(KnownPermission::Arcane)])), Ok(()));
}

// ---- The Dioxus server functions over the same functions ----

/// Runs a server function in a request with `session`, as Dioxus does.
async fn in_request<F: std::future::Future>(session: &Session, call: F) -> F::Output {
    use dioxus::fullstack::FullstackContext;
    let (mut parts, ()) = Request::builder().uri("/bff/fn").header(header::HOST, HOST).body(()).unwrap().into_parts();
    parts.extensions.insert(session.clone());
    FullstackContext::new(parts).scope(call).await
}

async fn session_of(token: Option<&str>) -> Session {
    let session = Session::new(None, Arc::new(MemoryStore::default()), None);
    if let Some(token) = token {
        session.insert("opaque_token", token).await.unwrap();
        session.insert("username", format!("user-{token}")).await.unwrap();
    }
    session
}

#[tokio::test]
async fn login_status_and_permissions_server_functions() {
    use ui::data_dir::LoginStatus;
    let _site = Site::new().await;

    let all = session_of(Some("all")).await;
    assert_eq!(in_request(&all, api::check_login_status()).await.unwrap(), LoginStatus::LoggedIn("user-all".into()));
    // Unknown names too: the Dioxus pages show what auth says.
    assert_eq!(
        in_request(&all, api::get_my_permissions()).await.unwrap(),
        ["llama", "enshrouded", "arcane", "manage_permissions", "brand_new"]
    );

    let down = session_of(Some("down")).await;
    assert_eq!(in_request(&down, api::check_login_status()).await.unwrap(), LoginStatus::LoggedIn("user-down".into()));
    assert!(in_request(&down, api::get_my_permissions()).await.unwrap().is_empty());

    let dead = session_of(Some("dead")).await;
    assert_eq!(in_request(&dead, api::check_login_status()).await.unwrap(), LoginStatus::LoggedOut);
    assert_eq!(dead.get::<String>("username").await.unwrap(), None, "the session is cleared");
    assert!(in_request(&session_of(None).await, api::get_my_permissions()).await.unwrap().is_empty());

    in_request(&all, api::logout()).await.unwrap();
    assert_eq!(all.get::<String>("username").await.unwrap(), None);
}

#[tokio::test]
async fn account_server_functions_logged_out_say_so() {
    use dioxus::prelude::ServerFnError;
    let _site = Site::new().await;
    let logged_out = session_of(None).await;
    let expect = |r: Result<api_types::AccountInfo, ServerFnError>| match r {
        Err(ServerFnError::ServerError { code, message, .. }) => assert_eq!((code, message.as_str()), (401, "You've been logged out. Log in again.")),
        other => panic!("{other:?}"),
    };
    expect(in_request(&logged_out, crate::account::get_account()).await);
    expect(in_request(&logged_out, crate::account::set_account_display_name("x".into())).await);
    // A token auth dropped meanwhile: auth's 401, the same answer.
    expect(in_request(&session_of(Some("dead")).await, crate::account::get_account()).await);
}

// ---- Account emails, invites and sharing (PR 2b) ----

#[tokio::test]
async fn emailed_codes_work_logged_out() {
    let mut site = Site::new().await.with_pictures();
    let good = json!({"code": "good"});
    let reply = site.json(Method::POST, "/api/email/confirm", good.clone()).await;
    assert_eq!((reply.status, &reply.body), (StatusCode::OK, &json!({"username": "user-all"})));
    let reply = site.json(Method::POST, "/api/deletion/check", good.clone()).await;
    assert_eq!((reply.status, &reply.body), (StatusCode::OK, &json!({"username": "user-all"})));

    let reply = site.json(Method::POST, "/api/deletion/confirm", good).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.body, json!({"username": "user-all", "logged_out": false}));
    // The pictures went before the account.
    let order = |q: &str| site.auth.queries.lock().unwrap().iter().position(|x| x.starts_with(q));
    let rolls = order("DELETE roll_sample").expect("shared rolls deleted");
    let picture = order("DELETE type::record('profile_picture'").expect("picture deleted");
    let account = order("auth delete/confirm").expect("account deleted");
    assert!(rolls < picture && picture < account, "{:?}", site.auth.queries.lock().unwrap());

    // A code auth doesn't know is auth's 404; one that can't be a code isn't sent to auth.
    for code in ["used", "a b", &"a".repeat(65)] {
        for path in ["/api/email/confirm", "/api/deletion/check", "/api/deletion/confirm", "/api/password/reset"] {
            let reply = site.json(Method::POST, path, json!({"code": code, "password": "long enough"})).await;
            envelope(&reply, 404, "gone");
            assert_eq!(reply.body["message"], "This link has expired or was already used. Ask for a new one.");
        }
    }
}

#[tokio::test]
async fn an_account_whose_pictures_cant_be_removed_is_not_deleted() {
    let mut site = Site::new().await.with_pictures();
    let reply = site.json(Method::POST, "/api/deletion/confirm", json!({"code": "stuck"})).await;
    envelope(&reply, 502, "upstream");
    assert_eq!(reply.body["message"], "Your data couldn't be removed right now, so nothing was deleted. Try again in a few minutes.");
    let queries = site.auth.queries.lock().unwrap().clone();
    assert!(!queries.iter().any(|q| q.starts_with("auth delete/confirm")), "auth was asked to delete: {queries:?}");
}

#[tokio::test]
async fn auths_refusals_keep_their_status() {
    let mut site = Site::new().await;
    let forgot = |login: &str| json!({"login": login});
    let reply = site.json(Method::POST, "/api/password/forgot", forgot(&"a".repeat(255))).await;
    envelope(&reply, 400, "invalid");
    assert_eq!(reply.body["message"], "Enter your username or email.");
    // One this site doesn't know is still an answer, with auth's status.
    envelope(&site.json(Method::POST, "/api/password/forgot", forgot("brand-new-refusal")).await, 422, "invalid");
    envelope(&site.json(Method::POST, "/api/password/forgot", forgot("auth-broke")).await, 502, "upstream");
}

#[tokio::test]
async fn passwords_are_reset_by_code_and_log_this_browser_out() {
    let mut site = Site::new().await;
    let reply = site.json(Method::POST, "/api/password/forgot", json!({"login": " "})).await;
    envelope(&reply, 400, "invalid");
    let reply = site.json(Method::POST, "/api/password/forgot", json!({"login": "anyone"})).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    drop(site);

    let mut site = Site::logged_in("all").await;
    let weak = site.json(Method::POST, "/api/password/reset", json!({"code": "good", "password": "short"})).await;
    envelope(&weak, 400, "invalid");
    let reply = site.json(Method::POST, "/api/password/reset", json!({"code": "good", "password": "long enough"})).await;
    assert_eq!(reply.body, json!({"username": "user-all", "logged_out": true}));
    assert_eq!(site.get("/api/session").await.body, json!({"viewer": null}));
}

#[tokio::test]
async fn account_emails_need_a_login() {
    let mut site = Site::new().await;
    for path in ["/api/account/email/send-confirmation", "/api/account/deletion/request"] {
        envelope(&site.send(Method::POST, path, Some(SITE), None).await, 401, "logged_out");
    }
    envelope(&site.json(Method::POST, "/api/account/deletion/direct", json!({"confirm": "x"})).await, 401, "logged_out");
    drop(site);

    let mut site = Site::logged_in("all").await;
    for path in ["/api/account/email/send-confirmation", "/api/account/deletion/request"] {
        let reply = site.send(Method::POST, path, Some(SITE), None).await;
        assert_eq!((reply.status, &reply.body), (StatusCode::OK, &json!({"email": "a@b.c"})), "{path}");
    }
    drop(site);
    let mut site = Site::logged_in("none").await;
    envelope(&site.send(Method::POST, "/api/account/email/send-confirmation", Some(SITE), None).await, 429, "rate_limited");
}

#[tokio::test]
async fn only_accounts_without_a_confirmed_email_delete_directly() {
    let mut site = Site::logged_in("all").await;
    envelope(&site.json(Method::POST, "/api/account/deletion/direct", json!({"confirm": "user-all"})).await, 409, "conflict");
    drop(site);

    let mut site = Site::logged_in("github").await;
    let wrong = site.json(Method::POST, "/api/account/deletion/direct", json!({"confirm": "someone"})).await;
    envelope(&wrong, 400, "invalid");
    let reply = site.json(Method::POST, "/api/account/deletion/direct", json!({"confirm": "user-github"})).await;
    assert_eq!(reply.body, json!({"username": "user-github", "logged_out": true}));
}

#[tokio::test]
async fn invites_join_or_wait_for_a_login() {
    let mut site = Site::new().await;
    let reply = site.json(Method::POST, "/api/invites/open", json!({"code": "join"})).await;
    assert_eq!(
        reply.body,
        json!({"kind": "needs_account", "role": "arcane_user", "group_label": "the Arcane dice test group"})
    );
    envelope(&site.json(Method::POST, "/api/invites/open", json!({"code": "a b"})).await, 400, "invalid");
    envelope(&site.json(Method::POST, "/api/invites/open", json!({"code": "expired"})).await, 404, "not_found");
    drop(site);

    let mut site = Site::logged_in("all").await;
    let reply = site.json(Method::POST, "/api/invites/open", json!({"code": "join"})).await;
    assert_eq!(reply.body["kind"], "joined");
    drop(site);
    // A session auth dropped joins after logging in, like a logged-out one.
    let mut site = Site::logged_in("dead").await;
    let reply = site.json(Method::POST, "/api/invites/open", json!({"code": "join"})).await;
    assert_eq!(reply.body["kind"], "needs_account");
}

#[tokio::test]
async fn sharing_needs_arcane_and_deleting_needs_only_a_username() {
    let mut site = Site::logged_in("none").await;
    envelope(&site.get("/api/account/sharing").await, 403, "forbidden");
    envelope(&site.json(Method::PUT, "/api/account/sharing", json!({"share": true})).await, 403, "forbidden");
    drop(site);

    let mut site = Site::logged_in("all").await;
    assert_eq!(site.get("/api/account/sharing").await.body, json!({"available": false, "share": false}));
    envelope(&site.json(Method::PUT, "/api/account/sharing", json!({"share": true})).await, 503, "unavailable");
    drop(site);
    let mut site = Site::logged_in("all").await.with_pictures();
    assert_eq!(site.get("/api/account/sharing").await.body, json!({"available": true, "share": true}));
    assert_eq!(site.json(Method::PUT, "/api/account/sharing", json!({"share": false})).await.body, json!({"available": true, "share": false}));
    drop(site);

    let mut site = Site::new().await.with_pictures();
    envelope(&site.send(Method::DELETE, "/api/account/sharing/data", Some(SITE), None).await, 401, "logged_out");
    drop(site);
    // Auth down: deleting still works, by the session's username.
    let mut site = Site::logged_in("down").await.with_pictures();
    let reply = site.send(Method::DELETE, "/api/account/sharing/data", Some(SITE), None).await;
    assert_eq!((reply.status, &reply.body), (StatusCode::OK, &json!({"deleted": 2})));
}

#[tokio::test]
async fn email_invite_and_sharing_server_functions() {
    use dioxus::prelude::ServerFnError;
    use crate::{emails, invites, sharing};
    let _site = Site::new().await;
    let status = |e: ServerFnError| match e {
        ServerFnError::ServerError { code, message, .. } => (code, message),
        other => panic!("{other:?}"),
    };
    let logged_out = || session_of(None);
    let all = session_of(Some("all")).await;

    assert_eq!(in_request(&logged_out().await, emails::confirm_email("good".into())).await.unwrap(), "user-all");
    assert_eq!(status(in_request(&all, emails::confirm_email("used".into())).await.unwrap_err()).0, 404);
    // Not sent to auth: it can't be a code.
    assert_eq!(status(in_request(&all, emails::account_to_delete("a&b".into())).await.unwrap_err()).0, 404);
    in_request(&logged_out().await, emails::forgot_password("anyone".into())).await.unwrap();
    assert_eq!(status(in_request(&all, emails::forgot_password(" ".into())).await.unwrap_err()).0, 400);
    assert_eq!(in_request(&all, emails::send_confirmation_email()).await.unwrap(), "a@b.c");
    assert_eq!(in_request(&all, emails::request_account_deletion()).await.unwrap(), "a@b.c");
    let (code, message) = status(in_request(&logged_out().await, emails::send_confirmation_email()).await.unwrap_err());
    assert_eq!((code, message.as_str()), (401, "You've been logged out. Log in again."));
    assert_eq!(in_request(&logged_out().await, emails::account_to_delete("good".into())).await.unwrap().username, "user-all");
    let done = in_request(&logged_out().await, emails::delete_account("good".into())).await.unwrap();
    assert_eq!((done.username.as_str(), done.logged_out), ("user-all", false));
    let reset = in_request(&all, emails::reset_password("good".into(), "long enough".into())).await.unwrap();
    assert!(reset.logged_out);
    assert_eq!(all.get::<String>("username").await.unwrap(), None);

    let github = session_of(Some("github")).await;
    assert_eq!(status(in_request(&github, emails::delete_account_without_email("x".into())).await.unwrap_err()).0, 400);
    let done = in_request(&github, emails::delete_account_without_email("user-github".into())).await.unwrap();
    assert!(done.logged_out);

    let opened = in_request(&logged_out().await, invites::open_invite("join".into())).await.unwrap();
    assert!(matches!(opened, api_types::Opened::NeedsAccount { .. }));
    let joined = in_request(&session_of(Some("all")).await, invites::open_invite("join".into())).await.unwrap();
    assert!(matches!(joined, api_types::Opened::Joined { .. }));
    assert_eq!(status(in_request(&logged_out().await, invites::open_invite("expired".into())).await.unwrap_err()).0, 404);

    // Sharing: no store configured here.
    let none = session_of(Some("none")).await;
    assert!(in_request(&none, sharing::get_dataset_sharing()).await.is_err(), "without arcane");
    let state = in_request(&session_of(Some("all")).await, sharing::get_dataset_sharing()).await.unwrap();
    assert_eq!(state, api_types::SharingState { available: false, share: false });
    let (code, _) = status(in_request(&session_of(Some("all")).await, sharing::delete_my_dataset()).await.unwrap_err());
    assert_eq!(code, 503);
    // A token auth dropped: logged out, not deleting by the session's stale username.
    let (code, _) = status(in_request(&session_of(Some("dead")).await, sharing::delete_my_dataset()).await.unwrap_err());
    assert_eq!(code, 401);
}

#[tokio::test]
async fn login_register_and_picture_server_functions() {
    use ui::data_dir::LoginStatus;
    let _site = Site::new().await.with_pictures();
    let session = session_of(None).await;
    let status = |r: Result<LoginStatus, dioxus::prelude::ServerFnError>| match r {
        Err(dioxus::prelude::ServerFnError::ServerError { code, .. }) => code,
        other => panic!("{other:?}"),
    };
    assert_eq!(status(in_request(&session, api::login_password("tester".into(), "wrong".into())).await), 401);
    assert_eq!(status(in_request(&session, api::login_password("outage".into(), "x".into())).await), 502);
    let ok = in_request(&session, api::login_password("tester".into(), "right".into())).await.unwrap();
    assert_eq!(ok, LoginStatus::LoggedIn("tester".into()));
    assert_eq!(session.get::<String>("opaque_token").await.unwrap().as_deref(), Some("all"));

    let fresh = session_of(None).await;
    assert_eq!(status(in_request(&fresh, api::register_password("taken".into(), "a@b.c".into(), "long enough".into())).await), 409);
    let new = in_request(&fresh, api::register_password("newcomer".into(), "a@b.c".into(), "long enough".into())).await;
    assert_eq!(new.unwrap(), LoginStatus::LoggedIn("newcomer".into()));

    let info = in_request(&session_of(Some("all")).await, crate::account::remove_profile_picture()).await.unwrap();
    assert_eq!(info.picture_version, None);
    let logged_out = in_request(&session_of(None).await, crate::account::remove_profile_picture()).await;
    assert!(matches!(logged_out, Err(dioxus::prelude::ServerFnError::ServerError { code: 401, .. })));
}

// ---- Ark, Enshrouded, admin and Arcane (PR 2c) ----

impl Site {
    fn calls(&self) -> Vec<String> {
        self.auth.calls.lock().unwrap().clone()
    }

    fn failing(self) -> Self {
        self.auth.failing.store(true, std::sync::atomic::Ordering::SeqCst);
        self
    }
}

/// Logged out 401, without the permission 403, auth down 503: the same for each route.
async fn guarded(routes: &[(Method, &str, Option<Value>)]) {
    for (token, status, error) in [(None, 401, "logged_out"), (Some("none"), 403, "forbidden"), (Some("down"), 503, "unavailable")] {
        let mut site = match token {
            Some(token) => Site::logged_in(token).await,
            None => Site::new().await,
        };
        for (method, path, body) in routes {
            let body = body.as_ref().map(|b| ("application/json", b.to_string().into_bytes()));
            let reply = site.send(method.clone(), path, Some(SITE), body).await;
            envelope(&reply, status, error);
        }
        assert!(site.calls().is_empty(), "auth's panels were asked: {:?}", site.calls());
    }
}

#[tokio::test]
async fn ark_needs_llama() {
    guarded(&[(Method::GET, "/api/ark/players", None), (Method::POST, "/api/ark/command", Some(json!({"cmd": "start"})))]).await;
}

#[tokio::test]
async fn ark_answers_players_and_commands() {
    let (exporter, _guard) = crate::trace_tests::exporting();
    let mut site = Site::logged_in("all").await;
    let reply = site.get("/api/ark/players").await;
    assert_eq!((reply.status, &reply.body), (StatusCode::OK, &json!({"count": 3})));
    crate::trace_tests::span(&exporter, "bff.ark_player_count").await;

    let reply = site.json(Method::POST, "/api/ark/command", json!({"cmd": "start"})).await;
    assert_eq!((reply.status, &reply.body), (StatusCode::OK, &json!({"outcome": "started"})));
    let reply = site.json(Method::POST, "/api/ark/command", json!({"cmd": "stop"})).await;
    assert_eq!(reply.body, json!({"outcome": "already_stopped"}));
    crate::trace_tests::span(&exporter, "bff.ark_command").await;
    // A command auth doesn't take never reaches it.
    envelope(&site.json(Method::POST, "/api/ark/command", json!({"cmd": "explode"})).await, 422, "invalid");
    assert_eq!(site.calls(), ["POST /internal/ark/num_players null", "POST /internal/ark/command \"start\"", "POST /internal/ark/command \"stop\""]);
}

#[tokio::test]
async fn ark_failures_are_gateway_errors() {
    let mut site = Site::logged_in("all").await;
    // A reply without a result.
    envelope(&site.json(Method::POST, "/api/ark/command", json!({"cmd": "restart"})).await, 502, "upstream");
    let mut site = { drop(site); Site::logged_in("all").await.failing() };
    for reply in [site.get("/api/ark/players").await, site.json(Method::POST, "/api/ark/command", json!({"cmd": "start"})).await] {
        envelope(&reply, 502, "upstream");
        assert_eq!(reply.body["message"], "The Ark server can't be reached right now. Try again later.");
    }
}

#[tokio::test]
async fn enshrouded_needs_enshrouded() {
    guarded(&[(Method::GET, "/api/enshrouded", None), (Method::POST, "/api/enshrouded/command", Some(json!({"cmd": "restart"})))])
        .await;
}

#[tokio::test]
async fn enshrouded_answers_status_and_commands() {
    let (exporter, _guard) = crate::trace_tests::exporting();
    let mut site = Site::logged_in("all").await;
    let reply = site.get("/api/enshrouded").await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!((&reply.body["state"], &reply.body["players"]["online"]), (&json!("Online"), &json!(1)));
    crate::trace_tests::span(&exporter, "bff.enshrouded_status").await;

    let reply = site.json(Method::POST, "/api/enshrouded/command", json!({"cmd": "restart"})).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.body);
    crate::trace_tests::span(&exporter, "bff.enshrouded_command").await;
    let reply = site.json(Method::POST, "/api/enshrouded/command", json!({"cmd": "update"})).await;
    envelope(&reply, 429, "rate_limited");
    assert_eq!(reply.body["message"], "Someone just sent a command. Wait a moment and try again.");
    envelope(&site.json(Method::POST, "/api/enshrouded/command", json!({"cmd": "update_now"})).await, 409, "conflict");
    envelope(&site.json(Method::POST, "/api/enshrouded/command", json!({"cmd": "dance"})).await, 422, "invalid");

    let mut site = { drop(site); Site::logged_in("all").await.failing() };
    let reply = site.get("/api/enshrouded").await;
    envelope(&reply, 503, "unavailable");
    assert_eq!(reply.body["message"], "The website can't control the server yet (no control password).");
}

#[tokio::test]
async fn admin_needs_manage_permissions() {
    guarded(&[
        (Method::GET, "/api/admin/users?page=0&limit=25", None),
        (Method::GET, "/api/admin/roles?page=0&limit=25", None),
        (Method::GET, "/api/admin/roles/all", None),
        (Method::GET, "/api/admin/permissions", None),
        (Method::PUT, "/api/admin/users/5/roles/2", None),
        (Method::DELETE, "/api/admin/users/5/roles/2", None),
        (Method::PUT, "/api/admin/roles/2/permissions/3", None),
        (Method::DELETE, "/api/admin/roles/2/permissions/3", None),
        (Method::GET, "/api/admin/invites", None),
        (Method::POST, "/api/admin/invites", Some(json!({"role_id": 2, "days": 7, "max_uses": null, "note": null}))),
        (Method::DELETE, "/api/admin/invites/4", None),
    ])
    .await;
}

#[tokio::test]
async fn admin_lists_and_changes() {
    let (exporter, _guard) = crate::trace_tests::exporting();
    let mut site = Site::logged_in("all").await;
    let reply = site.get("/api/admin/users?page=1&limit=25&search=a%20b").await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.body["items"][0]["roles"], json!([{"id": 2, "name": "arcane_user"}]));
    assert_eq!(reply.body["total"], 1);
    crate::trace_tests::span(&exporter, "bff.admin_list_users").await;
    // Search is optional.
    let reply = site.get("/api/admin/roles?page=0&limit=25").await;
    // A permission the site doesn't know yet is still listed.
    assert_eq!(reply.body["items"][0]["permissions"], json!([{"id": 3, "name": "arcane"}, {"id": 9, "name": "brand_new"}]));
    assert_eq!(site.get("/api/admin/roles/all").await.body, json!([{"id": 2, "name": "arcane_user"}]));
    assert_eq!(site.get("/api/admin/permissions").await.body[1], json!({"id": 9, "name": "brand_new"}));
    assert_eq!(site.get("/api/admin/invites").await.body[0]["id"], 4);

    for (method, path) in [
        (Method::PUT, "/api/admin/users/5/roles/2"),
        (Method::DELETE, "/api/admin/users/5/roles/2"),
        (Method::PUT, "/api/admin/roles/2/permissions/3"),
        (Method::DELETE, "/api/admin/roles/2/permissions/3"),
        (Method::DELETE, "/api/admin/invites/4"),
    ] {
        let reply = site.send(method.clone(), path, Some(SITE), None).await;
        assert_eq!(reply.status, StatusCode::NO_CONTENT, "{method} {path}: {}", reply.body);
    }
    crate::trace_tests::span(&exporter, "bff.admin_revoke_role_permission").await;
    assert_eq!(
        site.calls(),
        [
            "GET /internal/admin/users?page=1&limit=25&search=a+b",
            "GET /internal/admin/roles?page=0&limit=25&search=",
            "GET /internal/admin/roles/all",
            "GET /internal/admin/permissions",
            "GET /internal/admin/invites",
            "POST /internal/admin/users/5/roles/2",
            "DELETE /internal/admin/users/5/roles/2",
            "POST /internal/admin/roles/2/permissions/3",
            "DELETE /internal/admin/roles/2/permissions/3",
            "DELETE /internal/admin/invites/4",
        ]
    );

    let new = |role_id: i32, days: i64| json!({"role_id": role_id, "days": days, "max_uses": 3, "note": "for ann"});
    let reply = site.json(Method::POST, "/api/admin/invites", new(2, 7)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.body["link"], "https://milesstorm.test/invite#code");
    crate::trace_tests::span(&exporter, "bff.admin_create_invite").await;
}

#[tokio::test]
async fn admin_refusals_and_failures_have_messages() {
    let mut site = Site::logged_in("all").await;
    let new = |role_id: i32, days: i64| json!({"role_id": role_id, "days": days, "max_uses": null, "note": null});
    let reply = site.json(Method::POST, "/api/admin/invites", new(1, 7)).await;
    envelope(&reply, 400, "invalid");
    assert_eq!(reply.body["message"], "Links can't give a role with admin rights: give that role in the Users tab instead.");
    let reply = site.json(Method::POST, "/api/admin/invites", new(2, 400)).await;
    envelope(&reply, 400, "invalid");
    assert!(reply.body["message"].as_str().unwrap().starts_with("Check the fields"));
    envelope(&site.json(Method::POST, "/api/admin/invites", new(999, 7)).await, 404, "not_found");
    envelope(&site.send(Method::DELETE, "/api/admin/invites/999", Some(SITE), None).await, 404, "not_found");
    // Refusals without a message of their own: 404 is not_found, any other 4xx invalid.
    envelope(&site.send(Method::PUT, "/api/admin/users/404/roles/2", Some(SITE), None).await, 404, "not_found");
    envelope(&site.send(Method::DELETE, "/api/admin/users/400/roles/2", Some(SITE), None).await, 400, "invalid");

    // Bad ids and paging never reach auth.
    let before = site.calls().len();
    envelope(&site.send(Method::PUT, "/api/admin/users/ann/roles/2", Some(SITE), None).await, 400, "invalid");
    envelope(&site.get("/api/admin/users?page=0").await, 400, "invalid");
    envelope(&site.get("/api/admin/users?page=-1&limit=25").await, 400, "invalid");
    let reply = site.get("/api/admin/users?page=0&limit=1000").await;
    envelope(&reply, 400, "invalid");
    assert_eq!(reply.body["message"], "Ask for 1 to 100 rows at a time.");
    assert_eq!(site.calls().len(), before);

    let mut site = { drop(site); Site::logged_in("all").await.failing() };
    let reply = site.get("/api/admin/users?page=0&limit=25").await;
    envelope(&reply, 502, "upstream");
    assert_eq!(reply.body["message"], "Users can't be loaded right now. Try again later.");
    envelope(&site.send(Method::PUT, "/api/admin/users/5/roles/2", Some(SITE), None).await, 502, "upstream");
}

#[tokio::test]
async fn panel_changes_from_another_site_are_refused() {
    let mut site = Site::logged_in("all").await;
    let evil = Some("https://evil.example");
    for (method, path, body) in [
        (Method::POST, "/api/ark/command", Some(json!({"cmd": "start"}))),
        (Method::POST, "/api/enshrouded/command", Some(json!({"cmd": "restart"}))),
        (Method::PUT, "/api/admin/users/5/roles/2", None),
        (Method::DELETE, "/api/admin/users/5/roles/2", None),
        (Method::PUT, "/api/admin/roles/2/permissions/3", None),
        (Method::DELETE, "/api/admin/roles/2/permissions/3", None),
        (Method::POST, "/api/admin/invites", Some(json!({"role_id": 2, "days": 7, "max_uses": null, "note": null}))),
        (Method::DELETE, "/api/admin/invites/4", None),
        (Method::POST, "/api/arcane/flag", Some(json!({"roll_id": "18f3a2b-0badf00d", "values": []}))),
    ] {
        let body = body.map(|b| ("application/json", b.to_string().into_bytes()));
        envelope(&site.send(method.clone(), path, evil, body).await, 403, "forbidden");
    }
    assert!(site.calls().is_empty(), "{:?}", site.calls());
    // The extension's own origin is let in on the flag only.
    for origin in ["moz-extension://1b2c3d", "chrome-extension://abcdef"] {
        let ark = site.send(Method::POST, "/api/ark/command", Some(origin), Some(("application/json", br#"{"cmd":"start"}"#.to_vec()))).await;
        envelope(&ark, 403, "forbidden");
        let flag = br#"{"roll_id":"18f3a2b-0badf00d"}"#.to_vec();
        // Past the origin check: no roll hub in this test.
        envelope(&site.send(Method::POST, "/api/arcane/flag", Some(origin), Some(("application/json", flag))).await, 503, "unavailable");
    }
}

#[tokio::test]
async fn arcane_dev_origins_count_on_the_flag_only() {
    let mut site = Site::serving(super::router_with("https://dev.example")).await.log_in("all").await;
    let dev = Some("https://dev.example");
    let flag = site.send(Method::POST, "/api/arcane/flag", dev, Some(("application/json", br#"{"roll_id":"x"}"#.to_vec()))).await;
    // Past the origin check: no roll hub in this test.
    envelope(&flag, 503, "unavailable");
    let ark = site.send(Method::POST, "/api/ark/command", dev, Some(("application/json", br#"{"cmd":"start"}"#.to_vec()))).await;
    envelope(&ark, 403, "forbidden");
    // Not on the flag of a site without it.
    let mut site = { drop(site); Site::logged_in("all").await };
    let flag = site.send(Method::POST, "/api/arcane/flag", dev, Some(("application/json", br#"{"roll_id":"x"}"#.to_vec()))).await;
    envelope(&flag, 403, "forbidden");
}

#[tokio::test]
async fn arcane_me_answers_everyone() {
    for (token, me) in [
        (None, json!({"logged_in": false, "username": null, "has_arcane": false})),
        (Some("all"), json!({"logged_in": true, "username": "user-all", "has_arcane": true})),
        (Some("none"), json!({"logged_in": true, "username": "user-none", "has_arcane": false})),
        (Some("down"), json!({"logged_in": true, "username": "user-down", "has_arcane": false})),
        // A token auth dropped: the session is over.
        (Some("dead"), json!({"logged_in": false, "username": null, "has_arcane": false})),
    ] {
        let mut site = match token {
            Some(token) => Site::logged_in(token).await,
            None => Site::new().await,
        };
        let reply = site.get("/api/arcane/me").await;
        assert_eq!((reply.status, &reply.body), (StatusCode::OK, &me), "{token:?}");
    }
}

#[tokio::test]
async fn roll_streams_and_flags_need_arcane() {
    guarded(&[(Method::GET, "/api/arcane/rolls", None), (Method::POST, "/api/arcane/flag", Some(json!({"roll_id": "r"})))]).await;
    let mut site = Site::logged_in("all").await;
    let reply = site.get("/api/arcane/rolls").await;
    envelope(&reply, 503, "unavailable");
    assert_eq!(reply.body["message"], "Live rolls aren't available right now.");
}

/// A held roll of `user-all` (as the camera proxy keeps one) in `pool`'s Redis.
async fn hold_roll(pool: &tower_sessions_redis_store::fred::prelude::Pool, roll_id: &str) {
    use tower_sessions_redis_store::fred::prelude::KeysInterface as _;
    let roll: Value = serde_json::from_str(include_str!("../../../../../../contracts/arcane/roll_unread.json")).unwrap();
    let meta = json!({"roll_id": roll_id, "roll": roll, "frame": null}).to_string();
    let _: () = pool.set("arcane:held:user-all", meta, None, None, false).await.unwrap();
    let _: () = pool.set("arcane:held_jpeg:user-all", b"\xff\xd8\xff\xe0jpeg".to_vec(), None, None, false).await.unwrap();
}

/// Needs Docker (Redis), like the trace tests.
#[tokio::test]
async fn rolls_stream_and_flags_are_saved() {
    let (_node, config) = crate::trace_tests::redis().await;
    let (hub, pool) = crate::trace_tests::hub(&config, tokio_util::sync::CancellationToken::new()).await;
    let mut site = Site::logged_in("all").await.with_pictures();
    site.app = site.app.clone().layer(axum::Extension(hub.clone()));

    // The stream starts with the stored last roll, as ai_pipeline sent it.
    let roll = include_str!("../../../../../../contracts/arcane/roll.json");
    let stored: Value = serde_json::from_str(roll).unwrap();
    let last = tower_sessions_redis_store::fred::prelude::KeysInterface::set(&pool, "arcane:last_roll:user-all", stored.to_string(), None, None, false);
    let _: () = last.await.unwrap();
    let mut req = Request::builder().uri("/api/arcane/rolls").header(header::HOST, HOST);
    req = req.header(header::COOKIE, site.cookie.clone().unwrap());
    let res = site.app.clone().oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()[header::CONTENT_TYPE], "text/event-stream");
    let mut body = res.into_body().into_data_stream();
    let mut event = String::new();
    while !event.ends_with("\n\n") {
        let chunk = tokio::time::timeout(std::time::Duration::from_secs(5), futures_util::StreamExt::next(&mut body)).await.unwrap().unwrap().unwrap();
        event.push_str(std::str::from_utf8(&chunk).unwrap());
    }
    let data = event.strip_prefix("event: replay\ndata: ").unwrap().trim_end();
    assert!(matches!(serde_json::from_str(data).unwrap(), api_types::ArcaneServerMsg::Roll(_)), "{data}");
    drop(body);

    let flag = |roll_id: &str, values: Value| json!({"roll_id": roll_id, "values": values});
    envelope(&site.json(Method::POST, "/api/arcane/flag", flag("not a roll", json!([]))).await, 400, "invalid");
    let reply = site.json(Method::POST, "/api/arcane/flag", flag("18f3a2b-0badf00d", json!([null, "7"]))).await;
    envelope(&reply, 410, "gone");
    hold_roll(&pool, "18f3a2b-0badf00d").await;
    // One value per die (the held roll has two).
    envelope(&site.json(Method::POST, "/api/arcane/flag", flag("18f3a2b-0badf00d", json!(["7"]))).await, 400, "invalid");
    let reply = site.json(Method::POST, "/api/arcane/flag", flag("18f3a2b-0badf00d", json!([null, "7"]))).await;
    assert_eq!((reply.status, &reply.body), (StatusCode::OK, &json!({"ok": true})));
    assert!(site.auth.queries.lock().unwrap().iter().any(|q| q.contains("UPSERT $rec")), "saved");
    // From the extension too.
    let body = flag("18f3a2b-0badf00d", json!([])).to_string().into_bytes();
    let reply = site.send(Method::POST, "/api/arcane/flag", Some("moz-extension://1b2c3d"), Some(("application/json", body))).await;
    envelope(&reply, 400, "invalid");
    // At most 30 an hour.
    for _ in 0..29 {
        site.json(Method::POST, "/api/arcane/flag", flag("18f3a2b-0badf00d", json!([null, null]))).await;
    }
    envelope(&site.json(Method::POST, "/api/arcane/flag", flag("18f3a2b-0badf00d", json!([null, null]))).await, 429, "rate_limited");
}

#[tokio::test]
async fn panel_server_functions() {
    use dioxus::prelude::ServerFnError;
    let site = Site::new().await;
    let code = |e: ServerFnError| match e {
        ServerFnError::ServerError { code, .. } => code,
        other => panic!("{other:?}"),
    };
    let all = session_of(Some("all")).await;
    assert_eq!(in_request(&all, api::ark_player_count()).await.unwrap(), 3);
    assert_eq!(in_request(&all, api::ark_command(api::ArkCommand::Start)).await.unwrap(), api::ArkOutcome::Started);
    assert_eq!(code(in_request(&session_of(None).await, api::ark_player_count()).await.unwrap_err()), 401);
    // Auth checks the permission for Ark and Enshrouded.
    assert_eq!(code(in_request(&session_of(Some("none")).await, api::ark_player_count()).await.unwrap_err()), 403);

    assert_eq!(in_request(&all, api::enshrouded_status()).await.unwrap().state, api_types::EnshroudedState::Online);
    in_request(&all, api::enshrouded_command(api::EnshroudedCommand::Restart)).await.unwrap();
    let busy = in_request(&all, api::enshrouded_command(api::EnshroudedCommand::UpdateNow)).await.unwrap_err();
    assert_eq!(code(busy), 409);

    // The website checks admin rights itself: auth's admin endpoints trust it.
    let users = in_request(&all, api::admin_list_users(0, 25, String::new())).await.unwrap();
    assert_eq!(users.items[0].username, "ann");
    assert_eq!(in_request(&all, api::admin_list_all_roles()).await.unwrap()[0].name, "arcane_user");
    in_request(&all, api::admin_assign_user_role(5, 2)).await.unwrap();
    let invite = in_request(&all, api::admin_create_invite(2, 7, None, None)).await.unwrap();
    assert!(invite.link.is_some());
    let calls = site.calls().len();
    for (session, status) in [(session_of(Some("none")).await, 403), (session_of(None).await, 401), (session_of(Some("down")).await, 503)] {
        assert_eq!(code(in_request(&session, api::admin_list_permissions()).await.unwrap_err()), status);
        assert_eq!(code(in_request(&session, api::admin_revoke_invite(4)).await.unwrap_err()), status);
    }
    assert_eq!(site.calls().len(), calls, "refused before asking auth");
}
