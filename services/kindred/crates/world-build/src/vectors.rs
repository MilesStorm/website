//! Stage vectors (03-engine.md §2–§3, 02-world-data.md §4): the book vector of every searchable
//! work that can be placed, `phi = [1 | c | a | m]`, where each block came from, and which
//! works are recommendable. Twice: for the world, and for the cold-book test (variant `cold`).
//!
//! - `c`: the work's factors at unit length; without factors, the text map's prediction at unit
//!   length times `text_shrink`. A work with neither has no vector.
//! - `a`: the shelf-and-subject numbers; without them, the text map's prediction (clipped to ±3);
//!   with neither, zeros.
//! - `m`: z-scores over recommendable works of quality, popularity, ln(pages) and first year,
//!   clipped to ±3, 0 when unknown.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::matrix::Matrix;
use crate::stages::Run;
use crate::{factors, text_maps};

/// Bits of a work's flags in `vectors-flags.bin` (the low two as world::reader::source).
pub const C_FROM_TEXT: u8 = world::reader::source::C_FROM_TEXT;
pub const A_FROM_TEXT: u8 = world::reader::source::A_FROM_TEXT;
pub const RECOMMENDABLE: u8 = 4;

pub fn path(model: &Path, cold: bool) -> PathBuf {
    model.join(if cold {
        "vectors-cold.bin"
    } else {
        "vectors.bin"
    })
}

fn flags_path(model: &Path, cold: bool) -> PathBuf {
    model.join(if cold {
        "vectors-flags-cold.bin"
    } else {
        "vectors-flags.bin"
    })
}

pub fn report_path(model: &Path) -> PathBuf {
    model.join("vectors.json")
}

/// The vectors of one variant: `phi` rows, and per row its flags.
pub struct Vectors {
    pub layout: taste::Layout,
    pub phi: Matrix,
    pub flags: Vec<u8>,
}

impl Vectors {
    pub fn read(model: &Path, cold: bool, layout: taste::Layout) -> anyhow::Result<Vectors> {
        let phi = Matrix::read(&path(model, cold))?;
        let flags = Matrix::read(&flags_path(model, cold))?;
        anyhow::ensure!(
            phi.dim == layout.d() && flags.keys == phi.keys,
            "the vector files do not match"
        );
        Ok(Vectors {
            layout,
            flags: flags.data.iter().map(|&f| f as u8).collect(),
            phi,
        })
    }

    fn write(&self, model: &Path, cold: bool) -> anyhow::Result<()> {
        self.phi.write(&path(model, cold))?;
        let mut flags = Matrix::new(1);
        for (key, &f) in self.phi.keys.iter().zip(&self.flags) {
            flags.push(*key, &[f32::from(f)]);
        }
        flags.write(&flags_path(model, cold))
    }

    pub fn recommendable(&self, i: usize) -> bool {
        self.flags[i] & RECOMMENDABLE != 0
    }

    pub fn text_sourced(&self, i: usize) -> bool {
        self.flags[i] & C_FROM_TEXT != 0
    }
}

/// Counts for the build report.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Counts {
    pub with_vector: usize,
    pub c_from_interactions: usize,
    pub c_from_text: usize,
    pub a_from_shelves: usize,
    pub a_from_text: usize,
    pub a_missing: usize,
    pub recommendable: usize,
    pub recommendable_c_from_text: usize,
}

/// What the catalogue says about a work, for the meta block and the recommendable test.
struct Facts {
    work: u64,
    popularity: f64,
    quality: f64,
    pages: Option<f64>,
    year: Option<f64>,
    cover: bool,
    not_a_book: bool,
}

pub fn layout(run: &Run) -> taste::Layout {
    taste::Layout {
        k: run.config.factors.k,
        a: run.aspects.aspect.len(),
    }
}

pub fn run(run: &Run) -> anyhow::Result<()> {
    let facts = facts(run)?;
    let aspects = Matrix::read(&text_maps::aspects_path(&run.model))?;
    let cold = text_maps::cold_works(run)?;
    let mut counts = serde_json::Map::new();
    for variant in [false, true] {
        let raw_c = Matrix::read(&factors::path(&run.model, variant))?;
        let c_text = Matrix::read(&text_maps::c_text_path(&run.model, variant))?;
        let a_text = Matrix::read(&text_maps::a_text_path(&run.model, variant))?;
        let (vectors, n) = assemble(
            layout(run),
            &facts,
            &raw_c,
            &c_text,
            &aspects,
            &a_text,
            &|key| variant && cold.contains(&key),
            run.config.model.text_shrink,
            run.config.vectors.min_popularity,
        );
        vectors.write(&run.model, variant)?;
        let name = if variant { "cold" } else { "normal" };
        tracing::info!(variant = name, counts = ?n, "vectors assembled");
        counts.insert(name.into(), serde_json::to_value(&n)?);
    }
    std::fs::write(report_path(&run.model), serde_json::to_vec_pretty(&counts)?)?;
    Ok(())
}

fn facts(run: &Run) -> anyhow::Result<Vec<Facts>> {
    let mut statement = run.duck.conn().prepare(
        "SELECT c.work, p.popularity, p.quality, c.pages, c.year, c.cover IS NOT NULL, c.not_a_book
         FROM catalogue c JOIN popularity p ON p.work = c.work ORDER BY c.work",
    )?;
    let rows = statement.query_map([], |r| {
        Ok(Facts {
            work: r.get::<_, i64>(0)? as u64,
            popularity: r.get(1)?,
            quality: r.get(2)?,
            pages: r
                .get::<_, Option<i64>>(3)?
                .filter(|&p| p > 0)
                .map(|p| p as f64),
            year: r.get::<_, Option<i64>>(4)?.map(|y| y as f64),
            cover: r.get(5)?,
            not_a_book: r.get(6)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn unit(v: &[f32], scale: f32) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        v.iter().map(|x| x / norm * scale).collect()
    } else {
        v.to_vec()
    }
}

/// Mean and standard deviation of the values present.
fn stats(values: impl Iterator<Item = f64>) -> (f64, f64) {
    let (mut n, mut sum, mut sq) = (0.0, 0.0, 0.0);
    for v in values {
        n += 1.0;
        sum += v;
        sq += v * v;
    }
    if n < 2.0 {
        return (0.0, 1.0);
    }
    let mean = sum / n;
    let sd = (sq / n - mean * mean).max(0.0).sqrt();
    (mean, if sd > 0.0 { sd } else { 1.0 })
}

#[allow(clippy::too_many_arguments, reason = "one stage's inputs, named")]
fn assemble(
    layout: taste::Layout,
    facts: &[Facts],
    raw_c: &Matrix,
    c_text: &Matrix,
    aspects: &Matrix,
    a_text: &Matrix,
    excluded: &dyn Fn(u64) -> bool,
    text_shrink: f64,
    min_popularity: f32,
) -> (Vectors, Counts) {
    let mut counts = Counts::default();
    // First pass: c, a and the recommendable test.
    let mut placed: Vec<(usize, Vec<f32>, Vec<f32>, u8)> = Vec::new();
    for (i, f) in facts.iter().enumerate() {
        let (c, mut flags) = match (raw_c.get(f.work), c_text.get(f.work)) {
            (Some(c), _) => (unit(c, 1.0), 0),
            (None, Some(c)) => (unit(c, text_shrink as f32), C_FROM_TEXT),
            (None, None) => continue,
        };
        let a = match (
            aspects.get(f.work).filter(|_| !excluded(f.work)),
            a_text.get(f.work),
        ) {
            (Some(a), _) => {
                counts.a_from_shelves += 1;
                a.to_vec()
            }
            (None, Some(a)) => {
                counts.a_from_text += 1;
                flags |= A_FROM_TEXT;
                a.to_vec()
            }
            (None, None) => {
                counts.a_missing += 1;
                vec![0.0; layout.a]
            }
        };
        if f.cover && !f.not_a_book && f.popularity >= f64::from(min_popularity) {
            flags |= RECOMMENDABLE;
        }
        placed.push((i, c, a, flags));
    }
    // The meta block's scales, over recommendable works.
    let rec = || {
        placed
            .iter()
            .filter(|p| p.3 & RECOMMENDABLE != 0)
            .map(|p| &facts[p.0])
    };
    let scales = [
        stats(rec().map(|f| f.quality)),
        stats(rec().map(|f| f.popularity)),
        stats(rec().filter_map(|f| f.pages.map(f64::ln))),
        stats(rec().filter_map(|f| f.year)),
    ];
    let z = |value: Option<f64>, (mean, sd): (f64, f64)| {
        value.map_or(0.0, |v| ((v - mean) / sd).clamp(-3.0, 3.0)) as f32
    };
    let mut phi = Matrix::new(layout.d());
    let mut all_flags = Vec::with_capacity(placed.len());
    let mut row = vec![0f32; layout.d()];
    for (i, c, a, flags) in placed {
        let f = &facts[i];
        row[layout.bias()] = 1.0;
        row[layout.c()].copy_from_slice(&c);
        row[layout.a()].copy_from_slice(&a);
        let m = [
            z(Some(f.quality), scales[0]),
            z(Some(f.popularity), scales[1]),
            z(f.pages.map(f64::ln), scales[2]),
            z(f.year, scales[3]),
        ];
        row[layout.m()].copy_from_slice(&m);
        phi.push(f.work, &row);
        counts.with_vector += 1;
        if flags & C_FROM_TEXT != 0 {
            counts.c_from_text += 1;
        } else {
            counts.c_from_interactions += 1;
        }
        if flags & RECOMMENDABLE != 0 {
            counts.recommendable += 1;
            if flags & C_FROM_TEXT != 0 {
                counts.recommendable_c_from_text += 1;
            }
        }
        all_flags.push(flags);
    }
    (
        Vectors {
            layout,
            phi,
            flags: all_flags,
        },
        counts,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matrix(dim: usize, rows: &[(u64, Vec<f32>)]) -> Matrix {
        let mut m = Matrix::new(dim);
        for (k, r) in rows {
            m.push(*k, r);
        }
        m
    }

    #[test]
    fn blocks_come_from_the_right_source() {
        let layout = taste::Layout { k: 2, a: 1 };
        let facts: Vec<Facts> = [1u64, 2, 3, 4]
            .iter()
            .map(|&work| Facts {
                work,
                popularity: work as f64 * 2.0,
                quality: 4.0,
                pages: Some(300.0),
                year: None,
                cover: work != 2,
                not_a_book: false,
            })
            .collect();
        let raw_c = matrix(2, &[(1, vec![3.0, 4.0]), (4, vec![0.0, 2.0])]);
        let c_text = matrix(2, &[(2, vec![1.0, 0.0]), (3, vec![0.0, 5.0])]);
        let aspects = matrix(1, &[(1, vec![2.0]), (3, vec![1.5])]);
        let a_text = matrix(1, &[(2, vec![-1.0]), (3, vec![0.2])]);
        let (v, n) = assemble(
            layout,
            &facts,
            &raw_c,
            &c_text,
            &aspects,
            &a_text,
            &|k| k == 3,
            0.5,
            3.0,
        );
        assert_eq!(v.phi.keys, vec![1, 2, 3, 4]);
        // Work 1: unit factors, its own aspects; popularity 2 < 3: not recommendable.
        assert_eq!(&v.phi.row(0)[..4], &[1.0, 0.6, 0.8, 2.0]);
        assert_eq!(v.flags[0], 0);
        // Work 2: c from text, shrunk; a from text; no cover.
        assert_eq!(&v.phi.row(1)[..4], &[1.0, 0.5, 0.0, -1.0]);
        assert_eq!(v.flags[1], C_FROM_TEXT | A_FROM_TEXT);
        // Work 3: excluded from its shelf aspects (the cold variant): a from text.
        assert_eq!(v.phi.row(2)[3], 0.2);
        assert_eq!(v.flags[2], C_FROM_TEXT | A_FROM_TEXT | RECOMMENDABLE);
        // Work 4: factors, no aspects at all.
        assert_eq!(v.phi.row(3)[3], 0.0);
        assert_eq!(n.a_missing, 1);
        assert_eq!(n.recommendable, 2);
        // Meta: pages identical → z 0; unknown year → 0.
        assert_eq!(&v.phi.row(3)[4..], &[0.0, 1.0, 0.0, 0.0]);
    }
}
