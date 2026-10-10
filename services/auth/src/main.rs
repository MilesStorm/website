mod auth;

use opentelemetry::KeyValue;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{Resource, logs::SdkLoggerProvider, trace::SdkTracerProvider};
use tracing_subscriber::{
    EnvFilter, Layer, filter::filter_fn, layer::SubscriberExt, registry::LookupSpan,
    util::SubscriberInitExt,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Only init OTLP when the endpoint is explicitly configured. In dev (no env var) the
    // layers are None and tracing-subscriber skips them, so there are no connection errors.
    // Always register W3C trace-context propagator so OtelAxumLayer can extract
    // incoming traceparent headers regardless of whether OTLP export is configured.
    opentelemetry::global::set_text_map_propagator(
        opentelemetry_sdk::propagation::TraceContextPropagator::new(),
    );

    let (otel_layer, otel_log_layer, otel_provider, otel_log_provider): (
        Option<_>,
        Option<_>,
        Option<SdkTracerProvider>,
        Option<SdkLoggerProvider>,
    ) = match std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT") {
            Ok(endpoint) => {
                let exporter = opentelemetry_otlp::SpanExporter::builder()
                    .with_tonic()
                    .with_endpoint(endpoint.clone())
                    .build()?;

                let provider = SdkTracerProvider::builder()
                    .with_batch_exporter(exporter)
                    .with_resource(resource())
                    .build();

                // Take the SDK Tracer before handing provider to global — global::tracer() returns
                // BoxedTracer which doesn't satisfy tracing-opentelemetry's PreSampledTracer bound.
                let tracer = provider.tracer("auth");
                opentelemetry::global::set_tracer_provider(provider.clone());

                let log_exporter = opentelemetry_otlp::LogExporter::builder()
                    .with_tonic()
                    .with_endpoint(endpoint)
                    .build()?;

                let log_provider = SdkLoggerProvider::builder()
                    .with_batch_exporter(log_exporter)
                    .with_resource(resource())
                    .build();

                let log_bridge = log_bridge(&log_provider);

                (
                    Some(tracing_opentelemetry::layer().with_tracer(tracer)),
                    Some(log_bridge),
                    Some(provider),
                    Some(log_provider),
                )
            }
            Err(_) => (None, None, None, None),
        };

    // JSON structured logging — one object per line, parsed by Loki / any log aggregator.
    // The OTel log bridge additionally ships log events via OTLP so Loki entries carry
    // trace_id/span_id, enabling Tempo → Loki correlation.
    // The filter also decides which spans exist (TRACING.md, "Log level"). Targets match by
    // prefix: `sqlx=warn` would hide sqlx_tracing's query spans too, hence the override.
    tracing_subscriber::registry()
        .with(EnvFilter::new(std::env::var("RUST_LOG").unwrap_or_else(
            |_| "info,sqlx=warn,sqlx_tracing=info,tower_sessions=warn,axum_login=warn,opentelemetry=warn".into(),
        )))
        .with(tracing_subscriber::fmt::layer().json())
        .with(otel_layer)
        .with(otel_log_layer)
        .try_init()?;

    match dotenvy::dotenv() {
        Ok(_) => tracing::debug!("loaded .env file"),
        Err(_) if !cfg!(debug_assertions) => {
            tracing::debug!("no .env file found, using environment variables");
        }
        Err(e) => panic!("could not load .env: {e}"),
    }

    tracing::info!("starting auth service");

    let result = auth::Auth::new().await?.server().await;

    // Flush buffered spans and log records before exit.
    if let Some(provider) = otel_provider {
        provider.shutdown()?;
    }
    if let Some(provider) = otel_log_provider {
        let _ = provider.shutdown();
    }

    result
}

/// `service.version` is the commit the image was built from (Dockerfile `GIT_SHA`).
/// `OTEL_RESOURCE_ATTRIBUTES` adds the rest (`deployment.environment.name`).
fn resource() -> Resource {
    Resource::builder()
        .with_service_name("auth")
        .with_attribute(KeyValue::new(
            "service.version",
            std::env::var("GIT_SHA").unwrap_or_else(|_| "unknown".into()),
        ))
        .build()
}

/// The OTLP log bridge. Log records take their trace_id/span_id from the OTel context
/// tracing-opentelemetry activates with each span. The SDK's own logs (its warnings, such as
/// dropped spans, go to stdout at `warn`) stay out: exporting them would log more.
fn log_bridge<S>(provider: &SdkLoggerProvider) -> impl Layer<S>
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    OpenTelemetryTracingBridge::new(provider)
        .with_filter(filter_fn(|meta| !meta.target().starts_with("opentelemetry")))
}

#[cfg(test)]
mod tests {
    use opentelemetry::trace::{TraceContextExt as _, TracerProvider as _};
    use opentelemetry_sdk::logs::{InMemoryLogExporter, SdkLoggerProvider};
    use tracing_opentelemetry::OpenTelemetrySpanExt as _;
    use tracing_subscriber::layer::SubscriberExt as _;

    /// Log records carry the trace and span of the span they were written in (I8), and
    /// the SDK's own logs stay out of the bridge.
    #[test]
    fn log_records_carry_the_span_and_skip_sdk_logs() {
        let exporter = InMemoryLogExporter::default();
        let logs = SdkLoggerProvider::builder().with_simple_exporter(exporter.clone()).build();
        let tracer = opentelemetry_sdk::trace::SdkTracerProvider::builder().build().tracer("test");
        let subscriber = tracing_subscriber::registry()
            .with(tracing_opentelemetry::layer().with_tracer(tracer))
            .with(super::log_bridge(&logs));

        let sc = tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!("request");
            let _entered = span.enter();
            tracing::info!("in a span");
            tracing::warn!(target: "opentelemetry_sdk", "sdk warning");
            span.context().span().span_context().clone()
        });

        let records = exporter.get_emitted_logs().unwrap();
        let [record] = &records[..] else { panic!("{} records", records.len()) };
        let tc = record.record.trace_context().expect("no trace context");
        assert_eq!(tc.trace_id, sc.trace_id());
        assert_eq!(tc.span_id, sc.span_id());
    }
}
