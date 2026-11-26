//! Alternating least squares for the two factorizations of the factors stage (03-engine.md §2):
//!
//! - [`implicit`]: implicit-feedback ALS (Hu, Koren and Volinsky 2008) on liked pairs, each row
//!   solved by three conjugate-gradient steps from its previous value (Takács, Pilászy and Tikk
//!   2011). Same model and defaults as `discorec` 0.3's `fit_implicit`, which the stage used
//!   before: confidence `1 + 40 · value`, regularisation 0.01 added to `YᵀY`, start values
//!   uniform in `[0, 0.01)`.
//! - [`explicit`]: biased matrix factorization of star ratings, `r_ui − mu = b_u + b_i + p_u · q_i`,
//!   by closed-form alternating least squares with weighted-λ regularisation plus a prior count
//!   n0: a row with n ratings gets ridge n0 on its bias and λ·(n + n0) on its factors, on both
//!   sides (Zhou, Wilkinson, Schreiber and Pan 2008). A row's bias and factors are one
//!   least-squares solve.
//!
//! Deterministic: the start values come from a seeded generator, every row is solved by one
//! thread from fixed inputs, and the sums shared by all rows (`YᵀY`, the mean) are added in a
//! fixed order of fixed-size chunks. The result does not depend on the number of threads.

use nalgebra::{DMatrix, DVector};
use rand::{Rng as _, SeedableRng as _};
use rand_chacha::ChaCha8Rng;
use rayon::prelude::*;

/// Rows in one chunk of a shared sum: fixed, so the order of additions is too.
const CHUNK: usize = 4096;
/// Conjugate-gradient steps per row and round (as `discorec`).
const CG_STEPS: usize = 3;

/// A sparse matrix by rows: row `u` has columns `cols[offsets[u]..offsets[u + 1]]`.
struct Csr {
    offsets: Vec<usize>,
    cols: Vec<u32>,
    vals: Vec<f32>,
}

impl Csr {
    fn row(&self, u: usize) -> (&[u32], &[f32]) {
        let span = self.offsets[u]..self.offsets[u + 1];
        (&self.cols[span.clone()], &self.vals[span])
    }

    fn rows(&self) -> usize {
        self.offsets.len() - 1
    }

    /// The same matrix by columns.
    fn transpose(&self, columns: usize) -> Csr {
        let mut offsets = vec![0usize; columns + 1];
        for &c in &self.cols {
            offsets[c as usize + 1] += 1;
        }
        for i in 0..columns {
            offsets[i + 1] += offsets[i];
        }
        let mut next = offsets.clone();
        let mut cols = vec![0u32; self.cols.len()];
        let mut vals = vec![0f32; self.vals.len()];
        for u in 0..self.rows() {
            let (cs, vs) = self.row(u);
            for (&c, &v) in cs.iter().zip(vs) {
                let at = &mut next[c as usize];
                cols[*at] = u as u32;
                vals[*at] = v;
                *at += 1;
            }
        }
        Csr {
            offsets,
            cols,
            vals,
        }
    }
}

/// Observed (user, work, value) cells, by user and by work.
pub struct Cells {
    /// User keys, ascending: user `u` is `users[u]`.
    pub users: Vec<u64>,
    /// Work keys, ascending.
    pub items: Vec<u64>,
    by_user: Csr,
    by_item: Csr,
}

impl Cells {
    /// From cells ordered by user (works in any order within a user; a repeated cell counts
    /// twice).
    pub fn from_user_order(cells: impl IntoIterator<Item = (u64, u64, f32)>) -> Cells {
        let mut users = Vec::new();
        let mut offsets = vec![0usize];
        let mut keys: Vec<u64> = Vec::new();
        let mut vals = Vec::new();
        for (user, item, value) in cells {
            if users.last() != Some(&user) {
                assert!(
                    users.last().is_none_or(|&last| last < user),
                    "cells ordered by user"
                );
                if !users.is_empty() {
                    offsets.push(keys.len());
                }
                users.push(user);
            }
            keys.push(item);
            vals.push(value);
        }
        if !users.is_empty() {
            offsets.push(keys.len());
        }
        let mut items = keys.clone();
        items.sort_unstable();
        items.dedup();
        items.shrink_to_fit();
        let cols: Vec<u32> = keys
            .iter()
            .map(|k| items.binary_search(k).expect("a listed work") as u32)
            .collect();
        drop(keys);
        let by_user = Csr {
            offsets,
            cols,
            vals,
        };
        let by_item = by_user.transpose(items.len());
        Cells {
            users,
            items,
            by_user,
            by_item,
        }
    }

    pub fn len(&self) -> usize {
        self.by_user.cols.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_user.cols.is_empty()
    }
}

/// A fitted factorization. Row `u` of `user` is `user[u * k..(u + 1) * k]`, likewise `item`.
/// The implicit model has no biases and mean 0.
pub struct Model {
    pub k: usize,
    pub user: Vec<f32>,
    pub item: Vec<f32>,
    pub user_bias: Vec<f32>,
    pub item_bias: Vec<f32>,
    pub mean: f64,
}

impl Model {
    pub fn item_row(&self, i: usize) -> &[f32] {
        &self.item[i * self.k..(i + 1) * self.k]
    }

    pub fn user_row(&self, u: usize) -> &[f32] {
        &self.user[u * self.k..(u + 1) * self.k]
    }

    /// The model's value for user `u` and work `i` (dense indices of [`Cells`]).
    pub fn predict(&self, u: usize, i: usize) -> f64 {
        self.mean
            + f64::from(self.user_bias[u])
            + f64::from(self.item_bias[i])
            + f64::from(dot(self.user_row(u), self.item_row(i)))
    }
}

/// `rows × k` start values, uniform in `[0, scale)`, in row order from `g`.
fn start(rows: usize, k: usize, scale: f32, g: &mut ChaCha8Rng) -> Vec<f32> {
    (0..rows * k).map(|_| g.random::<f32>() * scale).collect()
}

/// Implicit ALS: confidence `1 + alpha · value` on observed cells, preference 1 there and 0
/// elsewhere (`discorec`'s defaults: `alpha` 40, `regularisation` 0.01).
pub struct Implicit {
    pub k: usize,
    pub iterations: u32,
    pub alpha: f32,
    pub regularisation: f32,
    pub seed: u64,
}

impl Implicit {
    pub fn new(k: usize, iterations: u32, seed: u64) -> Implicit {
        Implicit {
            k,
            iterations,
            alpha: 40.0,
            regularisation: 0.01,
            seed,
        }
    }

    pub fn fit(&self, cells: &Cells) -> Model {
        let k = self.k;
        let mut g = ChaCha8Rng::seed_from_u64(self.seed);
        let mut user = start(cells.users.len(), k, 0.01, &mut g);
        let mut item = start(cells.items.len(), k, 0.01, &mut g);
        for iteration in 0..self.iterations {
            self.half(&cells.by_user, &mut user, &item);
            self.half(&cells.by_item, &mut item, &user);
            tracing::info!(
                iteration = iteration + 1,
                "implicit factorization round done"
            );
        }
        Model {
            k,
            user_bias: vec![0.0; cells.users.len()],
            item_bias: vec![0.0; cells.items.len()],
            user,
            item,
            mean: 0.0,
        }
    }

    /// Updates every row of `x` against the fixed other side `y`.
    fn half(&self, data: &Csr, x: &mut [f32], y: &[f32]) {
        let k = self.k;
        let mut yty = gram(y, k);
        for d in 0..k {
            yty[d * k + d] += f64::from(self.regularisation);
        }
        let yty: Vec<f32> = yty.iter().map(|&v| v as f32).collect();
        x.par_chunks_mut(k).enumerate().for_each(|(u, xu)| {
            let (cols, vals) = data.row(u);
            let mut r = vec![0f32; k];
            sym_mul(&yty, xu, &mut r);
            r.iter_mut().for_each(|v| *v = -*v);
            for (&i, &v) in cols.iter().zip(vals) {
                let c = 1.0 + self.alpha * v;
                let yi = &y[i as usize * k..(i as usize + 1) * k];
                add_scaled(&mut r, c - (c - 1.0) * dot(yi, xu), yi);
            }
            let mut p = r.clone();
            let mut rs_old = dot(&r, &r);
            let mut ap = vec![0f32; k];
            for _ in 0..CG_STEPS {
                // Already solved (a zero residual would make the step 0 / 0).
                if rs_old < 1e-20 {
                    break;
                }
                sym_mul(&yty, &p, &mut ap);
                for (&i, &v) in cols.iter().zip(vals) {
                    let c = 1.0 + self.alpha * v;
                    let yi = &y[i as usize * k..(i as usize + 1) * k];
                    add_scaled(&mut ap, (c - 1.0) * dot(yi, &p), yi);
                }
                let alpha = rs_old / dot(&p, &ap);
                add_scaled(xu, alpha, &p);
                add_scaled(&mut r, -alpha, &ap);
                let rs_new = dot(&r, &r);
                if rs_new < 1e-20 {
                    break;
                }
                let beta = rs_new / rs_old;
                for (pi, ri) in p.iter_mut().zip(&r) {
                    *pi = ri + beta * *pi;
                }
                rs_old = rs_new;
            }
        });
    }
}

/// Explicit biased ALS with weighted-λ regularisation.
pub struct Explicit {
    pub k: usize,
    pub iterations: u32,
    /// λ: a row with `n` ratings has `λ · (n + prior_count)` on the factor part of the diagonal
    /// of its normal equations.
    pub regularisation: f64,
    /// `n0`: ridge on each bias (with the factors at 0, a row's bias is its mean residual times
    /// `n / (n + n0)`), and `λ · n0` more ridge on the factors, so rows with few ratings shrink
    /// toward 0.
    pub prior_count: f64,
    pub seed: u64,
}

impl Explicit {
    pub fn fit(&self, cells: &Cells) -> Model {
        let k = self.k;
        let mean = mean(&cells.by_user.vals);
        let mut g = ChaCha8Rng::seed_from_u64(self.seed);
        let mut user = start(cells.users.len(), k, 0.1, &mut g);
        let mut item = start(cells.items.len(), k, 0.1, &mut g);
        let mut user_bias = vec![0f32; cells.users.len()];
        let mut item_bias = vec![0f32; cells.items.len()];
        for iteration in 0..self.iterations {
            self.half(
                &cells.by_user,
                mean,
                &mut user,
                &mut user_bias,
                &item,
                &item_bias,
            );
            self.half(
                &cells.by_item,
                mean,
                &mut item,
                &mut item_bias,
                &user,
                &user_bias,
            );
            tracing::info!(iteration = iteration + 1, "star factorization round done");
        }
        Model {
            k,
            user,
            item,
            user_bias,
            item_bias,
            mean,
        }
    }

    /// Solves every row's `[b, x]` against the fixed other side: least squares of
    /// `r − mean − b_other` on `[1, y]`, plus ridge `n0` on the bias and `λ · (n + n0)` on the
    /// factors.
    fn half(
        &self,
        data: &Csr,
        mean: f64,
        x: &mut [f32],
        bias: &mut [f32],
        y: &[f32],
        y_bias: &[f32],
    ) {
        let k = self.k;
        let m = k + 1;
        x.par_chunks_mut(k)
            .zip(bias.par_iter_mut())
            .enumerate()
            .for_each(|(u, (xu, bu))| {
                let (cols, vals) = data.row(u);
                if cols.is_empty() {
                    xu.fill(0.0);
                    *bu = 0.0;
                    return;
                }
                let mut a = vec![0f64; m * m];
                let mut b = vec![0f64; m];
                let mut z = vec![0f64; m];
                z[0] = 1.0;
                for (&i, &v) in cols.iter().zip(vals) {
                    let i = i as usize;
                    for (zj, &yj) in z[1..].iter_mut().zip(&y[i * k..(i + 1) * k]) {
                        *zj = f64::from(yj);
                    }
                    let target = f64::from(v) - mean - f64::from(y_bias[i]);
                    for p in 0..m {
                        let zp = z[p];
                        b[p] += target * zp;
                        for q in p..m {
                            a[p * m + q] += zp * z[q];
                        }
                    }
                }
                let ridge = self.regularisation * (cols.len() as f64 + self.prior_count);
                let a = DMatrix::from_fn(m, m, |p, q| {
                    let v = if p <= q { a[p * m + q] } else { a[q * m + p] };
                    match (p == q, p) {
                        (true, 0) => v + self.prior_count,
                        (true, _) => v + ridge,
                        _ => v,
                    }
                });
                let solved = a
                    .cholesky()
                    .expect("positive definite: λ > 0 and n0 > 0 on the diagonal (Factors::check)")
                    .solve(&DVector::from_vec(b));
                *bu = solved[0] as f32;
                for (xj, &s) in xu.iter_mut().zip(solved.iter().skip(1)) {
                    *xj = s as f32;
                }
            });
    }
}

/// `YᵀY` (row-major `k × k`, f64) over the rows of `y`, summed chunk by chunk in order.
fn gram(y: &[f32], k: usize) -> Vec<f64> {
    let parts: Vec<Vec<f64>> = y
        .par_chunks(CHUNK * k)
        .map(|chunk| {
            let mut g = vec![0f64; k * k];
            for row in chunk.chunks_exact(k) {
                for p in 0..k {
                    let rp = f64::from(row[p]);
                    for q in p..k {
                        g[p * k + q] += rp * f64::from(row[q]);
                    }
                }
            }
            g
        })
        .collect();
    let mut g = vec![0f64; k * k];
    for part in parts {
        for (a, b) in g.iter_mut().zip(part) {
            *a += b;
        }
    }
    for p in 0..k {
        for q in 0..p {
            g[p * k + q] = g[q * k + p];
        }
    }
    g
}

/// The mean of `values`, summed chunk by chunk in order.
fn mean(values: &[f32]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let parts: Vec<f64> = values
        .par_chunks(CHUNK)
        .map(|c| c.iter().map(|&v| f64::from(v)).sum())
        .collect();
    parts.iter().sum::<f64>() / values.len() as f64
}

/// `out = a · v` for a row-major square `a`.
fn sym_mul(a: &[f32], v: &[f32], out: &mut [f32]) {
    let k = v.len();
    for (p, o) in out.iter_mut().enumerate() {
        *o = dot(&a[p * k..(p + 1) * k], v);
    }
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn add_scaled(x: &mut [f32], s: f32, v: &[f32]) {
    for (xi, vi) in x.iter_mut().zip(v) {
        *xi += s * vi;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand_distr::{Distribution as _, StandardNormal};

    fn normal(g: &mut ChaCha8Rng) -> f64 {
        StandardNormal.sample(g)
    }

    /// Ratings from a planted `mu + b_u + b_i + p_u · q_i` (rank 3), on about half the cells;
    /// returns the cells and the held-out cells with their true values.
    fn planted_stars(seed: u64) -> (Cells, Vec<(usize, usize, f64)>) {
        let (users, items, k) = (300, 200, 3);
        let mut g = ChaCha8Rng::seed_from_u64(seed);
        let p: Vec<f64> = (0..users * k).map(|_| 0.6 * normal(&mut g)).collect();
        let q: Vec<f64> = (0..items * k).map(|_| 0.6 * normal(&mut g)).collect();
        let bu: Vec<f64> = (0..users).map(|_| 0.4 * normal(&mut g)).collect();
        let bi: Vec<f64> = (0..items).map(|_| 0.5 * normal(&mut g)).collect();
        let value = |u: usize, i: usize| {
            3.5 + bu[u] + bi[i] + (0..k).map(|d| p[u * k + d] * q[i * k + d]).sum::<f64>()
        };
        let mut cells = Vec::new();
        let mut held = Vec::new();
        for u in 0..users {
            for i in 0..items {
                if g.random_bool(0.5) {
                    cells.push((u as u64 * 10, i as u64 * 7 + 3, value(u, i) as f32));
                } else {
                    held.push((u, i, value(u, i)));
                }
            }
        }
        (Cells::from_user_order(cells), held)
    }

    #[test]
    fn explicit_recovers_a_planted_biased_low_rank_matrix() {
        let (cells, held) = planted_stars(1);
        let model = Explicit {
            k: 3,
            iterations: 30,
            regularisation: 0.001,
            prior_count: 0.001,
            seed: 9,
        }
        .fit(&cells);
        let mean = model.mean;
        let (mut err, mut base) = (0.0, 0.0);
        for &(u, i, truth) in &held {
            err += (model.predict(u, i) - truth).powi(2);
            base += (mean - truth).powi(2);
        }
        let rmse = (err / held.len() as f64).sqrt();
        let base = (base / held.len() as f64).sqrt();
        assert!(
            rmse < 0.05 * base,
            "held-out RMSE {rmse} vs {base} for the mean"
        );
    }

    #[test]
    fn implicit_recovers_planted_groups() {
        // Four groups of users and works; a user likes half of its own group's works.
        let (users, items, groups) = (200, 120, 4);
        let mut g = ChaCha8Rng::seed_from_u64(2);
        let mut cells = Vec::new();
        let mut held = Vec::new();
        for u in 0..users {
            for i in 0..items {
                if u % groups == i % groups {
                    if g.random_bool(0.5) {
                        cells.push((u as u64, i as u64, 1.0));
                    } else {
                        held.push((u, i));
                    }
                }
            }
        }
        let cells = Cells::from_user_order(cells);
        let model = Implicit::new(4, 15, 5).fit(&cells);
        // A held-out work of the user's own group outscores every work of another group.
        let (mut right, mut pairs) = (0.0, 0.0);
        for &(u, i) in held.iter().step_by(7) {
            let own = model.predict(u, i);
            for j in (0..items).filter(|j| j % groups != u % groups) {
                right += f64::from(u8::from(own > model.predict(u, j)));
                pairs += 1.0;
            }
        }
        assert!(right / pairs > 0.97, "AUC {}", right / pairs);
    }

    #[test]
    fn the_thread_count_does_not_change_the_result() {
        // Enough rows for several chunks of the shared sums.
        let (cells, _) = planted_stars(3);
        let mut big = Vec::new();
        for u in 0..CHUNK as u64 + 500 {
            for j in 0..3u64 {
                big.push((u, (u * 13 + j * 101) % 997, 1.0 + ((u + j) % 5) as f32));
            }
        }
        let big = Cells::from_user_order(big);
        let run = |threads: usize| {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .unwrap();
            pool.install(|| {
                let e = Explicit {
                    k: 4,
                    iterations: 3,
                    regularisation: 0.05,
                    prior_count: 25.0,
                    seed: 1,
                }
                .fit(&cells);
                let i = Implicit::new(4, 3, 1).fit(&big);
                let e2 = Explicit {
                    k: 4,
                    iterations: 3,
                    regularisation: 0.05,
                    prior_count: 25.0,
                    seed: 1,
                }
                .fit(&big);
                (
                    e.item,
                    e.item_bias,
                    e.user,
                    e.mean,
                    i.item,
                    i.user,
                    e2.item,
                    e2.mean,
                )
            })
        };
        assert_eq!(run(1), run(4));
    }
}
