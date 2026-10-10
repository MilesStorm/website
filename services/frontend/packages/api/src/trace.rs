//! Trace propagation for the BFF. The conventions are in TRACING.md at the repository root.

use std::future::Future;

use dioxus::fullstack::Transportable;
use dioxus::prelude::{RenderError, Resource};
use tracing::instrument::{Instrument as _, Instrumented};

/// `use_server_future`, with the future joined to the page request's trace during SSR.
///
/// Dioxus runs server-function requests inside the request's span, but renders SSR on
/// its local pool without it (dioxus-server 0.7 `ssr.rs`, `spawn_pinned`). A server
/// future polled during SSR would otherwise start a trace of its own. `name` names its
/// span. TODO: drop this wrapper once Dioxus instruments the SSR render (check
/// `rt.spawn_pinned(create_render_future)` in dioxus-server's `ssr.rs` when upgrading).
#[track_caller]
#[allow(clippy::disallowed_methods, reason = "trace::use_server_future is the wrapper the lint points to")]
pub fn use_server_future<T, F, M>(
    name: &'static str,
    mut future: impl FnMut() -> F + 'static,
) -> Result<Resource<T>, RenderError>
where
    F: Future<Output = T> + 'static,
    T: Transportable<M>,
    M: 'static,
{
    dioxus::prelude::use_server_future(move || in_request_trace(name, future()))
}

/// Runs `fut` in a span that belongs to the current page request's trace. Outside a
/// request (and in the browser) the span is disabled and `fut` runs as it would anyway.
pub fn in_request_trace<F: Future>(name: &'static str, fut: F) -> Instrumented<F> {
    #[cfg(feature = "server")]
    if let Some(cx) = server::request_context() {
        use tracing_opentelemetry::OpenTelemetrySpanExt as _;
        // Parented through OTel only, so the span holds no reference that would keep
        // the request's span open. Fails only without the OTel layer: the span is new.
        let span = tracing::info_span!(parent: None, "ssr.server_future", otel.name = name);
        let _ = span.set_parent(cx);
        return fut.instrument(span);
    }
    let _ = name;
    fut.instrument(tracing::Span::none())
}

/// The W3C `traceparent` of the page request being served, for the `<meta>` the
/// browser's tracer continues from (TRACING.md, "Browser"). `None` in the browser,
/// outside a request, and for unsampled requests: the browser then decides for itself.
pub fn traceparent() -> Option<String> {
    #[cfg(feature = "server")]
    {
        use opentelemetry::trace::TraceContextExt as _;
        let cx = server::server_span_context()?;
        let sc = cx.span().span_context().clone();
        (sc.is_valid() && sc.is_sampled())
            .then(|| format!("00-{}-{}-{:02x}", sc.trace_id(), sc.span_id(), sc.trace_flags().to_u8()))
    }
    #[cfg(not(feature = "server"))]
    None
}

/// The commit this build was made from (the image's `GIT_SHA`), for the `<meta>` the
/// browser's tracer reads its `service.version` from: the browser runs the bundle of the
/// same build. `None` in the browser and where the image doesn't say.
pub fn service_version() -> Option<String> {
    #[cfg(feature = "server")]
    {
        std::env::var("GIT_SHA").ok().filter(|sha| !sha.is_empty())
    }
    #[cfg(not(feature = "server"))]
    None
}

/// An INFO span for work that outlives the current span: a spawned task, a queued job,
/// a WebSocket session. It belongs to the current trace, but through OTel only, so it
/// holds no reference that would keep the current span open (and unexported) until the
/// work ends. Takes `info_span!` arguments: `detached_span!("name", field = value)`.
#[cfg(feature = "server")]
#[macro_export]
macro_rules! detached_span {
    ($name:literal $(, $($fields:tt)*)?) => {{
        let span = $crate::trace::__tracing::info_span!(
            parent: None,
            $name,
            trace_id = $crate::trace::__tracing::field::Empty,
            span_id = $crate::trace::__tracing::field::Empty,
            $($($fields)*)?
        );
        $crate::trace::continue_current_trace(&span);
        span
    }};
}

/// An INFO span that roots a trace of its own: one unit of work inside something
/// long-lived (a roll in a camera session, one pass of a background loop). `link` names
/// what it belongs to or was caused by ([`here`], a session's open span), since a unit has
/// no parent. Takes `info_span!` arguments after the link:
/// `unit_span!(link, "name", field = value)`.
#[cfg(feature = "server")]
#[macro_export]
macro_rules! unit_span {
    ($link:expr, $name:literal $(, $($fields:tt)*)?) => {{
        let span = $crate::trace::__tracing::info_span!(
            parent: None,
            $name,
            trace_id = $crate::trace::__tracing::field::Empty,
            span_id = $crate::trace::__tracing::field::Empty,
            $($($fields)*)?
        );
        $crate::trace::start_unit(&span, $link);
        span
    }};
}

#[cfg(feature = "server")]
#[doc(hidden)]
pub use tracing as __tracing;

/// Puts `span` in the current span's trace (OTel parent only). Use [`detached_span!`].
#[cfg(feature = "server")]
pub fn continue_current_trace(span: &tracing::Span) {
    use tracing_opentelemetry::OpenTelemetrySpanExt as _;
    // Fails only without the OTel layer, or once `span` has been entered.
    let _ = span.set_parent(tracing::Span::current().context());
    server::record_ids(span);
}

#[cfg(feature = "server")]
pub use server::*;

#[cfg(feature = "server")]
mod server {
    use std::borrow::Cow;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::LazyLock;
    use std::task::{Context, Poll};
    use std::time::Duration;

    use dioxus::fullstack::FullstackContext;
    use dioxus::server::axum;
    use fred::prelude::{Builder, Config, ConnectionConfig, ReconnectPolicy, TracingConfig};
    use opentelemetry::propagation::Injector;
    use opentelemetry::trace::{SpanContext, Status, TraceContextExt as _};
    use reqwest_tracing::{reqwest_otel_span, ReqwestOtelSpanBackend, TracingMiddleware};
    use tokio::task::JoinHandle;
    use tokio_tungstenite::tungstenite;
    use tokio_util::task::TaskTracker;
    use tower_sessions::session::{Id, Record};
    use tower_sessions::{session_store, SessionStore};
    use tracing::Instrument as _;
    use tracing_opentelemetry::OpenTelemetrySpanExt as _;

    /// The OTel context of the request being served, for work Dioxus runs outside the
    /// request's span. Set by [`capture_request_context`].
    #[derive(Clone)]
    pub struct RequestTraceContext(pub opentelemetry::Context);

    /// Axum middleware, layered inside `OtelAxumLayer`: keeps the request span's OTel
    /// context in the request so [`in_request_trace`](super::in_request_trace) can find
    /// it, and records the trace ID on the span so every log line of the request has it.
    pub async fn capture_request_context(
        mut req: axum::extract::Request,
        next: axum::middleware::Next,
    ) -> axum::response::Response {
        let span = tracing::Span::current();
        let cx = span.context();
        let sc = cx.span().span_context().clone();
        if sc.is_valid() {
            span.record("trace_id", sc.trace_id().to_string());
            req.extensions_mut().insert(RequestTraceContext(cx));
        }
        next.run(req).await
    }

    /// The OTel context of the `ssr.render` span, for the server futures the render polls.
    /// Set by [`ssr_render`].
    #[derive(Clone)]
    pub struct RenderTraceContext(pub opentelemetry::Context);

    /// Where work Dioxus runs outside the request's span belongs: the page's `ssr.render`
    /// span while one is open, else the request's span.
    pub(crate) fn request_context() -> Option<opentelemetry::Context> {
        let cx = FullstackContext::current()?;
        cx.extension::<RenderTraceContext>().map(|c| c.0).or_else(|| cx.extension::<RequestTraceContext>().map(|c| c.0))
    }

    /// The request span's own context, whatever is rendering: what the browser continues from.
    pub(crate) fn server_span_context() -> Option<opentelemetry::Context> {
        FullstackContext::current()?.extension::<RequestTraceContext>().map(|c| c.0)
    }

    /// Axum middleware for the Dioxus page handler: an `ssr.render` span around the render,
    /// with the server futures it waits for as children. Without it the time a page spends
    /// rendering is the request span's unexplained own time.
    pub async fn ssr_render(mut req: axum::extract::Request, next: axum::middleware::Next) -> axum::response::Response {
        let span = tracing::info_span!("ssr.render");
        let cx = span.context();
        if cx.span().span_context().is_valid() {
            req.extensions_mut().insert(RenderTraceContext(cx));
        }
        next.run(req).instrument(span).await
    }

    /// Axum middleware, layered inside `OtelAxumLayer`: the request's span ends when the
    /// response body has been sent (or dropped), not when its headers are ready, so sending
    /// a large asset or a streamed page is inside the span (OTel HTTP semconv). Not for
    /// event streams and protocol upgrades: those last as long as the session, and their
    /// work is traced per unit (TRACING.md, "Streams").
    ///
    /// tower-http's `TraceLayer` also runs to the end of the body, but with a span of its
    /// own and for every response alike: it can't keep `OtelAxumLayer`'s span open, and it
    /// can't leave event streams and upgrades out.
    pub async fn end_span_with_body(req: axum::extract::Request, next: axum::middleware::Next) -> axum::response::Response {
        use axum::http::{header::CONTENT_TYPE, StatusCode};
        let res = next.run(req).await;
        let event_stream = res
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| ct.starts_with("text/event-stream"));
        if event_stream || res.status() == StatusCode::SWITCHING_PROTOCOLS {
            return res;
        }
        let span = tracing::Span::current();
        res.map(|inner| axum::body::Body::new(SpanBody { inner, span }))
    }

    /// A response body that keeps `span` open until it is dropped: tracing-opentelemetry
    /// ends a span when its last handle goes. Entered on each poll, so the time spent
    /// producing frames counts as the span's busy time.
    struct SpanBody {
        inner: axum::body::Body,
        span: tracing::Span,
    }

    impl http_body::Body for SpanBody {
        type Data = axum::body::Bytes;
        type Error = axum::Error;

        fn poll_frame(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
            let this = &mut *self;
            let _entered = this.span.enter();
            let polled = Pin::new(&mut this.inner).poll_frame(cx);
            if let Poll::Ready(Some(Err(_))) = &polled {
                // The status line said 200, but the client did not get the whole body.
                failed("response_body");
            }
            polled
        }

        fn is_end_stream(&self) -> bool {
            self.inner.is_end_stream()
        }

        fn size_hint(&self) -> http_body::SizeHint {
            self.inner.size_hint()
        }
    }

    /// An HTTP client from [`client`].
    pub type TracedClient = reqwest_middleware::ClientWithMiddleware;

    /// The only way to build an HTTP client (clippy bans reqwest's own constructors): every
    /// request gets a CLIENT span (child of the current span) named for `peer`, carries its
    /// trace context to the server, and gives up after `timeout`. `configure` sets anything
    /// else on the builder: `trace::client(peer, timeout, |b| b)`.
    #[allow(clippy::disallowed_types, clippy::disallowed_methods, reason = "trace::client is the traced HTTP client constructor")]
    pub fn client(
        peer: Peer,
        timeout: Duration,
        configure: impl FnOnce(reqwest::ClientBuilder) -> reqwest::ClientBuilder,
    ) -> reqwest::Result<TracedClient> {
        // The timeout goes on last, so `configure` can't take it off.
        let inner = configure(reqwest::Client::builder()).timeout(timeout).build()?;
        Ok(reqwest_middleware::ClientBuilder::new(inner)
            .with_init(reqwest_middleware::Extension(peer))
            .with(TracingMiddleware::<PathSpans>::new())
            .build())
    }

    /// The service a [`client`]'s requests go to: `peer.service` on its CLIENT spans (the
    /// node in Tempo's service graph), and for a database `db.system` too.
    #[derive(Clone, Copy, Debug)]
    pub struct Peer {
        pub service: &'static str,
        pub system: Option<&'static str>,
    }

    /// Request extension: response statuses that are an expected answer here, not a failure
    /// (auth's 401 for a wrong password or an unknown token). The CLIENT span then stays OK
    /// and still records the status code; any other 4xx/5xx is an error as OTel says.
    /// `client.post(url).with_extension(Expected(&[401]))`.
    #[derive(Clone, Copy, Debug)]
    pub struct Expected(pub &'static [u16]);

    /// On auth's token endpoints (`/internal/token/*`): 401 is a wrong password or a token
    /// that no longer exists, 404 an unknown one. The one documented deviation from OTel's
    /// "4xx is a client-span error" (TRACING.md, "Errors").
    pub const WRONG_TOKEN_OK: Expected = Expected(&[401, 404]);

    /// Runs the body of a server function whose 4xx replies are answers to the user (a
    /// wrong password, a used-up link: [`auth_error`](crate::auth_error)), and marks the
    /// current span failed for any other error. Functions without such replies use
    /// `#[instrument(err)]` instead.
    pub async fn rejectable<T>(
        body: impl Future<Output = Result<T, dioxus::prelude::ServerFnError>>,
    ) -> Result<T, dioxus::prelude::ServerFnError> {
        use dioxus::prelude::ServerFnError;
        let result = body.await;
        match &result {
            Ok(_) | Err(ServerFnError::ServerError { code: 400..=499, .. }) => {}
            Err(_) => failed("server_function"),
        }
        result
    }

    /// Request extension for a database call over HTTP: the span is named `name`
    /// (`surrealdb.query`) instead of by method and path, with `operation` as
    /// `db.operation.name` (the statement kind, `SELECT`).
    #[derive(Clone, Debug)]
    pub struct DbCall {
        pub name: &'static str,
        pub operation: String,
    }

    /// Names client spans `METHOD /path`, with numeric segments (IDs) as `{id}` so
    /// span names stay low-cardinality without a list of routes to keep up to date.
    struct PathSpans;

    impl ReqwestOtelSpanBackend for PathSpans {
        fn on_request_start(req: &reqwest::Request, ext: &mut http::Extensions) -> tracing::Span {
            // Templated in the attribute too: raw paths could carry user data.
            let path = route(req.url().path());
            let call = ext.get::<DbCall>();
            let name = match call {
                Some(call) => call.name.to_string(),
                None => format!("{} {path}", req.method()),
            };
            let peer = ext.get::<Peer>();
            reqwest_otel_span!(
                name = name,
                req,
                url.path = %path,
                peer.service = peer.map(|p| p.service),
                db.system = peer.and_then(|p| p.system),
                db.system.name = peer.and_then(|p| p.system),
                db.operation.name = call.map(|c| c.operation.as_str()),
            )
        }

        fn on_request_end(
            span: &tracing::Span,
            outcome: &reqwest_middleware::Result<reqwest::Response>,
            ext: &mut http::Extensions,
        ) {
            match outcome {
                Ok(res) if ext.get::<Expected>().is_some_and(|e| e.0.contains(&res.status().as_u16())) => {
                    span.record("http.response.status_code", res.status().as_u16());
                }
                _ => reqwest_tracing::default_on_request_end(span, outcome),
            }
        }
    }

    fn route(path: &str) -> Cow<'_, str> {
        let is_id = |seg: &str| !seg.is_empty() && seg.bytes().all(|b| b.is_ascii_digit());
        if !path.split('/').any(is_id) {
            return Cow::Borrowed(path);
        }
        let segs: Vec<&str> = path.split('/').map(|s| if is_id(s) { "{id}" } else { s }).collect();
        Cow::Owned(segs.join("/"))
    }

    /// The upstream side of a proxied WebSocket.
    pub type WsStream = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

    /// The only way to open a WebSocket to another service (clippy bans tungstenite's own
    /// connect functions): the handshake is a CLIENT span (child of the current span) named
    /// for `peer`, it carries the span's trace context in its headers, and it gives up after
    /// `timeout`.
    #[allow(clippy::disallowed_methods, reason = "trace::connect_ws is the traced WebSocket connect")]
    pub async fn connect_ws(url: &str, peer: &'static str, timeout: Duration) -> Result<WsStream, tungstenite::Error> {
        use tungstenite::client::IntoClientRequest as _;

        let mut req = url.into_client_request()?;
        let path = route(req.uri().path()).into_owned();
        let span = tracing::info_span!(
            "ws.connect",
            otel.name = format!("GET {path}"),
            otel.kind = "client",
            otel.status_code = tracing::field::Empty,
            "error.type" = tracing::field::Empty,
            http.request.method = "GET",
            http.response.status_code = tracing::field::Empty,
            network.protocol.name = "websocket",
            server.address = req.uri().host(),
            server.port = req.uri().port_u16(),
            url.path = %path,
            peer.service = peer,
        );
        inject(&span, req.headers_mut());
        let connected = tokio::time::timeout(timeout, tokio_tungstenite::connect_async(req))
            .instrument(span.clone())
            .await;
        match connected {
            Ok(Ok((stream, response))) => {
                span.record("http.response.status_code", response.status().as_u16());
                Ok(stream)
            }
            Ok(Err(e)) => {
                if let tungstenite::Error::Http(response) = &e {
                    span.record("http.response.status_code", response.status().as_u16());
                }
                span.record("otel.status_code", "error");
                span.record("error.type", "connect");
                Err(e)
            }
            Err(_) => {
                span.record("otel.status_code", "error");
                span.record("error.type", "timeout");
                Err(tungstenite::Error::Io(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "WebSocket handshake timed out",
                )))
            }
        }
    }

    /// Marks the current span failed: OTel status ERROR with `error_type` as its `error.type`.
    /// `#[instrument]` alone leaves a span OK when its function returns an error; use
    /// `#[instrument(err)]` where the error text is safe to record and no error is an
    /// expected answer, this where it isn't (TRACING.md, "Errors").
    pub fn failed(error_type: &'static str) {
        let span = tracing::Span::current();
        span.set_attribute("error.type", error_type);
        span.set_status(Status::error(error_type));
    }

    /// The current span's context, to link a unit of work ([`unit_span!`], [`spawn`]) to
    /// what caused it. `None` outside a sampled span.
    pub fn here() -> Option<SpanContext> {
        span_context(&tracing::Span::current())
    }

    /// `span`'s OTel context, to link other spans to it. `None` without the OTel layer.
    pub fn span_context(span: &tracing::Span) -> Option<SpanContext> {
        let sc = span.context().span().span_context().clone();
        sc.is_valid().then_some(sc)
    }

    /// Records a span's own IDs as fields, so every stdout log line written in it carries
    /// them (the request span gets its trace ID in [`capture_request_context`]). The span
    /// must declare `trace_id` and `span_id`; [`detached_span!`] and [`unit_span!`] do.
    pub(crate) fn record_ids(span: &tracing::Span) {
        if let Some(sc) = span_context(span) {
            span.record("trace_id", sc.trace_id().to_string());
            span.record("span_id", sc.span_id().to_string());
        }
    }

    /// Links a new root span to `link` and records its IDs. Use [`unit_span!`].
    pub fn start_unit(span: &tracing::Span, link: Option<SpanContext>) {
        if let Some(link) = link {
            span.add_link(link);
        }
        record_ids(span);
    }

    /// Makes `span` (new, `parent: None`) a child of `cx`, a context that arrived in a
    /// message, and records its IDs. With an empty `cx` the span stays a root.
    pub fn continue_from(span: &tracing::Span, cx: opentelemetry::Context) {
        if cx.span().span_context().is_valid() {
            // Fails only without the OTel layer.
            let _ = span.set_parent(cx);
        }
        record_ids(span);
    }

    /// The context a W3C `traceparent` value names; empty for an invalid one.
    pub fn context_from_traceparent(traceparent: &str) -> opentelemetry::Context {
        let carrier = std::collections::HashMap::from([("traceparent".to_string(), traceparent.to_string())]);
        opentelemetry::global::get_text_map_propagator(|p| p.extract_with_context(&opentelemetry::Context::new(), &carrier))
    }

    /// `span`'s context as a W3C `traceparent` value, to send in a message. `None` without
    /// the OTel layer.
    pub fn traceparent_of(span: &tracing::Span) -> Option<String> {
        let mut carrier = std::collections::HashMap::new();
        let cx = span.context();
        opentelemetry::global::get_text_map_propagator(|p| p.inject_context(&cx, &mut carrier));
        carrier.remove("traceparent")
    }

    /// The tasks a shutdown waits for ([`drain`]): everything spawned here except loops.
    static TASKS: LazyLock<TaskTracker> = LazyLock::new(TaskTracker::new);

    /// Spawns one unit of background work as a trace of its own, named `name` and linked to
    /// `link` (what caused it; [`here`]). Clippy bans `tokio::spawn`: a bare task has no
    /// span, so its work is either invisible or scattered over one-span traces.
    #[allow(clippy::disallowed_methods, reason = "trace::spawn is the chokepoint for units of background work")]
    pub fn spawn<F>(name: &'static str, link: Option<SpanContext>, fut: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        TASKS.spawn(fut.instrument(crate::unit_span!(link, "task", otel.name = name)))
    }

    /// Spawns work a request started but doesn't wait for, as a span named `name` in the
    /// request's trace, so nothing leaves the click's trace view. It may outlive its parent
    /// (TRACING.md: `trace.relation=follows`).
    #[allow(clippy::disallowed_methods, reason = "trace::spawn_in_trace is the chokepoint for a request's detached work")]
    pub fn spawn_in_trace<F>(name: &'static str, fut: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        TASKS.spawn(fut.instrument(crate::detached_span!("task", otel.name = name, trace.relation = "follows")))
    }

    /// Spawns a loop that runs for the life of the process, with no span of its own: each
    /// pass that does I/O starts its own trace ([`unit_span!`], [`spawn`]) or stays on
    /// untraced clients. `reason` says why it runs. Not waited for at shutdown.
    #[allow(clippy::disallowed_methods, reason = "trace::spawn_loop is the chokepoint for background loops")]
    pub fn spawn_loop<F>(reason: &'static str, fut: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        tracing::debug!(reason, "background loop started");
        tokio::spawn(fut)
    }

    /// Runs CPU-heavy `f` on the blocking pool in a span named `name`, a child of the
    /// current span. The span is made inside the closure, so it covers the work only, not
    /// the wait for a free thread.
    #[allow(clippy::disallowed_methods, reason = "trace::spawn_blocking is the chokepoint for blocking work")]
    pub fn spawn_blocking<T: Send + 'static>(name: &'static str, f: impl FnOnce() -> T + Send + 'static) -> JoinHandle<T> {
        let parent = tracing::Span::current();
        TASKS.spawn_blocking(move || tracing::info_span!(parent: &parent, "blocking", otel.name = name).in_scope(f))
    }

    /// Runs a long-lived session (a proxied WebSocket) that makes its own unit traces, as a
    /// task [`drain`] waits for.
    pub fn session<F: Future>(fut: F) -> impl Future<Output = F::Output> {
        TASKS.track_future(fut)
    }

    /// At shutdown: waits up to `timeout` for the tasks spawned here to finish. False if
    /// some were still running.
    pub async fn drain(timeout: Duration) -> bool {
        TASKS.close();
        tokio::time::timeout(timeout, TASKS.wait()).await.is_ok()
    }

    /// A message with the trace context it was sent in. Everything that crosses an
    /// in-process queue is one, so the receiver's work can continue (or link to) the
    /// sender's trace: build channels with [`channel`] and [`broadcast`].
    #[derive(Clone, Debug)]
    pub struct Carried<T> {
        cx: opentelemetry::Context,
        msg: T,
    }

    impl<T> Carried<T> {
        /// `msg`, with the current span's context.
        pub fn new(msg: T) -> Self {
            Self { cx: tracing::Span::current().context(), msg }
        }

        /// `msg`, with a context that arrived another way (in the message itself).
        pub fn with_context(cx: opentelemetry::Context, msg: T) -> Self {
            Self { cx, msg }
        }

        /// The sender's span, to link to. `None` if it had none.
        pub fn span_context(&self) -> Option<SpanContext> {
            let sc = self.cx.span().span_context().clone();
            sc.is_valid().then_some(sc)
        }

        /// The receiving side: makes `span` (new, `parent: None`) a child of the sender's
        /// span and returns the message. Without a sender's span, `span` stays a root.
        pub fn enter(self, span: &tracing::Span) -> T {
            continue_from(span, self.cx);
            self.msg
        }

        /// The message, for a receiver that links instead ([`Self::span_context`]).
        pub fn into_inner(self) -> T {
            self.msg
        }
    }

    impl<T> std::ops::Deref for Carried<T> {
        type Target = T;
        fn deref(&self) -> &T {
            &self.msg
        }
    }

    /// A bounded queue of [`Carried`] messages (clippy bans the plain channel constructors:
    /// a bare message loses the trace it belongs to).
    #[allow(clippy::disallowed_methods, reason = "trace::channel is the Carried channel constructor")]
    pub fn channel<T>(capacity: usize) -> (tokio::sync::mpsc::Sender<Carried<T>>, tokio::sync::mpsc::Receiver<Carried<T>>) {
        tokio::sync::mpsc::channel(capacity)
    }

    /// A broadcast of [`Carried`] messages; see [`channel`].
    #[allow(clippy::disallowed_methods, reason = "trace::broadcast is the Carried broadcast constructor")]
    pub fn broadcast<T: Clone>(capacity: usize) -> tokio::sync::broadcast::Sender<Carried<T>> {
        tokio::sync::broadcast::channel(capacity).0
    }

    /// Whether a Redis client's commands get spans. Every client is built here (clippy bans
    /// fred's own constructors), so each one states its choice.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum RedisTracing {
        /// fred's `partial-tracing`: a `fred.command` span per command, a child of the
        /// current span, with its round trip (`fred.rtt`) under it. For clients used by
        /// requests and by units of work that have a span.
        Commands,
        /// No spans. For clients only loops use (the subscriber's PING, the schema lock's
        /// renewal): with no span around them, every command would root a trace.
        Off,
    }

    fn redis_builder(mut config: Config, connection: ConnectionConfig, tracing: RedisTracing) -> Builder {
        config.tracing = TracingConfig::new(tracing == RedisTracing::Commands);
        let mut builder = Builder::from_config(config);
        builder
            .set_policy(ReconnectPolicy::new_exponential(0, 100, 30_000, 2))
            .with_connection_config(|c| *c = connection);
        builder
    }

    /// A pool of `size` Redis connections, reconnecting forever with backoff.
    #[allow(clippy::disallowed_methods, reason = "trace::redis_pool builds Redis clients with an explicit tracing choice")]
    pub fn redis_pool(
        config: Config,
        connection: ConnectionConfig,
        size: usize,
        tracing: RedisTracing,
    ) -> Result<fred::clients::Pool, fred::error::Error> {
        redis_builder(config, connection, tracing).build_pool(size)
    }

    /// One Redis connection, reconnecting forever with backoff.
    #[allow(clippy::disallowed_methods, reason = "trace::redis_client builds Redis clients with an explicit tracing choice")]
    pub fn redis_client(
        config: Config,
        connection: ConnectionConfig,
        tracing: RedisTracing,
    ) -> Result<fred::clients::Client, fred::error::Error> {
        redis_builder(config, connection, tracing).build()
    }

    /// A Redis pub/sub connection that can re-subscribe after a reconnect. fred makes no
    /// span for a received message whatever `tracing` says: the receiver does.
    #[allow(clippy::disallowed_methods, reason = "trace::redis_subscriber builds Redis clients with an explicit tracing choice")]
    pub fn redis_subscriber(
        config: Config,
        connection: ConnectionConfig,
        tracing: RedisTracing,
    ) -> Result<fred::clients::SubscriberClient, fred::error::Error> {
        redis_builder(config, connection, tracing).build_subscriber_client()
    }

    /// A session store whose calls get CLIENT spans (children of the current span), so
    /// the session database shows in traces and Tempo's service graph. `system` names it:
    /// `TracedStore::new(RedisStore::new(pool), "redis")`.
    #[derive(Clone, Debug)]
    pub struct TracedStore<S> {
        inner: S,
        system: &'static str,
    }

    impl<S: SessionStore> TracedStore<S> {
        pub fn new(inner: S, system: &'static str) -> Self {
            Self { inner, system }
        }

        async fn traced<T>(
            &self,
            op: &'static str,
            fut: impl std::future::Future<Output = session_store::Result<T>>,
        ) -> session_store::Result<T> {
            let span = tracing::info_span!(
                "session_store",
                otel.name = format!("{} {op}", self.system),
                otel.kind = "client",
                db.system = self.system,
                db.system.name = self.system,
                db.operation.name = op,
                peer.service = self.system,
            );
            let res = fut.instrument(span.clone()).await;
            if let Err(e) = &res {
                // By kind only: a store's error text can quote the record (session data).
                let error_type = match e {
                    session_store::Error::Encode(_) => "encode",
                    session_store::Error::Decode(_) => "decode",
                    session_store::Error::Backend(_) => "backend",
                };
                span.set_attribute("error.type", error_type);
                span.set_status(Status::error(error_type));
            }
            res
        }
    }

    #[async_trait::async_trait]
    impl<S: SessionStore> SessionStore for TracedStore<S> {
        // Forwarded, not the trait's default: stores override it (RedisStore: SET NX).
        async fn create(&self, record: &mut Record) -> session_store::Result<()> {
            self.traced("session.create", self.inner.create(record)).await
        }

        async fn save(&self, record: &Record) -> session_store::Result<()> {
            self.traced("session.save", self.inner.save(record)).await
        }

        async fn load(&self, id: &Id) -> session_store::Result<Option<Record>> {
            self.traced("session.load", self.inner.load(id)).await
        }

        async fn delete(&self, id: &Id) -> session_store::Result<()> {
            self.traced("session.delete", self.inner.delete(id)).await
        }
    }

    /// Writes `span`'s trace context into `headers`, for outbound calls that don't go
    /// through [`client`] (e.g. the ai_pipeline WebSocket handshake).
    pub fn inject(span: &tracing::Span, headers: &mut http::HeaderMap) {
        struct Headers<'a>(&'a mut http::HeaderMap);
        impl Injector for Headers<'_> {
            fn set(&mut self, key: &str, value: String) {
                if let (Ok(k), Ok(v)) = (
                    http::HeaderName::from_bytes(key.as_bytes()),
                    http::HeaderValue::from_str(&value),
                ) {
                    self.0.insert(k, v);
                }
            }
        }
        let cx = span.context();
        opentelemetry::global::get_text_map_propagator(|p| p.inject_context(&cx, &mut Headers(headers)));
    }

    /// Links `span` to the span a W3C `traceparent` value names, for a context that arrives
    /// outside the headers: the browser's camera session puts it in the WebSocket URL, which
    /// can't carry headers. A link, not a parent: the request already has its parent (the
    /// Gateway's span). Invalid values are ignored.
    pub fn link_traceparent(span: &tracing::Span, traceparent: &str) {
        let sc = context_from_traceparent(traceparent).span().span_context().clone();
        if sc.is_valid() {
            span.add_link(sc);
        }
    }

    /// For tests (here so that web's can use it too): call before `set_default`. While a
    /// test's subscriber is the only one alive, tracing asks whichever thread reaches a span first whether anyone wants it, and keeps
    /// the answer: another test's thread says no, and the span is never made (the flaky "no
    /// span" failure). With a second subscriber alive it asks each thread's own.
    #[doc(hidden)]
    pub fn several_subscribers() {
        static SECOND: std::sync::LazyLock<tracing::Dispatch> =
            std::sync::LazyLock::new(|| tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default()));
        std::sync::LazyLock::force(&SECOND);
    }

    #[cfg(test)]
    #[allow(clippy::disallowed_methods, clippy::disallowed_types, reason = "tests: stand-in servers on bare tasks and raw queues, with no trace to keep")]
    mod tests {
        use std::sync::{Arc, Mutex};
        use std::time::Duration;

        use opentelemetry::trace::{TraceContextExt as _, TracerProvider as _};
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        use tracing::Instrument as _;
        use tracing_opentelemetry::OpenTelemetrySpanExt as _;
        use tracing_subscriber::layer::SubscriberExt as _;

        const TEST_PEER: super::Peer = super::Peer { service: "auth", system: None };
        const TIMEOUT: Duration = Duration::from_secs(5);

        /// Records the names of closed spans.
        struct Closed(Arc<Mutex<Vec<String>>>);

        impl<S> tracing_subscriber::Layer<S> for Closed
        where
            S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
        {
            fn on_close(&self, id: tracing::span::Id, ctx: tracing_subscriber::layer::Context<'_, S>) {
                if let Some(span) = ctx.span(&id) {
                    self.0.lock().unwrap().push(span.name().to_string());
                }
            }
        }

        /// The same pipeline as production: SDK tracer, tracing-opentelemetry layer, W3C
        /// propagator. Returns the closed-span log; keep the guard for the test's length.
        fn pipeline() -> (Arc<Mutex<Vec<String>>>, tracing::subscriber::DefaultGuard) {
            opentelemetry::global::set_text_map_propagator(
                opentelemetry_sdk::propagation::TraceContextPropagator::new(),
            );
            let tracer = opentelemetry_sdk::trace::SdkTracerProvider::builder().build().tracer("test");
            let closed = Arc::new(Mutex::new(Vec::new()));
            let subscriber = tracing_subscriber::registry()
                .with(tracing_opentelemetry::layer().with_tracer(tracer))
                .with(Closed(closed.clone()));
            super::several_subscribers();
            (closed, tracing::subscriber::set_default(subscriber))
        }

        /// A keep-alive HTTP server that never closes a connection and reports each
        /// request's `traceparent`.
        async fn keep_alive_server() -> (String, tokio::sync::mpsc::UnboundedReceiver<String>) {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            tokio::spawn(async move {
                loop {
                    let (mut sock, _) = listener.accept().await.unwrap();
                    let tx = tx.clone();
                    tokio::spawn(async move {
                        let mut buf = [0u8; 4096];
                        while let Ok(n) = sock.read(&mut buf).await {
                            if n == 0 {
                                break;
                            }
                            let head = String::from_utf8_lossy(&buf[..n]).to_lowercase();
                            let tp = head
                                .lines()
                                .find_map(|l| l.strip_prefix("traceparent: "))
                                .unwrap_or_default()
                                .trim()
                                .to_string();
                            let _ = tx.send(tp);
                            let _ = sock.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok").await;
                        }
                    });
                }
            });
            (format!("http://{addr}/internal/admin/users/42/roles/7"), rx)
        }

        #[tokio::test(flavor = "current_thread")]
        async fn client_propagates_and_releases_the_callers_span() {
            let (closed, _guard) = pipeline();
            let (url, mut traceparents) = keep_alive_server().await;
            let client = super::client(TEST_PEER, TIMEOUT, |b| b).unwrap();

            let caller = tracing::info_span!("caller");
            let caller_cx = caller.context();
            async { client.get(&url).send().await.unwrap().text().await.unwrap() }
                .instrument(caller)
                .await;

            // Same trace as the caller, with the CLIENT span (not the caller) as parent.
            let tp = traceparents.recv().await.unwrap();
            let parts: Vec<&str> = tp.split('-').collect();
            assert_eq!(parts.len(), 4, "no traceparent sent: {tp:?}");
            let caller_sc = caller_cx.span().span_context().clone();
            assert_eq!(parts[1], caller_sc.trace_id().to_string());
            assert_ne!(parts[2], caller_sc.span_id().to_string());

            // The pooled connection stays open but holds no span (hyper-util >= 0.1.21
            // without `rt-tracing-exec-force`); a held span would never be exported.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let closed = closed.lock().unwrap();
            assert!(closed.iter().any(|n| n == "caller"), "caller span held open: {closed:?}");
            assert!(closed.iter().any(|n| n == "HTTP request"), "client span held open: {closed:?}");
        }

        /// The callee side of the hop: `OtelAxumLayer` (as in web's and auth's routers)
        /// continues the trace that [`client`](super::client) sent.
        #[tokio::test(flavor = "current_thread")]
        async fn server_continues_the_clients_trace() {
            use axum_tracing_opentelemetry::middleware::OtelAxumLayer;

            let (_, _guard) = pipeline();
            let app = axum::Router::new()
                .route(
                    "/",
                    axum::routing::get(|| async {
                        tracing::Span::current().context().span().span_context().trace_id().to_string()
                    }),
                )
                .layer(OtelAxumLayer::default());
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}/", listener.local_addr().unwrap());
            tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let client = super::client(TEST_PEER, TIMEOUT, |b| b).unwrap();

            let caller = tracing::info_span!("caller");
            let caller_trace = caller.context().span().span_context().trace_id().to_string();
            let callee_trace = async { client.get(&url).send().await.unwrap().text().await.unwrap() }
                .instrument(caller)
                .await;
            assert_eq!(callee_trace, caller_trace);
        }

        #[tokio::test(flavor = "current_thread")]
        async fn ssr_future_joins_the_request_trace() {
            let (closed, _guard) = pipeline();
            let request = tracing::info_span!("request");
            let request_cx = request.context();

            let (mut parts, _) = http::Request::new(()).into_parts();
            parts.extensions.insert(super::RequestTraceContext(request_cx.clone()));
            let ctx = dioxus::fullstack::FullstackContext::new(parts);

            // As in SSR: no span is current while the future is created and polled.
            let seen = ctx
                .scope(async {
                    super::super::in_request_trace("probe", async {
                        tracing::Span::current().context().span().span_context().trace_id()
                    })
                    .await
                })
                .await;
            assert_eq!(seen, request_cx.span().span_context().trace_id());

            // The SSR span links through OTel only: the request span closes as usual.
            drop(request);
            assert!(closed.lock().unwrap().iter().any(|n| n == "request"), "request span held open");
        }

        type Fields = Vec<(String, String)>;
        type Parent = Option<String>;

        /// Records each new span's name, parent's name and fields.
        #[derive(Clone, Default)]
        struct Opened(Arc<Mutex<Vec<(String, Parent, Fields)>>>);

        impl<S> tracing_subscriber::Layer<S> for Opened
        where
            S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
        {
            fn on_new_span(
                &self,
                attrs: &tracing::span::Attributes<'_>,
                id: &tracing::span::Id,
                ctx: tracing_subscriber::layer::Context<'_, S>,
            ) {
                struct Visitor(Fields);
                impl tracing::field::Visit for Visitor {
                    fn record_str(&mut self, f: &tracing::field::Field, v: &str) {
                        self.0.push((f.name().into(), v.into()));
                    }
                    fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
                        self.0.push((f.name().into(), format!("{v:?}")));
                    }
                }
                let mut fields = Visitor(Vec::new());
                attrs.record(&mut fields);
                let span = ctx.span(id).unwrap();
                let parent = span.parent().map(|p| p.name().to_string());
                self.0.lock().unwrap().push((span.name().to_string(), parent, fields.0));
            }
        }

        impl Opened {
            fn fields(&self, name: &str) -> Vec<(Parent, Fields)> {
                let spans = self.0.lock().unwrap();
                spans.iter().filter(|s| s.0 == name).map(|s| (s.1.clone(), s.2.clone())).collect()
            }
        }

        fn field<'a>(fields: &'a [(String, String)], name: &str) -> Option<&'a str> {
            fields.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
        }

        #[tokio::test(flavor = "current_thread")]
        async fn client_names_the_peer() {
            let opened = Opened::default();
            super::several_subscribers();
            let _guard = tracing::subscriber::set_default(tracing_subscriber::registry().with(opened.clone()));
            let (url, _) = keep_alive_server().await;
            let database = super::Peer { service: "surrealdb", system: Some("surrealdb") };
            let call = super::DbCall { name: "surrealdb.query", operation: "SELECT".into() };
            super::client(database, TIMEOUT, |b| b).unwrap().post(&url).with_extension(call).send().await.unwrap();
            super::client(TEST_PEER, TIMEOUT, |b| b).unwrap().post(&url).send().await.unwrap();

            let spans = opened.fields("HTTP request");
            let [(_, db), (_, service)] = &spans[..] else { panic!("{} client spans", spans.len()) };
            assert_eq!(field(db, "peer.service"), Some("surrealdb"));
            assert_eq!(field(db, "db.system"), Some("surrealdb"));
            assert_eq!(field(db, "db.system.name"), Some("surrealdb"));
            assert_eq!(field(db, "otel.name"), Some("surrealdb.query"));
            assert_eq!(field(db, "db.operation.name"), Some("SELECT"));
            // Every client names its peer; only a database has a `db.system`.
            assert_eq!(field(service, "peer.service"), Some("auth"));
            assert_eq!(field(service, "db.system"), None);
            assert_eq!(field(service, "otel.name"), Some("POST /internal/admin/users/{id}/roles/{id}"));
        }

        #[tokio::test(flavor = "current_thread")]
        async fn session_store_calls_are_client_spans_of_the_caller() {
            use tower_sessions::SessionStore as _;

            let opened = Opened::default();
            super::several_subscribers();
            let _guard = tracing::subscriber::set_default(tracing_subscriber::registry().with(opened.clone()));
            let store = super::TracedStore::new(tower_sessions::MemoryStore::default(), "redis");

            let id = tower_sessions::session::Id::default();
            let loaded = store.load(&id).instrument(tracing::info_span!("request")).await.unwrap();
            assert!(loaded.is_none());

            let spans = opened.fields("session_store");
            let [(parent, fields)] = &spans[..] else { panic!("{} store spans", spans.len()) };
            assert_eq!(parent.as_deref(), Some("request"));
            assert_eq!(field(fields, "otel.name"), Some("redis session.load"));
            assert_eq!(field(fields, "otel.kind"), Some("client"));
            assert_eq!(field(fields, "peer.service"), Some("redis"));
            assert_eq!(field(fields, "db.operation.name"), Some("session.load"));
        }

        #[tokio::test(flavor = "current_thread")]
        async fn traceparent_of_sampled_requests_only() {
            use opentelemetry::trace::{SpanContext, SpanId, TraceFlags, TraceId, TraceState};

            async fn in_request(flags: TraceFlags) -> Option<String> {
                let sc = SpanContext::new(
                    TraceId::from_hex("4bf92f3577b34da6a3ce929d0e0e4736").unwrap(),
                    SpanId::from_hex("00f067aa0ba902b7").unwrap(),
                    flags,
                    true,
                    TraceState::default(),
                );
                let cx = opentelemetry::Context::new().with_remote_span_context(sc);
                let (mut parts, _) = http::Request::new(()).into_parts();
                parts.extensions.insert(super::RequestTraceContext(cx));
                let ctx = dioxus::fullstack::FullstackContext::new(parts);
                ctx.scope(async { super::super::traceparent() }).await
            }

            assert_eq!(
                in_request(TraceFlags::SAMPLED).await.as_deref(),
                Some("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"),
            );
            assert_eq!(in_request(TraceFlags::default()).await, None);
            assert_eq!(super::super::traceparent(), None, "outside a request");
        }

        #[test]
        fn traceparent_becomes_a_link_not_a_parent() {
            use opentelemetry::trace::{SpanId, TraceId};
            use opentelemetry_sdk::trace::InMemorySpanExporter;

            opentelemetry::global::set_text_map_propagator(
                opentelemetry_sdk::propagation::TraceContextPropagator::new(),
            );
            let exporter = InMemorySpanExporter::default();
            let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
                .with_simple_exporter(exporter.clone())
                .build();
            let subscriber = tracing_subscriber::registry()
                .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));
            tracing::subscriber::with_default(subscriber, || {
                let linked = tracing::info_span!("linked");
                super::link_traceparent(&linked, "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01");
                let ignored = tracing::info_span!("ignored");
                super::link_traceparent(&ignored, "00-garbage");
            });
            let spans = exporter.get_finished_spans().unwrap();
            let span = |name: &str| spans.iter().find(|s| s.name == name).unwrap();
            let linked = span("linked");
            let links: Vec<_> = linked.links.iter().map(|l| (l.span_context.trace_id(), l.span_context.span_id())).collect();
            assert_eq!(
                links,
                [(
                    TraceId::from_hex("4bf92f3577b34da6a3ce929d0e0e4736").unwrap(),
                    SpanId::from_hex("00f067aa0ba902b7").unwrap()
                )]
            );
            assert_eq!(linked.parent_span_id, SpanId::INVALID, "still a root");
            assert_ne!(linked.span_context.trace_id(), links[0].0);
            assert!(span("ignored").links.is_empty());
        }

        use opentelemetry::trace::{SpanId, SpanKind, Status};
        use opentelemetry_sdk::trace::{InMemorySpanExporter, SpanData};

        /// The production pipeline with an exporter the test can read. For tests whose
        /// spans are all made on the test's own thread; keep the guard for its length.
        fn exporting() -> (InMemorySpanExporter, tracing::subscriber::DefaultGuard) {
            opentelemetry::global::set_text_map_propagator(
                opentelemetry_sdk::propagation::TraceContextPropagator::new(),
            );
            let exporter = InMemorySpanExporter::default();
            let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
                .with_simple_exporter(exporter.clone())
                .build();
            let subscriber = tracing_subscriber::registry()
                .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));
            super::several_subscribers();
            (exporter, tracing::subscriber::set_default(subscriber))
        }

        fn span(exporter: &InMemorySpanExporter, name: &str) -> SpanData {
            let spans = exporter.get_finished_spans().unwrap();
            let names: Vec<_> = spans.iter().map(|s| s.name.to_string()).collect();
            spans.into_iter().find(|s| s.name == name).unwrap_or_else(|| panic!("no span {name:?} in {names:?}"))
        }

        fn attr(span: &SpanData, key: &str) -> Option<String> {
            span.attributes.iter().find(|kv| kv.key.as_str() == key).map(|kv| kv.value.to_string())
        }

        /// Answers every request with `status`.
        async fn replying(status: u16) -> String {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move {
                loop {
                    let (mut sock, _) = listener.accept().await.unwrap();
                    tokio::spawn(async move {
                        let mut buf = [0u8; 4096];
                        while let Ok(n) = sock.read(&mut buf).await {
                            if n == 0 {
                                break;
                            }
                            let reply = format!("HTTP/1.1 {status} X\r\ncontent-length: 0\r\n\r\n");
                            let _ = sock.write_all(reply.as_bytes()).await;
                        }
                    });
                }
            });
            format!("http://{addr}")
        }

        #[tokio::test(flavor = "current_thread")]
        async fn a_client_error_is_a_span_error_unless_expected() {
            let (exporter, _guard) = exporting();
            let base = replying(401).await;
            let client = super::client(TEST_PEER, TIMEOUT, |b| b).unwrap();

            client.post(format!("{base}/internal/profile")).send().await.unwrap();
            let unexpected = span(&exporter, "POST /internal/profile");
            assert_eq!(unexpected.span_kind, SpanKind::Client);
            assert!(matches!(unexpected.status, Status::Error { .. }), "{:?}", unexpected.status);
            assert_eq!(attr(&unexpected, "http.response.status_code").as_deref(), Some("401"));

            // A wrong password: the answer the call exists to get.
            let exchange = client.post(format!("{base}/internal/token/exchange"));
            exchange.with_extension(super::WRONG_TOKEN_OK).send().await.unwrap();
            let expected = span(&exporter, "POST /internal/token/exchange");
            assert_eq!(expected.status, Status::Unset);
            assert_eq!(attr(&expected, "http.response.status_code").as_deref(), Some("401"));
            assert_eq!(attr(&expected, "peer.service").as_deref(), Some("auth"));
        }

        #[tokio::test(flavor = "current_thread")]
        async fn a_client_gives_up_after_its_timeout() {
            let (exporter, _guard) = exporting();
            // Accepts and never answers.
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}/slow", listener.local_addr().unwrap());
            // `configure` can't remove the timeout: it is set after it runs.
            let long = |b: reqwest::ClientBuilder| b.timeout(Duration::from_secs(60));
            let client = super::client(TEST_PEER, Duration::from_millis(100), long).unwrap();

            let started = std::time::Instant::now();
            let err = client.get(&url).send().await.unwrap_err();
            assert!(err.is_timeout(), "{err}");
            assert!(started.elapsed() < Duration::from_secs(5));
            assert!(matches!(span(&exporter, "GET /slow").status, Status::Error { .. }));
            drop(listener);
        }

        /// A WebSocket server that reports the handshake's `traceparent`.
        // The callback's signature is tungstenite's.
        #[allow(clippy::result_large_err)]
        async fn ws_server() -> (String, tokio::sync::mpsc::UnboundedReceiver<String>) {
            use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("ws://{}/", listener.local_addr().unwrap());
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            tokio::spawn(async move {
                loop {
                    let (sock, _) = listener.accept().await.unwrap();
                    let tx = tx.clone();
                    let seen = move |req: &Request, res: Response| -> Result<Response, ErrorResponse> {
                        let tp = req.headers().get("traceparent").and_then(|v| v.to_str().ok()).unwrap_or_default();
                        let _ = tx.send(tp.to_string());
                        Ok(res)
                    };
                    tokio::spawn(async move {
                        let _open = tokio_tungstenite::accept_hdr_async(sock, seen).await;
                        std::future::pending::<()>().await;
                    });
                }
            });
            (url, rx)
        }

        #[tokio::test(flavor = "current_thread")]
        async fn connect_ws_is_a_client_span_that_sends_its_context() {
            let (exporter, _guard) = exporting();
            let (url, mut traceparents) = ws_server().await;

            let caller = tracing::info_span!("caller");
            let caller_sc = caller.context().span().span_context().clone();
            let _stream = super::connect_ws(&url, "ai-pipeline", TIMEOUT).instrument(caller).await.unwrap();

            let connect = span(&exporter, "GET /");
            assert_eq!(connect.span_kind, SpanKind::Client);
            assert_eq!(connect.parent_span_id, caller_sc.span_id());
            assert_eq!(attr(&connect, "peer.service").as_deref(), Some("ai-pipeline"));
            assert_eq!(attr(&connect, "network.protocol.name").as_deref(), Some("websocket"));
            assert_eq!(attr(&connect, "http.response.status_code").as_deref(), Some("101"));
            assert_eq!(connect.status, Status::Unset);
            // The server's span parents to the CLIENT span, in the caller's trace.
            let tp = traceparents.recv().await.unwrap();
            assert_eq!(tp, format!("00-{}-{}-01", caller_sc.trace_id(), connect.span_context.span_id()));
        }

        #[tokio::test(flavor = "current_thread")]
        async fn connect_ws_gives_up_after_its_timeout() {
            let (exporter, _guard) = exporting();
            // Accepts the TCP connection and never answers the handshake.
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("ws://{}/", listener.local_addr().unwrap());

            let err = super::connect_ws(&url, "ai-pipeline", Duration::from_millis(100)).await.unwrap_err();
            assert!(err.to_string().contains("timed out"), "{err}");
            let connect = span(&exporter, "GET /");
            assert!(matches!(connect.status, Status::Error { .. }));
            assert_eq!(attr(&connect, "error.type").as_deref(), Some("timeout"));
            drop(listener);
        }

        #[tokio::test(flavor = "current_thread")]
        async fn spawned_units_are_linked_roots_and_request_work_stays_in_its_trace() {
            let (exporter, _guard) = exporting();
            let request = tracing::info_span!("request");
            let request_sc = request.context().span().span_context().clone();

            let (unit, detached, looped) = request.in_scope(|| {
                (
                    super::spawn("unit", super::here(), async {}),
                    super::spawn_in_trace("detached", async {}),
                    super::spawn_loop("test", async { tracing::Span::current().is_none() }),
                )
            });
            // Neither task holds the request's span open.
            drop(request);
            assert_eq!(span(&exporter, "request").span_context, request_sc);
            unit.await.unwrap();
            detached.await.unwrap();
            assert!(looped.await.unwrap(), "a loop has no span of its own");

            let unit = span(&exporter, "unit");
            assert_eq!(unit.parent_span_id, SpanId::INVALID, "a trace of its own");
            assert_ne!(unit.span_context.trace_id(), request_sc.trace_id());
            let links: Vec<_> = unit.links.iter().map(|l| l.span_context.clone()).collect();
            assert_eq!(links.as_slice(), std::slice::from_ref(&request_sc));
            assert_eq!(attr(&unit, "trace_id"), Some(unit.span_context.trace_id().to_string()), "for log lines");

            let detached = span(&exporter, "detached");
            assert_eq!(detached.parent_span_id, request_sc.span_id());
            assert_eq!(attr(&detached, "trace.relation").as_deref(), Some("follows"));
            assert_eq!(attr(&detached, "span_id"), Some(detached.span_context.span_id().to_string()));
        }

        #[tokio::test(flavor = "current_thread")]
        async fn drain_waits_for_spawned_work_but_not_for_loops() {
            let (tx, rx) = tokio::sync::oneshot::channel::<()>();
            let work = super::spawn("slow unit", None, async { rx.await.ok() });
            let _forever = super::spawn_loop("test", std::future::pending::<()>());
            assert!(!super::drain(Duration::from_millis(50)).await, "the unit is still running");
            tx.send(()).unwrap();
            work.await.unwrap();
            // Other tests' tasks are in the same tracker: generous, but not forever.
            assert!(super::drain(Duration::from_secs(10)).await);
        }

        /// One subscriber for every thread, for tests whose spans are made on other
        /// threads (the blocking pool). Spans of unrelated tests end up in it too.
        fn global_exporter() -> &'static InMemorySpanExporter {
            static EXPORTER: std::sync::LazyLock<InMemorySpanExporter> = std::sync::LazyLock::new(|| {
                let exporter = InMemorySpanExporter::default();
                let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
                    .with_simple_exporter(exporter.clone())
                    .build();
                let subscriber = tracing_subscriber::registry()
                    .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));
                tracing::subscriber::set_global_default(subscriber).unwrap();
                exporter
            });
            &EXPORTER
        }

        #[tokio::test(flavor = "current_thread")]
        async fn blocking_work_is_a_child_span() {
            let exporter = global_exporter();
            let request = tracing::info_span!("blocking request");
            let request_sc = request.context().span().span_context().clone();
            let in_span = request
                .in_scope(|| super::spawn_blocking("blocking.work", || !tracing::Span::current().is_none()))
                .await
                .unwrap();
            assert!(in_span);
            let work = span(exporter, "blocking.work");
            assert_eq!(work.parent_span_id, request_sc.span_id());
        }

        #[tokio::test(flavor = "current_thread")]
        async fn queued_messages_carry_the_senders_trace() {
            let (exporter, _guard) = exporting();
            let (tx, mut rx) = super::channel::<&str>(1);
            let casts = super::broadcast::<&str>(1);
            let mut cast_rx = casts.subscribe();

            let sender = tracing::info_span!("sender");
            let sender_sc = sender.context().span().span_context().clone();
            sender.in_scope(|| {
                tx.try_send(super::Carried::new("queued")).unwrap();
                casts.send(super::Carried::new("cast")).unwrap();
            });
            drop(sender);

            // The receiver continues the sender's trace ...
            let received = rx.recv().await.unwrap();
            assert_eq!(received.span_context(), Some(sender_sc.clone()));
            let child = tracing::info_span!(parent: None, "receiver");
            assert_eq!(received.enter(&child), "queued");
            drop(child);
            assert_eq!(span(&exporter, "receiver").parent_span_id, sender_sc.span_id());

            // ... or starts its own and links to it.
            let cast = cast_rx.recv().await.unwrap();
            let unit = crate::unit_span!(cast.span_context(), "late receiver");
            assert_eq!(cast.into_inner(), "cast");
            drop(unit);
            let unit = span(&exporter, "late receiver");
            assert_eq!(unit.parent_span_id, SpanId::INVALID);
            assert_eq!(unit.links.iter().map(|l| l.span_context.clone()).collect::<Vec<_>>(), [sender_sc]);

            // A message sent outside any span leaves the receiver's span a root.
            let orphan = tracing::info_span!(parent: None, "orphan receiver");
            super::Carried::new(()).enter(&orphan);
            drop(orphan);
            assert_eq!(span(&exporter, "orphan receiver").parent_span_id, SpanId::INVALID);
        }

        #[tokio::test(flavor = "current_thread")]
        async fn failures_set_the_span_status() {
            use dioxus::prelude::ServerFnError;
            let (exporter, _guard) = exporting();

            tracing::info_span!("store call").in_scope(|| super::failed("redis"));
            let failed = span(&exporter, "store call");
            assert!(matches!(failed.status, Status::Error { .. }));
            assert_eq!(attr(&failed, "error.type").as_deref(), Some("redis"));

            // `#[instrument]` alone: the span stays OK whatever the function returns.
            #[tracing::instrument(name = "plain", skip_all)]
            async fn plain() -> Result<(), ServerFnError> {
                Err(ServerFnError::new("down"))
            }
            #[tracing::instrument(name = "with err", skip_all, err)]
            async fn with_err() -> Result<(), ServerFnError> {
                Err(ServerFnError::new("down"))
            }
            #[tracing::instrument(name = "rejectable", skip_all)]
            async fn rejectable(status: u16) -> Result<(), ServerFnError> {
                super::rejectable(async move { Err(crate::auth_error(status, "no")) }).await
            }
            let _ = plain().await;
            let _ = with_err().await;
            assert_eq!(span(&exporter, "plain").status, Status::Unset);
            assert!(matches!(span(&exporter, "with err").status, Status::Error { .. }));

            // A wrong password is an answer; auth being down is a failure.
            let _ = rejectable(401).await;
            assert_eq!(span(&exporter, "rejectable").status, Status::Unset);
            exporter.reset();
            let _ = rejectable(503).await;
            assert!(matches!(span(&exporter, "rejectable").status, Status::Error { .. }));
        }

        /// A router as web's: `OtelAxumLayer` outside [`end_span_with_body`](super::end_span_with_body).
        /// `/stream` and `/events` send one chunk, then wait for the returned sender.
        async fn streaming_server() -> (String, tokio::sync::mpsc::UnboundedSender<&'static str>) {
            use axum::http::header::CONTENT_TYPE;
            use axum_tracing_opentelemetry::middleware::OtelAxumLayer;

            let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<&'static str>();
            let rx = Arc::new(tokio::sync::Mutex::new(rx));
            let body = move |content_type: &'static str| {
                let rx = rx.clone();
                move || async move {
                    let chunks = futures_util::stream::unfold((rx, true), |(rx, first)| async move {
                        let chunk = if first { Some("first") } else { rx.lock().await.recv().await };
                        chunk.map(|c| (Ok::<_, std::convert::Infallible>(c), (rx, false)))
                    });
                    ([(CONTENT_TYPE, content_type)], axum::body::Body::from_stream(chunks))
                }
            };
            let app = axum::Router::new()
                .route("/stream", axum::routing::get(body("application/wasm")))
                .route("/events", axum::routing::get(body("text/event-stream")))
                .layer(axum::middleware::from_fn(super::end_span_with_body))
                .layer(OtelAxumLayer::default());
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            (base, tx)
        }

        fn server_spans(exporter: &InMemorySpanExporter) -> usize {
            exporter.get_finished_spans().unwrap().iter().filter(|s| s.span_kind == SpanKind::Server).count()
        }

        #[tokio::test(flavor = "current_thread")]
        async fn the_server_span_ends_with_the_response_body() {
            let (exporter, _guard) = exporting();
            let (base, more) = streaming_server().await;
            let client = super::client(TEST_PEER, TIMEOUT, |b| b).unwrap();

            let mut response = client.get(format!("{base}/stream")).send().await.unwrap();
            assert_eq!(response.chunk().await.unwrap().as_deref(), Some(&b"first"[..]));
            tokio::time::sleep(Duration::from_millis(50)).await;
            assert_eq!(server_spans(&exporter), 0, "headers and a chunk are out, the body isn't");

            more.send("rest").unwrap();
            drop(more);
            while response.chunk().await.unwrap().is_some() {}
            tokio::time::sleep(Duration::from_millis(50)).await;
            let server = span(&exporter, "GET /stream");
            assert_eq!(attr(&server, "http.response.status_code").as_deref(), Some("200"));
        }

        #[tokio::test(flavor = "current_thread")]
        async fn an_event_streams_server_span_ends_at_its_headers() {
            let (exporter, _guard) = exporting();
            let (base, _more) = streaming_server().await;
            let client = super::client(TEST_PEER, TIMEOUT, |b| b).unwrap();

            let mut response = client.get(format!("{base}/events")).send().await.unwrap();
            assert_eq!(response.chunk().await.unwrap().as_deref(), Some(&b"first"[..]));
            tokio::time::sleep(Duration::from_millis(50)).await;
            assert_eq!(server_spans(&exporter), 1, "the stream is still open");
        }

        #[tokio::test(flavor = "current_thread")]
        async fn a_response_body_that_fails_marks_the_server_span_failed() {
            use axum_tracing_opentelemetry::middleware::OtelAxumLayer;

            let (exporter, _guard) = exporting();
            let broken = || async {
                let chunks = futures_util::stream::iter([Ok("first"), Err(std::io::Error::other("disk"))]);
                axum::body::Body::from_stream(chunks)
            };
            let app = axum::Router::new()
                .route("/broken", axum::routing::get(broken))
                .layer(axum::middleware::from_fn(super::end_span_with_body))
                .layer(OtelAxumLayer::default());
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}/broken", listener.local_addr().unwrap());
            tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

            let client = super::client(TEST_PEER, TIMEOUT, |b| b).unwrap();
            // Cut off before or after the headers, depending on when the server wrote them.
            let cut_off = match client.get(&url).send().await {
                Ok(response) => response.bytes().await.is_err(),
                Err(_) => true,
            };
            assert!(cut_off);
            tokio::time::sleep(Duration::from_millis(50)).await;
            let server = span(&exporter, "GET /broken");
            assert_eq!(attr(&server, "http.response.status_code").as_deref(), Some("200"));
            assert!(matches!(server.status, Status::Error { .. }), "{:?}", server.status);
            assert_eq!(attr(&server, "error.type").as_deref(), Some("response_body"));
        }

        #[tokio::test(flavor = "current_thread")]
        async fn a_failed_session_store_call_is_an_error_by_kind_only() {
            use tower_sessions::session::{Id, Record};
            use tower_sessions::session_store::{Error, Result};
            use tower_sessions::SessionStore as _;

            #[derive(Debug)]
            struct Down;
            #[async_trait::async_trait]
            impl tower_sessions::SessionStore for Down {
                async fn save(&self, _: &Record) -> Result<()> {
                    Err(Error::Backend("no".into()))
                }
                async fn load(&self, _: &Id) -> Result<Option<Record>> {
                    Err(Error::Backend("could not read the session of miles@example.com".into()))
                }
                async fn delete(&self, _: &Id) -> Result<()> {
                    Err(Error::Backend("no".into()))
                }
            }

            let (exporter, _guard) = exporting();
            let store = super::TracedStore::new(Down, "redis");
            assert!(store.load(&Id::default()).await.is_err());

            let call = span(&exporter, "redis session.load");
            assert!(matches!(&call.status, Status::Error { description } if description == "backend"), "{:?}", call.status);
            assert_eq!(attr(&call, "error.type").as_deref(), Some("backend"));
            // The store's own words can name the session's owner: none of them are kept.
            assert!(!format!("{call:?}").contains("miles"));
        }

        #[tokio::test(flavor = "current_thread")]
        async fn a_page_render_is_a_span_with_its_server_futures_under_it() {
            use axum_tracing_opentelemetry::middleware::OtelAxumLayer;

            let (exporter, _guard) = exporting();
            // As Dioxus's page handler: the render runs outside the request's span, with
            // the request's parts as its context.
            let page = |req: axum::extract::Request| async move {
                let (parts, _) = req.into_parts();
                let ctx = dioxus::fullstack::FullstackContext::new(parts);
                let render = ctx.scope(async {
                    super::super::in_request_trace("load", async {}).await;
                    super::super::traceparent().unwrap()
                });
                render.instrument(tracing::Span::none()).await
            };
            let app = axum::Router::new()
                .fallback(axum::routing::get(page))
                .layer(axum::middleware::from_fn(super::ssr_render))
                .layer(axum::middleware::from_fn(super::capture_request_context))
                .layer(OtelAxumLayer::default());
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}/login", listener.local_addr().unwrap());
            tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

            let client = super::client(TEST_PEER, TIMEOUT, |b| b).unwrap();
            let meta = client.get(&url).send().await.unwrap().text().await.unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;

            let spans = exporter.get_finished_spans().unwrap();
            let server = spans.iter().find(|s| s.span_kind == SpanKind::Server).unwrap();
            let render = span(&exporter, "ssr.render");
            let future = span(&exporter, "load");
            assert_eq!(render.parent_span_id, server.span_context.span_id());
            assert_eq!(future.parent_span_id, render.span_context.span_id());
            // The browser continues from the request's span, not from the render.
            let sc = &server.span_context;
            assert_eq!(meta, format!("00-{}-{}-01", sc.trace_id(), sc.span_id()));
        }

        /// stdout log lines carry the IDs of spans that aren't requests too (I8).
        #[test]
        fn unit_and_detached_spans_put_their_ids_on_log_lines() {
            #[derive(Clone, Default)]
            struct Lines(Arc<Mutex<Vec<u8>>>);
            impl std::io::Write for Lines {
                fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                    self.0.lock().unwrap().extend_from_slice(buf);
                    Ok(buf.len())
                }
                fn flush(&mut self) -> std::io::Result<()> {
                    Ok(())
                }
            }

            let lines = Lines::default();
            let writer = lines.clone();
            let tracer = opentelemetry_sdk::trace::SdkTracerProvider::builder().build().tracer("test");
            let subscriber = tracing_subscriber::registry()
                .with(tracing_subscriber::fmt::layer().json().with_writer(move || writer.clone()))
                .with(tracing_opentelemetry::layer().with_tracer(tracer));
            let ids = tracing::subscriber::with_default(subscriber, || {
                let unit = crate::unit_span!(None, "background pass", attempt = 1,);
                unit.in_scope(|| tracing::info!("in a unit"));
                let detached = tracing::info_span!("request").in_scope(|| crate::detached_span!("queued job"));
                detached.in_scope(|| tracing::info!("in detached work"));
                [unit, detached].map(|s| s.context().span().span_context().clone())
            });

            let out = String::from_utf8(lines.0.lock().unwrap().clone()).unwrap();
            for (message, sc) in [("in a unit", &ids[0]), ("in detached work", &ids[1])] {
                let line = out.lines().find(|l| l.contains(message)).unwrap();
                assert!(line.contains(&format!("\"trace_id\":\"{}\"", sc.trace_id())), "{line}");
                assert!(line.contains(&format!("\"span_id\":\"{}\"", sc.span_id())), "{line}");
            }
        }

        #[test]
        fn redis_clients_state_their_tracing_choice() {
            use fred::interfaces::ClientLike as _;
            use fred::prelude::{Config, ConnectionConfig};
            use super::RedisTracing::{Commands, Off};

            let traced = |on| super::redis_client(Config::default(), ConnectionConfig::default(), on).unwrap();
            assert!(traced(Commands).client_config().tracing.enabled);
            assert!(!traced(Off).client_config().tracing.enabled);
            let pool = super::redis_pool(Config::default(), ConnectionConfig::default(), 2, Commands).unwrap();
            assert!(pool.next().client_config().tracing.enabled);
            let subscriber = super::redis_subscriber(Config::default(), ConnectionConfig::default(), Off).unwrap();
            assert!(!subscriber.client_config().tracing.enabled);
        }

        #[test]
        fn route_templates_ids() {
            assert_eq!(super::route("/internal/admin/users/42/roles/7"), "/internal/admin/users/{id}/roles/{id}");
            assert_eq!(super::route("/internal/token/introspect"), "/internal/token/introspect");
            assert_eq!(super::route("/"), "/");
        }
    }
}
