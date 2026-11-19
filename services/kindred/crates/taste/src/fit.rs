//! Fitting one user (§6) and what a fit says about works: score, variance, fit band, the exact
//! breakdown of a score into the user's rows (§8), and the taste profile (§9).

use std::collections::{BTreeMap, BTreeSet};

use nalgebra::{Cholesky, DMatrix, DVector, Dyn};
use rand_distr::{Distribution as _, StandardNormal};

use crate::observe::{self, Event, Observation, Source, StopReason, UserPrior};
use crate::{Hyper, Layout, Prior, Row, Vectors, ln_sigmoid, sigmoid};

/// Newton stops after this many rounds; `L` is concave, so it converges well before.
const MAX_ROUNDS: u32 = 50;
/// Newton stops once the largest absolute entry of the gradient is below this. Tighter than the
/// 1e-6 of §6 because the breakdown check (B10) multiplies the leftover gradient by `S0` and by
/// a work's vector and still has to land within 1e-6; Newton's quadratic convergence makes the
/// difference about one extra round.
const GRAD_TOL: f64 = 1e-9;
/// Step halving gives up after this many halvings (the step is then below rounding).
const MAX_HALVINGS: u32 = 60;
/// The breakdown check of §8 (B10): `|mu - sum of parts| < B10_TOL * (1 + |mu|)`.
const B10_TOL: f64 = 1e-6;
/// Taste profile (§9): the |z| that makes an aspect a clear like or dislike, the rated works
/// with the aspect that make "not sure yet" worth saying, and the length of each list.
const PROFILE_Z: f64 = 1.5;
const PROFILE_MIN_RATED: u32 = 3;
const PROFILE_MAX: usize = 8;
/// An aspect counts as present in a work at this aspect score or above (§8, §9).
const ASPECT_PRESENT: f32 = 1.0;

/// One user's fitted model: the posterior mode `w_hat`, its covariance `Sigma = H^-1`, and
/// what is needed to explain scores. A pure function of the user's events and the world.
#[derive(Clone, Debug)]
pub struct Fit {
    layout: Layout,
    w_hat: Vec<f64>,
    /// `w_hat` in f32 with the bias weight zeroed, for [`Fit::score_rows`].
    w_rank: Vec<f32>,
    sigma: DMatrix<f64>,
    /// Lower Cholesky factor of `sigma`, for Thompson draws.
    sigma_chol: DMatrix<f64>,
    m0: Vec<f64>,
    s0: Vec<f64>,
    /// Per origin, the vector `v` with `r(i) = phi(i) · v` (§8).
    parts: Vec<(Origin, Vec<f64>)>,
    /// Per aspect, how many distinct rated works have it (`a_k >= 1`).
    rated_with_aspect: Vec<u32>,
    rounds: u32,
}

/// Fits one user from that user's events alone (§6): maximises
/// `L(w) = sum_n term_n(w) - 0.5 (w - m0)^T S0^-1 (w - m0)` by Newton's method from `w = m0`,
/// solving `H step = g` by Cholesky and halving the step until `L` does not decrease.
///
/// The signature is the isolation guarantee of §11 test 6: one user's events and the shared
/// world are the only inputs, so no other user's data can enter.
pub fn fit(events: &[Event], world: &impl Vectors, hyper: &Hyper, prior: &Prior) -> Fit {
    let layout = world.layout();
    let d = layout.d();
    let UserPrior { m0, s0 } = observe::user_prior(layout, hyper, prior, events);
    let obs = observe::observations(events, world, hyper);
    let terms = Terms::new(&obs, d);
    let m0v = DVector::from_column_slice(&m0);
    let s0_inv = DVector::from_iterator(d, s0.iter().map(|v| 1.0 / v));

    let mut w = m0v.clone();
    let mut rounds = 0;
    let mut objective = terms.objective(&w, &m0v, &s0_inv);
    while rounds < MAX_ROUNDS {
        let g = terms.gradient(&w, &m0v, &s0_inv);
        if g.amax() < GRAD_TOL {
            break;
        }
        rounds += 1;
        let step = spd_cholesky(terms.hessian(&w, &s0_inv)).solve(&g);
        // Near the optimum the gain of a full Newton step is below the rounding of L; accept it
        // then rather than halving into noise.
        let noise = 1e-12 * (1.0 + objective.abs());
        let mut alpha = 1.0;
        let mut accepted = false;
        for _ in 0..=MAX_HALVINGS {
            let candidate = &w + &step * alpha;
            let value = terms.objective(&candidate, &m0v, &s0_inv);
            if value.is_finite() && value >= objective - noise {
                w = candidate;
                objective = value;
                accepted = true;
                break;
            }
            alpha *= 0.5;
        }
        if !accepted {
            break;
        }
    }

    let (sigma, sigma_chol) = if obs.is_empty() {
        // H = S0^-1 is diagonal; its inverse is S0 exactly (§11 test 2), with no rounding from a
        // factorisation.
        (
            DMatrix::from_diagonal(&DVector::from_column_slice(&s0)),
            DMatrix::from_diagonal(&DVector::from_iterator(d, s0.iter().map(|v| v.sqrt()))),
        )
    } else {
        let sigma = spd_cholesky(terms.hessian(&w, &s0_inv)).inverse();
        let sigma = (&sigma + sigma.transpose()) * 0.5;
        let chol = spd_cholesky(sigma.clone()).l();
        (sigma, chol)
    };

    let parts = parts(&obs, &terms, &w, &s0);
    let rated_with_aspect = rated_with_aspect(events, world, layout);
    let w_hat: Vec<f64> = w.iter().copied().collect();
    let mut w_rank: Vec<f32> = w_hat.iter().map(|&v| v as f32).collect();
    w_rank[layout.bias()] = 0.0;
    Fit {
        layout,
        w_hat,
        w_rank,
        sigma,
        sigma_chol,
        m0,
        s0,
        parts,
        rated_with_aspect,
        rounds,
    }
}

/// The observations as columns: `X` (n × d) and per-term `t`, `y`, `tau`, `omega`.
struct Terms {
    x: DMatrix<f64>,
    t: DVector<f64>,
    y: DVector<f64>,
    tau: DVector<f64>,
    omega: DVector<f64>,
}

impl Terms {
    fn new(obs: &[Observation], d: usize) -> Self {
        let n = obs.len();
        Self {
            x: DMatrix::from_fn(n, d, |r, c| obs[r].x[c]),
            t: DVector::from_iterator(n, obs.iter().map(|o| o.t)),
            y: DVector::from_iterator(n, obs.iter().map(|o| o.y)),
            tau: DVector::from_iterator(n, obs.iter().map(|o| o.tau)),
            omega: DVector::from_iterator(n, obs.iter().map(|o| o.omega)),
        }
    }

    /// `z_n = y_n (x_n · w - t_n) / tau_n` for every term.
    fn z(&self, w: &DVector<f64>) -> DVector<f64> {
        let mut z = &self.x * w;
        for n in 0..z.len() {
            z[n] = self.y[n] * (z[n] - self.t[n]) / self.tau[n];
        }
        z
    }

    /// `g_n = omega_n y_n (1 - sigma(z_n)) / tau_n`: the coefficient of `x_n` in the gradient.
    fn coefficients(&self, w: &DVector<f64>) -> DVector<f64> {
        let mut c = self.z(w);
        for n in 0..c.len() {
            c[n] = self.omega[n] * self.y[n] * sigmoid(-c[n]) / self.tau[n];
        }
        c
    }

    fn objective(&self, w: &DVector<f64>, m0: &DVector<f64>, s0_inv: &DVector<f64>) -> f64 {
        let z = self.z(w);
        let likelihood: f64 = (0..z.len()).map(|n| self.omega[n] * ln_sigmoid(z[n])).sum();
        let dw = w - m0;
        likelihood - 0.5 * dw.component_mul(&dw).dot(s0_inv)
    }

    fn gradient(&self, w: &DVector<f64>, m0: &DVector<f64>, s0_inv: &DVector<f64>) -> DVector<f64> {
        self.x.tr_mul(&self.coefficients(w)) - (w - m0).component_mul(s0_inv)
    }

    /// `H = S0^-1 + sum_n omega_n sigma(z_n)(1 - sigma(z_n)) / tau_n^2 x_n x_n^T`, as
    /// `B^T B` with the rows of `B` the `x_n` scaled by the square roots of their curvatures.
    fn hessian(&self, w: &DVector<f64>, s0_inv: &DVector<f64>) -> DMatrix<f64> {
        let z = self.z(w);
        let mut b = self.x.clone();
        for n in 0..z.len() {
            let s = sigmoid(z[n]);
            let curvature = self.omega[n] * s * (1.0 - s) / (self.tau[n] * self.tau[n]);
            b.row_mut(n).scale_mut(curvature.sqrt());
        }
        let mut h = b.tr_mul(&b);
        for (i, v) in s0_inv.iter().enumerate() {
            h[(i, i)] += v;
        }
        h
    }
}

/// Cholesky of a matrix that is symmetric positive definite in exact arithmetic (a positive
/// diagonal plus positive semi-definite terms). If rounding has made it fail, adds a diagonal
/// jitter growing from 1e-12 of the largest diagonal entry until it succeeds. Why: a handful of
/// huge, nearly parallel observations can swamp the prior's diagonal in f64, and the fit must
/// still return finite numbers (§11 test 7).
fn spd_cholesky(m: DMatrix<f64>) -> Cholesky<f64, Dyn> {
    if let Some(c) = Cholesky::new(m.clone()) {
        return c;
    }
    let scale = m.diagonal().amax().max(1.0);
    let mut jitter = 1e-12 * scale;
    loop {
        let mut j = m.clone();
        for i in 0..j.nrows() {
            j[(i, i)] += jitter;
        }
        if let Some(c) = Cholesky::new(j) {
            return c;
        }
        assert!(
            jitter.is_finite(),
            "matrix has no finite Cholesky factor: non-finite input"
        );
        jitter *= 100.0;
    }
}

/// The exact contributions of §8, grouped by origin: at the optimum
/// `w_hat = m0 + S0 * sum_n g_n x_n`, so `r_n(i) = phi(i) · (g_n S0 x_n)`. A comparison's
/// vector is split between its two works.
fn parts(
    obs: &[Observation],
    terms: &Terms,
    w: &DVector<f64>,
    s0: &[f64],
) -> Vec<(Origin, Vec<f64>)> {
    let g = terms.coefficients(w);
    let mut by: BTreeMap<Origin, Vec<f64>> = BTreeMap::new();
    let mut add = |origin: Origin, coefficient: f64, x: &mut dyn Iterator<Item = f64>| {
        let v = by.entry(origin).or_insert_with(|| vec![0.0; s0.len()]);
        for ((vk, sk), xk) in v.iter_mut().zip(s0).zip(x) {
            *vk += coefficient * sk * xk;
        }
    };
    for (o, &gn) in obs.iter().zip(g.iter()) {
        match &o.source {
            Source::Work(r) => add(Origin::Work(*r), gn, &mut o.x.iter().copied()),
            Source::Aspect(k) => add(Origin::Aspect(*k), gn, &mut o.x.iter().copied()),
            Source::Pair {
                better,
                worse,
                phi_better,
            } => {
                add(Origin::Work(*better), gn, &mut phi_better.iter().copied());
                let phi_worse = phi_better.iter().zip(&o.x).map(|(p, x)| p - x);
                add(Origin::Work(*worse), -gn, &mut phi_worse.into_iter());
            }
        }
    }
    by.into_iter().collect()
}

/// Per aspect, the number of distinct works the user rated (a verdict, or stopped as not for
/// them) whose aspect score is at least 1.
fn rated_with_aspect(events: &[Event], world: &impl Vectors, layout: Layout) -> Vec<u32> {
    let rated: BTreeSet<Row> = events
        .iter()
        .filter_map(|e| match *e {
            Event::Read {
                work,
                verdict,
                stopped,
                ..
            } => match stopped {
                Some(StopReason::NotNow) => None,
                Some(StopReason::NotForMe) => Some(work),
                None => verdict.map(|_| work),
            },
            _ => None,
        })
        .collect();
    let mut counts = vec![0; layout.a];
    for work in rated {
        if let Some(phi) = world.phi(work).filter(|p| p.len() == layout.d()) {
            for (count, &a) in counts.iter_mut().zip(&phi[layout.a()]) {
                if a >= ASPECT_PRESENT {
                    *count += 1;
                }
            }
        }
    }
    counts
}

impl Fit {
    pub fn layout(&self) -> Layout {
        self.layout
    }

    /// `w_hat`, the posterior mode.
    pub fn weights(&self) -> &[f64] {
        &self.w_hat
    }

    /// `Sigma = H^-1` at `w_hat`.
    pub fn covariance(&self) -> &DMatrix<f64> {
        &self.sigma
    }

    /// This user's prior mean `m0`.
    pub fn prior_mean(&self) -> &[f64] {
        &self.m0
    }

    /// The diagonal of this user's prior covariance `S0`, after "Don't care" overrides.
    pub fn prior_variance(&self) -> &[f64] {
        &self.s0
    }

    /// Newton rounds taken.
    pub fn rounds(&self) -> u32 {
        self.rounds
    }

    /// The utility `phi · w_hat`, bias included: what the verdict thresholds compare against.
    pub fn utility(&self, phi: &[f32]) -> f64 {
        dot(phi, &self.w_hat)
    }

    /// `mu(i)` without the bias number: the ranking score. The bias is the same for every work,
    /// so dropping it never changes a ranking (§8).
    pub fn score(&self, phi: &[f32]) -> f64 {
        self.utility(phi) - f64::from(phi[self.layout.bias()]) * self.w_hat[self.layout.bias()]
    }

    /// Ranking scores of many works at once: `rows` holds `d` f32 numbers per work, row after
    /// row, and `out[r]` receives the score of row `r` (bias dropped). Plain f32 arithmetic with
    /// no allocation, for ranking the whole world; it agrees with [`Fit::score`] to f32 rounding.
    ///
    /// # Panics
    /// When `rows.len() != out.len() * d`.
    pub fn score_rows(&self, rows: &[f32], out: &mut [f32]) {
        let d = self.w_rank.len();
        assert_eq!(
            rows.len(),
            out.len() * d,
            "score_rows: rows must hold d numbers per output"
        );
        for (row, o) in rows.chunks_exact(d).zip(out.iter_mut()) {
            *o = dot_f32(row, &self.w_rank);
        }
    }

    /// `var(i) = phi^T Sigma phi`, plus `text_var` for a text-sourced work (§3, §8).
    pub fn variance(&self, phi: &[f32], text_sourced: bool, hyper: &Hyper) -> f64 {
        let v = DVector::from_iterator(phi.len(), phi.iter().map(|&x| f64::from(x)));
        let var = (&self.sigma * &v).dot(&v).max(0.0);
        if text_sourced {
            var + hyper.text_var
        } else {
            var
        }
    }

    /// The probability of "liked or better" and its band for a book page (§8):
    /// `p = sigma( (mu - b2) / sqrt(tau_v^2 + (pi/8) var) )`, the probit approximation of the
    /// logistic averaged over the score's uncertainty. `mu` here includes the bias, because the
    /// thresholds are absolute utilities.
    pub fn band(&self, phi: &[f32], text_sourced: bool, hyper: &Hyper) -> (f64, Band) {
        let var = self.variance(phi, text_sourced, hyper);
        let scale = (hyper.tau_v * hyper.tau_v + std::f64::consts::PI / 8.0 * var).sqrt();
        let p = sigmoid((self.utility(phi) - hyper.b2) / scale);
        (p, Band::from_probability(p))
    }

    /// The exact split of `mu(i)` (bias included) into the prior part `phi · m0` and one
    /// contribution per origin among the user's rows (§8), largest first. A verdict's terms,
    /// picks and rereads belong to their work; a comparison is split between its two works; a
    /// chip or profile answer belongs to its aspect.
    pub fn breakdown(&self, phi: &[f32]) -> Breakdown {
        let prior = dot(phi, &self.m0);
        let mut by_origin: Vec<(Origin, f64)> =
            self.parts.iter().map(|(o, v)| (*o, dot(phi, v))).collect();
        by_origin.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        let total = prior + by_origin.iter().map(|(_, r)| r).sum::<f64>();
        Breakdown {
            prior,
            by_origin,
            total,
        }
    }

    /// The second exact view of a score: `w_hat_k * phi_k(i)` for every number `k`; they sum to
    /// `mu(i)` with the bias.
    pub fn by_number(&self, phi: &[f32]) -> Vec<f64> {
        phi.iter()
            .zip(&self.w_hat)
            .map(|(&p, w)| f64::from(p) * w)
            .collect()
    }

    /// A Thompson draw `w_tilde = w_hat + chol(Sigma) z`, `z` standard normal from `rng` (§8).
    pub fn sample_weights(&self, rng: &mut impl rand::Rng) -> Vec<f64> {
        let d = self.w_hat.len();
        let z = DVector::from_iterator(d, (0..d).map(|_| StandardNormal.sample(rng)));
        let w = &self.sigma_chol * z;
        w.iter().zip(&self.w_hat).map(|(dw, w)| w + dw).collect()
    }

    /// The taste profile (§9) with `z_k = w_hat_k / sqrt(Sigma_kk)` per aspect: what the user
    /// seems to love (`z >= 1.5`), what tends to get in the way (`z <= -1.5`), and what is not
    /// clear yet (`|z| < 1.5` with at least three rated works that have the aspect). Each list
    /// strongest (largest `|z|`) first, at most eight.
    pub fn profile(&self) -> Profile {
        let mut profile = Profile::default();
        for k in 0..self.layout.a {
            let i = self.layout.aspect(k);
            let z = self.w_hat[i] / self.sigma[(i, i)].sqrt();
            let entry = AspectWeight { aspect: k, z };
            if z >= PROFILE_Z {
                profile.loves.push(entry);
            } else if z <= -PROFILE_Z {
                profile.in_the_way.push(entry);
            } else if self.rated_with_aspect[k] >= PROFILE_MIN_RATED {
                profile.not_sure.push(entry);
            }
        }
        for list in [
            &mut profile.loves,
            &mut profile.in_the_way,
            &mut profile.not_sure,
        ] {
            list.sort_by(|a, b| {
                b.z.abs()
                    .total_cmp(&a.z.abs())
                    .then(a.aspect.cmp(&b.aspect))
            });
            list.truncate(PROFILE_MAX);
        }
        profile
    }
}

/// `phi · v` in f64.
fn dot(phi: &[f32], v: &[f64]) -> f64 {
    phi.iter().zip(v).map(|(&p, v)| f64::from(p) * v).sum()
}

/// An f32 dot product with eight independent accumulators, so the compiler can vectorise it
/// (a single running sum would force strict left-to-right order).
fn dot_f32(a: &[f32], b: &[f32]) -> f32 {
    let mut acc = [0.0f32; 8];
    let (ac, ar) = a.as_chunks::<8>();
    let (bc, br) = b.as_chunks::<8>();
    for (x, y) in ac.iter().zip(bc) {
        for j in 0..8 {
            acc[j] += x[j] * y[j];
        }
    }
    let tail: f32 = ar.iter().zip(br).map(|(x, y)| x * y).sum();
    acc.iter().sum::<f32>() + tail
}

/// How a book page words the fit (§8), from the probability of "liked or better".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Band {
    /// `p >= 0.8`
    StrongFit,
    /// `0.6 <= p < 0.8`
    GoodFit,
    /// `0.4 <= p < 0.6`
    NotSure,
    /// `p < 0.4`
    ProbablyNot,
}

impl Band {
    pub fn from_probability(p: f64) -> Self {
        if p >= 0.8 {
            Band::StrongFit
        } else if p >= 0.6 {
            Band::GoodFit
        } else if p >= 0.4 {
            Band::NotSure
        } else {
            Band::ProbablyNot
        }
    }
}

/// Where a part of a score comes from.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// The user's rows about this work: its verdicts, picks, rereads and its side of comparisons.
    Work(Row),
    /// The user's chips and profile answers on this aspect.
    Aspect(usize),
    /// The prior mean. [`Breakdown::prior`] carries it; it never appears in
    /// [`Breakdown::by_origin`], and exists so a caller can list all parts in one sequence.
    Prior,
}

/// The exact split of one score (§8).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Breakdown {
    /// `phi(i) · m0`.
    pub prior: f64,
    /// One entry per origin with a nonzero vector, largest contribution first.
    pub by_origin: Vec<(Origin, f64)>,
    /// `prior` plus every contribution.
    pub total: f64,
}

impl Breakdown {
    /// The required check B10: the parts add up to the score,
    /// `|mu - total| < 1e-6 * (1 + |mu|)`, with `mu` the utility including the bias.
    pub fn check(&self, mu_with_bias: f64) -> bool {
        (mu_with_bias - self.total).abs() < B10_TOL * (1.0 + mu_with_bias.abs())
    }
}

/// One aspect in the taste profile, with its `z_k`.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AspectWeight {
    pub aspect: usize,
    pub z: f64,
}

/// The three lists of the taste profile (§9).
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Profile {
    pub loves: Vec<AspectWeight>,
    pub in_the_way: Vec<AspectWeight>,
    pub not_sure: Vec<AspectWeight>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ProfileChoice, Verdict};

    struct Grid {
        layout: Layout,
        rows: Vec<Vec<f32>>,
    }

    impl Vectors for Grid {
        fn layout(&self) -> Layout {
            self.layout
        }
        fn phi(&self, work: Row) -> Option<&[f32]> {
            self.rows.get(work.0 as usize).map(Vec::as_slice)
        }
        fn text_sourced(&self, _: Row) -> bool {
            false
        }
    }

    /// Works whose aspect `j % a` is 2.0 and whose collaborative block is a unit axis.
    fn grid(n: usize) -> Grid {
        let layout = Layout { k: 3, a: 4 };
        let rows = (0..n)
            .map(|j| {
                let mut phi = vec![0.0f32; layout.d()];
                phi[0] = 1.0;
                phi[layout.c().start + j % layout.k] = 1.0;
                phi[layout.aspect(j % layout.a)] = 2.0;
                phi
            })
            .collect();
        Grid { layout, rows }
    }

    fn loved(work: u32) -> Event {
        Event::Read {
            work: Row(work),
            verdict: Some(Verdict::Loved),
            stopped: None,
            nth_read: 1,
            latest: true,
            age_years: 0.0,
        }
    }

    #[test]
    fn band_cut_points() {
        assert_eq!(Band::from_probability(1.0), Band::StrongFit);
        assert_eq!(Band::from_probability(0.8), Band::StrongFit);
        assert_eq!(Band::from_probability(0.799_999), Band::GoodFit);
        assert_eq!(Band::from_probability(0.6), Band::GoodFit);
        assert_eq!(Band::from_probability(0.599_999), Band::NotSure);
        assert_eq!(Band::from_probability(0.4), Band::NotSure);
        assert_eq!(Band::from_probability(0.399_999), Band::ProbablyNot);
        assert_eq!(Band::from_probability(0.0), Band::ProbablyNot);
    }

    #[test]
    fn band_uses_the_probit_approximation() {
        let world = grid(4);
        let h = Hyper::default();
        let f = fit(&[loved(0)], &world, &h, &Prior::default());
        let phi = &world.rows[0];
        let (p, band) = f.band(phi, false, &h);
        let var = f.variance(phi, false, &h);
        let expected = sigmoid(f.utility(phi) / (1.0 + std::f64::consts::PI / 8.0 * var).sqrt());
        assert!((p - expected).abs() < 1e-15);
        assert_eq!(band, Band::from_probability(p));
        // More uncertainty pulls p toward one half.
        let (p_text, _) = f.band(phi, true, &h);
        assert!((p_text - 0.5).abs() < (p - 0.5).abs());
        assert!((f.variance(phi, true, &h) - var - h.text_var).abs() < 1e-12);
    }

    #[test]
    fn score_rows_matches_score() {
        let world = grid(9);
        let f = fit(
            &[loved(0), loved(4)],
            &world,
            &Hyper::default(),
            &Prior::default(),
        );
        let rows: Vec<f32> = world.rows.concat();
        let mut out = vec![0.0f32; world.rows.len()];
        f.score_rows(&rows, &mut out);
        for (phi, s) in world.rows.iter().zip(&out) {
            assert!((f.score(phi) - f64::from(*s)).abs() < 1e-5);
            assert!((f.utility(phi) - f.score(phi) - f.weights()[0]).abs() < 1e-12);
        }
    }

    #[test]
    fn dot_f32_handles_every_tail_length() {
        for n in 0..20 {
            let a: Vec<f32> = (0..n).map(|i| i as f32).collect();
            let b: Vec<f32> = (0..n).map(|i| 1.0 + i as f32).collect();
            let want: f32 = a.iter().zip(&b).map(|(x, y)| x * y).sum();
            assert_eq!(dot_f32(&a, &b), want, "n = {n}");
        }
    }

    #[test]
    fn profile_lists() {
        let world = grid(12);
        let l = world.layout;
        // Aspect 0 loved by profile answer, aspect 1 rejected; works 2, 6, 10 (aspect 2) rated
        // with mixed verdicts.
        let mut events = vec![
            Event::Profile {
                aspect: 0,
                choice: ProfileChoice::Yes,
            },
            Event::Profile {
                aspect: 1,
                choice: ProfileChoice::Opposite,
            },
        ];
        for (w, v) in [
            (2, Verdict::Loved),
            (6, Verdict::NotForMe),
            (10, Verdict::Okay),
        ] {
            events.push(Event::Read {
                work: Row(w),
                verdict: Some(v),
                stopped: None,
                nth_read: 1,
                latest: true,
                age_years: 0.0,
            });
        }
        // A wider aspect prior than the default: under var_a = 0.0225 one answer moves the
        // weight by about a third of its standard deviation, never to |z| >= 1.5.
        let h = Hyper {
            var_a: 1.0,
            ..Hyper::default()
        };
        let f = fit(&events, &world, &h, &Prior::default());
        let p = f.profile();
        assert_eq!(
            p.loves.iter().map(|a| a.aspect).collect::<Vec<_>>(),
            vec![0]
        );
        assert_eq!(
            p.in_the_way.iter().map(|a| a.aspect).collect::<Vec<_>>(),
            vec![1]
        );
        assert_eq!(
            p.not_sure.iter().map(|a| a.aspect).collect::<Vec<_>>(),
            vec![2]
        );
        let i = l.aspect(0);
        assert!((p.loves[0].z - f.weights()[i] / f.covariance()[(i, i)].sqrt()).abs() < 1e-12);
    }

    #[test]
    fn breakdown_groups_by_origin() {
        let world = grid(6);
        let events = [
            loved(0),
            Event::Comparison {
                better: Row(1),
                worse: Row(2),
                age_years: 0.0,
            },
            Event::Chip {
                aspect: 3,
                positive: true,
                age_years: 1.0,
            },
        ];
        let f = fit(&events, &world, &Hyper::default(), &Prior::default());
        let b = f.breakdown(&world.rows[5]);
        let origins: BTreeSet<Origin> = b.by_origin.iter().map(|(o, _)| *o).collect();
        let want: BTreeSet<Origin> = [
            Origin::Work(Row(0)),
            Origin::Work(Row(1)),
            Origin::Work(Row(2)),
            Origin::Aspect(3),
        ]
        .into();
        assert_eq!(origins, want);
        assert!(b.by_origin.windows(2).all(|w| w[0].1 >= w[1].1));
        assert!(b.check(f.utility(&world.rows[5])));
        let by_number: f64 = f.by_number(&world.rows[5]).iter().sum();
        assert!((by_number - f.utility(&world.rows[5])).abs() < 1e-12);
        assert!(!b.check(f.utility(&world.rows[5]) + 1e-3));
    }
}
