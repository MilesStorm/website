//! The monthly build: download → SQL stages → search-index → pack → verify → publish
//! (02-world-data.md §3, §7). The scratch volume keeps downloads, the DuckDB file and the
//! stages' done-markers between runs.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Context as _;
use world::{InputFile, Manifest, World};

use crate::config::{Acceptance, Config, SeriesOverrides};
use crate::download::{self, Source};
use crate::trace::{self, Bucket, Duck, Peer};
use crate::{pack, publish, report, stages, verify};

/// The commit media-world was built from (the image's `GIT_SHA`).
pub fn builder() -> String {
    std::env::var("GIT_SHA").unwrap_or_else(|_| "unknown".into())
}

pub struct Paths {
    pub downloads: PathBuf,
    pub markers: PathBuf,
    pub database: PathBuf,
    pub duckdb_temp: PathBuf,
    pub world: PathBuf,
}

impl Paths {
    pub fn new(scratch: &Path) -> Paths {
        Paths {
            downloads: scratch.join("downloads"),
            markers: scratch.join("markers"),
            database: scratch.join("world.duckdb"),
            duckdb_temp: scratch.join("duckdb-tmp"),
            world: scratch.join("world"),
        }
    }
}

/// The stages from ingest to pack: the world in `out`, with its manifest.
pub async fn assemble(
    scratch: &Path,
    out: &Path,
    config: &Config,
    overrides: &SeriesOverrides,
    sources: &[Source],
    inputs: &[InputFile],
) -> anyhow::Result<Manifest> {
    let paths = Paths::new(scratch);
    let (config, overrides, sources, inputs, out) = (
        config.clone(),
        overrides.clone(),
        sources.to_vec(),
        inputs.to_vec(),
        out.to_owned(),
    );
    trace::spawn_blocking("world.assemble", move || {
        std::fs::create_dir_all(&paths.duckdb_temp)?;
        let duck = Duck::open(
            &paths.database,
            &config.duckdb.memory_limit,
            config.duckdb.threads,
            &paths.duckdb_temp,
        )?;
        let run = stages::Run {
            duck: &duck,
            config: &config,
            overrides: &overrides,
            downloads: &paths.downloads,
            markers: paths.markers.clone(),
            sources: &sources,
            inputs: &inputs,
        };
        let mut durations = run.all()?;

        if out.exists() {
            std::fs::remove_dir_all(&out)?;
        }
        let started = Instant::now();
        let writer = pack::write(&duck, &out)?;
        durations.push(("pack".into(), Some(started.elapsed().as_secs_f64())));
        let report = report::compute(&duck, &durations)?;
        let built_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let manifest =
            writer.finish(built_at, builder(), inputs, config.search.params(), report)?;
        // One log line with every number (no titles, PLAN.md rule 14).
        tracing::info!(report = %manifest.report, "build report");
        Ok(manifest)
    })
    .await
}

/// Reads the world back through crate `world` and checks every gate.
pub async fn verify(
    dir: &Path,
    config: &Config,
    acceptance: &Acceptance,
    previous: Option<serde_json::Value>,
) -> anyhow::Result<()> {
    let (dir, config, acceptance) = (dir.to_owned(), config.clone(), acceptance.clone());
    trace::spawn_blocking("world.verify", move || {
        let world = World::open(&dir).context("the new world doesn't read back")?;
        verify::verify(&world, &config, &acceptance, previous.as_ref())
    })
    .await
}

/// The Garage key from the environment (Secret media-world-secrets).
pub fn bucket(config: &Config) -> anyhow::Result<Bucket> {
    let var = |name: &str| std::env::var(name).with_context(|| format!("{name} is not set"));
    Bucket::garage(
        &config.publish.endpoint,
        &config.publish.region,
        &config.publish.bucket,
        &var("S3_ACCESS_KEY_ID")?,
        &var("S3_SECRET_ACCESS_KEY")?,
    )
}

/// `media-world build`.
#[tracing::instrument(name = "world.build", skip_all)]
pub async fn run(
    scratch: &Path,
    config: &Config,
    overrides: &SeriesOverrides,
    acceptance: &Acceptance,
    publish: bool,
) -> anyhow::Result<()> {
    let paths = Paths::new(scratch);
    // Before the long part: a wrong key fails in seconds, not hours.
    let bucket = if publish { Some(bucket(config)?) } else { None };
    let previous = match &bucket {
        Some(bucket) => publish::current_report(bucket).await?,
        None => None,
    };

    let client = trace::client(
        Peer {
            service: "datasets",
        },
        &config.sources.user_agent,
        std::time::Duration::from_secs(30),
        std::time::Duration::from_secs(300),
    );
    let sources = download::sources(config);
    let inputs = download::fetch_all(&client, &paths.downloads, &sources).await?;

    assemble(scratch, &paths.world, config, overrides, &sources, &inputs).await?;
    verify(&paths.world, config, acceptance, previous).await?;

    match bucket {
        Some(bucket) => {
            let sha = builder();
            let base = format!(
                "{}-{}",
                chrono::Utc::now().format("%Y%m%d"),
                &sha[..sha.len().min(7)]
            );
            let version = publish::publish(&bucket, &paths.world, &base).await?;
            tracing::info!(version, "world published");
        }
        None => {
            tracing::info!(world = %paths.world.display(), "built and verified; not published (--no-publish)")
        }
    }
    Ok(())
}
