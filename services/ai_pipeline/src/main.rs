mod datasets;
mod helper;
pub mod model;
mod roll;
mod serve;
mod trace;
mod unit_trace;

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
/// File name of the head weights exported by `tools/train_head_torch.py --all`.
const HEAD_FILE: &str = "dice_head_resnet18.safetensors";
/// Local-dev default for DICE_HEAD_PATH (the Docker image sets it explicitly).
const DEV_HEAD_PATH: &str = "./runs/head_torch/resnet18_final";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = env::args().collect();

    // Only the serve path logs to stdout. The `folder`/`eval` paths hand stdout
    // to burn-train's ratatui TUI dashboard; installing a stdout subscriber
    // there interleaves JSON log lines (incl. cubecl autotune `log::` output
    // bridged in via LogTracer) into the alternate screen and garbles the TUI.
    // When we skip it, burn-train installs its own file logger
    // (experiment.log) instead and the dashboard stays clean.
    let is_serve = args.contains(&String::from("yolo"));
    let otel = setup_tracing(is_serve).await;

    let result = run(args).await;
    // Whatever was recorded last (a shutdown's `session close` spans) is still in the
    // batch exporters' queues: send it before the process exits.
    if let Some(otel) = otel {
        otel.shutdown();
    }
    result
}

/// Resolves on SIGTERM (Kubernetes stopping the pod) or Ctrl-C.
async fn shutdown_signal() {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("failed to install the SIGTERM handler");
    tokio::select! {
        _ = terminate.recv() => tracing::info!("received SIGTERM"),
        _ = tokio::signal::ctrl_c() => tracing::info!("received Ctrl-C"),
    }
}

async fn run(args: Vec<String>) -> anyhow::Result<()> {
    // f32, not bf16: the head is ~0.9M params on 64x64 crops, so f32 costs
    // little, while bf16 training diverged to NaN by epoch 3 (experiment_37).
    type MyBackend = Autodiff<Cuda<f32>>;

    if args.contains(&String::from("yolo")) {
        // WebSocket inference server: browser camera → YOLO → ResNet18 head → frame + roll JSON.
        // Default address can be overridden: cargo r --release -- yolo 0.0.0.0:9001
        let addr = args
            .iter()
            .skip(2)
            .find(|a| a.contains(':'))
            .map(String::as_str)
            .unwrap_or("0.0.0.0:9000");

        // DICE_HEAD_PATH: the safetensors file, or a directory containing it.
        let head_path = std::path::PathBuf::from(
            std::env::var("DICE_HEAD_PATH").unwrap_or_else(|_| DEV_HEAD_PATH.to_string()),
        );
        let head_path = if head_path.is_dir() { head_path.join(HEAD_FILE) } else { head_path };
        let dice_threshold = match std::env::var("DICE_CONF_THRESHOLD") {
            Ok(v) => v.parse().map_err(|e| anyhow::anyhow!("DICE_CONF_THRESHOLD={v}: {e}"))?,
            Err(_) => model::inferance::DEFAULT_DICE_THRESHOLD,
        };
        tracing::info!(path = %head_path.display(), dice_threshold, "loading model weights");

        serve::serve(addr, head_path, dice_threshold, shutdown_signal()).await?;
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

// The global tracer keeps a clone of the provider, so dropping these flushes nothing:
// `shutdown` does.
struct OtelProviders {
    tracer: opentelemetry_sdk::trace::SdkTracerProvider,
    logger: opentelemetry_sdk::logs::SdkLoggerProvider,
}

impl OtelProviders {
    /// Flushes and stops the exporters: spans first, then the logs (which include
    /// anything the span export logged). Each waits at most the SDK's 5 s.
    fn shutdown(self) {
        if let Err(e) = self.tracer.shutdown() {
            tracing::warn!(error = %e, "tracer shutdown failed; the last spans may be lost");
        }
        if let Err(e) = self.logger.shutdown() {
            eprintln!("logger shutdown failed; the last log records may be lost: {e}");
        }
    }
}

async fn setup_tracing(stdout_logging: bool) -> Option<OtelProviders> {
    use opentelemetry::KeyValue;
    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
    use opentelemetry_otlp::WithExportConfig;
    use opentelemetry_sdk::{Resource, logs::SdkLoggerProvider, trace::SdkTracerProvider};
    use tracing_subscriber::{
        EnvFilter, Layer as _, filter::filter_fn, layer::SubscriberExt, util::SubscriberInitExt,
    };

    // `service.version` is the commit the image was built from (Dockerfile `GIT_SHA`);
    // `OTEL_RESOURCE_ATTRIBUTES` adds the rest (`deployment.environment.name`).
    let resource = Resource::builder()
        .with_service_name("ai-pipeline")
        .with_attribute(KeyValue::new(
            "service.version",
            std::env::var("GIT_SHA").unwrap_or_else(|_| "unknown".into()),
        ))
        .build();
    // The SDK's own logs only at `warn` (e.g. dropped spans).
    let env_filter = EnvFilter::new(
        std::env::var("RUST_LOG").unwrap_or_else(|_| "info,opentelemetry=warn".into()),
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
        .with_batch_exporter(span_exporter)
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
        .with_batch_exporter(log_exporter)
        .with_resource(resource)
        .build();

    tracing_subscriber::registry()
        .with(env_filter)
        // The bridge leaves out the SDK's own warnings, so they go to stdout instead.
        .with(stdout_logging.then(|| {
            tracing_subscriber::fmt::layer()
                .json()
                .with_filter(filter_fn(|meta| meta.target().starts_with("opentelemetry")))
        }))
        .with(tracing_opentelemetry::layer().with_tracer(tracer))
        // Log records take trace_id/span_id from the OTel context tracing-opentelemetry
        // activates with each span. The SDK's own logs stay out: exporting them would log more.
        .with(
            OpenTelemetryTracingBridge::new(&logger_provider)
                .with_filter(filter_fn(|meta| !meta.target().starts_with("opentelemetry"))),
        )
        .init();

    Some(OtelProviders {
        tracer: tracer_provider,
        logger: logger_provider,
    })
}
