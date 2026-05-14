mod datasets;
mod helper;
pub mod model;
mod serve;

use std::{env, path::Path};

use burn::{
    backend::{Autodiff, Cuda, cuda::CudaDevice},
    optim::AdamConfig,
    tensor::bf16,
};

use crate::{
    datasets::dataset::DatasetType,
    helper::{latest_experiment_dir, next_experiment_dir},
    model::training::{TrainingConfig, eval, train},
};
const ART_ROOT: &str = "./art";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _otel = setup_tracing().await;
    type MyBackend = Autodiff<Cuda<bf16>>;

    let args: Vec<String> = env::args().collect();

    if args.contains(&String::from("yolo")) {
        // WebSocket inference server: browser webcam → YOLO bbox → DiceHead → JSON detections.
        // Default address can be overridden: cargo r --release -- yolo 0.0.0.0:9001
        let addr = args
            .iter()
            .skip(2)
            .find(|a| a.contains(':'))
            .map(String::as_str)
            .unwrap_or("0.0.0.0:9000");

        let exp_dir = latest_experiment_dir(Path::new(ART_ROOT))
            .unwrap_or_else(|| panic!("No experiment_* dirs found in {}", ART_ROOT));
        tracing::info!(path = %exp_dir.display(), "loading model weights");

        serve::serve(addr, exp_dir).await?;
        return Ok(());
    }

    tracing::info!("creating CUDA device");
    let device = CudaDevice::new(0);
    tracing::info!("CUDA device ready");

    let config = TrainingConfig::new(AdamConfig::new())
        .with_num_epochs(80)
        .with_batch_size(128)
        .with_num_workers(0)
        .with_seed(42)
        .with_learning_rate(1e-3)
        .with_weight_decay(5e-5);

    if args.contains(&String::from("eval")) {
        let exp_dir = latest_experiment_dir(Path::new(ART_ROOT))
            .unwrap_or_else(|| panic!("No experiment_* directories found in {}", ART_ROOT));
        let exp_dir_str = exp_dir.to_string_lossy().to_string();

        let data_path = std::path::Path::new("./data/dice_face");
        eval::<MyBackend>(&exp_dir_str, device, data_path, DatasetType::Folder);
    } else if args.contains(&String::from("folder")) {
        let exp_dir = next_experiment_dir(Path::new(ART_ROOT));
        let exp_dir_str = exp_dir.to_string_lossy().to_string();

        let data_path = std::path::Path::new("./data/dice_face");
        train::<MyBackend>(&exp_dir_str, config, device, data_path, DatasetType::Folder);
    } else {
        // evaluation pipeline should go here
    }

    Ok(())
}

// Kept alive for the process lifetime so batch exporters flush on drop.
struct OtelProviders {
    _tracer: opentelemetry_sdk::trace::TracerProvider,
    _logger: opentelemetry_sdk::logs::LoggerProvider,
}

async fn setup_tracing() -> Option<OtelProviders> {
    use opentelemetry::KeyValue;
    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
    use opentelemetry_otlp::WithExportConfig;
    use opentelemetry_sdk::{
        Resource,
        logs::LoggerProvider as SdkLoggerProvider,
        runtime::Tokio as OtelTokio,
        trace::TracerProvider as SdkTracerProvider,
    };
    use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

    let resource = Resource::new([KeyValue::new("service.name", "ai_pipeline")]);
    let env_filter = EnvFilter::new(
        std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()),
    );

    let Ok(endpoint) = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT") else {
        tracing_subscriber::registry()
            .with(env_filter)
            .with(tracing_subscriber::fmt::layer().json())
            .init();
        return None;
    };

    let span_exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_endpoint(endpoint.clone())
        .build()
        .expect("failed to build OTLP span exporter");

    let tracer_provider = SdkTracerProvider::builder()
        .with_batch_exporter(span_exporter, OtelTokio)
        .with_resource(resource.clone())
        .build();

    let tracer = tracer_provider.tracer("ai_pipeline");
    opentelemetry::global::set_tracer_provider(tracer_provider.clone());
    opentelemetry::global::set_text_map_propagator(
        opentelemetry_sdk::propagation::TraceContextPropagator::new(),
    );

    let log_exporter = opentelemetry_otlp::LogExporter::builder()
        .with_tonic()
        .with_endpoint(endpoint)
        .build()
        .expect("failed to build OTLP log exporter");

    let logger_provider = SdkLoggerProvider::builder()
        .with_batch_exporter(log_exporter, OtelTokio)
        .with_resource(resource)
        .build();

    tracing_subscriber::registry()
        .with(env_filter)
        .with(tracing_subscriber::fmt::layer().json())
        .with(tracing_opentelemetry::layer().with_tracer(tracer))
        .with(OpenTelemetryTracingBridge::new(&logger_provider))
        .init();

    Some(OtelProviders {
        _tracer: tracer_provider,
        _logger: logger_provider,
    })
}
