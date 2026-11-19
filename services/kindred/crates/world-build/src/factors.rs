//! Stage factors: the collaborative block (03-engine.md §2). Implicit-feedback matrix
//! factorization (`discorec`, conjugate-gradient ALS) on the liked pairs of `train_likes`, and
//! once more on `train_likes_cold` for the benchmark's cold-book test (04-benchmark.md §4).
//! Writes each work's raw factor vector (not yet unit length: the text map is fitted on raw
//! factors, 03-engine.md §3) to `model/factors.bin` and `model/factors-cold.bin`.

use std::path::{Path, PathBuf};

use anyhow::Context as _;

use crate::matrix::Matrix;
use crate::stages::Run;
use crate::trace::Duck;

pub fn path(model: &Path, cold: bool) -> PathBuf {
    model.join(if cold {
        "factors-cold.bin"
    } else {
        "factors.bin"
    })
}

pub fn run(run: &Run) -> anyhow::Result<()> {
    for (table, cold) in [("train_likes", false), ("train_likes_cold", true)] {
        let factors = fit(
            run.duck,
            table,
            run.config.factors.k,
            run.config.factors.iterations,
            run.config.interactions.seed,
        )
        .with_context(|| format!("factorizing {table}"))?;
        factors.write(&path(&run.model, cold))?;
    }
    Ok(())
}

/// Factorizes one table of (user, work) liked pairs. Pairs are streamed from DuckDB into the
/// factorization, which keeps them once per side.
#[tracing::instrument(name = "factors.fit", skip(duck), fields(pairs, works))]
pub fn fit(
    duck: &Duck,
    table: &str,
    k: usize,
    iterations: u32,
    seed: u64,
) -> anyhow::Result<Matrix> {
    let span = tracing::Span::current();
    let pairs: i64 = duck
        .conn()
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?;
    span.record("pairs", pairs);
    let mut statement = duck.conn().prepare(&format!(
        "SELECT user, work FROM {table} ORDER BY user, work"
    ))?;
    let mut failure = None;
    let rows = statement
        .query_map([], |r| {
            Ok((r.get::<_, i32>(0)? as u32, r.get::<_, i64>(1)? as u64))
        })?
        .map_while(|r| match r {
            Ok((user, work)) => Some((user, work, 1.0f32)),
            Err(e) => {
                failure = Some(e);
                None
            }
        });
    let recommender = discorec::RecommenderBuilder::new()
        .factors(k as u32)
        .iterations(iterations)
        .seed(seed)
        .callback(|info| tracing::info!(iteration = info.iteration, "factorization round done"))
        .fit_implicit(rows);
    if let Some(e) = failure {
        return Err(e.into());
    }
    let mut works: Vec<u64> = recommender.item_ids().to_vec();
    works.sort_unstable();
    span.record("works", works.len());
    let mut out = Matrix::new(k);
    for work in works {
        out.push(
            work,
            recommender
                .item_factors(&work)
                .expect("a factorized work has factors"),
        );
    }
    Ok(out)
}
