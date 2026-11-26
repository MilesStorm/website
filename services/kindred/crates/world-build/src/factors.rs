//! Stage factors: the collaborative block and the star block (03-engine.md §2).
//!
//! - Implicit-feedback ALS ([`crate::als::Implicit`]) on the liked pairs of `train_likes`, and
//!   once more on `train_likes_cold` for the benchmark's cold-book test (04-benchmark.md §4).
//!   Writes each work's raw factor vector (not yet unit length: the text map is fitted on raw
//!   factors, 03-engine.md §3) to `model/factors.bin` and `model/factors-cold.bin`.
//! - Explicit biased ALS ([`crate::als::Explicit`]) on the star ratings of the works of the same
//!   table, by users who are in neither benchmark role (no test or tuning user: the tuning scores
//!   must not have seen their own stars) and have at least `[factors] min_user_likes` of them.
//!   Writes the works' star factors to `model/stars.bin` and their biases to
//!   `model/stars-bias.bin` (`-cold` for the cold variant).

use std::path::{Path, PathBuf};

use anyhow::Context as _;

use crate::als::{Cells, Explicit, Implicit, Model};
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

/// The works' star factors (`[factors] k_stars` numbers each).
pub fn stars_path(model: &Path, cold: bool) -> PathBuf {
    model.join(if cold { "stars-cold.bin" } else { "stars.bin" })
}

/// The works' star biases `b_i` (one number each).
pub fn stars_bias_path(model: &Path, cold: bool) -> PathBuf {
    model.join(if cold {
        "stars-bias-cold.bin"
    } else {
        "stars-bias.bin"
    })
}

pub fn run(run: &Run) -> anyhow::Result<()> {
    let f = &run.config.factors;
    let seed = run.config.interactions.seed;
    for (table, cold) in [("train_likes", false), ("train_likes_cold", true)] {
        let likes = cells(
            run.duck,
            &format!("SELECT user, work, 1.0::FLOAT FROM {table} ORDER BY user, work"),
        )
        .with_context(|| format!("reading {table}"))?;
        tracing::info!(
            table,
            pairs = likes.len(),
            works = likes.items.len(),
            "liked pairs read"
        );
        let model = Implicit::new(f.k, f.iterations, seed).fit(&likes);
        items(&likes, &model, f.k, |m, i| m.item_row(i)).write(&path(&run.model, cold))?;
        drop(model);
        drop(likes);

        let stars = cells(
            run.duck,
            &format!(
                "WITH rated AS (
                     SELECT r.user, r.work, r.rating FROM ratings r
                     WHERE r.work IN (SELECT DISTINCT work FROM {table})
                       AND r.user NOT IN (SELECT user FROM bench_users)
                 )
                 SELECT user, work, rating::FLOAT FROM rated
                 WHERE user IN (SELECT user FROM rated GROUP BY user HAVING count(*) >= {})
                 ORDER BY user, work",
                f.min_user_likes
            ),
        )
        .with_context(|| format!("reading the star ratings on the works of {table}"))?;
        tracing::info!(
            table,
            ratings = stars.len(),
            works = stars.items.len(),
            "star ratings read"
        );
        anyhow::ensure!(
            f.stars_regularisation > 0.0 && f.stars_prior_count > 0.0,
            "[factors] stars_regularisation and stars_prior_count must be positive"
        );
        let model = Explicit {
            k: f.k_stars,
            iterations: f.stars_iterations,
            regularisation: f.stars_regularisation,
            prior_count: f.stars_prior_count,
            seed,
        }
        .fit(&stars);
        tracing::info!(table, mean = model.mean, "star factorization done");
        items(&stars, &model, f.k_stars, |m, i| m.item_row(i))
            .write(&stars_path(&run.model, cold))?;
        items(&stars, &model, 1, |m, i| {
            std::slice::from_ref(&m.item_bias[i])
        })
        .write(&stars_bias_path(&run.model, cold))?;
    }
    Ok(())
}

/// Reads (user, work, value) cells, ordered by user, from one query.
#[tracing::instrument(name = "factors.cells", skip(duck))]
fn cells(duck: &Duck, query: &str) -> anyhow::Result<Cells> {
    let mut statement = duck.conn().prepare(query)?;
    let mut failure = None;
    let rows = statement
        .query_map([], |r| {
            Ok((
                r.get::<_, i32>(0)? as u64,
                r.get::<_, i64>(1)? as u64,
                r.get::<_, f32>(2)?,
            ))
        })?
        .map_while(|r| match r {
            Ok(cell) => Some(cell),
            Err(e) => {
                failure = Some(e);
                None
            }
        });
    let cells = Cells::from_user_order(rows);
    if let Some(e) = failure {
        return Err(e.into());
    }
    Ok(cells)
}

/// One row per work of `cells` (keys ascending), from the fitted model.
fn items(
    cells: &Cells,
    model: &Model,
    dim: usize,
    row: impl Fn(&Model, usize) -> &[f32],
) -> Matrix {
    let mut out = Matrix::new(dim);
    for (i, &work) in cells.items.iter().enumerate() {
        out.push(work, row(model, i));
    }
    out
}
