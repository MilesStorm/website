//! How likely a reader is to pick a work up (`03-engine.md` §8, "Pick-up").
//!
//! The fitted utility `mu(i)` answers "how much would this reader like `i` if they read it". A
//! recommendation must also be something they would read at all: among the books people read,
//! popularity barely predicts the rating, yet it strongly predicts what they read next. The
//! pick-up term adds that, from two sources:
//! - readers who liked this reader's liked works also liked `i` (the neighbour lists the world
//!   ships: cosine of co-like counts, each work's nearest works), minus the same for
//!   `not_for_me` works;
//! - the work's popularity z-score (the meta block's second number).
//!
//! `rank(i) = mu(i) + pickup * (s(i) / max_j s(j) + pickup_pop * popularity_z(i))`, where `s` is
//! the neighbour sum. Dividing by the largest `s` keeps the term on the same scale for a reader
//! with five ratings and one with five hundred.

use std::collections::HashMap;

use crate::{Hyper, Layout, Row};

/// One of the reader's verdicts, as the pick-up term reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lean {
    /// Liked or loved: its neighbours count for.
    Toward,
    /// Not for me: its neighbours count against.
    Away,
}

/// The neighbour sums of one reader, normalised so that the largest is 1.
#[derive(Clone, Debug, Default)]
pub struct Pickup {
    s: HashMap<Row, f64>,
    /// `1 / max_j s(j)`, for [`Pickup::share`].
    scale: f64,
}

impl Pickup {
    /// `reads` are the reader's works with a lean; `neighbours(w)` is `w`'s neighbour list (work,
    /// similarity), empty for a work without one.
    pub fn new<'a>(reads: &[(Row, Lean)], neighbours: impl Fn(Row) -> &'a [(Row, f32)]) -> Pickup {
        let mut s: HashMap<Row, f64> = HashMap::new();
        for &(work, lean) in reads {
            let sign = match lean {
                Lean::Toward => 1.0,
                Lean::Away => -1.0,
            };
            for &(other, sim) in neighbours(work) {
                *s.entry(other).or_default() += sign * f64::from(sim);
            }
        }
        let max = s.values().copied().fold(0.0f64, f64::max);
        let scale = if max > 0.0 { 1.0 / max } else { 0.0 };
        s.values_mut().for_each(|v| *v *= scale);
        Pickup { s, scale }
    }

    /// `s(i) / max_j s(j)`: 0 for a work no read work points to, and for every work when no
    /// liked work has neighbours.
    pub fn get(&self, work: Row) -> f64 {
        self.s.get(&work).copied().unwrap_or(0.0)
    }

    /// The works with a nonzero neighbour sum.
    pub fn iter(&self) -> impl Iterator<Item = (Row, f64)> + '_ {
        self.s.iter().map(|(&r, &v)| (r, v))
    }

    /// The part of `get(i)` that read work `j` contributes, given `sim(j, i)` and `j`'s lean:
    /// the per-work split behind "Because you loved X" (§8 reasons).
    pub fn share(&self, sim: f32, lean: Lean) -> f64 {
        let sign = match lean {
            Lean::Toward => 1.0,
            Lean::Away => -1.0,
        };
        sign * f64::from(sim) * self.scale
    }
}

/// The popularity z-score of a work's vector: the meta block's second number.
pub fn popularity_z(layout: Layout, phi: &[f32]) -> f64 {
    f64::from(phi[layout.m().start + 1])
}

/// What `rank(i)` adds to `mu(i)`.
pub fn bonus(hyper: &Hyper, pickup: f64, popularity_z: f64) -> f64 {
    hyper.pickup * (pickup + hyper.pickup_pop * popularity_z)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_normalise_to_one_and_dislikes_subtract() {
        let lists: HashMap<Row, Vec<(Row, f32)>> = [
            (Row(1), vec![(Row(10), 0.4), (Row(11), 0.2)]),
            (Row(2), vec![(Row(10), 0.4), (Row(12), 0.1)]),
            (Row(3), vec![(Row(11), 0.2)]),
        ]
        .into_iter()
        .collect();
        let get = |w: Row| lists.get(&w).map_or(&[][..], Vec::as_slice);
        let p = Pickup::new(
            &[
                (Row(1), Lean::Toward),
                (Row(2), Lean::Toward),
                (Row(3), Lean::Away),
            ],
            get,
        );
        assert!((p.get(Row(10)) - 1.0).abs() < 1e-12);
        assert!(p.get(Row(11)).abs() < 1e-12, "0.2 for, 0.2 against");
        assert!((p.get(Row(12)) - 0.125).abs() < 1e-6);
        assert_eq!(p.get(Row(99)), 0.0);
        // The shares of Row(10) add up to its value.
        assert!((p.share(0.4, Lean::Toward) * 2.0 - p.get(Row(10))).abs() < 1e-6);
    }

    #[test]
    fn no_liked_neighbours_means_no_term() {
        let p = Pickup::new(&[(Row(1), Lean::Away)], |_| &[(Row(5), 0.3)][..]);
        assert_eq!(p.get(Row(5)), 0.0);
        let h = Hyper {
            pickup: 2.0,
            pickup_pop: 0.5,
            ..Hyper::default()
        };
        assert!((bonus(&h, 0.5, 1.0) - 2.0).abs() < 1e-12);
    }
}
