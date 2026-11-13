//! The traced paths for media-world's I/O and work off the async threads: outbound HTTP
//! (downloads), Garage (S3), DuckDB and CPU-bound stages. clippy bans the raw APIs everywhere
//! else (`clippy.toml`, exceptions in `allowed_raw_io.toml`). Conventions: TRACING.md.

use std::borrow::Cow;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use opentelemetry::trace::Status;
use reqwest_middleware::Extension;
use reqwest_tracing::{ReqwestOtelSpanBackend, TracingMiddleware, reqwest_otel_span};
use tracing::Instrument as _;
use tracing_opentelemetry::OpenTelemetrySpanExt as _;

// ---- Errors ----

/// Marks `span` as failed: OTel status ERROR and `error.type`, never the error's text.
pub fn fail(span: &tracing::Span, error_type: impl Into<Cow<'static, str>>) {
    span.set_status(Status::error(""));
    span.set_attribute("error.type", error_type.into());
}

// ---- Outbound HTTP ----

pub type TracedClient = reqwest_middleware::ClientWithMiddleware;

/// Who a [`client`] talks to.
#[derive(Clone, Copy, Debug)]
pub struct Peer {
    /// `peer.service` on the CLIENT span.
    pub service: &'static str,
}

/// The only way to build an HTTP client: every request gets a CLIENT span named
/// `METHOD /path` with `peer.service`. Downloads take long, so there is no total timeout: a
/// connection must open within `connect` and the body may stall for at most `read` at a time.
/// Redirects are followed (archive.org serves the dumps).
#[allow(
    clippy::disallowed_types,
    clippy::disallowed_methods,
    reason = "the one place a reqwest client is built, so none is without tracing and timeouts"
)]
pub fn client(peer: Peer, user_agent: &str, connect: Duration, read: Duration) -> TracedClient {
    let inner = reqwest::Client::builder()
        .user_agent(user_agent)
        .connect_timeout(connect)
        .read_timeout(read)
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .expect("could not build an HTTP client");
    reqwest_middleware::ClientBuilder::new(inner)
        .with_init(Extension(peer))
        .with(TracingMiddleware::<PeerSpans>::new())
        .build()
}

/// Names client spans `METHOD /path` and applies the client's [`Peer`]. The query is never
/// recorded.
struct PeerSpans;

impl ReqwestOtelSpanBackend for PeerSpans {
    fn on_request_start(req: &reqwest::Request, ext: &mut http::Extensions) -> tracing::Span {
        let path = req.url().path().to_owned();
        let name = format!("{} {path}", req.method());
        let peer = ext.get::<Peer>();
        reqwest_otel_span!(name = name, req, url.path = %path, peer.service = peer.map(|p| p.service))
    }

    fn on_request_end(
        span: &tracing::Span,
        outcome: &reqwest_middleware::Result<reqwest::Response>,
        _: &mut http::Extensions,
    ) {
        match outcome {
            Ok(response) => {
                let status = response.status();
                span.record("http.response.status_code", status.as_u16());
                if status.is_client_error() || status.is_server_error() {
                    fail(span, status.as_str().to_owned());
                }
            }
            Err(e) => {
                reqwest_tracing::default_on_request_failure(span, e);
                fail(
                    span,
                    match e {
                        reqwest_middleware::Error::Reqwest(e) if e.is_timeout() => "timeout",
                        reqwest_middleware::Error::Reqwest(e) if e.is_connect() => "connect",
                        _ => "_OTHER",
                    },
                );
            }
        }
    }
}

// ---- Work off the async threads ----

/// Blocking work (DuckDB, indexing, hashing) on the blocking pool, in a span under the
/// current one.
#[allow(
    clippy::disallowed_methods,
    reason = "the traced wrapper around tokio::task::spawn_blocking"
)]
pub async fn spawn_blocking<F, R>(name: &'static str, f: F) -> anyhow::Result<R>
where
    F: FnOnce() -> anyhow::Result<R> + Send + 'static,
    R: Send + 'static,
{
    let span = tracing::info_span!("blocking", otel.name = name);
    let inner = span.clone();
    let result = tokio::task::spawn_blocking(move || inner.in_scope(f)).await;
    match result {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(e)) => {
            fail(&span, "failed");
            Err(e)
        }
        Err(join) => {
            fail(&span, "panic");
            Err(anyhow::anyhow!("{name} panicked: {join}"))
        }
    }
}

// ---- DuckDB ----

/// The build's DuckDB database. DuckDB does its own file I/O in-process; every statement batch
/// runs in a `duckdb.exec` span.
pub struct Duck(duckdb::Connection);

impl Duck {
    #[allow(
        clippy::disallowed_methods,
        reason = "the traced wrapper: the build's one DuckDB connection"
    )]
    pub fn open(
        path: &std::path::Path,
        memory_limit: &str,
        threads: u32,
        temp_dir: &std::path::Path,
    ) -> anyhow::Result<Duck> {
        let conn = duckdb::Connection::open(path)?;
        conn.execute_batch(OFFLINE)?;
        conn.execute_batch(&format!(
            "SET memory_limit = '{}'; SET threads = {threads}; SET temp_directory = '{}'; SET preserve_insertion_order = false;",
            sql_text(memory_limit),
            sql_text(&temp_dir.display().to_string()),
        ))?;
        Ok(Duck(conn))
    }

    #[allow(
        clippy::disallowed_methods,
        reason = "the traced wrapper: an in-memory database for tests"
    )]
    #[cfg(test)]
    pub fn memory() -> Duck {
        let conn = duckdb::Connection::open_in_memory().expect("in-memory DuckDB");
        conn.execute_batch(OFFLINE).expect("extension settings");
        Duck(conn)
    }

    /// Runs a batch of statements (one stage's SQL) in a span named after it.
    pub fn exec(&self, name: &str, sql: &str) -> anyhow::Result<()> {
        let span = tracing::info_span!(
            "duckdb.exec",
            otel.name = format!("duckdb {name}"),
            db.system.name = "duckdb"
        );
        let _entered = span.enter();
        self.0
            .execute_batch(sql)
            .inspect_err(|_| fail(&span, "duckdb"))
            .map_err(|e| anyhow::anyhow!("{name}: {}", duckdb_error(&e)))
    }

    /// Sets a session variable for `getvariable(name)`.
    pub fn set(&self, name: &str, value: &Value) -> anyhow::Result<()> {
        let literal = match value {
            Value::Text(s) => format!("'{}'", sql_text(s)),
            Value::Int(i) => i.to_string(),
            Value::Float(f) => format!("{f:?}"),
        };
        self.0
            .execute_batch(&format!("SET VARIABLE {name} = {literal};"))?;
        Ok(())
    }

    /// For queries that read results (the report, packing).
    pub fn conn(&self) -> &duckdb::Connection {
        &self.0
    }
}

/// DuckDB never installs or loads an extension by itself: JSON is compiled in (Cargo.toml),
/// and nothing is fetched from extensions.duckdb.org into `$HOME/.duckdb` at run time.
const OFFLINE: &str =
    "SET autoinstall_known_extensions = false; SET autoload_known_extensions = false;";

/// DuckDB error classes whose text is about our SQL, never the data: shown whole.
const SQL_ERRORS: [&str; 4] = [
    "Parser Error",
    "Binder Error",
    "Catalog Error",
    "Not implemented Error",
];

/// A DuckDB error as it may be logged (PLAN.md rule 14). Errors about the data (conversion,
/// invalid input, CSV and JSON reading) quote input lines, which hold book titles: only their
/// class is kept.
pub fn duckdb_error(e: &duckdb::Error) -> String {
    let text = e.to_string();
    let class = text
        .split_once(": ")
        .map(|(c, _)| c)
        .filter(|c| c.ends_with("Error"));
    match class {
        Some(c) if SQL_ERRORS.contains(&c) => text.lines().next().unwrap_or_default().to_owned(),
        Some(c) => format!("{c} (details withheld: they may quote input data)"),
        None => "DuckDB error (details withheld: they may quote input data)".to_owned(),
    }
}

/// An error chain as it may be logged: DuckDB errors in it go through [`duckdb_error`].
pub fn loggable(e: &anyhow::Error) -> String {
    e.chain()
        .map(|cause| match cause.downcast_ref::<duckdb::Error>() {
            Some(d) => duckdb_error(d),
            None => cause.to_string(),
        })
        .collect::<Vec<_>>()
        .join(": ")
}

/// A value for [`Duck::set`].
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Text(String),
    Int(i64),
    Float(f64),
}

/// A string inside single quotes in SQL.
fn sql_text(s: &str) -> String {
    s.replace('\'', "''")
}

// ---- Garage (S3) ----

/// The world bucket. object_store does its own HTTP; each call here is a CLIENT span with
/// `peer.service = "garage"`.
#[derive(Clone)]
pub struct Bucket(Arc<dyn object_store::ObjectStore>);

impl Bucket {
    #[allow(
        clippy::disallowed_types,
        reason = "the traced wrapper: the one S3 client"
    )]
    pub fn garage(
        endpoint: &str,
        region: &str,
        bucket: &str,
        key_id: &str,
        secret: &str,
    ) -> anyhow::Result<Bucket> {
        let store = object_store::aws::AmazonS3Builder::new()
            .with_endpoint(endpoint)
            .with_region(region)
            .with_bucket_name(bucket)
            .with_access_key_id(key_id)
            .with_secret_access_key(secret)
            .with_allow_http(true)
            .with_virtual_hosted_style_request(false)
            .build()?;
        Ok(Bucket(Arc::new(store)))
    }

    /// Any store (tests use object_store's in-memory one).
    pub fn from_store(store: Arc<dyn object_store::ObjectStore>) -> Bucket {
        Bucket(store)
    }

    pub fn store(&self) -> &Arc<dyn object_store::ObjectStore> {
        &self.0
    }

    /// Runs one S3 operation in a CLIENT span.
    pub async fn call<T, F>(&self, operation: &'static str, fut: F) -> object_store::Result<T>
    where
        F: Future<Output = object_store::Result<T>>,
    {
        let span = tracing::info_span!(
            "s3",
            otel.name = format!("S3 {operation}"),
            otel.kind = "client",
            peer.service = "garage",
            rpc.system = "aws-api",
            rpc.method = operation,
        );
        let result = fut.instrument(span.clone()).await;
        match &result {
            // A missing object is a normal answer (no world published yet).
            Err(object_store::Error::NotFound { .. }) | Ok(_) => {}
            Err(object_store::Error::Precondition { .. }) => fail(&span, "precondition"),
            Err(_) => fail(&span, "_OTHER"),
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rule 14: a stage failing on a malformed input line logs no text from that line.
    #[test]
    fn duckdb_errors_about_data_quote_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let csv = dir.path().join("bad.csv");
        std::fs::write(
            &csv,
            "work,rating\n1,5\nThe Secret Title Of A Book,not a number\n",
        )
        .unwrap();
        let duck = Duck::memory();
        let sql = format!(
            "CREATE TABLE t AS SELECT * FROM read_csv('{}', header = true, columns = {{'work': 'BIGINT', 'rating': 'INTEGER'}});",
            csv.display()
        );
        let err = duck.exec("ingest", &sql).unwrap_err().to_string();
        assert!(err.starts_with("ingest: "), "{err}");
        assert!(
            !err.contains("Secret") && !err.contains("not a number"),
            "{err}"
        );

        // Raw DuckDB errors that reach main through `?` are cleaned when logged.
        let raw = duck
            .conn()
            .query_row("SELECT CAST('The Secret Title' AS INTEGER)", [], |r| {
                r.get::<_, i32>(0)
            })
            .unwrap_err();
        let logged = loggable(&anyhow::Error::from(raw).context("pack"));
        assert!(
            logged.starts_with("pack: ") && !logged.contains("Secret"),
            "{logged}"
        );

        // Errors about our SQL stay readable.
        let err = duck
            .exec("series", "SELECT no_such_column FROM range(1)")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("Binder Error") && err.contains("no_such_column"),
            "{err}"
        );
    }
}
