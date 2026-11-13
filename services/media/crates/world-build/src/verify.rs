//! Stage verify: the new world, read back through crate `world` (format and checksums), must
//! pass every gate before it may be published (02-world-data.md §7). A failure stops the
//! build; the published pointer does not move.

use std::time::Instant;

use anyhow::bail;
use serde_json::Value;
use world::World;
use world::search::fold;

use crate::config::{Acceptance, AcceptanceCase, Config};
use crate::report::COMPARED;

/// Runs of each acceptance query for the latency measurement.
const TIMING_RUNS: usize = 5;

#[tracing::instrument(name = "verify", skip_all)]
pub fn verify(
    world: &World,
    config: &Config,
    acceptance: &Acceptance,
    previous: Option<&Value>,
) -> anyhow::Result<()> {
    let report = &world.manifest().report;
    let mut failures = Vec::new();

    let share = report["gr_ratings_mapped_share"].as_f64().unwrap_or(0.0);
    if share < config.identity.min_mapped_share {
        failures.push(format!(
            "Goodreads ratings on mapped works: {share:.3}, below {}",
            config.identity.min_mapped_share
        ));
    }
    match previous {
        None => tracing::info!("no previous world: change gates skipped"),
        Some(previous) => {
            for name in COMPARED {
                let (Some(now), Some(before)) = (report[name].as_f64(), previous[name].as_f64())
                else {
                    tracing::warn!(count = name, "not in both reports: change gate skipped");
                    continue;
                };
                let change = if before == 0.0 {
                    f64::INFINITY
                } else {
                    (now - before).abs() / before
                };
                if change > config.verify.max_change {
                    failures.push(format!(
                        "{name}: {before} → {now}, a change of {:.0}%",
                        change * 100.0
                    ));
                }
            }
        }
    }

    let mut timings = Vec::new();
    for (number, case) in acceptance.case.iter().enumerate() {
        let limit = case.top.max(10);
        let mut hits = Vec::new();
        for _ in 0..TIMING_RUNS {
            let started = Instant::now();
            hits = world.search(&case.query, limit)?;
            timings.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        // Named by its number in search-acceptance.toml: search text stays out of logs
        // (PLAN.md rule 14), without exceptions for our own queries.
        if !hits.iter().take(case.top).any(|hit| matches(case, hit)) {
            failures.push(format!(
                "search-acceptance.toml case {}: expected work not in the first {}",
                number + 1,
                case.top
            ));
        }
    }
    timings.sort_by(f64::total_cmp);
    let p95 = timings[(timings.len() * 95).div_ceil(100).saturating_sub(1)];
    tracing::info!(
        p95_ms = p95,
        cases = acceptance.case.len(),
        "search acceptance timed"
    );
    if p95 > config.search.p95_ms as f64 {
        failures.push(format!(
            "search p95 {p95:.1} ms, above {} ms",
            config.search.p95_ms
        ));
    }

    if !failures.is_empty() {
        bail!(
            "the world failed verification:\n- {}",
            failures.join("\n- ")
        );
    }
    tracing::info!(works = world.len(), "the world passed verification");
    Ok(())
}

pub fn matches(case: &AcceptanceCase, hit: &world::Hit) -> bool {
    case.title
        .as_ref()
        .is_none_or(|t| fold(&hit.title) == fold(t))
        && case
            .author
            .as_ref()
            .is_none_or(|a| hit.authors.iter().any(|h| fold(h).contains(&fold(a))))
        && case.series.as_ref().is_none_or(|s| {
            hit.series
                .as_ref()
                .is_some_and(|h| fold(h).contains(&fold(s)))
        })
}
