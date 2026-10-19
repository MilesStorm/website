//! Contract tests of the JSON API: statuses, the error envelope, access by login, the origin
//! check and one introspect call to auth per request; and the Dioxus server functions over the
//! same functions. Auth is a stand-in that answers by token: `all` (every permission, plus one
//! the site doesn't know), `none`, `lost` (an account auth can't load), `dead` (a token auth
//! dropped) and `down` (auth failing). The same stand-in plays SurrealDB for pictures.

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

    let pictures = auth.clone();
    let introspect = move |body: axum::Json<Value>| async move {
        let token = body["token"].as_str().unwrap_or_default().to_string();
        *auth.introspected.lock().unwrap().entry(token.clone()).or_default() += 1;
        match token.as_str() {
            "all" => axum::Json(json!({"permissions": ["llama", "enshrouded", "arcane", "manage_permissions", "brand_new"]})).into_response(),
            "none" | "lost" => axum::Json(json!({"permissions": []})).into_response(),
            "dead" => (StatusCode::UNAUTHORIZED, "Invalid or expired token").into_response(),
            _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        }
    };
    let profile_of = |body: axum::Json<Value>| async move {
        match body["token"].as_str() {
            Some(t @ ("all" | "none")) => axum::Json(profile(t, Some("Shown"))).into_response(),
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
        let mut stored = pictures.picture.lock().unwrap();
        let result = if sql.starts_with("UPSERT") {
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
    let app = axum::Router::new()
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
        let app = super::router()
            .route("/test/seed", post(seed))
            .layer(SessionManagerLayer::new(MemoryStore::default()).with_secure(false));
        Self { app, auth, cookie: None, _env: env }
    }

    async fn logged_in(token: &str) -> Self {
        let mut site = Self::new().await;
        assert_eq!(site.send(Method::POST, "/test/seed", Some(SITE), Some(("text/plain", token.into()))).await.status, StatusCode::OK);
        site
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
    // Still logged in.
    assert_eq!(site.get("/api/session").await.body["viewer"]["username"], "user-all");
    // Reads are fine from anywhere (the browser keeps the answer from the other site), and
    // requests without an Origin aren't from a browser.
    assert_eq!(site.send(Method::GET, "/api/session", evil, None).await.status, StatusCode::OK);
    let no_origin = site.send(Method::POST, "/api/session/login", None, Some(("application/json", login))).await;
    assert_eq!(no_origin.status, StatusCode::OK, "{}", no_origin.body);
}

#[test]
fn errors_answer_their_status_with_the_envelope() {
    use axum::response::IntoResponse;
    let error = api_types::ApiError::new(api_types::ErrorCode::Invalid, "too big").with_status(413);
    assert_eq!(super::ApiError(error).into_response().status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(super::ApiError(api_types::ApiError::internal()).into_response().status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[test]
fn the_api_mounts_beside_the_dioxus_app() {
    use dioxus::prelude::*;
    // Dioxus routes each file of its public dir (as the image has it: dx's bundle). Nothing
    // else reads DIOXUS_PUBLIC_PATH; it is put back, and the directory removed, either way.
    struct Public(std::path::PathBuf, Option<std::ffi::OsString>);
    impl Drop for Public {
        fn drop(&mut self) {
            match self.1.take() {
                Some(old) => std::env::set_var("DIOXUS_PUBLIC_PATH", old),
                None => std::env::remove_var("DIOXUS_PUBLIC_PATH"),
            }
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let public = Public(
        std::env::temp_dir().join(format!("web-public-{}", std::process::id())),
        std::env::var_os("DIOXUS_PUBLIC_PATH"),
    );
    std::fs::create_dir_all(public.0.join("assets")).unwrap();
    std::fs::write(public.0.join("assets/web.js"), "").unwrap();
    std::fs::write(public.0.join("favicon.ico"), "").unwrap();
    std::env::set_var("DIOXUS_PUBLIC_PATH", &public.0);
    // axum panics on overlapping routes when the router is built.
    let _ = axum::Router::new()
        .serve_dioxus_application(ServeConfig::default(), crate::App)
        .route("/api/arcane/me", axum::routing::get(|| async {}))
        .merge(super::router());
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
            "DELETE /api/session",
            "GET /api/account",
            "GET /api/account/picture",
            "GET /api/session",
            "POST /api/account/picture",
            "POST /api/session/login",
            "POST /api/session/register",
            "PUT /api/account/display-name",
        ]
    );
    // Literal unions for TypeScript.
    assert_eq!(doc["components"]["schemas"]["KnownPermission"]["enum"], json!(["llama", "enshrouded", "arcane", "manage_permissions"]));
    assert_eq!(doc["components"]["schemas"]["Bootstrap"]["required"], json!(["viewer"]));
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
    let mut site = Site::logged_in("all").await;
    let old = std::env::var_os("ARCANE_ALLOWED_ORIGINS");
    std::env::set_var("ARCANE_ALLOWED_ORIGINS", "https://dev.example");
    let reply = site.send(Method::DELETE, "/api/session", Some("https://dev.example"), None).await;
    match old {
        Some(old) => std::env::set_var("ARCANE_ALLOWED_ORIGINS", old),
        None => std::env::remove_var("ARCANE_ALLOWED_ORIGINS"),
    }
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
    site.app = site.app.clone().route("/api/arcane/me", axum::routing::get(|| async { "plain" }));
    envelope(&site.get("/api/nope").await, 404, "not_found");
    envelope(&site.send(Method::POST, "/api/account/nope", Some(SITE), None).await, 404, "not_found");
    envelope(&site.send(Method::PATCH, "/api/session", Some(SITE), None).await, 405, "invalid");
    let plain = site.get("/api/arcane/me").await;
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
