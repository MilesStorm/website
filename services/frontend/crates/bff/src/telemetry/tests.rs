use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt as _;

use super::{SourceMaps, Telemetry, MAX_BODY, PER_VISITOR};

const HOST: &str = "milesstorm.test";
const SITE: &str = "https://milesstorm.test";

/// A build's map directory with one map: `app.Ab12.js`, whose line 1 column 11 is
/// `src/islands/LoginForm.tsx` line 5 column 3, in `submit`.
fn maps() -> SourceMaps {
    let dir = std::env::temp_dir().join(format!("bff-maps-{}-{:?}", std::process::id(), std::thread::current().id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut builder = sourcemap::SourceMapBuilder::new(Some("app.Ab12.js"));
    builder.add(0, 10, 4, 2, Some("../../src/islands/LoginForm.tsx"), Some("submit"), false);
    let mut out = Vec::new();
    builder.into_sourcemap().to_writer(&mut out).unwrap();
    std::fs::write(dir.join("app.Ab12.js.map"), out).unwrap();
    // Not a script's map: never looked up.
    std::fs::write(dir.join("notes.txt"), "x").unwrap();
    SourceMaps::load(dir)
}

/// A stand-in for Alloy's faro.receiver that keeps what it gets and answers `status`.
async fn alloy(status: StatusCode) -> (String, Arc<Mutex<Vec<Value>>>) {
    let got = Arc::new(Mutex::new(Vec::new()));
    let keep = got.clone();
    let app = axum::Router::new().route(
        "/collect",
        axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
            let keep = keep.clone();
            async move {
                keep.lock().unwrap().push(body);
                status
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/collect", listener.local_addr().unwrap());
    crate::trace::spawn_loop("test: stand-in Alloy", async move { axum::serve(listener, app).await.unwrap() });
    (url, got)
}

fn app(url: String) -> axum::Router {
    super::router(Arc::new(Telemetry::new(maps(), url, PER_VISITOR).unwrap()))
}

fn upload(body: &[u8]) -> axum::http::request::Builder {
    Request::builder()
        .method(Method::POST)
        .uri("/telemetry/collect")
        .header(header::HOST, HOST)
        .header(header::ORIGIN, SITE)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CONTENT_LENGTH, body.len())
}

/// An upload of `body` with header `name` set to `value` instead.
fn with(body: &[u8], name: header::HeaderName, value: &str, sent: Vec<u8>) -> Request<Body> {
    let mut req = upload(body).body(Body::from(sent)).unwrap();
    req.headers_mut().insert(name, value.parse().unwrap());
    req
}

async fn send(app: &axum::Router, req: Request<Body>) -> StatusCode {
    app.clone().oneshot(req).await.unwrap().status()
}

fn payload() -> Value {
    json!({
        "meta": {"app": {"name": "milesstorm-web", "environment": "production"}},
        "events": [{"name": "faro.user.action", "attributes": {"userActionName": "log in", "trace_id": "ab"}}],
        "exceptions": [{
            "type": "TypeError",
            "value": "x is undefined",
            "timestamp": "2026-10-22T12:00:00Z",
            "context": {"island": "LoginForm"},
            "stacktrace": {"frames": [
                {"filename": "https://milesstorm.test/_astro/app.Ab12.js?v=1", "function": "a", "lineno": 1, "colno": 11},
                {"filename": "https://milesstorm.test/_astro/other.Cd34.js", "function": "b", "lineno": 1, "colno": 11},
                {"filename": "https://evil.test/_astro/app.Ab12.js", "function": "c", "lineno": 1, "colno": 11},
                {"filename": "https://milesstorm.test/_astro/../app.Ab12.js", "function": "d", "lineno": 1, "colno": 11},
                {"filename": "https://milesstorm.test/login", "function": "?", "lineno": 3, "colno": 4}
            ]}
        }]
    })
}

#[tokio::test]
async fn frames_in_the_builds_scripts_are_mapped_and_the_rest_passes_unchanged() {
    let (url, got) = alloy(StatusCode::ACCEPTED).await;
    let app = app(url);
    let body = serde_json::to_vec(&payload()).unwrap();
    assert_eq!(send(&app, upload(&body).body(Body::from(body.clone())).unwrap()).await, StatusCode::ACCEPTED);

    let sent = got.lock().unwrap().pop().expect("forwarded");
    let mut expected = payload();
    expected["exceptions"][0]["stacktrace"]["frames"][0] =
        json!({"filename": "src/islands/LoginForm.tsx", "function": "submit", "lineno": 5, "colno": 3});
    assert_eq!(sent, expected, "only the frame in the build's own file, on this host, changes");
}

#[tokio::test]
async fn uploads_the_gateway_would_refuse_are_refused() {
    let (url, got) = alloy(StatusCode::ACCEPTED).await;
    let app = app(url);
    let body = serde_json::to_vec(&payload()).unwrap();

    let big = vec![b' '; MAX_BODY + 1];
    assert_eq!(send(&app, upload(&big).body(Body::from(big.clone())).unwrap()).await, StatusCode::PAYLOAD_TOO_LARGE);
    // Without a length (HTTP/2), the read stops at the cap.
    let mut unsized_big = upload(&big).body(Body::from(big.clone())).unwrap();
    unsized_big.headers_mut().remove(header::CONTENT_LENGTH);
    assert_eq!(send(&app, unsized_big).await, StatusCode::PAYLOAD_TOO_LARGE);
    // A body longer than its header says.
    let lying = with(&body, header::CONTENT_LENGTH, "10", big);
    assert_ne!(send(&app, lying).await, StatusCode::ACCEPTED);

    let gzip = with(&body, header::CONTENT_ENCODING, "gzip", body.clone());
    assert_eq!(send(&app, gzip).await, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    let text = with(&body, header::CONTENT_TYPE, "text/plain", body.clone());
    assert_eq!(send(&app, text).await, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    let foreign = with(&body, header::ORIGIN, "https://evil.test", body.clone());
    assert_eq!(send(&app, foreign).await, StatusCode::FORBIDDEN);

    let not_faro = br#"{"hello": 1}"#;
    assert_eq!(send(&app, upload(not_faro).body(Body::from(&not_faro[..])).unwrap()).await, StatusCode::BAD_REQUEST);
    assert!(got.lock().unwrap().is_empty(), "nothing refused reaches the collector");
}

#[tokio::test]
async fn an_upload_without_a_length_is_taken() {
    let (url, got) = alloy(StatusCode::ACCEPTED).await;
    let app = app(url);
    let body = br#"{"meta": {}}"#;
    let mut req = upload(body).body(Body::from(&body[..])).unwrap();
    req.headers_mut().remove(header::CONTENT_LENGTH);
    assert_eq!(send(&app, req).await, StatusCode::ACCEPTED);
    assert_eq!(got.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn one_visitor_is_limited_and_the_collectors_answer_is_passed_on() {
    let (url, got) = alloy(StatusCode::ACCEPTED).await;
    let app = app(url);
    let body = br#"{"meta": {}}"#;
    let from = |ip: &'static str| upload(body).header("cf-connecting-ip", ip).body(Body::from(&body[..])).unwrap();
    for _ in 0..PER_VISITOR {
        assert_eq!(send(&app, from("203.0.113.7")).await, StatusCode::ACCEPTED);
    }
    assert_eq!(send(&app, from("203.0.113.7")).await, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(send(&app, from("198.51.100.1")).await, StatusCode::ACCEPTED);
    assert_eq!(got.lock().unwrap().len(), PER_VISITOR as usize + 1);
    // Requests without Cloudflare's address are one visitor, limited like any other.
    let unknown = || upload(body).body(Body::from(&body[..])).unwrap();
    for _ in 0..PER_VISITOR {
        assert_eq!(send(&app, unknown()).await, StatusCode::ACCEPTED);
    }
    assert_eq!(send(&app, unknown()).await, StatusCode::TOO_MANY_REQUESTS);

    let (busy, _) = alloy(StatusCode::TOO_MANY_REQUESTS).await;
    assert_eq!(send(&self::app(busy), upload(body).body(Body::from(&body[..])).unwrap()).await, StatusCode::TOO_MANY_REQUESTS);
    let down = self::app("http://127.0.0.1:1/collect".into());
    assert_eq!(send(&down, upload(body).body(Body::from(&body[..])).unwrap()).await, StatusCode::BAD_GATEWAY);
}

#[test]
fn map_sources_become_repository_paths() {
    assert_eq!(super::source_path("../../src/islands/LoginForm.tsx"), "src/islands/LoginForm.tsx");
    assert_eq!(super::source_path("../../node_modules/@grafana/faro-core/dist/esm/a.js"), "node_modules/@grafana/faro-core/dist/esm/a.js");
    assert_eq!(super::source_path("./assets/trace.js"), "assets/trace.js");
}

#[tokio::test]
async fn a_map_that_failed_to_read_is_tried_again() {
    let maps = maps();
    let path = maps.dir.join("app.Ab12.js.map");
    let good = std::fs::read(&path).unwrap();
    std::fs::write(&path, "not a map").unwrap();
    let payload = || serde_json::from_value::<super::Payload>(payload()).unwrap();
    let mut first = payload();
    assert_eq!(maps.resolve(&mut first, HOST).await, 0);
    std::fs::write(&path, good).unwrap();
    let mut second = payload();
    assert_eq!(maps.resolve(&mut second, HOST).await, 1);
}
