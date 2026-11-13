//! kindred-world: the monthly world build, the fixture world, and the one operational action
//! (rollback). See README.md.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use opentelemetry::KeyValue;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_sdk::{Resource, logs::SdkLoggerProvider, trace::SdkTracerProvider};
use tracing_subscriber::{
    EnvFilter, Layer as _, filter::filter_fn, layer::SubscriberExt as _,
    util::SubscriberInitExt as _,
};
use world_build::config::{self, Acceptance, Config, SeriesOverrides};
use world_build::{build, fixture, publish, refresh};

/// The log filter when `RUST_LOG` isn't set: kindred-world's own spans and logs at info,
/// libraries at warn (TRACING.md, "Log level").
const LOG_FILTER: &str = "warn,kindred_world=info,world_build=info,world=info";

#[derive(Parser)]
#[command(name = "kindred-world", about = "Builds and publishes the books world")]
struct Cli {
    /// world.toml to use instead of the built-in one.
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// series-overrides.toml to use instead of the built-in one.
    #[arg(long, global = true)]
    series_overrides: Option<PathBuf>,
    /// search-acceptance.toml to use instead of the built-in one.
    #[arg(long, global = true)]
    acceptance: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Download the inputs, build, verify and publish a new world (the CronJob).
    Build {
        /// The scratch volume: downloads, the DuckDB file and done-markers survive between runs.
        #[arg(long, default_value = "/scratch")]
        scratch: PathBuf,
        /// Build and verify only; leave the bucket alone.
        #[arg(long)]
        no_publish: bool,
    },
    /// Build the fixture world from fixtures/ into an empty directory.
    Fixture {
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value_os_t = fixture::default_dir())]
        fixtures: PathBuf,
    },
    /// Rollback: point current.json at a version still in the bucket.
    Publish {
        #[arg(long)]
        version: String,
    },
    /// List the versions in the bucket and the current one.
    Versions,
    /// Search a world directory and print the hits (developer tool; prints to the terminal,
    /// never to logs).
    Search {
        #[arg(long)]
        world: PathBuf,
        query: String,
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
    /// Download the fixture works' Open Library records again (developer tool).
    FixtureRefresh {
        #[arg(long, default_value_os_t = fixture::default_dir())]
        fixtures: PathBuf,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let providers = match telemetry() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("telemetry: {e}");
            return ExitCode::FAILURE;
        }
    };
    let result = run(cli).await;
    if let Err(e) = &result {
        tracing::error!(
            error = world_build::trace::loggable(e),
            "kindred-world failed"
        );
    }
    // Flush spans and logs before exit; the tracer first, the logger still carries its failure.
    if let Some((spans, logs)) = providers {
        if let Err(e) = spans.shutdown() {
            tracing::error!(error = %e, "flushing spans failed");
        }
        let _ = logs.shutdown();
    }
    if result.is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

async fn run(cli: Cli) -> anyhow::Result<()> {
    let config: Config = config::read(cli.config.as_deref(), config::WORLD_TOML, "world.toml")?;
    let overrides: SeriesOverrides = config::read(
        cli.series_overrides.as_deref(),
        config::SERIES_OVERRIDES_TOML,
        "series-overrides.toml",
    )?;
    let acceptance: Acceptance = config::read(
        cli.acceptance.as_deref(),
        config::ACCEPTANCE_TOML,
        "search-acceptance.toml",
    )?;
    match cli.command {
        Command::Build {
            scratch,
            no_publish,
        } => build::run(&scratch, &config, &overrides, &acceptance, !no_publish).await,
        Command::Fixture { out, fixtures } => {
            let manifest =
                fixture::build(&fixtures, &out, &config, &overrides, &acceptance).await?;
            tracing::info!(files = manifest.files.len(), "fixture world built");
            Ok(())
        }
        Command::Publish { version } => {
            let bucket = build::bucket(&config)?;
            publish::rollback(&bucket, &version).await?;
            tracing::info!(version, "current.json now points at this version");
            Ok(())
        }
        Command::Versions => {
            let bucket = build::bucket(&config)?;
            let current = publish::current(&bucket).await?;
            for version in publish::versions(&bucket).await? {
                let mark = if current.as_ref().is_some_and(|c| c.version == version) {
                    " (current)"
                } else {
                    ""
                };
                println!("{version}{mark}");
            }
            Ok(())
        }
        Command::Search {
            world,
            query,
            limit,
        } => {
            let world = world::World::open(&world)?;
            for hit in world.search(&query, limit)? {
                let series = hit
                    .series
                    .map(|s| format!(" [{s} {:?}]", hit.series_position))
                    .unwrap_or_default();
                println!(
                    "{:7.2}  {}  {} — {}{series}",
                    hit.score,
                    hit.key,
                    hit.title,
                    hit.authors.join(", ")
                );
            }
            Ok(())
        }
        Command::FixtureRefresh { fixtures } => {
            refresh::refresh(&fixtures, &config.sources.user_agent).await
        }
    }
}

type Providers = Option<(SdkTracerProvider, SdkLoggerProvider)>;

/// JSON logs on stdout; spans and logs over OTLP when `OTEL_EXPORTER_OTLP_ENDPOINT` is set.
fn telemetry() -> anyhow::Result<Providers> {
    let filter = || EnvFilter::new(std::env::var("RUST_LOG").unwrap_or_else(|_| LOG_FILTER.into()));
    let resource = || {
        Resource::builder()
            .with_service_name("kindred-world")
            .with_attribute(KeyValue::new("service.version", build::builder()))
            .build()
    };
    let Ok(endpoint) = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT") else {
        tracing_subscriber::registry()
            .with(filter())
            .with(tracing_subscriber::fmt::layer().json())
            .try_init()?;
        return Ok(None);
    };
    use opentelemetry_otlp::WithExportConfig as _;
    let spans = SdkTracerProvider::builder()
        .with_batch_exporter(
            opentelemetry_otlp::SpanExporter::builder()
                .with_tonic()
                .with_endpoint(endpoint.clone())
                .build()?,
        )
        .with_resource(resource())
        .build();
    let tracer = spans.tracer("kindred-world");
    opentelemetry::global::set_tracer_provider(spans.clone());
    let logs = SdkLoggerProvider::builder()
        .with_batch_exporter(
            opentelemetry_otlp::LogExporter::builder()
                .with_tonic()
                .with_endpoint(endpoint)
                .build()?,
        )
        .with_resource(resource())
        .build();
    // The SDK's own logs stay out of the bridge: exporting them would log more.
    let bridge = OpenTelemetryTracingBridge::new(&logs)
        .with_filter(filter_fn(|m| !m.target().starts_with("opentelemetry")));
    tracing_subscriber::registry()
        .with(filter())
        .with(tracing_subscriber::fmt::layer().json())
        .with(tracing_opentelemetry::layer().with_tracer(tracer))
        .with(bridge)
        .try_init()?;
    Ok(Some((spans, logs)))
}
