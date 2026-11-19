//! Stage text-maps (03-engine.md §3, 02-world-data.md §8): ridge regressions from a work's text
//! embedding to its collaborative block (raw factors, before unit length) and to its aspect
//! block, fitted on works that have both, and applied to every work with an embedding. Each
//! ridge strength is chosen on a held-out tenth of the training works.
//!
//! Twice: for the world (`normal`) and for the benchmark's cold-book test (`cold`: works first
//! published from `factors.cold_from_year` on are neither in the factorization nor trained on,
//! and their aspects count as missing). Writes `model/aspects.bin` (the shelf-and-subject
//! aspects) and, per variant, `model/c-text[-cold].bin` and `model/a-text[-cold].bin`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use nalgebra::{DMatrix, DVector};
use serde::Serialize;

use crate::matrix::Matrix;
use crate::stages::Run;
use crate::{embed, factors};

pub fn aspects_path(model: &Path) -> PathBuf {
    model.join("aspects.bin")
}

pub fn c_text_path(model: &Path, cold: bool) -> PathBuf {
    model.join(if cold { "c-text-cold.bin" } else { "c-text.bin" })
}

pub fn a_text_path(model: &Path, cold: bool) -> PathBuf {
    model.join(if cold { "a-text-cold.bin" } else { "a-text.bin" })
}

pub fn run(run: &Run) -> anyhow::Result<()> {
    let aspects = read_aspects(run)?;
    aspects.write(&aspects_path(&run.model))?;
    let embeddings = Matrix::read(&embed::path(&run.model))?;
    let cold = cold_works(run)?;
    let mut quality = serde_json::Map::new();
    for variant in [false, true] {
        let raw_c = Matrix::read(&factors::path(&run.model, variant))?;
        let excluded = |key: u64| variant && cold.contains(&key);
        let (c_map, c_fit) = fit(&embeddings, &raw_c, &excluded, &run.config.text_maps.ridge)?;
        let (a_map, a_fit) = fit(&embeddings, &aspects, &excluded, &run.config.text_maps.ridge)?;
        c_map.apply(&embeddings).write(&c_text_path(&run.model, variant))?;
        let mut a = a_map.apply(&embeddings);
        a.data.iter_mut().for_each(|x| *x = x.clamp(-3.0, 3.0));
        a.write(&a_text_path(&run.model, variant))?;
        let name = if variant { "cold" } else { "normal" };
        tracing::info!(variant = name, c = ?c_fit, a = ?a_fit, "text maps fitted");
        quality.insert(
            name.into(),
            serde_json::json!({"collaborative": c_fit, "aspects": a_fit}),
        );
    }
    std::fs::write(
        run.model.join("text-maps.json"),
        serde_json::to_vec_pretty(&quality)?,
    )?;
    Ok(())
}

/// The shelf-and-subject aspect numbers of `work_aspects`, one row per work.
fn read_aspects(run: &Run) -> anyhow::Result<Matrix> {
    let a = run.aspects.aspect.len();
    let mut out = Matrix::new(a);
    let mut statement = run
        .duck
        .conn()
        .prepare("SELECT work, k, a FROM work_aspects ORDER BY work, k")?;
    let mut rows = statement.query([])?;
    let mut current: Option<u64> = None;
    let mut row = vec![0f32; a];
    while let Some(r) = rows.next()? {
        let work = r.get::<_, i64>(0)? as u64;
        if current != Some(work) {
            if let Some(done) = current {
                out.push(done, &row);
            }
            current = Some(work);
            row.fill(0.0);
        }
        row[r.get::<_, i32>(1)? as usize] = r.get(2)?;
    }
    if let Some(done) = current {
        out.push(done, &row);
    }
    Ok(out)
}

/// Works first published from `factors.cold_from_year` on (04-benchmark.md §4).
pub fn cold_works(run: &Run) -> anyhow::Result<HashSet<u64>> {
    let mut statement = run
        .duck
        .conn()
        .prepare("SELECT work FROM catalogue WHERE year >= ?")?;
    let rows = statement.query_map([run.config.factors.cold_from_year], |r| {
        r.get::<_, i64>(0)
    })?;
    Ok(rows.map(|r| r.map(|k| k as u64)).collect::<Result<_, _>>()?)
}

/// How well a map predicts on its held-out tenth.
#[derive(Clone, Debug, Serialize)]
pub struct FitQuality {
    pub trained_on: usize,
    pub ridge: f64,
    /// 1 - residual / total sum of squares on the held-out works.
    pub held_out_r2: f64,
}

/// `y ≈ (x - mean_x) w + mean_y`.
pub struct RidgeMap {
    mean_x: DVector<f64>,
    mean_y: DVector<f64>,
    w: DMatrix<f64>,
}

impl RidgeMap {
    pub fn predict(&self, x: &[f32]) -> Vec<f32> {
        let x = DVector::from_iterator(x.len(), x.iter().map(|&v| f64::from(v))) - &self.mean_x;
        let y = self.w.tr_mul(&x) + &self.mean_y;
        y.iter().map(|&v| v as f32).collect()
    }

    /// The prediction for every row of `x`.
    pub fn apply(&self, x: &Matrix) -> Matrix {
        let mut out = Matrix::new(self.mean_y.len());
        for (i, &key) in x.keys.iter().enumerate() {
            out.push(key, &self.predict(x.row(i)));
        }
        out
    }
}

/// Sums for one ridge fit: x'x, x'y, sums of x and y, n.
struct Sums {
    xx: DMatrix<f64>,
    xy: DMatrix<f64>,
    sx: DVector<f64>,
    sy: DVector<f64>,
    n: f64,
}

impl Sums {
    fn new(p: usize, q: usize) -> Sums {
        Sums {
            xx: DMatrix::zeros(p, p),
            xy: DMatrix::zeros(p, q),
            sx: DVector::zeros(p),
            sy: DVector::zeros(q),
            n: 0.0,
        }
    }

    /// Adds rows in blocks, so the products run as matrix products.
    fn add(&mut self, rows: &[(&[f32], &[f32])]) {
        const BLOCK: usize = 2048;
        let (p, q) = (self.sx.len(), self.sy.len());
        for block in rows.chunks(BLOCK) {
            let x = DMatrix::from_fn(block.len(), p, |i, j| f64::from(block[i].0[j]));
            let y = DMatrix::from_fn(block.len(), q, |i, j| f64::from(block[i].1[j]));
            self.xx.gemm_tr(1.0, &x, &x, 1.0);
            self.xy.gemm_tr(1.0, &x, &y, 1.0);
            self.sx += x.row_sum().transpose();
            self.sy += y.row_sum().transpose();
            self.n += block.len() as f64;
        }
    }

    fn plus(&self, other: &Sums) -> Sums {
        Sums {
            xx: &self.xx + &other.xx,
            xy: &self.xy + &other.xy,
            sx: &self.sx + &other.sx,
            sy: &self.sy + &other.sy,
            n: self.n + other.n,
        }
    }

    fn solve(&self, ridge: f64) -> anyhow::Result<RidgeMap> {
        anyhow::ensure!(self.n >= 2.0, "too few works to fit a text map");
        let mean_x = &self.sx / self.n;
        let mean_y = &self.sy / self.n;
        // Centred: x'x - n mx mx', x'y - n mx my'.
        let mut a = &self.xx - (&mean_x * mean_x.transpose()) * self.n;
        let b = &self.xy - (&mean_x * mean_y.transpose()) * self.n;
        for i in 0..a.nrows() {
            a[(i, i)] += ridge;
        }
        let w = a
            .cholesky()
            .ok_or_else(|| anyhow::anyhow!("the ridge system is not positive definite"))?
            .solve(&b);
        Ok(RidgeMap { mean_x, mean_y, w })
    }
}

/// Held-out tenth: a work's key decides, so it is the same every run.
fn held_out(key: u64) -> bool {
    key.wrapping_mul(0x9E37_79B9_7F4A_7C15).is_multiple_of(10)
}

/// Fits `x → y` on the works that have both (and are not excluded); the ridge strength with the
/// smallest held-out error, then refitted on all of them.
pub fn fit(
    x: &Matrix,
    y: &Matrix,
    excluded: &dyn Fn(u64) -> bool,
    ridges: &[f64],
) -> anyhow::Result<(RidgeMap, FitQuality)> {
    let (mut train, mut test) = (Vec::new(), Vec::new());
    for (i, &key) in x.keys.iter().enumerate() {
        if excluded(key) {
            continue;
        }
        if let Some(target) = y.get(key) {
            let pair = (x.row(i), target);
            if held_out(key) {
                test.push(pair);
            } else {
                train.push(pair);
            }
        }
    }
    let mut train_sums = Sums::new(x.dim, y.dim);
    train_sums.add(&train);
    let mut best: Option<(f64, f64)> = None;
    let mean_test: Vec<f64> = (0..y.dim)
        .map(|j| test.iter().map(|t| f64::from(t.1[j])).sum::<f64>() / test.len().max(1) as f64)
        .collect();
    let total: f64 = test
        .iter()
        .map(|t| {
            t.1.iter()
                .zip(&mean_test)
                .map(|(&v, m)| (f64::from(v) - m).powi(2))
                .sum::<f64>()
        })
        .sum();
    for &ridge in ridges {
        let map = train_sums.solve(ridge)?;
        let residual: f64 = test
            .iter()
            .map(|t| {
                map.predict(t.0)
                    .iter()
                    .zip(t.1)
                    .map(|(&p, &v)| f64::from(p - v).powi(2))
                    .sum::<f64>()
            })
            .sum();
        if best.is_none_or(|(_, r)| residual < r) {
            best = Some((ridge, residual));
        }
    }
    let (ridge, residual) = best.ok_or_else(|| anyhow::anyhow!("no ridge strengths configured"))?;
    let mut test_sums = Sums::new(x.dim, y.dim);
    test_sums.add(&test);
    let map = train_sums.plus(&test_sums).solve(ridge)?;
    Ok((
        map,
        FitQuality {
            trained_on: train.len() + test.len(),
            ridge,
            held_out_r2: if total > 0.0 { 1.0 - residual / total } else { 0.0 },
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::{Rng as _, SeedableRng as _};

    #[test]
    fn recovers_a_linear_map() {
        let mut rng = rand_chacha::ChaCha8Rng::seed_from_u64(1);
        let (p, q) = (8, 3);
        let truth: Vec<f32> = (0..p * q).map(|_| rng.random_range(-1.0..1.0)).collect();
        let (mut x, mut y) = (Matrix::new(p), Matrix::new(q));
        for key in 1..2000u64 {
            let xi: Vec<f32> = (0..p).map(|_| rng.random_range(-1.0..1.0)).collect();
            let yi: Vec<f32> = (0..q)
                .map(|j| 0.5 + (0..p).map(|i| xi[i] * truth[i * q + j]).sum::<f32>()
                    + rng.random_range(-0.01..0.01))
                .collect();
            x.push(key, &xi);
            y.push(key, &yi);
        }
        let (map, quality) = fit(&x, &y, &|_| false, &[0.01, 1.0, 1000.0]).unwrap();
        assert!(quality.held_out_r2 > 0.99, "{quality:?}");
        assert!(quality.ridge < 1000.0);
        let got = map.predict(x.row(5));
        for (g, w) in got.iter().zip(y.row(5)) {
            assert!((g - w).abs() < 0.05);
        }
        let (_, none) = fit(&x, &y, &|k| k > 10, &[1.0]).unwrap();
        assert_eq!(none.trained_on, 10);
    }

    #[test]
    fn about_a_tenth_is_held_out() {
        let n = (0..100_000u64).filter(|&k| held_out(k)).count();
        assert!((8_000..14_000).contains(&n), "{n}");
    }
}
