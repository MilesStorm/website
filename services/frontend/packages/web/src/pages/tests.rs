//! The Astro pages: the startup checks of the build, the splice and its escaping, what a page
//! says about the viewer, and that the page's `traceparent` is the request's own span.

use std::path::PathBuf;
use std::sync::Arc;

use api_types::{Bootstrap, KnownPermission, Viewer};
use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt as _;

use super::{Built, Injected, Split, Ui, MARKER};
use crate::json_api::tests::{Public, Site};

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
        Self(dir)
    }

    fn built(&self) -> Arc<Built> {
        Arc::new(Built::load(&self.0).unwrap())
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
fn the_flag_is_astro_or_dioxus() {
    assert_eq!(Ui::parse(None).unwrap(), Ui::Dioxus);
    assert_eq!(Ui::parse(Some("")).unwrap(), Ui::Dioxus);
    assert_eq!(Ui::parse(Some("dioxus")).unwrap(), Ui::Dioxus);
    assert_eq!(Ui::parse(Some("astro")).unwrap(), Ui::Astro);
    assert!(Ui::parse(Some("Astro!")).is_err());
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
    let built = Built::load(&dist.0).unwrap();
    assert_eq!(built.pages.len(), api_types::PAGES.len() + api_types::STATUS_PAGES.len());

    std::fs::create_dir_all(dist.0.join("admin")).unwrap();
    std::fs::write(dist.0.join("admin/secret.html"), page("")).unwrap();
    let e = Built::load(&dist.0).unwrap_err().to_string();
    assert!(e.contains("admin/secret.html"), "{e}");
    std::fs::remove_dir_all(dist.0.join("admin")).unwrap();

    std::fs::remove_file(dist.0.join("asshole.html")).unwrap();
    let e = Built::load(&dist.0).unwrap_err().to_string();
    assert!(e.contains("asshole.html"), "{e}");

    std::fs::write(dist.0.join("asshole.html"), "<html></html>").unwrap();
    let e = Built::load(&dist.0).unwrap_err().to_string();
    assert!(e.contains("asshole.html") && e.contains("bff-inject"), "{e}");

    assert!(Built::load(&dist.0.join("nowhere")).is_err());
}

#[tokio::test]
async fn the_pages_mount_beside_the_dioxus_app_and_the_api() {
    use dioxus::prelude::*;
    let _public = Public::new("pages");
    let dist = Dist::new("mount");
    for built in [None, Some(dist.built())] {
        let astro = built.is_some();
        // axum panics on overlapping routes when the router is built.
        let router = axum::Router::new()
            .serve_dioxus_application(ServeConfig::default(), crate::App)
            .route("/ws/arcane", axum::routing::get(|| async {}))
            .merge(crate::json_api::router());
        let app = super::mount(router, built).unwrap();
        if astro {
            // A page's path with a slash goes to the page, never to Dioxus's fallback.
            let res = app.oneshot(Request::get("/asshole/?a=1").body(Body::empty()).unwrap()).await.unwrap();
            assert_eq!(res.status(), StatusCode::PERMANENT_REDIRECT);
            assert_eq!(res.headers()[header::LOCATION], "/asshole?a=1");
        }
    }
}

#[tokio::test]
async fn pages_and_astro_files_are_served_and_nothing_else() {
    let dist = Dist::new("serve");
    let mut site = Site::serving(super::mount(axum::Router::new(), Some(dist.built())).unwrap()).await;

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
    let app = super::mount(axum::Router::new(), Some(dist.built()))
        .unwrap()
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
    let app = super::mount(axum::Router::new(), Some(dist.built())).unwrap();
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
    let mut site = Site::serving(super::mount(axum::Router::new(), Some(dist.built())).unwrap()).await.log_in("all").await;
    let html = String::from_utf8(site.get("/").await.bytes).unwrap();
    // Known permissions only, in auth's order.
    assert!(html.contains(r#"<html data-session="in" data-perms="llama enshrouded arcane manage_permissions""#), "{html}");
    let viewer = &bootstrap_of(&html)["viewer"];
    assert_eq!((viewer["username"].as_str(), viewer["shown_name"].as_str()), (Some("user-all"), Some("Shown")));
    assert_eq!(site.auth.introspected("all"), 1);
    // One site at a time: each holds the stand-in auth's environment.
    drop(site);

    // Auth down: logged in, with no permissions.
    let mut site = Site::serving(super::mount(axum::Router::new(), Some(dist.built())).unwrap()).await.log_in("down").await;
    let html = String::from_utf8(site.get("/").await.bytes).unwrap();
    assert!(html.contains(r#"data-session="in" data-perms="""#), "{html}");
}

#[test]
fn every_public_page_is_a_leaf_span() {
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
    for page in api_types::PAGES.iter().filter(|p| p.access == api_types::Access::Public) {
        let name = format!("GET {}", page.path.as_str());
        assert!(leaf(&name), "tracing/leaf_spans.toml has no [[leaf]] {name:?}: an anonymous request for it has no children");
        if page.path.as_str() != "/" {
            assert!(leaf(&format!("{name}/")), "tracing/leaf_spans.toml has no [[leaf]] \"{name}/\": its redirect has no children");
        }
    }
    assert!(leaf("GET /_astro/*"), "no [[leaf]] for Astro's files");
}

/// The browser continues the page request's own SERVER span (TRACING.md, "Browser").
#[tokio::test]
async fn the_page_traceparent_is_the_request_span() {
    use opentelemetry::trace::SpanKind;
    let (exporter, _guard) = crate::trace_tests::exporting();
    let dist = Dist::new("trace");
    let app = super::mount(axum::Router::new(), Some(dist.built()))
        .unwrap()
        // As the server layers them: the session's own spans are current in the handler.
        .layer(tower_sessions::SessionManagerLayer::new(tower_sessions::MemoryStore::default()))
        .layer(axum::middleware::from_fn(api::trace::capture_request_context))
        .layer(axum_tracing_opentelemetry::middleware::OtelAxumLayer::default());
    let res = app.oneshot(Request::get("/").body(Body::empty()).unwrap()).await.unwrap();
    let html = String::from_utf8(axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap().to_vec()).unwrap();

    let server = crate::trace_tests::span(&exporter, "GET /").await;
    assert_eq!(server.span_kind, SpanKind::Server);
    let sc = &server.span_context;
    let expected = format!(r#"<meta name="traceparent" content="00-{}-{}-01">"#, sc.trace_id(), sc.span_id());
    assert!(html.contains(&expected), "{expected} not in {html}");
}
