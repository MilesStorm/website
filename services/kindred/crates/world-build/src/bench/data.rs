//! What the benchmark reads: one variant of the book vectors as the taste model sees them
//! ([`Space`]), the benchmark users' ratings in time order ([`User`]), the text embeddings for
//! the content baseline, and the training likes for the item-to-item baseline.

use std::collections::HashMap;
use std::path::Path;

use crate::matrix::Matrix;
use crate::trace::Duck;
use crate::vectors::{self, Vectors};

/// One variant of the vectors, rows numbered as `taste::Row`.
pub struct Space {
    pub layout: taste::Layout,
    pub phi: Matrix,
    pub flags: Vec<u8>,
    /// The recommendable rows, ascending.
    pub cand: Vec<u32>,
    /// Their vectors, packed: `cand.len() * d` numbers, for `Fit::score_rows`.
    pub cand_phi: Vec<f32>,
    /// Per row: its popularity's percentile among recommendable works (1 = most popular, 0 for
    /// works that are not recommendable).
    pub pop_pct: Vec<f32>,
}

impl Space {
    pub fn load(model: &Path, cold: bool, layout: taste::Layout) -> anyhow::Result<Space> {
        let v = Vectors::read(model, cold, layout)?;
        Ok(Space::new(v))
    }

    pub fn new(v: Vectors) -> Space {
        let layout = v.layout;
        let cand: Vec<u32> = (0..v.phi.len())
            .filter(|&i| v.recommendable(i))
            .map(|i| i as u32)
            .collect();
        let mut cand_phi = Vec::with_capacity(cand.len() * layout.d());
        for &r in &cand {
            cand_phi.extend_from_slice(v.phi.row(r as usize));
        }
        // Popularity is the meta block's second number.
        let pop = layout.m().start + 1;
        let mut order: Vec<u32> = cand.clone();
        order.sort_by(|&a, &b| v.phi.row(b as usize)[pop].total_cmp(&v.phi.row(a as usize)[pop]));
        let mut pop_pct = vec![0.0f32; v.phi.len()];
        for (rank, &r) in order.iter().enumerate() {
            pop_pct[r as usize] = 1.0 - rank as f32 / order.len().max(1) as f32;
        }
        Space {
            layout,
            phi: v.phi,
            flags: v.flags,
            cand,
            cand_phi,
            pop_pct,
        }
    }

    /// The same vectors with the text-placed collaborative blocks rescaled from the length the
    /// build gave them (`built`) to `shrink`: tuning `text_shrink` without rebuilding.
    pub fn with_text_shrink(&self, built: f64, shrink: f64) -> Space {
        let mut phi = self.phi.clone();
        let factor = (shrink / built) as f32;
        let c = self.layout.c();
        let d = self.layout.d();
        for (i, &f) in self.flags.iter().enumerate() {
            if f & vectors::C_FROM_TEXT != 0 {
                phi.data[i * d + c.start..i * d + c.end]
                    .iter_mut()
                    .for_each(|x| *x *= factor);
            }
        }
        Space::new(Vectors {
            layout: self.layout,
            phi,
            flags: self.flags.clone(),
        })
    }

    /// The same vectors with some numbers set to zero (an ablation, exact and well conditioned).
    pub fn without(&self, blocks: &[std::ops::Range<usize>]) -> Space {
        let mut phi = self.phi.clone();
        let d = self.layout.d();
        for i in 0..phi.len() {
            for block in blocks {
                phi.data[i * d + block.start..i * d + block.end].fill(0.0);
            }
        }
        Space::new(Vectors {
            layout: self.layout,
            phi,
            flags: self.flags.clone(),
        })
    }

    pub fn row_of(&self, work: u64) -> Option<u32> {
        self.phi.find(work).map(|i| i as u32)
    }

    pub fn vector(&self, row: u32) -> &[f32] {
        self.phi.row(row as usize)
    }

    pub fn c(&self, row: u32) -> &[f32] {
        &self.vector(row)[self.layout.c()]
    }

    pub fn recommendable(&self, row: u32) -> bool {
        self.flags[row as usize] & vectors::RECOMMENDABLE != 0
    }

    pub fn quality_z(&self, row: u32) -> f64 {
        f64::from(self.vector(row)[self.layout.m().start])
    }
}

impl taste::Vectors for Space {
    fn layout(&self) -> taste::Layout {
        self.layout
    }

    fn phi(&self, work: taste::Row) -> Option<&[f32]> {
        ((work.0 as usize) < self.phi.len()).then(|| self.vector(work.0))
    }

    fn text_sourced(&self, work: taste::Row) -> bool {
        self.flags
            .get(work.0 as usize)
            .is_some_and(|f| f & vectors::C_FROM_TEXT != 0)
    }
}

/// One rated work of a benchmark user.
#[derive(Clone, Copy, Debug)]
pub struct Rating {
    pub work: u64,
    pub stars: u8,
    /// Days since 1970, NaN when the date is unknown.
    pub day: f64,
}

#[derive(Clone)]
pub struct User {
    pub id: i32,
    pub tuning: bool,
    pub gate: bool,
    /// In the order the user added them (04-benchmark.md §2).
    pub ratings: Vec<Rating>,
}

/// Which benchmark users.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Users {
    /// The 5,000 test users: the reported numbers.
    Test,
    /// The tuning users: the prior mean and the hyperparameters.
    Tuning,
    /// The test users' fixed gate subset: the monthly gate.
    Gate,
}

/// Ratings with the same date (a library import) are ordered by a seeded hash of the work, not
/// by its key: within such a block revealed and future are then a random split, not "older
/// records first".
pub fn users(duck: &Duck, which: Users, seed: u64) -> anyhow::Result<Vec<User>> {
    let filter = match which {
        Users::Test => "b.role = 'test'",
        Users::Tuning => "b.role = 'tuning'",
        Users::Gate => "b.role = 'test' AND b.gate",
    };
    let mut statement = duck.conn().prepare(&format!(
        "SELECT b.user, b.role = 'tuning', b.gate, r.work, r.rating, epoch(r.added) / 86400.0
         FROM bench_users b JOIN ratings r ON r.user = b.user
         WHERE {filter}
         ORDER BY b.user, r.added NULLS LAST, md5(b.user || ':' || r.work || ':' || {seed}), r.work"
    ))?;
    let mut rows = statement.query([])?;
    let mut out: Vec<User> = Vec::new();
    while let Some(r) = rows.next()? {
        let id: i32 = r.get(0)?;
        if out.last().is_none_or(|u| u.id != id) {
            out.push(User {
                id,
                tuning: r.get(1)?,
                gate: r.get(2)?,
                ratings: Vec::new(),
            });
        }
        out.last_mut().expect("pushed").ratings.push(Rating {
            work: r.get::<_, i64>(3)? as u64,
            stars: r.get::<_, i8>(4)? as u8,
            day: r.get::<_, Option<f64>>(5)?.unwrap_or(f64::NAN),
        });
    }
    Ok(out)
}

/// Stars → verdict (04-benchmark.md §2).
pub fn verdict(stars: u8) -> taste::Verdict {
    match stars {
        5 => taste::Verdict::Loved,
        4 => taste::Verdict::Liked,
        3 => taste::Verdict::Okay,
        _ => taste::Verdict::NotForMe,
    }
}

/// Training likes as work → users and user → works, for the item-to-item baseline.
pub struct Likes {
    pub by_work: HashMap<u64, Vec<u32>>,
    pub by_user: Vec<Vec<u64>>,
}

/// The training users' likes: `train_likes`, or `train_likes_cold` (without works first published
/// from the cold year on) when `cold`.
pub fn likes(duck: &Duck, cold: bool) -> anyhow::Result<Likes> {
    let table = if cold {
        "train_likes_cold"
    } else {
        "train_likes"
    };
    let mut statement = duck.conn().prepare(&format!(
        "SELECT user, work FROM {table} ORDER BY user, work"
    ))?;
    let mut rows = statement.query([])?;
    let mut users: HashMap<i32, u32> = HashMap::new();
    let mut by_user: Vec<Vec<u64>> = Vec::new();
    let mut by_work: HashMap<u64, Vec<u32>> = HashMap::new();
    while let Some(r) = rows.next()? {
        let user: i32 = r.get(0)?;
        let work = r.get::<_, i64>(1)? as u64;
        let next = by_user.len() as u32;
        let u = *users.entry(user).or_insert_with(|| {
            by_user.push(Vec::new());
            next
        });
        by_user[u as usize].push(work);
        by_work.entry(work).or_default().push(u);
    }
    Ok(Likes { by_work, by_user })
}
