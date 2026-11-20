//! The baselines of 04-benchmark.md §5 that are not the taste model: content nearest (C),
//! item-to-item (I) and plain matrix-factorization fold-in (MF). The taste model's methods
//! (P, FULL and its ablations) call crate `taste` directly (bench/mod.rs).

use std::collections::{HashMap, HashSet};

use nalgebra::{DMatrix, DVector};
use rayon::prelude::*;

use super::data::{Likes, Space};
use crate::matrix::Matrix;

/// C: cosine between a work's text embedding and the mean embedding of the user's liked
/// revealed works. Embeddings are unit length, so this is a dot product with the normalised mean.
pub struct Content<'a> {
    pub embeddings: &'a Matrix,
    /// Per space row, its embedding row (`u32::MAX`: none).
    pub of_row: Vec<u32>,
}

impl<'a> Content<'a> {
    pub fn new(space: &Space, embeddings: &'a Matrix) -> Content<'a> {
        let of_row = space
            .phi
            .keys
            .iter()
            .map(|&k| embeddings.find(k).map_or(u32::MAX, |i| i as u32))
            .collect();
        Content { embeddings, of_row }
    }

    /// The query: the normalised mean embedding of `liked` rows; `None` without any.
    pub fn query(&self, liked: &[u32]) -> Option<Vec<f32>> {
        let mut q = vec![0f32; self.embeddings.dim];
        let mut n = 0;
        for &r in liked {
            let e = self.of_row[r as usize];
            if e != u32::MAX {
                q.iter_mut()
                    .zip(self.embeddings.row(e as usize))
                    .for_each(|(q, x)| *q += x);
                n += 1;
            }
        }
        let norm = q.iter().map(|x| x * x).sum::<f32>().sqrt();
        (n > 0 && norm > 0.0).then(|| q.iter().map(|x| x / norm).collect())
    }

    pub fn score(&self, query: &[f32], row: u32) -> f32 {
        let e = self.of_row[row as usize];
        if e == u32::MAX {
            return f32::NEG_INFINITY;
        }
        self.embeddings
            .row(e as usize)
            .iter()
            .zip(query)
            .map(|(a, b)| a * b)
            .sum()
    }
}

/// I: each work's nearest works by cosine of co-like counts over the training users,
/// `co(i, j) / sqrt(n_i n_j)`, keeping `keep` neighbours per work.
pub struct Neighbours {
    pub of: HashMap<u64, Vec<(u64, f32)>>,
}

impl Neighbours {
    /// Neighbours of the works in `queries` (the ones the measured users rated).
    pub fn build(likes: &Likes, queries: &HashSet<u64>, keep: usize) -> Neighbours {
        let works: Vec<u64> = likes.by_work.keys().copied().collect();
        let index: HashMap<u64, u32> = works
            .iter()
            .enumerate()
            .map(|(i, &w)| (w, i as u32))
            .collect();
        let count: Vec<f32> = works
            .iter()
            .map(|w| likes.by_work[w].len() as f32)
            .collect();
        let queries: Vec<u64> = queries
            .iter()
            .copied()
            .filter(|q| index.contains_key(q))
            .collect();
        let of = queries
            .par_iter()
            .map_init(
                || (vec![0u32; works.len()], Vec::<u32>::new()),
                |(co, touched), &q| {
                    let qi = index[&q];
                    for &u in &likes.by_work[&q] {
                        for w in &likes.by_user[u as usize] {
                            let j = index[w];
                            if j != qi {
                                if co[j as usize] == 0 {
                                    touched.push(j);
                                }
                                co[j as usize] += 1;
                            }
                        }
                    }
                    let nq = count[qi as usize];
                    let mut sims: Vec<(u64, f32)> = touched
                        .iter()
                        .map(|&j| {
                            let s = co[j as usize] as f32 / (nq * count[j as usize]).sqrt();
                            (works[j as usize], s)
                        })
                        .collect();
                    for &j in touched.iter() {
                        co[j as usize] = 0;
                    }
                    touched.clear();
                    if sims.len() > keep {
                        sims.select_nth_unstable_by(keep - 1, |a, b| {
                            b.1.total_cmp(&a.1).then(a.0.cmp(&b.0))
                        });
                        sims.truncate(keep);
                    }
                    // `touched` can be most of the works: give back what truncate keeps, or
                    // every query holds megabytes (tens of GB over the test users' queries).
                    sims.shrink_to_fit();
                    (q, sims)
                },
            )
            .collect();
        Neighbours { of }
    }

    /// `score(i) = sum over liked j of sim(i, j) - sum over not-for-me j of sim(i, j)`, as a
    /// sparse map from space row to score; every other work scores 0.
    pub fn scores(&self, space: &Space, liked: &[u64], disliked: &[u64]) -> HashMap<u32, f32> {
        let mut out: HashMap<u32, f32> = HashMap::new();
        for (works, sign) in [(liked, 1.0f32), (disliked, -1.0)] {
            for j in works {
                for &(i, s) in self.of.get(j).map_or(&[][..], Vec::as_slice) {
                    if let Some(r) = space.row_of(i) {
                        *out.entry(r).or_default() += sign * s;
                    }
                }
            }
        }
        out
    }
}

/// The neighbour lists by row of one space, for the pick-up term (`taste::Pickup`): works
/// without a row, and neighbours without one, are dropped.
pub struct RowLists(Vec<Vec<(taste::Row, f32)>>);

impl RowLists {
    pub fn new(space: &Space, neighbours: &Neighbours) -> RowLists {
        let mut lists = vec![Vec::new(); space.phi.len()];
        for (&work, list) in &neighbours.of {
            if let Some(r) = space.row_of(work) {
                lists[r as usize] = list
                    .iter()
                    .filter_map(|&(other, s)| space.row_of(other).map(|o| (taste::Row(o), s)))
                    .collect();
            }
        }
        RowLists(lists)
    }

    pub fn get(&self, row: taste::Row) -> &[(taste::Row, f32)] {
        self.0.get(row.0 as usize).map_or(&[], Vec::as_slice)
    }
}

/// MF: the user's vector in factor space by ridge regression of the revealed works' targets (1
/// for liked or loved, 0 otherwise) on their collaborative blocks; a work's score is that
/// vector times its collaborative block.
pub fn fold_in(space: &Space, revealed: &[(u32, bool)], ridge: f64) -> Option<Vec<f32>> {
    if !revealed.iter().any(|&(_, liked)| liked) {
        return None;
    }
    let k = space.layout.k;
    let mut a = DMatrix::<f64>::identity(k, k) * ridge;
    let mut b = DVector::<f64>::zeros(k);
    for &(row, liked) in revealed {
        let c = DVector::from_iterator(k, space.c(row).iter().map(|&x| f64::from(x)));
        a += &c * c.transpose();
        if liked {
            b += &c;
        }
    }
    let x = a.cholesky()?.solve(&b);
    Some(x.iter().map(|&v| v as f32).collect())
}

pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn co_like_cosine() {
        // Users 0 and 1 like works 10 and 20; user 2 likes 20 and 30.
        let mut by_work: HashMap<u64, Vec<u32>> = HashMap::new();
        let by_user = vec![vec![10, 20], vec![10, 20], vec![20, 30]];
        for (u, works) in by_user.iter().enumerate() {
            for &w in works {
                by_work.entry(w).or_default().push(u as u32);
            }
        }
        let likes = Likes { by_work, by_user };
        let n = Neighbours::build(&likes, &[10u64, 20].into_iter().collect(), 10);
        let of10: HashMap<u64, f32> = n.of[&10].iter().copied().collect();
        // co(10,20) = 2, n_10 = 2, n_20 = 3.
        assert!((of10[&20] - 2.0 / 6f32.sqrt()).abs() < 1e-6);
        assert!(!of10.contains_key(&30));
        let n1 = Neighbours::build(&likes, &[20u64].into_iter().collect(), 1);
        assert_eq!(n1.of[&20].len(), 1);
        assert_eq!(n1.of[&20][0].0, 10, "the closest one is kept");
    }
}
