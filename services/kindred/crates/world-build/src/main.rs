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
use world_build::bench::data::Users;
use world_build::config::{self, Acceptance, Aspects, Config, SeriesOverrides};
use world_build::{bench, build, fixture, publish, refresh};

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
    /// aspects.toml to use instead of the built-in one.
    #[arg(long, global = true)]
    aspects: Option<PathBuf>,
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
    /// The benchmark (04-benchmark.md) on the scratch volume of a finished build: the report on
    /// the test users (printed as Markdown; aggregates only), or the hyperparameter search on the
    /// tuning users (printed as world.toml's [model] section).
    Bench {
        #[arg(long, default_value = "/scratch")]
        scratch: PathBuf,
        #[arg(long, value_enum, default_value_t = Users::Test)]
        users: Users,
        /// With `--users tuning`: the full report instead of the search (04-benchmark.md §3,
        /// "report both sets of numbers").
        #[arg(long)]
        report: bool,
        /// With `--users tuning`: P, FULL and FULL without comparisons at n = 20 on 2,000 users,
        /// with the prior refitted (minutes; for development).
        #[arg(long, conflicts_with = "report")]
        quick: bool,
        /// With `--quick`: override a `[model]` setting, `name=value` (repeatable).
        #[arg(long = "set", requires = "quick", value_name = "NAME=VALUE")]
        set: Vec<String>,
        /// With `--users tuning`: search only these settings (comma-separated), from world.toml's
        /// values for the rest.
        #[arg(long, value_delimiter = ',', conflicts_with_all = ["report", "quick"])]
        only: Vec<String>,
        /// Where the per-reader work (replays, fits, ranking) runs: the CPU, or batched across
        /// users on an NVIDIA GPU (needs a build with `--features gpu`).
        #[arg(long, value_enum, default_value_t = bench::Compute::Cpu)]
        device: bench::Compute,
    },
    /// Check the text embedding against the reference embeddings (the image runs this when it
    /// is built).
    EmbedCheck,
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
    let aspects: Aspects =
        config::read(cli.aspects.as_deref(), config::ASPECTS_TOML, "aspects.toml")?;
    aspects.check()?;
    match cli.command {
        Command::Build {
            scratch,
            no_publish,
        } => {
            let settings = build::Settings {
                config,
                overrides,
                aspects,
                fixture: false,
            };
            build::run(&scratch, &settings, &acceptance, !no_publish).await
        }
        Command::Bench {
            scratch,
            users,
            report,
            quick,
            set,
            only,
            device,
        } => {
            let mut config = config;
            config.model = override_model(&config.model, &set)?;
            let paths = build::Paths::new(&scratch);
            let report = world_build::trace::spawn_blocking("world.bench", move || {
                let duck = world_build::trace::Duck::open(
                    &paths.database,
                    &config.duckdb.memory_limit,
                    config.duckdb.threads,
                    &paths.duckdb_temp,
                )?;
                bench::run_full(
                    &duck,
                    &config,
                    aspects.aspect.len(),
                    &paths.model,
                    users,
                    report,
                    quick,
                    &only,
                    device,
                )
            })
            .await?;
            // The report is aggregates only (PLAN.md rule 13); it goes to the terminal.
            println!("{report}");
            Ok(())
        }
        Command::Fixture { out, fixtures } => {
            let manifest =
                fixture::build(&fixtures, &out, &config, &overrides, &aspects, &acceptance).await?;
            tracing::info!(files = manifest.files.len(), "fixture world built");
            Ok(())
        }
        Command::EmbedCheck => {
            let worst = world_build::embed::check(&config.embed)?;
            tracing::info!(
                worst = f64::from(worst),
                "the embedding matches the reference"
            );
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

/// `[model]` with `name=value` overrides (TOML values); unknown names are refused.
fn override_model(model: &taste::Hyper, set: &[String]) -> anyhow::Result<taste::Hyper> {
    let mut table = toml::Table::try_from(model)?;
    for item in set {
        let (name, value) = item
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("--set {item}: expected name=value"))?;
        let parsed: toml::Table = toml::from_str(&format!("v = {value}"))?;
        table.insert(name.trim().to_string(), parsed["v"].clone());
    }
    Ok(table.try_into()?)
}
