//! The traced paths for auth's I/O and background work. Outbound HTTP, transactions, the
//! session store and spawned tasks go through here; clippy bans the raw APIs everywhere else
//! (`clippy.toml`, exceptions listed in `allowed_raw_io.toml`). The conventions are in
//! TRACING.md at the repository root.

use std::borrow::Cow;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::LazyLock;
use std::time::{Duration, SystemTime};

use futures_util::FutureExt as _;
use opentelemetry::trace::{
    Span as _, SpanContext, SpanKind, Status, TraceContextExt as _, Tracer as _,
};
use reqwest_middleware::Extension;
use reqwest_tracing::{ReqwestOtelSpanBackend, TracingMiddleware, reqwest_otel_span};
use tokio::task::JoinHandle;
use tokio_util::task::TaskTracker;
use tower_sessions::session::{Id, Record};
use tower_sessions::session_store::{self, ExpiredDeletion};
use tower_sessions::SessionStore;
use tracing::Instrument as _;
use tracing::field::Empty;
use tracing_opentelemetry::OpenTelemetrySpanExt as _;

use super::Db;

// ---- Errors ----

/// Marks `span` as failed: OTel status ERROR and `error.type`, and nothing else. For errors
/// whose text can hold user data (sqlx and Resend errors, provider replies), which must not
/// reach a span. Where the text is safe, an ERROR-level event in the span (`tracing::error!`,
/// `#[instrument(err)]`) sets the status as well.
pub fn fail(span: &tracing::Span, error_type: impl Into<Cow<'static, str>>) {
    span.set_status(Status::error(""));
    span.set_attribute("error.type", error_type.into());
}

/// `error.type` for a database error: the SQLSTATE code when Postgres answered, else what
/// went wrong on the way there.
pub fn sqlx_error_type(e: &sqlx::Error) -> Cow<'static, str> {
    match e {
        sqlx::Error::Database(db) => db.code().map_or("database".into(), |c| c.into_owned().into()),
        sqlx::Error::PoolTimedOut => "pool_timed_out".into(),
        sqlx::Error::PoolClosed => "pool_closed".into(),
        sqlx::Error::Io(_) => "io".into(),
        sqlx::Error::Tls(_) => "tls".into(),
        sqlx::Error::Protocol(_) => "protocol".into(),
        sqlx::Error::RowNotFound => "row_not_found".into(),
        _ => "_OTHER".into(),
    }
}

/// Marks the current span as failed by a database error: `.inspect_err(trace::db_failed)`.
pub fn db_failed(e: &sqlx::Error) {
    fail(&tracing::Span::current(), sqlx_error_type(e));
}

/// Records the span's trace and span IDs as fields, so stdout log lines written in it carry
/// them, as a request's do (`record_trace_id`). Call it last: reading the context starts the
/// span, after which its parent and links are fixed.
fn record_ids(span: &tracing::Span) {
    let cx = span.context();
    let sc = cx.span().span_context().clone();
    if sc.is_valid() {
        span.record("trace_id", tracing::field::display(sc.trace_id()));
        span.record("span_id", tracing::field::display(sc.span_id()));
    }
}

// ---- Outbound HTTP ----

/// An HTTP client from [`client`].
pub type TracedClient = reqwest_middleware::ClientWithMiddleware;

/// Who a [`client`] talks to.
#[derive(Clone, Copy, Debug)]
pub struct Peer {
    /// `peer.service` on the CLIENT span: the node in Tempo's service graph.
    pub service: &'static str,
    /// 4xx/5xx statuses that are a normal answer from this peer. Their spans stay OK (the
    /// status code is still recorded); every other 4xx/5xx is an error, as OTel's HTTP
    /// conventions say for the client side.
    pub expected: &'static [u16],
}

/// The only way to build an HTTP client: every request gets a CLIENT span (a child of the
/// current span) named `METHOD /path`, carries `traceparent`, and gives up after `timeout`.
/// Build clients once at startup: loading the system CA bundle costs 30–100 ms of CPU.
#[allow(
    clippy::disallowed_types,
    clippy::disallowed_methods,
    reason = "the one place a reqwest client is built, so none is without tracing and a timeout"
)]
pub fn client(peer: Peer, timeout: Duration, redirect: reqwest::redirect::Policy) -> TracedClient {
    let inner = reqwest::Client::builder()
        .timeout(timeout)
        .redirect(redirect)
        .build()
        .expect("could not build an HTTP client");
    reqwest_middleware::ClientBuilder::new(inner)
        .with_init(Extension(peer))
        .with(TracingMiddleware::<PeerSpans>::new())
        .build()
}

/// Names client spans `METHOD /path`, with numeric segments (IDs) as `{id}`, and applies the
/// client's [`Peer`]. The URL's query is never recorded.
struct PeerSpans;

impl ReqwestOtelSpanBackend for PeerSpans {
    fn on_request_start(req: &reqwest::Request, ext: &mut http::Extensions) -> tracing::Span {
        // Templated in the attribute too: raw paths could carry user data.
        let path = route(req.url().path());
        let name = format!("{} {path}", req.method());
        let peer = ext.get::<Peer>();
        reqwest_otel_span!(name = name, req, url.path = %path, peer.service = peer.map(|p| p.service))
    }

    fn on_request_end(
        span: &tracing::Span,
        outcome: &reqwest_middleware::Result<reqwest::Response>,
        ext: &mut http::Extensions,
    ) {
        match outcome {
            Ok(response) => {
                let status = response.status();
                span.record("http.response.status_code", status.as_u16());
                let expected = ext.get::<Peer>().is_some_and(|p| p.expected.contains(&status.as_u16()));
                if (status.is_client_error() || status.is_server_error()) && !expected {
                    fail(span, status.as_str().to_owned());
                }
            }
            // A transport error's text is the URL (ours, without a query) and the I/O cause:
            // the request and response bodies are never part of it.
            Err(e) => {
                reqwest_tracing::default_on_request_failure(span, e);
                fail(span, transport_error_type(e));
            }
        }
    }
}

fn transport_error_type(e: &reqwest_middleware::Error) -> &'static str {
    match e {
        reqwest_middleware::Error::Reqwest(e) if e.is_timeout() => "timeout",
        reqwest_middleware::Error::Reqwest(e) if e.is_connect() => "connect",
        reqwest_middleware::Error::Reqwest(e) if e.is_redirect() => "redirect",
        _ => "_OTHER",
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

// ---- Postgres ----

/// A transaction on [`Db`]: its queries get spans through `tx.executor()`.
pub type Tx<'c> = sqlx_tracing::Transaction<'c, sqlx::Postgres>;

/// A CLIENT span for a transaction statement. sqlx-tracing spans a transaction's queries but
/// not these, and each is a round trip to Postgres.
fn tx_span(name: &'static str, statement: &'static str) -> tracing::Span {
    tracing::info_span!(
        "sqlx.transaction",
        otel.name = name,
        otel.kind = "client",
        db.system.name = "postgresql",
        db.operation.name = statement,
        db.query.text = statement,
        peer.service = "postgres",
    )
}

/// `db.begin()` in a span: taking a connection from the pool, then `BEGIN`.
#[allow(clippy::disallowed_methods, reason = "the traced wrapper around Pool::begin")]
pub async fn begin(db: &Db) -> Result<Tx<'_>, sqlx::Error> {
    let span = tx_span("sqlx.begin", "BEGIN");
    db.begin().instrument(span.clone()).await.inspect_err(|e| fail(&span, sqlx_error_type(e)))
}

/// `tx.commit()` in a span.
#[allow(clippy::disallowed_methods, reason = "the traced wrapper around Transaction::commit")]
pub async fn commit(tx: Tx<'_>) -> Result<(), sqlx::Error> {
    let span = tx_span("sqlx.commit", "COMMIT");
    tx.commit().instrument(span.clone()).await.inspect_err(|e| fail(&span, sqlx_error_type(e)))
}

/// `tx.rollback()` in a span, for a transaction given up without an error. A failure is
/// only logged: the connection is broken then, and sqlx discards it. A transaction that is
/// dropped instead (an error's `?`) is rolled back by sqlx when its connection goes back to
/// the pool, after the request and without a span.
#[allow(clippy::disallowed_methods, reason = "the traced wrapper around Transaction::rollback")]
pub async fn rollback(tx: Tx<'_>) {
    let span = tx_span("sqlx.rollback", "ROLLBACK");
    if let Err(e) = tx.rollback().instrument(span.clone()).await {
        fail(&span, sqlx_error_type(&e));
        tracing::warn!(error = %e, "rolling back a transaction failed");
    }
}

/// Waiting for a pool connection, as a `db.pool.acquire` span under whatever asked for it
/// (a query, `sqlx.begin`, a session store call). sqlx measures every acquire itself and
/// reports the slow ones as an event (`acquire_slow_threshold`, set in `pool_options`); this
/// layer turns that event into a span of the same length, so the wait reads as its own bar
/// and not as time in Postgres. Needs `sqlx::pool::acquire=info` in the log filter.
///
/// The event is for this layer: with a threshold of a millisecond it would otherwise be a
/// log line and a span event per acquire that waits. The other layers take
/// [`QuietPoolAcquire`] as their filter, which lets through only the waits of
/// [`SLOW_ACQUIRE`] or more (sqlx's own default for reporting one). An acquire outside any
/// span (startup, a metrics tick) makes no span: it would be a trace of its own, of one bar.
pub struct PoolAcquireSpans(pub opentelemetry_sdk::trace::SdkTracer);

const POOL_ACQUIRE: &str = "sqlx::pool::acquire";
/// A wait for a pool connection that is long enough to log.
pub const SLOW_ACQUIRE: Duration = Duration::from_secs(2);

/// How long the acquire that sqlx reports in `event` waited.
fn acquire_wait(event: &tracing::Event<'_>) -> Option<Duration> {
    struct Waited(Option<f64>);
    impl tracing::field::Visit for Waited {
        fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
            // sqlx's spelling.
            if field.name() == "aquired_after_secs" {
                self.0 = Some(value);
            }
        }
        fn record_debug(&mut self, _: &tracing::field::Field, _: &dyn std::fmt::Debug) {}
    }

    if event.metadata().target() != POOL_ACQUIRE {
        return None;
    }
    let mut waited = Waited(None);
    event.record(&mut waited);
    waited.0.and_then(|secs| Duration::try_from_secs_f64(secs).ok())
}

/// The filter for every layer but [`PoolAcquireSpans`]: sqlx's pool-acquire events stay out
/// of stdout, span events and exported logs unless the wait was [`SLOW_ACQUIRE`] or more.
pub struct QuietPoolAcquire;

impl<S> tracing_subscriber::layer::Filter<S> for QuietPoolAcquire {
    fn enabled(&self, _: &tracing::Metadata<'_>, _: &tracing_subscriber::layer::Context<'_, S>) -> bool {
        true
    }

    fn event_enabled(&self, event: &tracing::Event<'_>, _: &tracing_subscriber::layer::Context<'_, S>) -> bool {
        event.metadata().target() != POOL_ACQUIRE || acquire_wait(event).is_some_and(|waited| waited >= SLOW_ACQUIRE)
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for PoolAcquireSpans {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        let Some(waited) = acquire_wait(event) else { return };
        // The event is written the moment the connection is handed over. The parent is the
        // current OTel context, which tracing-opentelemetry sets to the span being run.
        if !opentelemetry::Context::current().span().span_context().is_valid() {
            return;
        }
        let end = SystemTime::now();
        self.0
            .span_builder("db.pool.acquire")
            .with_kind(SpanKind::Internal)
            .with_start_time(end - waited)
            .start(&self.0)
            .end_with_timestamp(end);
    }
}

/// A session store whose calls get CLIENT spans (children of the current span), so the
/// session table shows in traces and Tempo's service graph. `system` names the database:
/// `TracedStore::new(PostgresStore::new(pool), "postgres")`.
#[derive(Clone, Debug)]
pub struct TracedStore<S> {
    inner: S,
    system: &'static str,
}

impl<S> TracedStore<S> {
    pub fn new(inner: S, system: &'static str) -> Self {
        Self { inner, system }
    }

    async fn traced<T>(
        &self,
        op: &'static str,
        fut: impl Future<Output = session_store::Result<T>>,
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
        // The error's text comes from sqlx or from decoding a stored session: type only.
        fut.instrument(span.clone()).await.inspect_err(|e| {
            fail(
                &span,
                match e {
                    session_store::Error::Encode(_) => "encode",
                    session_store::Error::Decode(_) => "decode",
                    session_store::Error::Backend(_) => "backend",
                },
            )
        })
    }
}

#[async_trait::async_trait]
impl<S: SessionStore> SessionStore for TracedStore<S> {
    // Forwarded, not the trait's default: stores override it (PostgresStore: one insert).
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

#[async_trait::async_trait]
impl<S: ExpiredDeletion> ExpiredDeletion for TracedStore<S> {
    async fn delete_expired(&self) -> session_store::Result<()> {
        self.traced("session.delete_expired", self.inner.delete_expired()).await
    }
}

// ---- Background work ----

/// The tasks [`finish_tasks`] waits for at shutdown.
static TASKS: LazyLock<TaskTracker> = LazyLock::new(TaskTracker::new);

/// A panic in background work: its span ends with status ERROR and `error.type=panic`, and
/// an error is logged in it. The payload stays out (it can quote data; the panic hook has
/// printed it to stderr). The panic then goes on to the task's `JoinHandle` as usual.
fn panicked(span: &tracing::Span, payload: Box<dyn std::any::Any + Send>) -> ! {
    fail(span, "panic");
    tracing::error!(parent: span, "background work panicked");
    std::panic::resume_unwind(payload)
}

/// `fut` in `span`, with a panic marked on the span ([`panicked`]).
async fn in_task_span<F: Future>(span: tracing::Span, fut: F) -> F::Output {
    match AssertUnwindSafe(fut).catch_unwind().instrument(span.clone()).await {
        Ok(output) => output,
        Err(payload) => panicked(&span, payload),
    }
}

/// A unit of background work as a trace of its own: `name` is its root span, with a link to
/// `link` when something caused it (`span.context().span().span_context()`). For work no
/// request is waiting on and that isn't part of one: an iteration of a [`spawn_loop`].
#[allow(clippy::disallowed_methods, reason = "the traced wrapper around tokio::spawn")]
pub fn spawn<F>(name: &'static str, link: Option<SpanContext>, fut: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let span = tracing::info_span!(parent: None, "task", otel.name = name, trace_id = Empty, span_id = Empty);
    if let Some(link) = link {
        span.add_link(link);
    }
    record_ids(&span);
    TASKS.spawn(in_task_span(span, fut))
}

/// Work a request starts and doesn't wait for (sending an email after replying), as a span
/// in the request's trace. It is parented through OTel only, so the request's span still
/// closes, and is exported, when the response goes out; `.in_current_span()` would hold it
/// open until the task ends. The span outlives its parent, which `trace.relation=follows`
/// says (TRACING.md).
#[allow(clippy::disallowed_methods, reason = "the traced wrapper around tokio::spawn")]
pub fn spawn_in_trace<F>(name: &'static str, fut: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let span = tracing::info_span!(
        parent: None,
        "task",
        otel.name = name,
        trace.relation = "follows",
        trace_id = Empty,
        span_id = Empty,
    );
    // Fails only without the OTel layer (no export anyway): the span is new, so not started.
    let _ = span.set_parent(tracing::Span::current().context());
    record_ids(&span);
    TASKS.spawn(in_task_span(span, fut))
}

/// A loop that runs for the life of the process (`name` is for the log). It gets no span: a
/// span is exported when it closes, and this one never would. Each iteration that does I/O
/// is a [`spawn`], so it is a trace of its own and a panic in it doesn't end the loop.
#[allow(clippy::disallowed_methods, reason = "a forever-loop is not a unit of work: its iterations are")]
pub fn spawn_loop<F>(name: &'static str, fut: F) -> JoinHandle<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    tokio::spawn(async move {
        fut.await;
        tracing::error!(name, "background loop ended");
    })
}

/// CPU work on the blocking pool (password hashing), as a span under the current one. The
/// span is entered inside the closure, so its busy time is the work's CPU time.
#[allow(clippy::disallowed_methods, reason = "the traced wrapper around tokio::task::spawn_blocking")]
pub fn spawn_blocking<F, R>(name: &'static str, f: F) -> JoinHandle<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    let span = tracing::info_span!("blocking", otel.name = name, trace_id = Empty, span_id = Empty);
    record_ids(&span);
    TASKS.spawn_blocking(move || {
        span.in_scope(|| std::panic::catch_unwind(AssertUnwindSafe(f))).unwrap_or_else(|payload| panicked(&span, payload))
    })
}

/// At shutdown: waits for the tasks from [`spawn`], [`spawn_in_trace`] and [`spawn_blocking`]
/// (an email being sent), at most `deadline`, so their spans are complete before the flush.
pub async fn finish_tasks(deadline: Duration) {
    TASKS.close();
    if tokio::time::timeout(deadline, TASKS.wait()).await.is_err() {
        tracing::warn!(running = TASKS.len(), "shutdown: background tasks didn't finish in time");
    }
}

/// What the trace tests share: the production pipeline with in-memory exporters, stand-in
/// servers and a throwaway Postgres.
#[cfg(test)]
pub mod testing {
    use std::sync::{Arc, Mutex};

    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider, SpanData};
    use testcontainers_modules::postgres::Postgres;
    use testcontainers_modules::testcontainers::{ContainerAsync, runners::AsyncRunner as _};
    use tracing_subscriber::Layer as _;
    use tracing_subscriber::layer::SubscriberExt as _;

    /// The stdout log, kept in memory.
    #[derive(Clone, Default)]
    struct Logs(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Logs {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl tracing_subscriber::fmt::MakeWriter<'_> for Logs {
        type Writer = Logs;
        fn make_writer(&self) -> Logs {
            self.clone()
        }
    }

    /// `main`'s subscriber for the length of a test, on the test's thread.
    pub struct Pipeline {
        exporter: InMemorySpanExporter,
        logs: Logs,
        _provider: SdkTracerProvider,
        _guard: tracing::subscriber::DefaultGuard,
    }

    /// The same layers and log filter as `main`, exporting to memory (each span as it ends).
    pub fn pipeline() -> Pipeline {
        opentelemetry::global::set_text_map_propagator(
            opentelemetry_sdk::propagation::TraceContextPropagator::new(),
        );
        let exporter = InMemorySpanExporter::default();
        let provider = SdkTracerProvider::builder().with_simple_exporter(exporter.clone()).build();
        let tracer = provider.tracer("test");
        let logs = Logs::default();
        let subscriber = tracing_subscriber::registry()
            .with(tracing_subscriber::EnvFilter::new(crate::LOG_FILTER))
            .with(tracing_subscriber::fmt::layer().json().with_writer(logs.clone()).with_filter(super::QuietPoolAcquire))
            .with(tracing_opentelemetry::layer().with_tracer(tracer.clone()).with_filter(super::QuietPoolAcquire))
            .with(super::PoolAcquireSpans(tracer));
        // tracing keeps "nobody wants this span" per callsite when one subscriber exists and
        // the span is first reached on a thread without it (another test): a second one,
        // kept alive, makes it ask each thread's own.
        static SECOND: std::sync::LazyLock<tracing::Dispatch> =
            std::sync::LazyLock::new(|| tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default()));
        std::sync::LazyLock::force(&SECOND);
        Pipeline { exporter, logs, _provider: provider, _guard: tracing::subscriber::set_default(subscriber) }
    }

    impl Pipeline {
        /// The spans that have ended, in that order.
        pub fn spans(&self) -> Vec<SpanData> {
            self.exporter.get_finished_spans().unwrap()
        }

        /// The ended spans called `name`.
        pub fn named(&self, name: &str) -> Vec<SpanData> {
            self.spans().into_iter().filter(|s| s.name == name).collect()
        }

        /// The one ended span called `name`.
        pub fn span(&self, name: &str) -> SpanData {
            let mut found = self.named(name);
            let names: Vec<_> = self.spans().into_iter().map(|s| s.name).collect();
            assert_eq!(found.len(), 1, "spans called {name:?} among {names:?}");
            found.remove(0)
        }

        /// The stdout log so far: one JSON object per line.
        pub fn logs(&self) -> String {
            String::from_utf8(self.logs.0.lock().unwrap().clone()).unwrap()
        }
    }

    pub fn attr(span: &SpanData, key: &str) -> Option<String> {
        span.attributes.iter().find(|kv| kv.key.as_str() == key).map(|kv| kv.value.to_string())
    }

    /// Everything a span says in text (attributes, status, events), to check what isn't there.
    pub fn text(span: &SpanData) -> String {
        format!("{:?} {:?} {:?}", span.attributes, span.status, span.events)
    }

    /// Serves `app` on a local port and returns its base URL. On the test's runtime and
    /// thread, so the server's own spans are exported with the test's.
    #[allow(clippy::disallowed_methods, reason = "a stand-in server for a test, not the service's own work")]
    pub async fn serve(app: axum::Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        url
    }

    /// A handler that answers with the `traceparent` it was sent.
    pub async fn echo_traceparent(headers: axum::http::HeaderMap) -> String {
        headers.get("traceparent").and_then(|v| v.to_str().ok()).unwrap_or_default().to_string()
    }

    /// A Postgres for one test. Keep it for the test's length: dropping it removes the
    /// container.
    #[allow(clippy::disallowed_types, reason = "tests build pools as `Auth::new` does")]
    pub struct TestDb {
        pub raw: sqlx::PgPool,
        _container: Option<ContainerAsync<Postgres>>,
    }

    impl TestDb {
        pub fn db(&self) -> crate::auth::Db {
            sqlx_tracing::PoolBuilder::from(self.raw.clone()).with_name("postgres").build()
        }
    }

    /// An empty Postgres in a container (needs Docker), with a pool built as in production.
    /// `TEST_DATABASE_URL` uses an existing server instead.
    pub async fn postgres(options: sqlx::postgres::PgPoolOptions) -> TestDb {
        let (url, container) = match std::env::var("TEST_DATABASE_URL") {
            Ok(url) => (url, None),
            Err(_) => {
                let container = Postgres::default().start().await.expect("could not start Postgres (is Docker running?)");
                let port = container.get_host_port_ipv4(5432).await.unwrap();
                (format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres"), Some(container))
            }
        };
        TestDb { raw: options.connect(&url).await.unwrap(), _container: container }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::routing::get;
    use opentelemetry::trace::{SpanId, SpanKind, Status};

    use super::testing::{attr, echo_traceparent, pipeline, serve};
    use super::*;

    fn context_of(span: &tracing::Span) -> SpanContext {
        span.context().span().span_context().clone()
    }

    #[tokio::test]
    async fn client_requests_are_client_spans_that_carry_the_trace() {
        let traced = pipeline();
        let base = serve(
            axum::Router::new()
                .route("/users/{id}/roles", get(echo_traceparent))
                .route("/missing", get(|| async { axum::http::StatusCode::NOT_FOUND }))
                .route("/broken", get(|| async { axum::http::StatusCode::INTERNAL_SERVER_ERROR })),
        )
        .await;
        let http = client(
            Peer { service: "ark", expected: &[404] },
            Duration::from_secs(5),
            reqwest::redirect::Policy::default(),
        );

        let caller = tracing::info_span!("caller");
        let caller_sc = context_of(&caller);
        let traceparent = async {
            let sent = http.get(format!("{base}/users/42/roles?code=secret")).send().await.unwrap();
            http.get(format!("{base}/missing")).send().await.unwrap();
            http.get(format!("{base}/broken")).send().await.unwrap();
            sent.text().await.unwrap()
        }
        .instrument(caller)
        .await;

        let span = traced.span("GET /users/{id}/roles");
        assert_eq!(span.span_kind, SpanKind::Client);
        assert_eq!(span.parent_span_id, caller_sc.span_id());
        assert_eq!(span.status, Status::Unset);
        assert_eq!(attr(&span, "peer.service").as_deref(), Some("ark"));
        assert_eq!(attr(&span, "http.response.status_code").as_deref(), Some("200"));
        assert_eq!(attr(&span, "url.path").as_deref(), Some("/users/{id}/roles"));
        assert!(!super::testing::text(&span).contains("secret"), "the query is on the span");
        // The callee is told the caller's trace and the CLIENT span as its parent.
        assert_eq!(
            traceparent,
            format!("00-{}-{}-01", caller_sc.trace_id(), span.span_context.span_id())
        );

        // An expected status is an answer; any other 4xx/5xx is an error (OTel, client side).
        let missing = traced.span("GET /missing");
        assert_eq!(missing.status, Status::Unset);
        assert_eq!(attr(&missing, "http.response.status_code").as_deref(), Some("404"));
        assert_eq!(attr(&missing, "error.type"), None);
        let broken = traced.span("GET /broken");
        assert_eq!(broken.status, Status::error(""));
        assert_eq!(attr(&broken, "error.type").as_deref(), Some("500"));
    }

    #[tokio::test]
    async fn client_failures_are_errors_with_a_type() {
        let traced = pipeline();
        let base = serve(axum::Router::new().route(
            "/slow",
            get(|| async { tokio::time::sleep(Duration::from_secs(5)).await }),
        ))
        .await;
        let http = client(
            Peer { service: "ark", expected: &[] },
            Duration::from_millis(50),
            reqwest::redirect::Policy::default(),
        );
        // A port nothing listens on: bound, then closed.
        let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap();

        assert!(http.get(format!("{base}/slow")).send().await.is_err());
        assert!(http.get(format!("http://{closed}/gone")).send().await.is_err());

        let slow = traced.span("GET /slow");
        assert_eq!(slow.status, Status::error(""));
        assert_eq!(attr(&slow, "error.type").as_deref(), Some("timeout"));
        let gone = traced.span("GET /gone");
        assert_eq!(gone.status, Status::error(""));
        assert_eq!(attr(&gone, "error.type").as_deref(), Some("connect"));
    }

    #[tokio::test]
    async fn spawn_starts_a_linked_trace_of_its_own() {
        let traced = pipeline();
        let cause = tracing::info_span!("cause");
        let cause_sc = context_of(&cause);

        let unit_sc = async {
            let unit = spawn("unit", Some(cause_sc.clone()), async {
                tracing::info!("in the unit");
                context_of(&tracing::Span::current())
            });
            unit.await.unwrap()
        }
        .instrument(cause)
        .await;

        let unit = traced.span("unit");
        assert_eq!(unit.parent_span_id, SpanId::INVALID, "a root");
        assert_ne!(unit.span_context.trace_id(), cause_sc.trace_id());
        let links: Vec<_> = unit.links.iter().map(|l| l.span_context.clone()).collect();
        assert_eq!(links, [cause_sc]);
        // Its log lines carry its IDs (I8).
        let logs = traced.logs();
        let line = logs.lines().find(|l| l.contains("in the unit")).expect("no log line");
        assert!(line.contains(&format!("\"trace_id\":\"{}\"", unit_sc.trace_id())), "{line}");
        assert!(line.contains(&format!("\"span_id\":\"{}\"", unit_sc.span_id())), "{line}");
    }

    #[tokio::test]
    async fn spawn_in_trace_follows_the_request_without_holding_it_open() {
        let traced = pipeline();
        let request = tracing::info_span!("request");
        let request_sc = context_of(&request);
        let release = std::sync::Arc::new(tokio::sync::Notify::new());

        let task = {
            let release = release.clone();
            let _in_request = request.enter();
            spawn_in_trace("email.verify_send", async move {
                release.notified().await;
                tracing::info!("sent");
            })
        };
        drop(request);

        // The response is out: the request's span is exported while the work still runs.
        assert_eq!(traced.named("request").len(), 1, "request span held open");
        assert!(traced.named("email.verify_send").is_empty());
        release.notify_one();
        task.await.unwrap();

        let work = traced.span("email.verify_send");
        assert_eq!(work.span_context.trace_id(), request_sc.trace_id());
        assert_eq!(work.parent_span_id, request_sc.span_id());
        assert_eq!(attr(&work, "trace.relation").as_deref(), Some("follows"));
        let logs = traced.logs();
        let line = logs.lines().find(|l| l.contains("\"sent\"")).expect("no log line");
        assert!(line.contains(&format!("\"trace_id\":\"{}\"", request_sc.trace_id())), "{line}");
    }

    #[tokio::test]
    async fn spawn_blocking_is_a_child_span() {
        let traced = pipeline();
        let caller = tracing::info_span!("caller");
        let caller_sc = context_of(&caller);

        // The blocking thread has no subscriber of its own (in a test): ask OTel what is current.
        let inside = async {
            spawn_blocking("password.hash", || opentelemetry::Context::current().span().span_context().span_id())
                .await
                .unwrap()
        }
        .instrument(caller)
        .await;

        let span = traced.span("password.hash");
        assert_eq!(span.parent_span_id, caller_sc.span_id());
        assert_eq!(inside, span.span_context.span_id(), "the closure runs in the span");
    }

    #[tokio::test]
    async fn a_panic_in_spawned_work_is_an_error_on_its_span() {
        let traced = pipeline();
        let async_panic = |name: &'static str| async move { panic!("{name}: ada@example.com") };

        assert!(spawn("unit", None, async_panic("unit")).await.unwrap_err().is_panic());
        assert!(spawn_in_trace("follower", async_panic("follower")).await.unwrap_err().is_panic());
        let blocking: JoinHandle<()> = spawn_blocking("blocking", || panic!("blocking: ada@example.com"));
        assert!(blocking.await.unwrap_err().is_panic());

        for name in ["unit", "follower", "blocking"] {
            let span = traced.span(name);
            assert_eq!(span.status, Status::error(""), "{name}");
            assert_eq!(attr(&span, "error.type").as_deref(), Some("panic"), "{name}");
            assert!(!super::testing::text(&span).contains("ada@example.com"), "{name} holds the payload");
        }
        // Logged at error, in the span (the blocking thread has no subscriber in a test).
        let logs = traced.logs();
        let lines: Vec<_> = logs.lines().filter(|l| l.contains("background work panicked")).collect();
        assert_eq!(lines.len(), 2, "{logs}");
        for line in lines {
            assert!(line.contains("\"level\":\"ERROR\"") && line.contains("\"trace_id\""), "{line}");
            assert!(!line.contains("ada@example.com"), "{line}");
        }
    }

    #[tokio::test]
    async fn finish_tasks_waits_for_spawned_work_until_the_deadline() {
        let traced = pipeline();
        let _quick = spawn_in_trace("quick", tokio::time::sleep(Duration::from_millis(50)));
        finish_tasks(Duration::from_secs(5)).await;
        assert_eq!(traced.named("quick").len(), 1, "shutdown didn't wait for the task");

        let _stuck = spawn("stuck", None, tokio::time::sleep(Duration::from_secs(60)));
        let started = std::time::Instant::now();
        finish_tasks(Duration::from_millis(50)).await;
        assert!(started.elapsed() < Duration::from_secs(5), "shutdown waited past its deadline");
    }

    /// A store that fails, with user data in the error's text.
    #[derive(Clone, Debug)]
    struct BrokenStore;

    #[async_trait::async_trait]
    impl SessionStore for BrokenStore {
        async fn save(&self, _: &Record) -> session_store::Result<()> {
            Err(session_store::Error::Backend("duplicate key ada@example.com".into()))
        }
        async fn load(&self, _: &Id) -> session_store::Result<Option<Record>> {
            Err(session_store::Error::Backend("duplicate key ada@example.com".into()))
        }
        async fn delete(&self, _: &Id) -> session_store::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn session_store_calls_are_client_spans_of_the_caller() {
        let traced = pipeline();
        let store = TracedStore::new(tower_sessions::MemoryStore::default(), "postgres");
        let caller = tracing::info_span!("request");
        let caller_sc = context_of(&caller);
        let id = Id::default();

        async {
            let record = Record { id, data: Default::default(), expiry_date: time_far_ahead() };
            store.save(&record).await.unwrap();
            assert!(store.load(&id).await.unwrap().is_some());
            store.delete(&id).await.unwrap();
            assert!(TracedStore::new(BrokenStore, "postgres").load(&id).await.is_err());
        }
        .instrument(caller)
        .await;

        for op in ["session.save", "session.delete"] {
            let span = traced.span(&format!("postgres {op}"));
            assert_eq!(span.span_kind, SpanKind::Client);
            assert_eq!(span.parent_span_id, caller_sc.span_id());
            assert_eq!(span.status, Status::Unset);
            assert_eq!(attr(&span, "peer.service").as_deref(), Some("postgres"));
            assert_eq!(attr(&span, "db.operation.name").as_deref(), Some(op));
        }
        let [loaded, failed] = &traced.named("postgres session.load")[..] else { panic!("two loads") };
        assert_eq!(loaded.status, Status::Unset);
        assert_eq!(failed.status, Status::error(""));
        assert_eq!(attr(failed, "error.type").as_deref(), Some("backend"));
        assert!(!super::testing::text(failed).contains("ada@example.com"), "the error's text is on the span");
    }

    fn time_far_ahead() -> tower_sessions::cookie::time::OffsetDateTime {
        tower_sessions::cookie::time::OffsetDateTime::now_utc() + tower_sessions::cookie::time::Duration::days(1)
    }

    #[test]
    fn database_errors_have_a_type() {
        assert_eq!(sqlx_error_type(&sqlx::Error::PoolTimedOut), "pool_timed_out");
        assert_eq!(sqlx_error_type(&sqlx::Error::RowNotFound), "row_not_found");
        assert_eq!(sqlx_error_type(&sqlx::Error::WorkerCrashed), "_OTHER");
    }

    #[test]
    fn route_templates_ids() {
        assert_eq!(route("/users/42/roles/7"), "/users/{id}/roles/{id}");
        assert_eq!(route("/ark/num_players"), "/ark/num_players");
        assert_eq!(route("/"), "/");
    }
}
