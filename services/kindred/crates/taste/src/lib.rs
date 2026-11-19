//! The per-user taste model of `03-engine.md`: a Bayesian linear preference model over book
//! vectors.
//!
//! Every work has a vector `phi` (bias, collaborative block, aspect block, meta block; see
//! [`Layout`]). A user is a weight vector `w`, never stored: [`fit`] recomputes it from that
//! user's own [`Event`]s and the world, as the posterior mode `w_hat` with covariance `Sigma`.
//! The model's opinion of a work is the utility `phi · w`. From the fit come scores
//! ([`Fit::score`], [`Fit::score_rows`]), the fit band of a book page ([`Fit::band`]), the exact
//! breakdown of a score into the user's own rows ([`Fit::breakdown`]), the taste profile
//! ([`Fit::profile`]), the next comparison question ([`next_comparison`]) and the three picks
//! ([`picks`]).
//!
//! The crate is pure: no I/O, no clock, no global state, and randomness only through a seeded
//! generator the caller passes in. Why: the benchmark and the service must run identical code,
//! and every result must be reproducible from its inputs.

mod choose;
mod fit;
mod observe;

pub use choose::{
    Candidate, Picks, Rated, SeriesNext, context_aspects, cosine, ctx, have_you_read, lighter,
    next_comparison, novelty, picks, seed, shorter_page_limit, stretch,
};
pub use fit::{AspectWeight, Band, Breakdown, Fit, Origin, Profile, fit};
pub use observe::{Event, Feedback, ProfileChoice, StopReason, Verdict};

use std::ops::Range;

/// A work, as the caller's dense row in its vector table. The crate never interprets it beyond
/// asking [`Vectors`] for the work's vector and handing it back in results.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct Row(pub u32);

/// The shape of a book vector: `[ 1 | c (k) | a (a) | m (4) ]`, in this order (§2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Layout {
    /// Size of the collaborative block.
    pub k: usize,
    /// Number of aspects.
    pub a: usize,
}

impl Layout {
    /// Size of the meta block: quality, popularity, length, age.
    pub const META: usize = 4;

    /// Length of a whole vector.
    pub fn d(&self) -> usize {
        1 + self.k + self.a + Self::META
    }

    /// Index of the bias number, always 1 in `phi`.
    pub fn bias(&self) -> usize {
        0
    }

    /// The collaborative block.
    pub fn c(&self) -> Range<usize> {
        1..1 + self.k
    }

    /// The aspect block; aspect `k` sits at `a().start + k`.
    pub fn a(&self) -> Range<usize> {
        1 + self.k..1 + self.k + self.a
    }

    /// The meta block.
    pub fn m(&self) -> Range<usize> {
        1 + self.k + self.a..self.d()
    }

    /// Index of aspect `k` in a whole vector.
    pub fn aspect(&self, k: usize) -> usize {
        debug_assert!(k < self.a, "aspect {k} out of 0..{}", self.a);
        1 + self.k + k
    }
}

/// Every number `03-engine.md` calls a hyperparameter or constant of the fit. The world's
/// `world.toml` sets them (the benchmark chooses the values); [`Default`] is the document's
/// defaults.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Hyper {
    /// Prior variance of the bias weight.
    pub var_bias: f64,
    /// Prior variance of each collaborative weight.
    pub var_c: f64,
    /// Prior variance of each aspect weight.
    pub var_a: f64,
    /// Prior variance of each meta weight.
    pub var_m: f64,
    /// Noise of a verdict (and of a pick or reread term, which compare utility to `b2` too).
    pub tau_v: f64,
    /// Noise of a comparison.
    pub tau_c: f64,
    /// Weight of each of a verdict's three terms.
    pub omega_v: f64,
    /// Weight of a chip.
    pub omega_h: f64,
    /// Extra score variance of a work whose collaborative block came from its text (§3).
    pub text_var: f64,
    /// Length of a text-predicted collaborative block (§3); used by the world build.
    pub text_shrink: f64,
    /// Smallest `info * memory` worth a comparison question (§7).
    pub ask_min: f64,
    /// Strength of the context term `ctx` and of the Lighter steer (§8).
    pub beta: f64,
    /// Utility threshold between `not_for_me` and `okay`.
    pub b1: f64,
    /// Utility threshold between `okay` and `liked`.
    pub b2: f64,
    /// Utility threshold between `liked` and `loved`.
    pub b3: f64,
}

impl Default for Hyper {
    fn default() -> Self {
        Self {
            var_bias: 4.0,
            var_c: 2.25,
            var_a: 0.0225,
            var_m: 0.25,
            tau_v: 1.0,
            tau_c: 0.5,
            omega_v: 0.5,
            omega_h: 0.5,
            text_var: 0.25,
            text_shrink: 0.7,
            ask_min: 0.05,
            beta: 0.5,
            b1: -1.0,
            b2: 0.0,
            b3: 1.0,
        }
    }
}

/// The prior mean of the meta block (quality, popularity, length, age), fitted once by the
/// benchmark on its training users and stored in the world's manifest. Every other prior mean
/// is zero. Why: with no data the model should prefer well-rated, widely read books.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Prior {
    pub m0_meta: [f64; 4],
}

/// What the model needs from the world: each work's vector and where it came from.
pub trait Vectors {
    fn layout(&self) -> Layout;
    /// The work's vector, `layout().d()` numbers; `None` when the work has none, in which case
    /// nothing the user did with it produces a term.
    fn phi(&self, work: Row) -> Option<&[f32]>;
    /// Whether the work's collaborative block was predicted from its text (§3): its score
    /// variance gets `text_var` added.
    fn text_sourced(&self, work: Row) -> bool;
}

/// `sigma(z) = 1 / (1 + exp(-z))`, without overflow for any finite `z`.
pub fn sigmoid(z: f64) -> f64 {
    if z >= 0.0 {
        1.0 / (1.0 + (-z).exp())
    } else {
        let e = z.exp();
        e / (1.0 + e)
    }
}

/// `ln sigma(z)`, finite for every finite `z`: for very negative `z` it is about `z`, not
/// `ln 0`. Why: one confidently wrong observation must cost a large finite amount, not break the
/// fit (§11 test 7).
pub fn ln_sigmoid(z: f64) -> f64 {
    if z >= 0.0 {
        -(-z).exp().ln_1p()
    } else {
        z - z.exp().ln_1p()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_blocks_tile_the_vector() {
        let l = Layout { k: 64, a: 50 };
        assert_eq!(l.d(), 119);
        assert_eq!(l.bias(), 0);
        assert_eq!(l.c(), 1..65);
        assert_eq!(l.a(), 65..115);
        assert_eq!(l.m(), 115..119);
        assert_eq!(l.aspect(3), 68);
    }

    #[test]
    fn log_sigmoid_is_finite_and_accurate() {
        for z in [
            -1e300, -1e6, -800.0, -40.0, -1.0, 0.0, 1.0, 40.0, 800.0, 1e6, 1e300,
        ] {
            let v = ln_sigmoid(z);
            assert!(v.is_finite() && v <= 0.0, "ln_sigmoid({z}) = {v}");
            let s = sigmoid(z);
            assert!(
                s.is_finite() && (0.0..=1.0).contains(&s),
                "sigmoid({z}) = {s}"
            );
        }
        assert!((ln_sigmoid(-1000.0) - -1000.0).abs() < 1e-9);
        assert!((ln_sigmoid(0.0) - 0.5f64.ln()).abs() < 1e-15);
        assert!((ln_sigmoid(2.0) - sigmoid(2.0).ln()).abs() < 1e-15);
        assert!((sigmoid(-3.0) + sigmoid(3.0) - 1.0).abs() < 1e-15);
    }

    #[test]
    fn hyper_missing_fields_keep_their_defaults() {
        let h = Hyper::default();
        let partial: Hyper =
            serde::Deserialize::deserialize(serde::de::value::MapDeserializer::<
                _,
                serde::de::value::Error,
            >::new([("tau_c", 0.25f64)].into_iter()))
            .unwrap();
        assert_eq!(partial.tau_c, 0.25);
        assert_eq!(partial.var_c, h.var_c);
    }
}
