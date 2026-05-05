mod auth;

use opentelemetry::KeyValue;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{Resource, runtime::Tokio as OtelTokio, trace::TracerProvider as SdkTracerProvider};
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Only init OTLP when the endpoint is explicitly configured. In dev (no env var) the
    // layer is None and tracing-subscriber skips it, so there are no connection errors.
    let (otel_layer, otel_provider) = match std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT") {
        Ok(endpoint) => {
            let exporter = opentelemetry_otlp::SpanExporter::builder()
                .with_tonic()
                .with_endpoint(endpoint)
                .build()?;

            let provider = SdkTracerProvider::builder()
                .with_batch_exporter(exporter, OtelTokio)
                .with_resource(Resource::new([KeyValue::new("service.name", "auth")]))
                .build();

            // Take the SDK Tracer before handing provider to global — global::tracer() returns
            // BoxedTracer which doesn't satisfy tracing-opentelemetry's PreSampledTracer bound.
            let tracer = provider.tracer("auth");
            opentelemetry::global::set_tracer_provider(provider.clone());

            (Some(tracing_opentelemetry::layer().with_tracer(tracer)), Some(provider))
        }
        Err(_) => (None, None),
    };

    // JSON structured logging — one object per line, parsed by Loki / any log aggregator.
    tracing_subscriber::registry()
        .with(EnvFilter::new(std::env::var("RUST_LOG").unwrap_or_else(
            |_| "info,sqlx=warn,tower_sessions=warn".into(),
        )))
        .with(tracing_subscriber::fmt::layer().json())
        .with(otel_layer)
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

    // Flush buffered spans before exit.
    if let Some(provider) = otel_provider {
        provider.shutdown()?;
    }

    result
}
