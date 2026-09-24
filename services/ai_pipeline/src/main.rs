mod datasets;
mod helper;
pub mod model;
mod serve;

use std::{env, path::Path};

use burn::{
    backend::{Autodiff, Cuda, cuda::CudaDevice},
    optim::AdamConfig,
};

use crate::{
    datasets::dataset::DatasetType,
    helper::{latest_experiment_dir, next_experiment_dir},
    model::inferance::export_yolo_crops,
    model::training::{TrainingConfig, audit, eval, train},
};
const ART_ROOT: &str = "./art";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // f32, not bf16: the head is ~0.9M params on 64x64 crops, so f32 costs
    // little, while bf16 training diverged to NaN by epoch 3 (experiment_37).
    type MyBackend = Autodiff<Cuda<f32>>;

    let args: Vec<String> = env::args().collect();

    // Only the serve path logs to stdout. The `folder`/`eval` paths hand stdout
    // to burn-train's ratatui TUI dashboard; installing a stdout subscriber
    // there interleaves JSON log lines (incl. cubecl autotune `log::` output
    // bridged in via LogTracer) into the alternate screen and garbles the TUI.
    // When we skip it, burn-train installs its own file logger
    // (experiment.log) instead and the dashboard stays clean.
    let is_serve = args.contains(&String::from("yolo"));
    let _otel = setup_tracing(is_serve).await;

    if args.contains(&String::from("yolo")) {
        // WebSocket inference server: browser webcam → YOLO bbox → DiceHead → JSON detections.
        // Default address can be overridden: cargo r --release -- yolo 0.0.0.0:9001
        let addr = args
            .iter()
            .skip(2)
            .find(|a| a.contains(':'))
            .map(String::as_str)
            .unwrap_or("0.0.0.0:9000");

        let exp_dir = if let Ok(p) = std::env::var("DICE_HEAD_PATH") {
            std::path::PathBuf::from(p)
        } else {
            latest_experiment_dir(Path::new(ART_ROOT))
                .unwrap_or_else(|| panic!("No experiment_* dirs found in {}", ART_ROOT))
        };
        tracing::info!(path = %exp_dir.display(), "loading model weights");

        serve::serve(addr, exp_dir).await?;
        return Ok(());
    }

    if args.contains(&String::from("export-crops")) {
        // Build the head's training set from labeled dice_face photos by running
        // the real YOLO detector and saving its top-number crops through the
        // exact serve-time crop path, so the head trains on what it is served.
        //   cargo r --release -- export-crops [src] [dst] [conf]
        let positional: Vec<&String> = args.iter().skip(2).filter(|a| !a.contains(':')).collect();
        let src = positional
            .first()
            .map(|s| s.as_str())
            .unwrap_or("./data/dice_face");
        let dst = positional
            .get(1)
            .map(|s| s.as_str())
            .unwrap_or("./data/dice_face_crops");
        let conf = positional
            .get(2)
            .and_then(|s| s.parse::<f32>().ok())
            .unwrap_or(0.25);

        let device = CudaDevice::new(0);
        println!("exporting YOLO number crops: {src} -> {dst} (conf {conf})");
        export_yolo_crops::<Cuda<f32>>(device, Path::new(src), Path::new(dst), conf);
        return Ok(());
    }

    if args.contains(&String::from("audit")) {
        // Out-of-fold label audit over one or more experiments (one per fold):
        //   cargo r --release -- audit art/experiment_40 art/experiment_41
        let exps: Vec<String> = args.iter().skip(2).cloned().collect();
        for e in &exps {
            assert!(
                Path::new(e).join("model/model.bpk").is_file(),
                "{e} is not an experiment dir (no model/model.bpk)"
            );
        }
        assert!(!exps.is_empty(), "usage: audit <experiment dir>...");
        audit::<Cuda<f32>>(
            &exps,
            CudaDevice::new(0),
            Path::new("./data/dice_face_crops"),
            &Path::new(ART_ROOT).join("audit.csv"),
        );
        return Ok(());
    }

    tracing::info!("creating CUDA device");
    let device = CudaDevice::new(0);
    tracing::info!("CUDA device ready");

    // Optional overrides: --epochs N --folds K --fold I
    let flag = |name: &str| -> Option<usize> {
        let i = args.iter().position(|a| a == name)?;
        let v = args.get(i + 1)?;
        Some(v.parse().unwrap_or_else(|_| panic!("{name} expects a number, got {v}")))
    };

    let config = TrainingConfig::new(AdamConfig::new())
        .with_num_epochs(flag("--epochs").unwrap_or(80))
        .with_num_folds(flag("--folds").unwrap_or(5))
        .with_val_fold(flag("--fold").unwrap_or(0))
        .with_batch_size(128)
        .with_num_workers(0)
        .with_seed(42)
        .with_learning_rate(1e-3)
        .with_weight_decay(5e-5);

    if args.contains(&String::from("eval")) {
        let exp_dir = latest_experiment_dir(Path::new(ART_ROOT))
            .unwrap_or_else(|| panic!("No experiment_* directories found in {}", ART_ROOT));
        let exp_dir_str = exp_dir.to_string_lossy().to_string();

        let data_path = std::path::Path::new("./data/dice_face_crops");
        eval::<MyBackend>(&exp_dir_str, device, data_path, DatasetType::Folder);
    } else if args.contains(&String::from("folder")) {
        let exp_dir = next_experiment_dir(Path::new(ART_ROOT));
        let exp_dir_str = exp_dir.to_string_lossy().to_string();

        let data_path = std::path::Path::new("./data/dice_face_crops");
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

async fn setup_tracing(stdout_logging: bool) -> Option<OtelProviders> {
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

    let resource = Resource::new([KeyValue::new("service.name", "ai-pipeline")]);
    let env_filter = EnvFilter::new(
        std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()),
    );

    let Ok(endpoint) = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT") else {
        // Skip the stdout layer for training/eval so burn-train's TUI owns the
        // terminal cleanly; burn-train then installs its own file logger.
        if stdout_logging {
            tracing_subscriber::registry()
                .with(env_filter)
                .with(tracing_subscriber::fmt::layer().json())
                .init();
        }
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

    let tracer = tracer_provider.tracer("ai-pipeline");
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
        .with(tracing_opentelemetry::layer().with_tracer(tracer))
        .with(OpenTelemetryTracingBridge::new(&logger_provider))
        .init();

    Some(OtelProviders {
        _tracer: tracer_provider,
        _logger: logger_provider,
    })
}
