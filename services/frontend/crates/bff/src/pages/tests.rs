//! The Astro pages: the startup checks of the build, the splice and its escaping, what a page
//! says about the viewer, the guards of the pages that aren't public, and that the page's
//! `traceparent` is the request's own span.

use std::path::PathBuf;
use std::sync::Arc;

use api_types::{Access, Bootstrap, KnownPermission, Page, PagePath, Viewer, PAGES};
use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt as _;

use super::{verdict, Built, Injected, Split, Verdict, MARKER};
use crate::json_api::tests::Site;

/// A page as Astro writes it: the charset first, then the marker.
fn page(body: &str) -> String {
    format!(r#"<!DOCTYPE html><html lang="en" class="dark"><head><meta charset="utf-8">{MARKER}<title>t</title></head><body>{body}</body></html>"#)
}

/// A build in a directory of its own, removed when dropped: every page and status page (their
/// file name as the body) and one asset, with a brotli copy.
struct Dist(PathBuf);

impl Dist {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("web-dist-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("_astro")).unwrap();
        for file in api_types::PAGES.iter().map(|p| p.file).chain(api_types::STATUS_PAGES.iter().copied()) {
            std::fs::write(dir.join(file), page(file)).unwrap();
        }
        std::fs::write(dir.join("_astro/app.Ab12.js"), "console.log(1)").unwrap();
        std::fs::write(dir.join("_astro/app.Ab12.js.br"), "brotli").unwrap();
        std::fs::write(dir.join("favicon.ico"), "icon").unwrap();
        Self(dir)
    }

    fn built(&self) -> Arc<Built> {
        Arc::new(Built::load(&self.0, PAGES).unwrap())
    }
}

impl Drop for Dist {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn viewer(name: &str, permissions: Vec<KnownPermission>) -> Bootstrap {
    Bootstrap {
        viewer: Some(Viewer { username: "u".into(), shown_name: name.into(), picture_url: None, permissions }),
    }
}

/// The bootstrap JSON of a rendered page, as the browser reads it: the text up to the first
/// `</script>`.
fn bootstrap_of(html: &str) -> Value {
    let open = r#"<script id="bootstrap" type="application/json">"#;
    let start = html.find(open).expect("no bootstrap") + open.len();
    let end = start + html[start..].find("</script>").expect("bootstrap not closed");
    serde_json::from_str(&html[start..end]).expect("the bootstrap is JSON")
}

#[test]
fn a_page_splits_at_one_html_tag_and_one_marker() {
    assert!(Split::parse(&page("")).is_ok());
    assert!(Split::parse("<head><meta name=\"bff-inject\"></head>").unwrap_err().contains("no <html"));
    assert!(Split::parse("<html><head></head></html>").unwrap_err().contains("no <meta"));
    assert!(Split::parse(&page(MARKER)).unwrap_err().contains("more than once"));
    assert!(Split::parse(&format!("{MARKER}<html></html>")).unwrap_err().contains("before"));
}

#[test]
fn the_viewer_is_on_the_html_tag_and_in_the_bootstrap() {
    let split = Split::parse(&page("x")).unwrap();
    let out = split.render(&Injected { traceparent: None, version: None, bootstrap: &Bootstrap { viewer: None } });
    assert!(out.starts_with(r#"<!DOCTYPE html><html data-session="out" data-perms="" lang="en" class="dark"><head><meta charset="utf-8"><script id="bootstrap""#), "{out}");
    assert!(!out.contains("traceparent") && !out.contains("service-version") && !out.contains(MARKER));
    assert_eq!(bootstrap_of(&out), json!({"viewer": null}));

    let tp = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
    let bootstrap = viewer("Ann", vec![KnownPermission::Arcane, KnownPermission::ManagePermissions]);
    let out = split.render(&Injected { traceparent: Some(tp), version: Some("abc123"), bootstrap: &bootstrap });
    assert!(out.contains(r#"<html data-session="in" data-perms="arcane manage_permissions" lang="en""#), "{out}");
    assert!(out.contains(&format!(
        r#"<meta charset="utf-8"><meta name="traceparent" content="{tp}"><meta name="service-version" content="abc123"><script id="bootstrap""#
    )));
    assert_eq!(bootstrap_of(&out), serde_json::to_value(&bootstrap).unwrap());
    assert!(out.ends_with("<title>t</title></head><body>x</body></html>"));
}

#[test]
fn a_hostile_display_name_stays_data() {
    let split = Split::parse(&page("")).unwrap();
    for name in [
        "</script><script>alert(1)</script>",
        "</SCRIPT ><script>alert(1)</script>",
        "<!--<script>",
        "a\u{2028}b\u{2029}c",
        "&lt;script&gt; & \"quotes\" 'too' \\ </",
    ] {
        let bootstrap = viewer(name, vec![]);
        let out = split.render(&Injected { traceparent: None, version: None, bootstrap: &bootstrap });
        let json = &out[out.find(r#"type="application/json">"#).unwrap()..out.find("</script>").unwrap()];
        for c in ['<', '>', '&', '\u{2028}', '\u{2029}'] {
            assert!(!json[r#"type="application/json">"#.len()..].contains(c), "{c:?} in {json}");
        }
        assert_eq!(out.matches("</script>").count(), 1, "{out}");
        assert_eq!(bootstrap_of(&out)["viewer"]["shown_name"], name);
    }
}

#[test]
fn version_attributes_are_escaped() {
    let split = Split::parse(&page("")).unwrap();
    let out = split.render(&Injected { traceparent: None, version: Some(r#""><script>"#), bootstrap: &Bootstrap { viewer: None } });
    assert!(out.contains(r#"<meta name="service-version" content="&quot;&gt;&lt;script&gt;">"#), "{out}");
}

#[test]
fn the_build_must_have_exactly_the_pages() {
    let dist = Dist::new("exact");
    let built = Built::load(&dist.0, PAGES).unwrap();
    assert_eq!(built.pages.len(), api_types::PAGES.len() + api_types::STATUS_PAGES.len());

    std::fs::create_dir_all(dist.0.join("admin")).unwrap();
    std::fs::write(dist.0.join("admin/secret.html"), page("")).unwrap();
    let e = Built::load(&dist.0, PAGES).unwrap_err().to_string();
    assert!(e.contains("admin/secret.html"), "{e}");
    std::fs::remove_dir_all(dist.0.join("admin")).unwrap();

    std::fs::remove_file(dist.0.join("asshole.html")).unwrap();
    let e = Built::load(&dist.0, PAGES).unwrap_err().to_string();
    assert!(e.contains("asshole.html"), "{e}");

    std::fs::write(dist.0.join("asshole.html"), "<html></html>").unwrap();
    let e = Built::load(&dist.0, PAGES).unwrap_err().to_string();
    assert!(e.contains("asshole.html") && e.contains("bff-inject"), "{e}");

    assert!(Built::load(&dist.0.join("nowhere"), PAGES).is_err());
}

#[test]
fn a_build_that_would_serve_its_source_maps_is_refused() {
    let dist = Dist::new("maps");
    std::fs::write(dist.0.join("_astro/app.Ab12.js.map"), "{}").unwrap();
    let e = Built::load(&dist.0, PAGES).unwrap_err().to_string();
    assert!(e.contains("_astro/app.Ab12.js.map"), "{e}");
}

#[tokio::test]
async fn the_pages_mount_beside_the_api() {
    let dist = Dist::new("mount");
    // axum panics on overlapping routes when the router is built.
    let router = axum::Router::new()
        .route("/ws/arcane", axum::routing::get(|| async {}))
        .merge(crate::json_api::router());
    let app = super::mount(router, dist.built());
    // A page's path with a slash goes to the page.
    let res = app.clone().oneshot(Request::get("/asshole/?a=1").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(res.status(), StatusCode::PERMANENT_REDIRECT);
    assert_eq!(res.headers()[header::LOCATION], "/asshole?a=1");
    // Any other path is Astro's 404 page: an unknown path, and an encoded or upper-case
    // spelling of a page (routes match the path as sent).
    for path in ["/nope", "/%61sshole", "/ASSHOLE", "/index.html"] {
        let res = app.clone().oneshot(Request::get(path).body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND, "{path}");
        let body = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let html = String::from_utf8_lossy(&body);
        assert!(html.contains("<body>404.html</body>") && html.contains(r#"id="bootstrap""#), "{path}: {html}");
    }
    // An API path that doesn't exist is the error envelope, an old server function's too.
    for path in ["/api/nope", "/bff/nope"] {
        let res = app.clone().oneshot(Request::get(path).body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND, "{path}");
        let body: Value = serde_json::from_slice(&axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap()).unwrap();
        assert_eq!(body["error"], "not_found", "{path}");
    }
}

#[tokio::test]
async fn pages_and_astro_files_are_served_and_nothing_else() {
    let dist = Dist::new("serve");
    let mut site = Site::serving(super::mount(axum::Router::new(), dist.built())).await;

    let home = site.get("/").await;
    assert_eq!(home.status, StatusCode::OK);
    assert_eq!(home.content_type.as_deref(), Some("text/html; charset=utf-8"));
    let html = String::from_utf8(home.bytes).unwrap();
    assert!(html.contains(r#"data-session="out""#) && html.contains("<body>index.html</body>"), "{html}");
    assert_eq!(bootstrap_of(&html), json!({"viewer": null}));
    assert!(site.auth.introspected.lock().unwrap().is_empty(), "a logged-out page asks auth nothing");

    let asshole = site.get("/asshole").await;
    assert!(String::from_utf8(asshole.bytes).unwrap().contains("<body>asshole.html</body>"));

    for path in ["/index.html", "/asshole.html", "/404.html", "/_astro/../index.html", "/_astro/missing.js", "/_astro/"] {
        assert_eq!(site.get(path).await.status, StatusCode::NOT_FOUND, "{path}");
    }
    let asset = site.get("/_astro/app.Ab12.js").await;
    assert_eq!(asset.status, StatusCode::OK);
    assert_eq!(asset.bytes, b"console.log(1)");
}

#[tokio::test]
async fn cache_headers_keep_pages_private_and_files_forever() {
    let dist = Dist::new("cache");
    let app = super::mount(axum::Router::new(), dist.built())
        .layer(tower_sessions::SessionManagerLayer::new(tower_sessions::MemoryStore::default()));
    let get = |path: &'static str| {
        let app = app.clone();
        async move { app.oneshot(Request::get(path).body(Body::empty()).unwrap()).await.unwrap() }
    };
    let page = get("/").await;
    assert_eq!(page.headers()[header::CACHE_CONTROL], "private, no-cache");
    let asset = get("/_astro/app.Ab12.js").await;
    assert_eq!(asset.headers()[header::CACHE_CONTROL], "public, max-age=31536000, immutable");
    let missing = get("/_astro/missing.js").await;
    assert!(missing.headers().get(header::CACHE_CONTROL).is_none());

    // A revalidation keeps the file cacheable.
    let modified = asset.headers()[header::LAST_MODIFIED].clone();
    let req = Request::get("/_astro/app.Ab12.js").header(header::IF_MODIFIED_SINCE, modified).body(Body::empty()).unwrap();
    let not_modified = app.clone().oneshot(req).await.unwrap();
    assert_eq!(not_modified.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(not_modified.headers()[header::CACHE_CONTROL], "public, max-age=31536000, immutable");
}

#[tokio::test]
async fn astro_files_go_compressed_when_the_browser_takes_it() {
    let dist = Dist::new("compressed");
    let app = super::mount(axum::Router::new(), dist.built());
    let req = Request::get("/_astro/app.Ab12.js").header(header::ACCEPT_ENCODING, "gzip, br").body(Body::empty()).unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.headers()[header::CONTENT_ENCODING], "br");
    assert_eq!(res.headers()[header::VARY], "accept-encoding");
    let body = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    assert_eq!(&body[..], b"brotli");

    // Without brotli (and no gzip copy here): the file as it is.
    let req = Request::get("/_astro/app.Ab12.js").header(header::ACCEPT_ENCODING, "gzip").body(Body::empty()).unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert!(res.headers().get(header::CONTENT_ENCODING).is_none());
    let body = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    assert_eq!(&body[..], b"console.log(1)");
}

#[tokio::test]
async fn a_logged_in_page_knows_the_viewer_with_one_call_to_auth() {
    let dist = Dist::new("viewer");
    let mut site = Site::serving(super::mount(axum::Router::new(), dist.built())).await.log_in("all").await;
    let html = String::from_utf8(site.get("/").await.bytes).unwrap();
    // Known permissions only, in auth's order.
    assert!(html.contains(r#"<html data-session="in" data-perms="llama enshrouded arcane manage_permissions""#), "{html}");
    let viewer = &bootstrap_of(&html)["viewer"];
    assert_eq!((viewer["username"].as_str(), viewer["shown_name"].as_str()), (Some("user-all"), Some("Shown")));
    assert_eq!(site.auth.introspected("all"), 1);
    // One site at a time: each holds the stand-in auth's environment.
    drop(site);

    // Auth down: logged in, with no permissions.
    let mut site = Site::serving(super::mount(axum::Router::new(), dist.built())).await.log_in("down").await;
    let html = String::from_utf8(site.get("/").await.bytes).unwrap();
    assert!(html.contains(r#"data-session="in" data-perms="""#), "{html}");
}

#[test]
fn every_page_is_a_leaf_span() {
    #[derive(serde::Deserialize)]
    struct Registry {
        leaf: Vec<Leaf>,
    }
    #[derive(serde::Deserialize)]
    struct Leaf {
        service: String,
        name: String,
    }
    let registry: Registry = toml::from_str(include_str!("../../../../../../tracing/leaf_spans.toml")).unwrap();
    let leaf = |name: &str| registry.leaf.iter().any(|l| l.service == "frontend" && l.name == name);
    // A guarded page's redirect of the logged out is childless too, under the same name.
    for page in PAGES {
        let name = format!("GET {}", page.path.as_str());
        assert!(leaf(&name), "tracing/leaf_spans.toml has no [[leaf]] {name:?}: an anonymous request for it has no children");
        if page.path.as_str() != "/" {
            assert!(leaf(&format!("{name}/")), "tracing/leaf_spans.toml has no [[leaf]] \"{name}/\": its redirect has no children");
        }
    }
    assert!(leaf("GET /_astro/*"), "no [[leaf]] for Astro's files");
    assert!(leaf("GET NotFound"), "no [[leaf]] for the 404 page");
    assert!(leaf("GET /favicon.ico"), "no [[leaf]] for the icon");
    assert!(leaf("* /bff/{*rest}") && leaf("* /api/{*rest}"), "no [[leaf]] for the API's 404s");
}

/// The browser continues the page request's own SERVER span (TRACING.md, "Browser").
#[tokio::test]
async fn the_page_traceparent_is_the_request_span() {
    use opentelemetry::trace::SpanKind;
    let (exporter, _guard) = crate::trace_tests::exporting();
    let dist = Dist::new("trace");
    let app = super::mount(axum::Router::new(), dist.built())
        // As the server layers them: the session's own spans are current in the handler.
        .layer(tower_sessions::SessionManagerLayer::new(tower_sessions::MemoryStore::default()))
        .layer(axum::middleware::from_fn(crate::trace::capture_request_context))
        .layer(axum_tracing_opentelemetry::middleware::OtelAxumLayer::default());
    let res = app.oneshot(Request::get("/").body(Body::empty()).unwrap()).await.unwrap();
    let html = String::from_utf8(axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap().to_vec()).unwrap();

    let server = crate::trace_tests::span(&exporter, "GET /").await;
    assert_eq!(server.span_kind, SpanKind::Server);
    let sc = &server.span_context;
    let expected = format!(r#"<meta name="traceparent" content="00-{}-{}-01">"#, sc.trace_id(), sc.span_id());
    assert!(html.contains(&expected), "{expected} not in {html}");
}

// ---- Guards ----

/// The access levels, each on one page (`/asshole`) of a table otherwise public.
const LEVELS: [Access; 4] = [Access::Public, Access::GuestOnly, Access::LoggedIn, Access::Perm(KnownPermission::Arcane)];

/// What a page of `access` answers each kind of viewer, written out by hand: the status, and
/// where a redirect goes or which file is the body.
fn expected(access: Access, token: Option<&str>, path: &str) -> (StatusCode, String) {
    let page = |file: &str| (StatusCode::OK, file.to_string());
    let file = PAGES.iter().find(|p| p.path.as_str() == path).map_or("", |p| p.file);
    match (access, token) {
        (Access::Public, _) | (Access::GuestOnly, None) => page(file),
        (Access::GuestOnly, Some(_)) => (StatusCode::SEE_OTHER, "/".into()),
        (_, None) => (StatusCode::SEE_OTHER, format!("/login?next={path}")),
        (_, Some("down")) => (StatusCode::SERVICE_UNAVAILABLE, "503.html".into()),
        (Access::LoggedIn, Some(_)) | (Access::Perm(_), Some("all")) => page(file),
        (Access::Perm(_), Some(_)) => (StatusCode::FORBIDDEN, "403.html".into()),
    }
}

/// Anonymous, logged in without permissions, with every permission, and auth down.
const VIEWERS: [Option<&str>; 4] = [None, Some("none"), Some("all"), Some("down")];

/// Requests `path` as `token` from `app` and checks the answer against [`expected`].
async fn check(app: axum::Router, access: Access, token: Option<&str>, path: &str) {
    let site = Site::serving(app).await;
    let mut site = match token {
        Some(token) => site.log_in(token).await,
        None => site,
    };
    let reply = site.get(path).await;
    let (status, target) = expected(access, token, path);
    let who = format!("{path} ({access:?}) as {token:?}");
    assert_eq!(reply.status, status, "{who}");
    if status == StatusCode::SEE_OTHER {
        assert_eq!(reply.headers[header::LOCATION], target.as_str(), "{who}");
        return;
    }
    // Status pages too: spliced, private, with the viewer on the html tag.
    assert_eq!(reply.content_type.as_deref(), Some("text/html; charset=utf-8"), "{who}");
    assert_eq!(reply.headers[header::CACHE_CONTROL], "private, no-cache", "{who}");
    let retry = reply.headers.get(header::RETRY_AFTER).map(|v| v.to_str().unwrap());
    assert_eq!(retry, (status == StatusCode::SERVICE_UNAVAILABLE).then_some("30"), "{who}");
    let html = String::from_utf8(reply.bytes).unwrap();
    assert!(html.contains(&format!("<body>{target}</body>")), "{who}: {html}");
    assert!(!html.contains(MARKER), "{who}");
    let session = if token.is_some() { "in" } else { "out" };
    assert!(html.contains(&format!(r#"<html data-session="{session}""#)), "{who}: {html}");
    assert_eq!(bootstrap_of(&html)["viewer"].is_null(), token.is_none(), "{who}");
    if let Some(token) = token {
        // The guard and the page share one viewer: one introspect.
        assert_eq!(site.auth.introspected(token), 1, "{who}");
    }
}

#[test]
fn the_guard_rule_fails_closed() {
    use crate::auth::Viewer as V;
    use api_types::Permission;
    let user = |permissions: Vec<Permission>| {
        crate::json_api::Viewer(V::User { username: "u".into(), token: "t".into(), permissions })
    };
    let anonymous = crate::json_api::Viewer(V::Anonymous);
    let plain = user(vec![Permission::Other("arcane_admin".into())]);
    let arcane = user(vec![Permission::Known(KnownPermission::Arcane)]);
    let degraded = crate::json_api::Viewer(V::Degraded { username: "u".into() });
    let [public, guest, logged_in, perm] = LEVELS;
    // (access, anonymous, no permission, the permission, auth down, session unreadable)
    for (access, rules) in [
        (public, [Verdict::Serve; 5]),
        (guest, [Verdict::Serve, Verdict::Home, Verdict::Home, Verdict::Home, Verdict::Serve]),
        (logged_in, [Verdict::LogIn, Verdict::Serve, Verdict::Serve, Verdict::Unavailable, Verdict::Unavailable]),
        (perm, [Verdict::LogIn, Verdict::Forbidden, Verdict::Serve, Verdict::Unavailable, Verdict::Unavailable]),
    ] {
        let viewers = [Some(&anonymous), Some(&plain), Some(&arcane), Some(&degraded), None];
        for (viewer, rule) in viewers.into_iter().zip(rules) {
            assert_eq!(verdict(access, viewer), rule, "{access:?} for {viewer:?}");
        }
    }
    // Another permission, or one the site doesn't know by that name, is no permission.
    let llama = user(vec![Permission::Known(KnownPermission::Llama), Permission::Other("arcane".into())]);
    assert_eq!(verdict(perm, Some(&llama)), Verdict::Forbidden);
}

/// Every access level for every kind of viewer, through the router: the page, a redirect, the
/// 403 page or the 503 page.
#[tokio::test]
async fn every_access_level_answers_every_viewer() {
    let dist = Dist::new("guards");
    let built = dist.built();
    for access in LEVELS {
        let rows = [
            Page { path: PagePath::Home, file: "index.html", access: Access::Public },
            Page { path: PagePath::Asshole, file: "asshole.html", access },
            Page { path: PagePath::AccountDeleted, file: "account-deleted.html", access: Access::Public },
        ];
        for token in VIEWERS {
            check(super::mount_rows(axum::Router::new(), built.clone(), &rows), access, token, "/asshole").await;
        }
    }
}

/// The real table, every row for every kind of viewer.
#[tokio::test]
async fn every_page_answers_every_viewer() {
    let dist = Dist::new("table");
    for page in PAGES {
        for token in VIEWERS {
            check(super::mount(axum::Router::new(), dist.built()), page.access, token, page.path.as_str()).await;
        }
    }
}

#[tokio::test]
async fn a_logged_out_redirect_asks_auth_nothing() {
    let dist = Dist::new("anonymous");
    let rows = [Page { path: PagePath::Asshole, file: "asshole.html", access: Access::Perm(KnownPermission::Arcane) }];
    let mut site = Site::serving(super::mount_rows(axum::Router::new(), dist.built(), &rows)).await;
    // Its trailing-slash form is still a redirect to the page, which then guards it.
    let slash = site.get("/asshole/?x=1").await;
    assert_eq!(slash.status, StatusCode::PERMANENT_REDIRECT);
    assert_eq!(slash.headers[header::LOCATION], "/asshole?x=1");
    let reply = site.get("/asshole?x=1").await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    assert_eq!(reply.headers[header::LOCATION], "/login?next=/asshole");
    assert!(site.auth.introspected.lock().unwrap().is_empty());
}

/// A session store that can't be read, counting the attempts.
#[derive(Debug, Clone, Default)]
struct Unreadable(Arc<std::sync::atomic::AtomicUsize>);

#[async_trait::async_trait]
impl tower_sessions::SessionStore for Unreadable {
    async fn save(&self, _: &tower_sessions::session::Record) -> tower_sessions::session_store::Result<()> {
        Ok(())
    }
    async fn load(&self, _: &tower_sessions::session::Id) -> tower_sessions::session_store::Result<Option<tower_sessions::session::Record>> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err(tower_sessions::session_store::Error::Backend("down".into()))
    }
    async fn delete(&self, _: &tower_sessions::session::Id) -> tower_sessions::session_store::Result<()> {
        Ok(())
    }
}

/// A session that can't be read: a guest-only page is the page, logged out, as the guard decided
/// (the session isn't read again for it); a guarded page is the 503 page.
#[tokio::test]
async fn an_unreadable_session_is_read_once() {
    let dist = Dist::new("unreadable");
    for (access, status, file) in [
        (Access::GuestOnly, StatusCode::OK, "asshole.html"),
        (Access::LoggedIn, StatusCode::SERVICE_UNAVAILABLE, "503.html"),
    ] {
        let store = Unreadable::default();
        let rows = [Page { path: PagePath::Asshole, file: "asshole.html", access }];
        let app = super::mount_rows(axum::Router::new(), dist.built(), &rows)
            .layer(tower_sessions::SessionManagerLayer::new(store.clone()));
        let cookie = format!("id={}", tower_sessions::session::Id::default());
        let req = Request::get("/asshole").header(header::COOKIE, cookie).body(Body::empty()).unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), status, "{access:?}");
        let html = String::from_utf8(axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap().to_vec()).unwrap();
        assert!(html.contains(&format!("<body>{file}</body>")) && html.contains(r#"data-session="out""#), "{access:?}: {html}");
        assert_eq!(store.0.load(std::sync::atomic::Ordering::SeqCst), 1, "{access:?}");
    }
}

/// An unknown path: the 404 page with the viewer, private, as a 404; its span is `GET NotFound`
/// (a bounded name, never the path). A missing file's bare 404 isn't the page: its span keeps
/// the method alone.
#[tokio::test]
async fn an_unknown_path_is_the_404_page_and_its_span_is_named_for_it() {
    let (exporter, _guard) = crate::trace_tests::exporting();
    let dist = Dist::new("notfound");
    let app = super::mount(axum::Router::new(), dist.built())
        .layer(axum_tracing_opentelemetry::middleware::OtelAxumLayer::default());
    let mut site = Site::serving(app).await.log_in("all").await;
    let reply = site.get("/no/such/page?x=1").await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert_eq!(reply.headers[header::CACHE_CONTROL], "private, no-cache");
    let html = String::from_utf8(reply.bytes).unwrap();
    assert!(html.contains("<body>404.html</body>") && html.contains(r#"<html data-session="in""#), "{html}");
    crate::trace_tests::span(&exporter, "GET NotFound").await;

    assert_eq!(site.get("/robots.txt").await.status, StatusCode::NOT_FOUND);
    crate::trace_tests::span(&exporter, "GET").await;
    assert_eq!(crate::trace_tests::spans(&exporter, "GET NotFound").len(), 1);
}

/// What isn't a page load gets no page: an unknown API or server-function path is the error
/// envelope, a file the build doesn't have or another method a bare 404, all without asking
/// auth; the root icon is served.
#[tokio::test]
async fn only_page_loads_get_the_404_page() {
    let dist = Dist::new("notpages");
    let app = super::mount(axum::Router::new().merge(crate::json_api::router()), dist.built());
    let mut site = Site::serving(app).await.log_in("all").await;
    for path in ["/api/nope", "/bff/nope", "/bff/get_account99", "/api/", "/bff/"] {
        let reply = site.get(path).await;
        assert_eq!((reply.status, reply.body["error"].as_str()), (StatusCode::NOT_FOUND, Some("not_found")), "{path}");
    }
    for path in ["/robots.txt", "/apple-touch-icon.png", "/no/such.js"] {
        let reply = site.get(path).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{path}");
        assert!(reply.bytes.is_empty(), "{path}: a bare 404");
    }
    let posted = site.send(axum::http::Method::POST, "/nope", None, None).await;
    assert_eq!(posted.status, StatusCode::NOT_FOUND);
    assert!(posted.bytes.is_empty());
    assert_eq!(site.auth.introspected("all"), 0, "none of these asks auth");

    let icon = site.get("/favicon.ico").await;
    assert_eq!((icon.status, icon.bytes.as_slice()), (StatusCode::OK, &b"icon"[..]));
    assert_eq!(icon.headers[header::CACHE_CONTROL], "public, max-age=86400");
    // A page load still is the 404 page.
    assert_eq!(site.get("/nope").await.status, StatusCode::NOT_FOUND);
    assert_eq!(site.auth.introspected("all"), 1);
}
