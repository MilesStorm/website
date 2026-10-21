//! Astro's pages (`web/dist`). Only the paths in [`api_types::PAGES`] are pages: each is its own route, so
//! its span is named after it (`GET /`). The HTML is read once at startup and split where the
//! request's data goes: into the `<html` tag (`data-session`, `data-perms`, for the CSS gates)
//! and at the `<meta name="bff-inject">` marker (the page request's `traceparent`, the
//! `service-version` and the [`Bootstrap`] JSON). Astro's hashed files under `/_astro/` are
//! served from disk and cached for good; no other file of the build is served.
//!
//! A page that isn't public has a guard ([`guard`], a route layer): logged out, it redirects to
//! `/login?next=<page>`; without the permission it is the 403 page, and while auth can't say
//! who is logged in the 503 page. A guest-only page sends the logged in to `/`.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use api_types::{Access, Bootstrap, ErrorCode, Page, PAGES, STATUS_PAGES};
use axum::extract::{FromRequestParts, Request, State};
use axum::http::header::{HeaderValue, CACHE_CONTROL, CONTENT_TYPE};
use axum::http::StatusCode;
use axum::middleware::{from_fn_with_state, Next};
use axum::response::{IntoResponse, Redirect, Response};
use axum::Router;

use crate::json_api;

/// Where the request's data goes in each page's `<head>`. An element, not a comment: Astro's
/// HTML compression may drop comments.
pub(crate) const MARKER: &str = r#"<meta name="bff-inject">"#;
const HTML_OPEN: &str = "<html";

/// The Astro build, read from `WEB_DIST` (default `web/dist`, as in a checkout).
pub fn from_env() -> anyhow::Result<Arc<Built>> {
    let dir = std::env::var_os("WEB_DIST").map_or_else(|| PathBuf::from("web/dist"), PathBuf::from);
    let built = Built::load(&dir, PAGES)?;
    tracing::info!(dir = %dir.display(), pages = PAGES.len(), "serving the Astro pages");
    Ok(Arc::new(built))
}

/// A page split at its two insertion points.
#[derive(Debug)]
pub struct Split {
    /// Up to and including `<html`.
    start: String,
    /// The rest of the `<html …>` tag up to the marker.
    head: String,
    /// After the marker.
    rest: String,
}

/// What one request puts into a page.
pub struct Injected<'a> {
    pub traceparent: Option<&'a str>,
    pub version: Option<&'a str>,
    pub bootstrap: &'a Bootstrap,
}

impl Split {
    pub fn parse(html: &str) -> Result<Self, String> {
        let at = |needle: &str| match html.match_indices(needle).map(|(i, _)| i).collect::<Vec<_>>()[..] {
            [i] => Ok(i),
            [] => Err(format!("no {needle}")),
            _ => Err(format!("{needle} more than once")),
        };
        let open = at(HTML_OPEN)? + HTML_OPEN.len();
        let marker = at(MARKER)?;
        if marker < open {
            return Err(format!("{MARKER} before {HTML_OPEN}"));
        }
        Ok(Self {
            start: html[..open].to_string(),
            head: html[open..marker].to_string(),
            rest: html[marker + MARKER.len()..].to_string(),
        })
    }

    pub fn render(&self, with: &Injected) -> String {
        let viewer = with.bootstrap.viewer.as_ref();
        let session = if viewer.is_some() { "in" } else { "out" };
        let perms: Vec<&str> = viewer.map_or(vec![], |v| v.permissions.iter().map(|p| p.as_str()).collect());
        let json = script_json(with.bootstrap);

        let mut html = String::with_capacity(self.start.len() + self.head.len() + self.rest.len() + json.len() + 256);
        html.push_str(&self.start);
        // Values the site names itself (`in`/`out`, KnownPermission): nothing to escape.
        html.push_str(&format!(r#" data-session="{session}" data-perms="{}""#, perms.join(" ")));
        html.push_str(&self.head);
        if let Some(tp) = with.traceparent {
            html.push_str(&format!(r#"<meta name="traceparent" content="{}">"#, attr(tp)));
        }
        if let Some(version) = with.version {
            html.push_str(&format!(r#"<meta name="service-version" content="{}">"#, attr(version)));
        }
        html.push_str(r#"<script id="bootstrap" type="application/json">"#);
        html.push_str(&json);
        html.push_str("</script>");
        html.push_str(&self.rest);
        html
    }
}

/// `value` as JSON that is safe inside `<script>`: nothing in it can end the element (`</script>`),
/// open a comment (`<!--`) or an entity, or end a line for an old JavaScript parser (U+2028,
/// U+2029). The characters are escaped as `\uXXXX`, which only occurs inside JSON strings, so
/// the JSON reads back the same.
pub fn script_json(value: &impl serde::Serialize) -> String {
    let json = serde_json::to_string(value).expect("the bootstrap serializes");
    let mut out = String::with_capacity(json.len());
    for c in json.chars() {
        match c {
            '<' => out.push_str(r"\u003c"),
            '>' => out.push_str(r"\u003e"),
            '&' => out.push_str(r"\u0026"),
            '\u{2028}' => out.push_str(r"\u2028"),
            '\u{2029}' => out.push_str(r"\u2029"),
            c => out.push(c),
        }
    }
    out
}

/// `value` inside a double-quoted attribute.
fn attr(value: &str) -> String {
    value.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;").replace('>', "&gt;")
}

/// The Astro build: every page and status page, split.
#[derive(Debug)]
pub struct Built {
    dir: PathBuf,
    pages: HashMap<&'static str, Split>,
}

impl Built {
    /// Reads `dir`, the build of `rows` ([`PAGES`]). Fails when a page or status page is
    /// missing, can't be split, or the build has an HTML file that is neither: a page without a
    /// row in `PAGES` would have no access rule.
    pub fn load(dir: &Path, rows: &[Page]) -> anyhow::Result<Self> {
        let mut found = BTreeSet::new();
        html_files(dir, dir, &mut found).map_err(|e| anyhow::anyhow!("can't read the Astro build at {}: {e}", dir.display()))?;
        let expected: BTreeSet<&str> = rows.iter().map(|p| p.file).chain(STATUS_PAGES.iter().copied()).collect();
        let unlisted: Vec<_> = found.iter().filter(|f| !expected.contains(f.as_str())).collect();
        if !unlisted.is_empty() {
            anyhow::bail!("{}: HTML that isn't a page in PAGES or a status page: {unlisted:?}", dir.display());
        }
        let mut pages = HashMap::new();
        for file in expected {
            let path = dir.join(file);
            let html = std::fs::read_to_string(&path).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
            let split = Split::parse(&html).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
            pages.insert(file, split);
        }
        Ok(Self { dir: dir.to_path_buf(), pages })
    }

    fn page(&self, file: &str) -> &Split {
        &self.pages[file]
    }
}

/// The `.html` files under `dir`, by their path relative to `root`.
fn html_files(root: &Path, dir: &Path, found: &mut BTreeSet<String>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            html_files(root, &path, found)?;
        } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("html")) {
            let relative = path.strip_prefix(root).expect("under the root");
            found.insert(relative.to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(())
}

/// Adds the pages and `/_astro/*` to `router`, and the 404 page as its fallback: every page has
/// its own route.
pub fn mount<S: Clone + Send + Sync + 'static>(router: Router<S>, built: Arc<Built>) -> Router<S> {
    mount_rows(router, built, PAGES)
}

/// [`mount`] with `rows` for the pages: a page that isn't public gets its [`guard`].
fn mount_rows<S: Clone + Send + Sync + 'static>(router: Router<S>, built: Arc<Built>, rows: &[Page]) -> Router<S> {
    let mut router = router;
    for page in rows {
        let route = axum::routing::get(serve).with_state((built.clone(), page.file));
        let route = match page.access {
            Access::Public => route,
            _ => route.route_layer(from_fn_with_state((built.clone(), *page), guard)),
        };
        router = router.route(page.path.as_str(), route);
        // `/asshole/` is the same page: sent to its one address.
        if page.path.as_str() != "/" {
            router = router.route(&format!("{}/", page.path.as_str()), axum::routing::get(canonical));
        }
    }
    let files = tower_http::services::ServeDir::new(&built.dir).precompressed_br().precompressed_gzip();
    let assets = Router::new()
        .route_service("/_astro/{*path}", files)
        .layer(axum::middleware::map_response(immutable));
    // The icon browsers ask for at the root (Astro's `public/`), not hashed: kept a day.
    let favicon = Router::new()
        .route_service("/favicon.ico", tower_http::services::ServeFile::new(built.dir.join("favicon.ico")))
        .layer(axum::middleware::map_response(a_day));
    router
        .merge(assets)
        .merge(favicon)
        // The old Dioxus app's server functions, which a tab still open from before may call:
        // an API that doesn't exist, answered as `/api/*`'s are.
        .route("/bff/{*rest}", axum::routing::any(|| async { json_api::no_such_api() }))
        .fallback(move |req: Request| not_found(built.clone(), req))
}

/// `favicon.ico`'s cache header, a 304 included.
async fn a_day(mut res: Response) -> Response {
    if res.status().is_success() || res.status() == StatusCode::NOT_MODIFIED {
        res.headers_mut().insert(CACHE_CONTROL, HeaderValue::from_static("public, max-age=86400"));
    }
    res
}

/// Any other path: the 404 page, spliced like every page, with status 404, its span named
/// `GET NotFound` (a bounded name, never the path). A request that isn't a page load (a file the
/// build doesn't have, such as `/robots.txt`, or a method other than GET/HEAD) gets a bare 404,
/// asks auth nothing, and its span keeps the method alone as its name (no route matched).
async fn not_found(built: Arc<Built>, req: Request) -> Response {
    use opentelemetry::trace::TraceContextExt as _;
    use tracing_opentelemetry::OpenTelemetrySpanExt as _;
    if !page_like(&req) {
        return StatusCode::NOT_FOUND.into_response();
    }
    // tracing-opentelemetry takes an `otel.name` recorded only before the span has started; by
    // now it has, so the started span is renamed itself.
    tracing::Span::current().context().span().update_name(format!("{} NotFound", req.method()));
    let request = req.extensions().get::<api::trace::RequestTraceContext>().cloned();
    let (mut parts, _) = req.into_parts();
    let bootstrap = match json_api::Viewer::from_request_parts(&mut parts, &()).await {
        Ok(viewer) => json_api::bootstrap(viewer.0).await,
        // Logged out is what the page then shows; the reason is logged by the extractor.
        Err(_) => Bootstrap { viewer: None },
    };
    respond(&built, "404.html", StatusCode::NOT_FOUND, request.as_ref(), &bootstrap)
}

/// Whether `req` could be a page load: GET or HEAD, of a path whose last part names no file
/// other than an HTML one.
fn page_like(req: &Request) -> bool {
    let method = req.method();
    let last = req.uri().path().rsplit('/').next().unwrap_or("");
    (method == axum::http::Method::GET || method == axum::http::Method::HEAD)
        && (!last.contains('.') || last.ends_with(".html"))
}

/// What a page that isn't public answers a viewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// The page.
    Serve,
    /// A redirect to `/login?next=<page>`.
    LogIn,
    /// A redirect to `/`: a guest-only page, logged in.
    Home,
    /// The 403 page: logged in, without the permission.
    Forbidden,
    /// The 503 page: auth can't say who is logged in, or the session can't be read.
    Unavailable,
}

/// The access rule of a page: the API's ([`json_api::allowed`]), with the page's answers.
/// `viewer` is `None` when the session can't be read. Fails closed: whatever isn't known to be
/// allowed is no page.
fn verdict(access: Access, viewer: Option<&json_api::Viewer>) -> Verdict {
    let logged_in = |v: &json_api::Viewer| !matches!(v.0, api::Viewer::Anonymous);
    match (access, viewer) {
        (Access::Public, _) => Verdict::Serve,
        // Unknown counts as logged out: the page shows the logged-out navbar too.
        (Access::GuestOnly, Some(v)) if logged_in(v) => Verdict::Home,
        (Access::GuestOnly, _) => Verdict::Serve,
        (_, None) => Verdict::Unavailable,
        (access, Some(v)) => match json_api::allowed(access, v) {
            Ok(()) => Verdict::Serve,
            Err(e) => match e.0.error {
                ErrorCode::LoggedOut => Verdict::LogIn,
                ErrorCode::Forbidden => Verdict::Forbidden,
                _ => Verdict::Unavailable,
            },
        },
    }
}

/// The route layer of a page that isn't public. The viewer is resolved here and kept in the
/// request, so the page's own splice asks auth nothing more. Redirects and status pages are
/// traced under the page's route (`GET /admin`).
async fn guard(State((built, page)): State<(Arc<Built>, Page)>, req: Request, next: Next) -> Response {
    let (mut parts, body) = req.into_parts();
    // Logged by the extractor when it fails.
    let viewer = json_api::Viewer::from_request_parts(&mut parts, &()).await.ok();
    let (file, status) = match verdict(page.access, viewer.as_ref()) {
        Verdict::Serve => {
            // The page shows what was decided here: a session that can't be read is logged out
            // (a guest-only page), not read a second time.
            if viewer.is_none() {
                parts.extensions.insert(api::Viewer::Anonymous);
            }
            return next.run(Request::from_parts(parts, body)).await;
        }
        // A path has nothing to escape in a query (`api-types` tests it).
        Verdict::LogIn => return Redirect::to(&format!("/login?next={}", page.path.as_str())).into_response(),
        Verdict::Home => return Redirect::to("/").into_response(),
        Verdict::Forbidden => ("403.html", StatusCode::FORBIDDEN),
        Verdict::Unavailable => ("503.html", StatusCode::SERVICE_UNAVAILABLE),
    };
    let bootstrap = match viewer {
        Some(viewer) => json_api::bootstrap(viewer.0).await,
        None => Bootstrap { viewer: None },
    };
    let request = parts.extensions.get::<api::trace::RequestTraceContext>();
    let mut res = respond(&built, file, status, request, &bootstrap);
    if status == StatusCode::SERVICE_UNAVAILABLE {
        // Auth is usually back within moments.
        res.headers_mut().insert(axum::http::header::RETRY_AFTER, HeaderValue::from_static("30"));
    }
    res
}

/// A page's path with a trailing slash: a permanent redirect to the path without it, keeping
/// the query.
async fn canonical(uri: axum::http::Uri) -> axum::response::Redirect {
    let path = uri.path().trim_end_matches('/');
    let to = match uri.query() {
        Some(query) => format!("{path}?{query}"),
        None => path.to_owned(),
    };
    axum::response::Redirect::permanent(&to)
}

/// Astro's file names carry a hash of their content, so a file never changes: cached for good,
/// a 304 included (a revalidation answered without the header would leave it uncacheable). The
/// body may be brotli or gzip, chosen by the request's `Accept-Encoding`.
async fn immutable(mut res: Response) -> Response {
    if res.status().is_success() || res.status() == axum::http::StatusCode::NOT_MODIFIED {
        let headers = res.headers_mut();
        headers.insert(CACHE_CONTROL, HeaderValue::from_static("public, max-age=31536000, immutable"));
        headers.insert(axum::http::header::VARY, HeaderValue::from_static("accept-encoding"));
    }
    res
}

/// A page, with this request's data spliced in.
async fn serve(
    State((built, file)): State<(Arc<Built>, &'static str)>,
    request: Option<axum::Extension<api::trace::RequestTraceContext>>,
    viewer: Result<json_api::Viewer, json_api::ApiError>,
) -> Response {
    let bootstrap = match viewer {
        Ok(viewer) => json_api::bootstrap(viewer.0).await,
        // Logged out is what the page then shows; the reason is logged by the extractor.
        Err(_) => Bootstrap { viewer: None },
    };
    respond(&built, file, StatusCode::OK, request.as_ref().map(|r| &r.0), &bootstrap)
}

/// `file` of the build with `status`, spliced with `bootstrap` and the request's trace.
fn respond(
    built: &Built,
    file: &str,
    status: StatusCode,
    request: Option<&api::trace::RequestTraceContext>,
    bootstrap: &Bootstrap,
) -> Response {
    let traceparent = request.and_then(api::trace::page_traceparent);
    let version = api::trace::service_version();
    let html = built.page(file).render(&Injected {
        traceparent: traceparent.as_deref(),
        version: version.as_deref(),
        bootstrap,
    });
    (
        status,
        [
            (CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8")),
            // Per request (the viewer, the trace): never from a shared cache. The browser's
            // back/forward cache stays on.
            (CACHE_CONTROL, HeaderValue::from_static("private, no-cache")),
        ],
        html,
    )
        .into_response()
}

#[cfg(test)]
mod tests;
