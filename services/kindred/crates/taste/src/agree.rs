//! How this reader judged a work's nearest works (`03-engine.md` §8, "Neighbourhood").
//!
//! `mu(i)` places `i` among the reader's verdicts through the work's vector, which other
//! readers' data shaped. With few verdicts, the strongest personal sign of how much a reader will
//! like `i` is how they judged the works most like `i`, each against their own usual verdict (the
//! neighbourhood term of Koren, 2008):
//!
//! `n(i) = sum_j sim(j, i) * (level(j) - mean level) / (shrink + sum_j sim(j, i))`
//!
//! over the reader's works `j` with a verdict whose neighbour list holds `i` (the world's co-like
//! lists, as for the pick-up term). `shrink` pulls `n(i)` toward 0 when little supports it.
//! Levels are 0 for not for me to 3 for loved.

use std::collections::HashMap;

use crate::{Row, Verdict};

/// One reader's neighbourhood sums: for each work, (sum of sim × deviation, sum of sim).
#[derive(Clone, Debug, Default)]
pub struct Agreement {
    sums: HashMap<Row, (f64, f64)>,
}

impl Agreement {
    /// `judged` are the reader's works with a verdict; `neighbours(w)` is `w`'s neighbour list
    /// (work, similarity), empty for a work without one.
    pub fn new<I: IntoIterator<Item = (Row, f32)>>(
        judged: &[(Row, Verdict)],
        neighbours: impl Fn(Row) -> I,
    ) -> Agreement {
        if judged.is_empty() {
            return Agreement::default();
        }
        let mean = judged.iter().map(|&(_, v)| v.level() as f64).sum::<f64>() / judged.len() as f64;
        let mut sums: HashMap<Row, (f64, f64)> = HashMap::new();
        for &(work, verdict) in judged {
            let deviation = verdict.level() as f64 - mean;
            for (other, sim) in neighbours(work) {
                let sim = f64::from(sim);
                if sim <= 0.0 {
                    continue;
                }
                let e = sums.entry(other).or_default();
                e.0 += sim * deviation;
                e.1 += sim;
            }
        }
        Agreement { sums }
    }

    /// `n(i)` with the given shrink; 0 for a work no judged work points to.
    pub fn get(&self, work: Row, shrink: f64) -> f64 {
        self.sums
            .get(&work)
            .map_or(0.0, |&(num, den)| num / (shrink + den))
    }

    /// The works some judged work points to.
    pub fn iter(&self, shrink: f64) -> impl Iterator<Item = (Row, f64)> + '_ {
        self.sums
            .iter()
            .map(move |(&r, &(num, den))| (r, num / (shrink + den)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deviations_from_the_readers_mean_weighted_by_similarity() {
        let lists: HashMap<Row, Vec<(Row, f32)>> = [
            (Row(1), vec![(Row(10), 0.5)]),
            (Row(2), vec![(Row(10), 0.25), (Row(11), 0.5)]),
        ]
        .into_iter()
        .collect();
        let get = |w: Row| lists.get(&w).into_iter().flatten().copied();
        // Levels 3 and 1: mean 2, deviations +1 and -1.
        let a = Agreement::new(&[(Row(1), Verdict::Loved), (Row(2), Verdict::Okay)], get);
        // Work 10: (0.5 * 1 + 0.25 * -1) / (1 + 0.75).
        assert!((a.get(Row(10), 1.0) - 0.25 / 1.75).abs() < 1e-12);
        // Work 11: only the okay work points to it.
        assert!((a.get(Row(11), 0.0) + 1.0).abs() < 1e-12);
        assert_eq!(a.get(Row(99), 1.0), 0.0);
        assert_eq!(Agreement::default().get(Row(10), 1.0), 0.0);
    }
}
