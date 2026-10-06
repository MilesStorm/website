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
#[allow(clippy::disallowed_methods)] // this is the wrapper the lint points to
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
        // the request's span open.
        let span = tracing::info_span!(parent: None, "ssr.server_future", otel.name = name);
        span.set_parent(cx);
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
        let cx = server::request_context()?;
        let sc = cx.span().span_context().clone();
        (sc.is_valid() && sc.is_sampled())
            .then(|| format!("00-{}-{}-{:02x}", sc.trace_id(), sc.span_id(), sc.trace_flags().to_u8()))
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
    ($($args:tt)*) => {{
        let span = $crate::trace::__tracing::info_span!(parent: None, $($args)*);
        $crate::trace::continue_current_trace(&span);
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
    span.set_parent(tracing::Span::current().context());
}

#[cfg(feature = "server")]
pub use server::*;

#[cfg(feature = "server")]
mod server {
    use std::borrow::Cow;

    use dioxus::fullstack::FullstackContext;
    use dioxus::server::axum;
    use opentelemetry::propagation::Injector;
    use opentelemetry::trace::TraceContextExt as _;
    use reqwest_tracing::{reqwest_otel_span, ReqwestOtelSpanBackend, TracingMiddleware};
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

    pub(crate) fn request_context() -> Option<opentelemetry::Context> {
        FullstackContext::current()?.extension::<RequestTraceContext>().map(|c| c.0)
    }

    /// An HTTP client from [`client`].
    pub type TracedClient = reqwest_middleware::ClientWithMiddleware;

    /// Wraps `inner` so every request gets a CLIENT span (child of the current span)
    /// and carries its trace context to the server. Use for all outbound HTTP.
    pub fn client(inner: reqwest::Client) -> TracedClient {
        reqwest_middleware::ClientBuilder::new(inner)
            .with(TracingMiddleware::<PathSpans>::new())
            .build()
    }

    /// Request extension naming the service a [`client`] request goes to, for Tempo's
    /// service graph: `peer.service` and `db.system` on the CLIENT span. Only for peers
    /// that make no spans of their own (a database); traced services show up anyway.
    /// `client.post(url).with_extension(Peer { .. })`.
    #[derive(Clone, Copy, Debug)]
    pub struct Peer {
        pub service: &'static str,
        pub system: &'static str,
    }

    /// Names client spans `METHOD /path`, with numeric segments (IDs) as `{id}` so
    /// span names stay low-cardinality without a list of routes to keep up to date.
    struct PathSpans;

    impl ReqwestOtelSpanBackend for PathSpans {
        fn on_request_start(req: &reqwest::Request, ext: &mut http::Extensions) -> tracing::Span {
            // Templated in the attribute too: raw paths could carry user data.
            let path = route(req.url().path());
            let name = format!("{} {path}", req.method());
            let peer = ext.get::<Peer>();
            reqwest_otel_span!(
                name = name,
                req,
                url.path = %path,
                peer.service = peer.map(|p| p.service),
                db.system = peer.map(|p| p.system),
                db.system.name = peer.map(|p| p.system),
            )
        }

        fn on_request_end(
            span: &tracing::Span,
            outcome: &reqwest_middleware::Result<reqwest::Response>,
            _: &mut http::Extensions,
        ) {
            reqwest_tracing::default_on_request_end(span, outcome)
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
                otel.status_code = tracing::field::Empty,
                error.message = tracing::field::Empty,
                db.system = self.system,
                db.system.name = self.system,
                db.operation.name = op,
                peer.service = self.system,
            );
            let res = fut.instrument(span.clone()).await;
            if let Err(e) = &res {
                span.record("otel.status_code", "error");
                span.record("error.message", tracing::field::display(e));
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

    #[cfg(test)]
    mod tests {
        use std::sync::{Arc, Mutex};

        use opentelemetry::trace::{TraceContextExt as _, TracerProvider as _};
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        use tracing::Instrument as _;
        use tracing_opentelemetry::OpenTelemetrySpanExt as _;
        use tracing_subscriber::layer::SubscriberExt as _;

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
            let tracer = opentelemetry_sdk::trace::TracerProvider::builder().build().tracer("test");
            let closed = Arc::new(Mutex::new(Vec::new()));
            let subscriber = tracing_subscriber::registry()
                .with(tracing_opentelemetry::layer().with_tracer(tracer))
                .with(Closed(closed.clone()));
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
            let client = super::client(reqwest::Client::new());

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
            let _guard = tracing::subscriber::set_default(tracing_subscriber::registry().with(opened.clone()));
            let (url, _) = keep_alive_server().await;
            let client = super::client(reqwest::Client::new());

            let peer = super::Peer { service: "surrealdb", system: "surrealdb" };
            client.post(&url).with_extension(peer).send().await.unwrap();
            client.post(&url).send().await.unwrap();

            let spans = opened.fields("HTTP request");
            let [(_, with), (_, without)] = &spans[..] else { panic!("{} client spans", spans.len()) };
            assert_eq!(field(with, "peer.service"), Some("surrealdb"));
            assert_eq!(field(with, "db.system"), Some("surrealdb"));
            assert_eq!(field(with, "db.system.name"), Some("surrealdb"));
            assert_eq!(field(without, "peer.service"), None);
        }

        #[tokio::test(flavor = "current_thread")]
        async fn session_store_calls_are_client_spans_of_the_caller() {
            use tower_sessions::SessionStore as _;

            let opened = Opened::default();
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
        fn route_templates_ids() {
            assert_eq!(super::route("/internal/admin/users/42/roles/7"), "/internal/admin/users/{id}/roles/{id}");
            assert_eq!(super::route("/internal/token/introspect"), "/internal/token/introspect");
            assert_eq!(super::route("/"), "/");
        }
    }
}
