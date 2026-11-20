//! The benchmark (04-benchmark.md): replays Goodreads readers as if they were new users of the
//! site and measures how well each method predicts what they went on to like. Fitting, scoring,
//! question choice and the stretch pick are crate `taste`'s own functions: the measured code is
//! the served code (§1).
//!
//! Three entry points:
//! - stage `prior`: the prior mean of the meta block, from the tuning users (§3);
//! - stage `bench`: the monthly gate on the fixed subset, and the confidence cut points;
//! - `kindred-world bench`: the full report on the test users, or the hyperparameter search on
//!   the tuning users.
//!
//! Outputs are aggregates only: no user and no work appears in them (PLAN.md rule 13).

pub mod data;
pub mod methods;

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::Context as _;
use rand::{Rng as _, SeedableRng as _};
use rand_chacha::ChaCha8Rng;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use taste::{Event, Hyper, Prior, Rated, Row};

use crate::config::Config;
use crate::matrix::Matrix;
use crate::stages::Run;
use crate::trace::Duck;
use crate::vectors;
use data::{Rating, Space, User, Users};
use methods::{Content, Neighbours, RowLists};

pub fn prior_path(model: &Path) -> std::path::PathBuf {
    model.join("prior.json")
}

pub fn gate_path(model: &Path) -> std::path::PathBuf {
    model.join("bench-gate.json")
}

/// The revealed count the gates and the tuning look at.
const N_MAIN: usize = 20;
/// Questions the product asks after logging a book, at most (03-engine.md §7).
const QUESTIONS_PER_LOG: usize = 2;
/// "Logged within three years" for a comparison's memory factor.
const RECENT_DAYS: f64 = 3.0 * 365.25;
/// Ridge strength of the MF fold-in.
const FOLD_IN_RIDGE: f64 = 1.0;
/// Users and works per user sampled for the confidence cut points.
const CUT_USERS: usize = 50;
const CUT_WORKS: usize = 5_000;

/// A seeded generator for one purpose and user: the same numbers every run.
fn rng(seed: u64, parts: &[u64]) -> ChaCha8Rng {
    let mut h = Sha256::new();
    h.update(seed.to_le_bytes());
    for p in parts {
        h.update(p.to_le_bytes());
    }
    ChaCha8Rng::from_seed(h.finalize().into())
}

fn age(now: f64, day: f64) -> f64 {
    if now.is_finite() && day.is_finite() {
        ((now - day) / 365.25).max(0.0)
    } else {
        0.0
    }
}

/// The revealed works that have a vector in `space`, with their ratings.
fn placed(space: &Space, revealed: &[Rating]) -> Vec<(u32, Rating)> {
    revealed
        .iter()
        .filter_map(|r| space.row_of(r.work).map(|row| (row, *r)))
        .collect()
}

fn read_event(row: u32, r: &Rating, now: f64) -> Event {
    Event::Read {
        work: Row(row),
        verdict: Some(data::verdict(r.stars)),
        stopped: None,
        nth_read: 1,
        latest: true,
        age_years: age(now, r.day),
    }
}

/// How comparison questions are chosen in a simulation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Rule {
    None,
    Random,
    /// 03-engine.md §7, `taste::next_comparison`.
    Model,
}

/// Asks about `new` against the earlier works `earlier` by `rule`; answers with stars (more
/// stars is better, equal stars a skip). Returns whether a question was asked.
#[allow(clippy::too_many_arguments, reason = "one simulated question's inputs")]
fn ask(
    space: &Space,
    hyper: &Hyper,
    prior: &Prior,
    events: &mut Vec<Event>,
    asked: &mut HashSet<(u32, u32)>,
    new: (u32, Rating),
    earlier: &[(u32, Rating)],
    rule: Rule,
    now: f64,
    rng: &mut ChaCha8Rng,
) -> bool {
    let key = |a: u32, b: u32| (a.min(b), a.max(b));
    let other = match rule {
        Rule::None => None,
        Rule::Random => {
            let open: Vec<&(u32, Rating)> = earlier
                .iter()
                .filter(|(row, _)| *row != new.0 && !asked.contains(&key(new.0, *row)))
                .collect();
            (!open.is_empty()).then(|| open[rng.random_range(0..open.len())].0)
        }
        Rule::Model => {
            let fit = taste::fit(events, space, hyper, prior);
            let rated: Vec<Rated> = earlier
                .iter()
                .map(|(row, r)| Rated {
                    work: Row(*row),
                    verdict: data::verdict(r.stars),
                    recent: (now - r.day).is_nan() || now - r.day <= RECENT_DAYS,
                })
                .collect();
            taste::next_comparison(&fit, space, hyper, Row(new.0), &rated, &|a, b| {
                asked.contains(&key(a.0, b.0))
            })
            .map(|r| r.0)
        }
    };
    let Some(other) = other else {
        return false;
    };
    asked.insert(key(new.0, other));
    let other_stars = earlier
        .iter()
        .find(|(row, _)| *row == other)
        .map(|(_, r)| r.stars)
        .expect("chosen among the earlier works");
    if other_stars != new.1.stars {
        let (better, worse) = if new.1.stars > other_stars {
            (new.0, other)
        } else {
            (other, new.0)
        };
        events.push(Event::Comparison {
            better: Row(better),
            worse: Row(worse),
            age_years: age(now, new.1.day),
        });
    }
    true
}

/// The product's flow replayed: log each revealed work in order and, after each, ask up to two
/// questions by `rule`. Returns the user's events.
fn replay(
    space: &Space,
    hyper: &Hyper,
    prior: &Prior,
    revealed: &[(u32, Rating)],
    rule: Rule,
    now: f64,
    rng: &mut ChaCha8Rng,
) -> Vec<Event> {
    let mut events = Vec::new();
    let mut asked = HashSet::new();
    for (i, item) in revealed.iter().enumerate() {
        events.push(read_event(item.0, &item.1, now));
        if rule == Rule::None || i == 0 {
            continue;
        }
        for _ in 0..QUESTIONS_PER_LOG {
            if !ask(
                space,
                hyper,
                prior,
                &mut events,
                &mut asked,
                *item,
                &revealed[..i],
                rule,
                now,
                rng,
            ) {
                break;
            }
        }
    }
    events
}

/// Verdicts of all revealed works first, then `count` questions by `rule`, each about a revealed
/// work against the ones logged before it, in logging order, as many rounds as needed
/// (04-benchmark.md §4, question value).
#[allow(clippy::too_many_arguments, reason = "one simulation's inputs")]
fn ask_after(
    space: &Space,
    hyper: &Hyper,
    prior: &Prior,
    revealed: &[(u32, Rating)],
    count: usize,
    rule: Rule,
    now: f64,
    rng: &mut ChaCha8Rng,
) -> Vec<Event> {
    let mut events: Vec<Event> = revealed
        .iter()
        .map(|(row, r)| read_event(*row, r, now))
        .collect();
    let mut asked = HashSet::new();
    let mut done = 0;
    while done < count {
        let before = done;
        for i in 1..revealed.len() {
            if done >= count {
                break;
            }
            if ask(
                space,
                hyper,
                prior,
                &mut events,
                &mut asked,
                revealed[i],
                &revealed[..i],
                rule,
                now,
                rng,
            ) {
                done += 1;
            }
        }
        if done == before {
            break;
        }
    }
    events
}

/// The methods of 04-benchmark.md §5, in report order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum Method {
    P,
    C,
    I,
    MF,
    Full,
    FullWithoutCollaborative,
    FullWithoutAspects,
    FullWithoutComparisons,
}

impl Method {
    pub const ALL: [Method; 8] = [
        Method::P,
        Method::C,
        Method::I,
        Method::MF,
        Method::Full,
        Method::FullWithoutCollaborative,
        Method::FullWithoutAspects,
        Method::FullWithoutComparisons,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Method::P => "P (prior only)",
            Method::C => "C (content nearest)",
            Method::I => "I (item-to-item)",
            Method::MF => "MF (fold-in)",
            Method::Full => "FULL",
            Method::FullWithoutCollaborative => "FULL without collaborative block",
            Method::FullWithoutAspects => "FULL without aspect block",
            Method::FullWithoutComparisons => "FULL without comparisons",
        }
    }
}

/// One method's scores for one user.
enum Scorer {
    /// The taste model: `mu(i)` plus the pick-up term (03-engine.md §8).
    Model(Box<taste::Fit>, taste::Pickup),
    Content(Option<Vec<f32>>),
    Sparse(HashMap<u32, f32>),
    Factor(Option<Vec<f32>>),
}

impl Scorer {
    fn score(&self, space: &Space, content: &Content, hyper: &Hyper, row: u32) -> f32 {
        match self {
            Scorer::Model(fit, pickup) => model_score(fit, pickup, hyper, space, row) as f32,
            Scorer::Content(q) => q.as_ref().map_or(0.0, |q| content.score(q, row)),
            Scorer::Sparse(s) => s.get(&row).copied().unwrap_or(0.0),
            Scorer::Factor(x) => x.as_ref().map_or(0.0, |x| methods::dot(x, space.c(row))),
        }
    }

    /// Scores of every recommendable row, in `space.cand` order.
    fn fill(&self, space: &Space, content: &Content, hyper: &Hyper, out: &mut [f32]) {
        match self {
            Scorer::Model(fit, pickup) => model_rank(fit, pickup, hyper, space, out),
            _ => {
                for (o, &row) in out.iter_mut().zip(&space.cand) {
                    *o = self.score(space, content, hyper, row);
                }
            }
        }
    }
}

/// The taste model's ranking score of one work: `mu(i)` (bias dropped) plus the pick-up term.
fn model_score(
    fit: &taste::Fit,
    pickup: &taste::Pickup,
    hyper: &Hyper,
    space: &Space,
    row: u32,
) -> f64 {
    let phi = space.vector(row);
    fit.score(phi)
        + taste::bonus(
            hyper,
            pickup.get(Row(row)),
            taste::popularity_z(space.layout, phi),
        )
}

/// [`model_score`] of every recommendable row, in `space.cand` order.
fn model_rank(
    fit: &taste::Fit,
    pickup: &taste::Pickup,
    hyper: &Hyper,
    space: &Space,
    out: &mut [f32],
) {
    fit.score_rows(&space.cand_phi, out);
    if hyper.pickup == 0.0 {
        return;
    }
    let d = space.layout.d();
    for (o, phi) in out.iter_mut().zip(space.cand_phi.chunks_exact(d)) {
        *o += taste::bonus(hyper, 0.0, taste::popularity_z(space.layout, phi)) as f32;
    }
    for (row, p) in pickup.iter() {
        if let Ok(i) = space.cand.binary_search(&row.0) {
            out[i] += (hyper.pickup * p) as f32;
        }
    }
}

/// The reader's pick-up neighbour sums from their revealed works: liked or loved count for,
/// not for me against (03-engine.md §8).
fn pickup(lists: Option<&RowLists>, shown: &[(u32, Rating)]) -> taste::Pickup {
    let Some(lists) = lists else {
        return taste::Pickup::default();
    };
    let reads: Vec<(Row, taste::Lean)> = shown
        .iter()
        .filter_map(|(row, r)| match r.stars {
            4 | 5 => Some((Row(*row), taste::Lean::Toward)),
            1 | 2 => Some((Row(*row), taste::Lean::Away)),
            _ => None,
        })
        .collect();
    taste::Pickup::new(&reads, |w| lists.get(w))
}

/// The top `k` recommendable rows by score, excluding `revealed`; ties go to the more popular.
fn top(space: &Space, scores: &[f32], revealed: &HashSet<u32>, k: usize) -> Vec<u32> {
    let mut items: Vec<(f32, u32)> = space
        .cand
        .iter()
        .zip(scores)
        .filter(|(row, _)| !revealed.contains(row))
        .map(|(&row, &s)| (if s.is_nan() { f32::NEG_INFINITY } else { s }, row))
        .collect();
    let order = |a: &(f32, u32), b: &(f32, u32)| {
        b.0.total_cmp(&a.0)
            .then(space.pop_pct[b.1 as usize].total_cmp(&space.pop_pct[a.1 as usize]))
            .then(a.1.cmp(&b.1))
    };
    if items.len() > k {
        items.select_nth_unstable_by(k - 1, order);
        items.truncate(k);
    }
    items.sort_by(order);
    items.into_iter().map(|(_, r)| r).collect()
}

fn ndcg(top: &[u32], relevant: &HashSet<u32>, k: usize) -> f64 {
    let dcg: f64 = top
        .iter()
        .take(k)
        .enumerate()
        .filter(|(_, r)| relevant.contains(r))
        .map(|(i, _)| 1.0 / ((i + 2) as f64).log2())
        .sum();
    let ideal: f64 = (0..relevant.len().min(k))
        .map(|i| 1.0 / ((i + 2) as f64).log2())
        .sum();
    if ideal > 0.0 { dcg / ideal } else { 0.0 }
}

/// Share of pairs of future works with different stars that the scores order correctly (ties
/// count half); `None` without such pairs.
fn pairwise(scored: &[(f32, u8)]) -> Option<f64> {
    let (mut right, mut pairs) = (0.0, 0.0);
    for (i, a) in scored.iter().enumerate() {
        for b in &scored[i + 1..] {
            if a.1 == b.1 {
                continue;
            }
            pairs += 1.0;
            let (hi, lo) = if a.1 > b.1 { (a.0, b.0) } else { (b.0, a.0) };
            right += if hi > lo {
                1.0
            } else if hi == lo {
                0.5
            } else {
                0.0
            };
        }
    }
    (pairs > 0.0).then(|| right / pairs)
}

/// Sums of one method at one `n`, over users.
#[derive(Clone, Debug, Default)]
struct Acc {
    ndcg: f64,
    recall: f64,
    ranked_users: f64,
    pairwise: f64,
    pairwise_users: f64,
    novelty: f64,
    diversity: f64,
    top_users: f64,
    covered: HashSet<u32>,
}

impl Acc {
    fn merge(&mut self, o: Acc) {
        self.ndcg += o.ndcg;
        self.recall += o.recall;
        self.ranked_users += o.ranked_users;
        self.pairwise += o.pairwise;
        self.pairwise_users += o.pairwise_users;
        self.novelty += o.novelty;
        self.diversity += o.diversity;
        self.top_users += o.top_users;
        self.covered.extend(o.covered);
    }

    fn mean_ndcg(&self) -> f64 {
        self.ndcg / self.ranked_users.max(1.0)
    }
}

/// Everything measured, summed over users.
#[derive(Clone, Debug, Default)]
struct Sums {
    /// (method, index of n).
    main: HashMap<(Method, usize), Acc>,
    /// FULL's bands per index of n: (works, relevant works) for Strong, Good, Not sure, Probably not.
    bands: HashMap<usize, [[f64; 2]; 4]>,
    stretch_hits: f64,
    stretch_random_hits: f64,
    stretch_users: f64,
    /// (rule, questions): (NDCG@10 sum, users, pairwise sum, pairwise users).
    questions: HashMap<(Rule, usize), [f64; 4]>,
    /// P, FULL with text-placed vectors, FULL with real vectors: (NDCG@10 sum, users).
    cold: [[f64; 2]; 3],
}

impl Sums {
    fn merge(mut self, o: Sums) -> Sums {
        for (k, v) in o.main {
            self.main.entry(k).or_default().merge(v);
        }
        for (k, v) in o.bands {
            let e = self.bands.entry(k).or_default();
            for b in 0..4 {
                e[b][0] += v[b][0];
                e[b][1] += v[b][1];
            }
        }
        self.stretch_hits += o.stretch_hits;
        self.stretch_random_hits += o.stretch_random_hits;
        self.stretch_users += o.stretch_users;
        for (k, v) in o.questions {
            let e = self.questions.entry(k).or_default();
            for i in 0..4 {
                e[i] += v[i];
            }
        }
        for i in 0..3 {
            self.cold[i][0] += o.cold[i][0];
            self.cold[i][1] += o.cold[i][1];
        }
        self
    }
}

/// What a measurement needs, shared by all users.
pub struct Setup<'a> {
    pub space: &'a Space,
    /// The vectors with the collaborative block, and with the aspect block, set to zero: the
    /// ablations (only for the report).
    pub without_c: Option<Space>,
    pub without_a: Option<Space>,
    pub cold_space: Option<&'a Space>,
    /// Works first published from the cold year on (rows of `space`).
    pub cold_rows: HashSet<u32>,
    pub content: Option<Content<'a>>,
    /// Method I's neighbour lists (only for the report).
    pub neighbours: Option<Neighbours>,
    /// The same lists by row, for the pick-up term; and from the cold-book likes.
    pub lists: Option<&'a RowLists>,
    pub cold_lists: Option<&'a RowLists>,
    pub hyper: Hyper,
    pub prior: Prior,
    pub revealed: Vec<usize>,
    pub seed: u64,
}

impl Setup<'_> {
    /// The vectors a method sees.
    fn view(&self, method: Method) -> &Space {
        match method {
            Method::FullWithoutCollaborative => self.without_c.as_ref().unwrap_or(self.space),
            Method::FullWithoutAspects => self.without_a.as_ref().unwrap_or(self.space),
            _ => self.space,
        }
    }
}

/// What to measure for each user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Everything of 04-benchmark.md §4–§5.
    Report,
    /// FULL's NDCG@10 at n = 20 only (tuning, the monthly gate).
    FullAtMain,
}

fn measure_user(setup: &Setup, user: &User, scope: Scope) -> Sums {
    let space = setup.space;
    let mut sums = Sums::default();
    let empty_content;
    let content = match &setup.content {
        Some(c) => c,
        None => {
            empty_content = Content {
                embeddings: &EMPTY,
                of_row: vec![u32::MAX; space.phi.len()],
            };
            &empty_content
        }
    };
    let mut buf = vec![0f32; space.cand.len()];
    let ns: Vec<(usize, usize)> = match scope {
        Scope::Report => setup.revealed.iter().copied().enumerate().collect(),
        Scope::FullAtMain => vec![(0, N_MAIN)],
    };
    for (ni, n) in ns {
        if user.ratings.len() <= n {
            continue;
        }
        let (revealed, future) = user.ratings.split_at(n);
        let now = revealed.last().map_or(f64::NAN, |r| r.day);
        let shown = placed(space, revealed);
        let revealed_rows: HashSet<u32> = shown.iter().map(|p| p.0).collect();
        let relevant: HashSet<u32> = future
            .iter()
            .filter(|r| r.stars >= 4)
            .filter_map(|r| space.row_of(r.work))
            .filter(|&row| space.recommendable(row) && !revealed_rows.contains(&row))
            .collect();
        let future_rows: Vec<(u32, u8)> = future
            .iter()
            .filter_map(|r| space.row_of(r.work).map(|row| (row, r.stars)))
            .collect();
        let methods: &[Method] = match scope {
            Scope::Report => &Method::ALL,
            Scope::FullAtMain => &[Method::Full],
        };
        let mut full_fit = None;
        let full_pickup = pickup(setup.lists, &shown);
        for &method in methods {
            let mut g = rng(setup.seed, &[user.id as u64, n as u64, method as u64]);
            let scorer = match method {
                Method::P => Scorer::Model(
                    Box::new(taste::fit(&[], space, &setup.hyper, &setup.prior)),
                    taste::Pickup::default(),
                ),
                Method::Full | Method::FullWithoutCollaborative | Method::FullWithoutAspects => {
                    let view = setup.view(method);
                    let events = replay(
                        view,
                        &setup.hyper,
                        &setup.prior,
                        &shown,
                        Rule::Model,
                        now,
                        &mut g,
                    );
                    // Without the collaborative block there are no co-likes either.
                    let pickup = if method == Method::FullWithoutCollaborative {
                        taste::Pickup::default()
                    } else {
                        full_pickup.clone()
                    };
                    Scorer::Model(
                        Box::new(taste::fit(&events, view, &setup.hyper, &setup.prior)),
                        pickup,
                    )
                }
                Method::FullWithoutComparisons => {
                    let events = replay(
                        space,
                        &setup.hyper,
                        &setup.prior,
                        &shown,
                        Rule::None,
                        now,
                        &mut g,
                    );
                    Scorer::Model(
                        Box::new(taste::fit(&events, space, &setup.hyper, &setup.prior)),
                        full_pickup.clone(),
                    )
                }
                Method::C => {
                    let liked: Vec<u32> = shown
                        .iter()
                        .filter(|p| p.1.stars >= 4)
                        .map(|p| p.0)
                        .collect();
                    Scorer::Content(content.query(&liked))
                }
                Method::I => {
                    let liked: Vec<u64> = revealed
                        .iter()
                        .filter(|r| r.stars >= 4)
                        .map(|r| r.work)
                        .collect();
                    let disliked: Vec<u64> = revealed
                        .iter()
                        .filter(|r| r.stars <= 2)
                        .map(|r| r.work)
                        .collect();
                    Scorer::Sparse(
                        setup
                            .neighbours
                            .as_ref()
                            .map_or_else(HashMap::new, |nb| nb.scores(space, &liked, &disliked)),
                    )
                }
                Method::MF => {
                    let items: Vec<(u32, bool)> =
                        shown.iter().map(|p| (p.0, p.1.stars >= 4)).collect();
                    Scorer::Factor(methods::fold_in(space, &items, FOLD_IN_RIDGE))
                }
            };
            // An ablation scores on its own view of the vectors; the metrics below read the real
            // ones.
            let view = setup.view(method);
            scorer.fill(view, content, &setup.hyper, &mut buf);
            let ranked = top(space, &buf, &revealed_rows, 50);
            let acc = sums.main.entry((method, ni)).or_default();
            if !relevant.is_empty() {
                acc.ndcg += ndcg(&ranked, &relevant, 10);
                acc.recall += ranked.iter().filter(|r| relevant.contains(r)).count() as f64
                    / relevant.len() as f64;
                acc.ranked_users += 1.0;
            }
            if scope == Scope::FullAtMain {
                continue;
            }
            let scored: Vec<(f32, u8)> = future_rows
                .iter()
                .map(|&(row, stars)| (scorer.score(view, content, &setup.hyper, row), stars))
                .collect();
            if let Some(p) = pairwise(&scored) {
                acc.pairwise += p;
                acc.pairwise_users += 1.0;
            }
            let ten = &ranked[..ranked.len().min(10)];
            if !ten.is_empty() {
                acc.novelty += ten
                    .iter()
                    .map(|&r| f64::from(space.pop_pct[r as usize]))
                    .sum::<f64>()
                    / ten.len() as f64;
                let mut d = 0.0;
                let mut pairs = 0.0;
                for (i, &a) in ten.iter().enumerate() {
                    for &b in &ten[i + 1..] {
                        d += 1.0 - taste::cosine(space.c(a), space.c(b));
                        pairs += 1.0;
                    }
                }
                acc.diversity += if pairs > 0.0 { d / pairs } else { 0.0 };
                acc.top_users += 1.0;
                acc.covered.extend(ten.iter().copied());
            }
            if method == Method::Full
                && let Scorer::Model(fit, _) = scorer
            {
                full_fit = Some(*fit);
            }
        }
        let Some(fit) = full_fit else { continue };
        // Band calibration: each future work with a vector, in its band.
        let bands = sums.bands.entry(ni).or_default();
        for &(row, stars) in &future_rows {
            let (_, band) = fit.band(
                space.vector(row),
                taste::Vectors::text_sourced(space, Row(row)),
                &setup.hyper,
            );
            let b = match band {
                taste::Band::StrongFit => 0,
                taste::Band::GoodFit => 1,
                taste::Band::NotSure => 2,
                taste::Band::ProbablyNot => 3,
            };
            bands[b][0] += 1.0;
            bands[b][1] += f64::from(u8::from(stars >= 4));
        }
        if n != N_MAIN {
            continue;
        }
        // The stretch pick, against a random candidate meeting the same novelty rule.
        let liked_c: Vec<&[f32]> = shown
            .iter()
            .filter(|p| p.1.stars >= 4)
            .map(|p| space.c(p.0))
            .collect();
        if !liked_c.is_empty() && !relevant.is_empty() {
            let candidates: Vec<taste::Candidate> = space
                .cand
                .iter()
                .filter(|r| !revealed_rows.contains(r))
                .map(|&r| taste::Candidate {
                    work: Row(r),
                    author: u64::from(r),
                    novelty: taste::novelty(space.c(r), &liked_c),
                    quality_z: space.quality_z(r),
                    ctx: 0.0,
                    pickup: 0.0,
                })
                .collect();
            let mut g = rng(setup.seed, &[user.id as u64, n as u64, 100]);
            if let Some(pick) = taste::stretch(&fit, space, &setup.hyper, &candidates, 0.5, &mut g)
            {
                sums.stretch_users += 1.0;
                sums.stretch_hits += f64::from(u8::from(relevant.contains(&pick.0)));
                let mut novelties: Vec<f64> = candidates.iter().map(|c| c.novelty).collect();
                novelties.sort_by(f64::total_cmp);
                let median = novelties[novelties.len() / 2];
                let eligible: Vec<&taste::Candidate> = candidates
                    .iter()
                    .filter(|c| c.novelty >= median && c.quality_z >= 0.0)
                    .collect();
                if !eligible.is_empty() {
                    let r = eligible[g.random_range(0..eligible.len())].work.0;
                    sums.stretch_random_hits += f64::from(u8::from(relevant.contains(&r)));
                }
            }
        }
        // Question value: 0, 10, 20 questions after revealing, for each rule.
        for rule in [Rule::None, Rule::Random, Rule::Model] {
            for count in [0usize, 10, 20] {
                if rule == Rule::None && count > 0 {
                    continue;
                }
                let mut g = rng(
                    setup.seed,
                    &[user.id as u64, 200 + count as u64, rule as u64],
                );
                let events = ask_after(
                    space,
                    &setup.hyper,
                    &setup.prior,
                    &shown,
                    count,
                    rule,
                    now,
                    &mut g,
                );
                let fit = taste::fit(&events, space, &setup.hyper, &setup.prior);
                model_rank(&fit, &full_pickup, &setup.hyper, space, &mut buf);
                let ranked = top(space, &buf, &revealed_rows, 10);
                let scored: Vec<(f32, u8)> = future_rows
                    .iter()
                    .map(|&(row, stars)| {
                        let s = model_score(&fit, &full_pickup, &setup.hyper, space, row);
                        (s as f32, stars)
                    })
                    .collect();
                let e = sums.questions.entry((rule, count)).or_default();
                if !relevant.is_empty() {
                    e[0] += ndcg(&ranked, &relevant, 10);
                    e[1] += 1.0;
                }
                if let Some(p) = pairwise(&scored) {
                    e[2] += p;
                    e[3] += 1.0;
                }
            }
        }
        // The cold-book test: rank only cold candidates.
        if let Some(cold_space) = setup.cold_space {
            let cold_relevant: HashSet<u32> = relevant
                .iter()
                .copied()
                .filter(|r| setup.cold_rows.contains(r))
                .collect();
            if cold_relevant.is_empty() {
                continue;
            }
            let cold_cands: Vec<u32> = space
                .cand
                .iter()
                .copied()
                .filter(|r| setup.cold_rows.contains(r) && !revealed_rows.contains(r))
                .filter(|&r| cold_space.row_of(space.phi.keys[r as usize]).is_some())
                .collect();
            let rank = |score: &dyn Fn(u32) -> f32| -> f64 {
                let mut items: Vec<(f32, u32)> =
                    cold_cands.iter().map(|&r| (score(r), r)).collect();
                items.sort_by(|a, b| {
                    b.0.total_cmp(&a.0)
                        .then(space.pop_pct[b.1 as usize].total_cmp(&space.pop_pct[a.1 as usize]))
                });
                let ranked: Vec<u32> = items.iter().take(10).map(|p| p.1).collect();
                ndcg(&ranked, &cold_relevant, 10)
            };
            let prior_fit = taste::fit(&[], space, &setup.hyper, &setup.prior);
            let none = taste::Pickup::default();
            sums.cold[0][0] +=
                rank(&|r| model_score(&prior_fit, &none, &setup.hyper, space, r) as f32);
            let cold_shown = placed(cold_space, revealed);
            let mut g = rng(setup.seed, &[user.id as u64, 300]);
            let events = replay(
                cold_space,
                &setup.hyper,
                &setup.prior,
                &cold_shown,
                Rule::Model,
                now,
                &mut g,
            );
            let text_fit = taste::fit(&events, cold_space, &setup.hyper, &setup.prior);
            // Co-likes without the cold works: they get no pick-up from them.
            let cold_pickup = pickup(setup.cold_lists, &cold_shown);
            sums.cold[1][0] += rank(&|r| {
                let row = cold_space
                    .row_of(space.phi.keys[r as usize])
                    .expect("filtered above");
                model_score(&text_fit, &cold_pickup, &setup.hyper, cold_space, row) as f32
            });
            sums.cold[2][0] +=
                rank(&|r| model_score(&fit, &full_pickup, &setup.hyper, space, r) as f32);
            for c in &mut sums.cold {
                c[1] += 1.0;
            }
        }
    }
    sums
}

static EMPTY: Matrix = Matrix {
    keys: Vec::new(),
    dim: 0,
    data: Vec::new(),
};

fn measure(setup: &Setup, users: &[User], scope: Scope) -> Sums {
    // Users in parallel, then summed in user order: the same floating-point sums every run.
    let per_user: Vec<Sums> = users
        .par_iter()
        .map(|u| measure_user(setup, u, scope))
        .collect();
    per_user.into_iter().fold(Sums::default(), Sums::merge)
}

/// The benchmark's report: tables, the gates, and the numbers to keep.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Report {
    pub users: usize,
    pub revealed: Vec<usize>,
    /// metric → method → value per n.
    pub tables: serde_json::Value,
    pub bands: serde_json::Value,
    pub stretch: serde_json::Value,
    pub questions: serde_json::Value,
    pub cold: serde_json::Value,
    pub gates: Vec<Gate>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Gate {
    pub id: String,
    pub what: String,
    pub passed: bool,
    pub detail: String,
}

fn report(setup: &Setup, users: usize, sums: &Sums, agreement: Option<f64>) -> Report {
    let ns = &setup.revealed;
    let get = |m: Method, ni: usize| sums.main.get(&(m, ni)).cloned().unwrap_or_default();
    let mut tables = serde_json::Map::new();
    let metric = |f: &dyn Fn(&Acc) -> Option<f64>| -> serde_json::Value {
        let mut t = serde_json::Map::new();
        for m in Method::ALL {
            let row: Vec<Option<f64>> = (0..ns.len()).map(|ni| f(&get(m, ni))).collect();
            t.insert(m.name().into(), serde_json::json!(row));
        }
        serde_json::Value::Object(t)
    };
    let per = |sum: f64, n: f64| (n > 0.0).then(|| sum / n);
    let recommendable = setup.space.cand.len() as f64;
    tables.insert("ndcg@10".into(), metric(&|a| per(a.ndcg, a.ranked_users)));
    tables.insert(
        "recall@50".into(),
        metric(&|a| per(a.recall, a.ranked_users)),
    );
    tables.insert(
        "pairwise accuracy".into(),
        metric(&|a| per(a.pairwise, a.pairwise_users)),
    );
    tables.insert(
        "novelty (popularity percentile of the top 10; lower = less obvious)".into(),
        metric(&|a| per(a.novelty, a.top_users)),
    );
    tables.insert(
        "diversity".into(),
        metric(&|a| per(a.diversity, a.top_users)),
    );
    tables.insert(
        "coverage".into(),
        metric(&|a| (a.top_users > 0.0).then(|| a.covered.len() as f64 / recommendable)),
    );

    let bands: Vec<serde_json::Value> = (0..ns.len())
        .map(|ni| {
            let b = sums.bands.get(&ni).copied().unwrap_or_default();
            serde_json::json!({
                "strong_fit": per(b[0][1], b[0][0]), "good_fit": per(b[1][1], b[1][0]),
                "not_sure": per(b[2][1], b[2][0]), "probably_not": per(b[3][1], b[3][0]),
                "works": [b[0][0], b[1][0], b[2][0], b[3][0]],
            })
        })
        .collect();
    let mut questions = serde_json::Map::new();
    for ((rule, count), v) in &sums.questions {
        questions.insert(
            format!("{rule:?} {count}"),
            serde_json::json!({"ndcg@10": per(v[0], v[1]), "pairwise accuracy": per(v[2], v[3])}),
        );
    }
    let cold = serde_json::json!({
        "P": per(sums.cold[0][0], sums.cold[0][1]),
        "FULL, text-placed vectors": per(sums.cold[1][0], sums.cold[1][1]),
        "FULL, real vectors": per(sums.cold[2][0], sums.cold[2][1]),
        "users": sums.cold[0][1],
        "agreement (mean cosine of text-placed and real c)": agreement,
    });
    let stretch = serde_json::json!({
        "hit rate": per(sums.stretch_hits, sums.stretch_users),
        "random candidate, same rule": per(sums.stretch_random_hits, sums.stretch_users),
        "users": sums.stretch_users,
    });

    let ndcg_at = |m: Method, n: usize| {
        ns.iter()
            .position(|&x| x == n)
            .map(|ni| get(m, ni).mean_ndcg())
    };
    let mut gates = Vec::new();
    let mut gate = |id: &str, what: &str, passed: bool, detail: String| {
        gates.push(Gate {
            id: id.into(),
            what: what.into(),
            passed,
            detail,
        });
    };
    let over = |a: Method, b: Method, from: usize| -> (bool, String) {
        let mut ok = true;
        let mut detail = Vec::new();
        for &n in ns.iter().filter(|&&n| n >= from) {
            let (x, y) = (ndcg_at(a, n).unwrap_or(0.0), ndcg_at(b, n).unwrap_or(0.0));
            ok &= x > y;
            detail.push(format!("n={n}: {x:.4} vs {y:.4}"));
        }
        (ok, detail.join(", "))
    };
    let (ok, d) = over(Method::Full, Method::P, 5);
    gate("G1", "FULL beats P on NDCG@10 at every n >= 5", ok, d);
    let (ok, d) = over(Method::Full, Method::C, 10);
    gate("G2", "FULL beats C on NDCG@10 at every n >= 10", ok, d);
    let mut ok = true;
    let mut detail = Vec::new();
    for n in [20, 50] {
        let full = ndcg_at(Method::Full, n).unwrap_or(0.0);
        let best = ndcg_at(Method::I, n)
            .unwrap_or(0.0)
            .max(ndcg_at(Method::MF, n).unwrap_or(0.0));
        ok &= full >= 0.95 * best;
        detail.push(format!("n={n}: {full:.4} vs 95% of {best:.4}"));
    }
    gate(
        "G3",
        "FULL reaches 95% of the better of I and MF on NDCG@10 at n = 20 and 50",
        ok,
        detail.join(", "),
    );
    let pw = |m: Method| {
        ns.iter().position(|&x| x == N_MAIN).and_then(|ni| {
            let a = get(m, ni);
            per(a.pairwise, a.pairwise_users)
        })
    };
    let (f, p) = (
        pw(Method::Full).unwrap_or(0.0),
        pw(Method::P).unwrap_or(0.0),
    );
    gate(
        "G4",
        "FULL's pairwise accuracy at n = 20 exceeds P's by 0.03",
        f >= p + 0.03,
        format!("{f:.4} vs {p:.4}"),
    );
    let (a, b) = (
        ndcg_at(Method::Full, 50).unwrap_or(0.0),
        ndcg_at(Method::Full, 5).unwrap_or(0.0),
    );
    gate(
        "G5",
        "FULL's NDCG@10 at n = 50 is higher than at n = 5",
        a > b,
        format!("{a:.4} vs {b:.4}"),
    );
    let main = ns
        .iter()
        .position(|&x| x == N_MAIN)
        .and_then(|ni| sums.bands.get(&ni))
        .copied()
        .unwrap_or_default();
    let shares: Vec<Option<f64>> = (0..4).rev().map(|b| per(main[b][1], main[b][0])).collect();
    let rising = shares
        .windows(2)
        .all(|w| matches!((w[0], w[1]), (Some(x), Some(y)) if y > x));
    gate(
        "G6",
        "band calibration rises from Probably not to Strong fit (n = 20)",
        rising,
        format!("{shares:?}"),
    );
    let (t, p) = (
        per(sums.cold[1][0], sums.cold[1][1]).unwrap_or(0.0),
        per(sums.cold[0][0], sums.cold[0][1]).unwrap_or(0.0),
    );
    gate(
        "G7",
        "on cold works, FULL with text-placed vectors beats P",
        t > p,
        format!("{t:.4} vs {p:.4}"),
    );

    Report {
        users,
        revealed: ns.clone(),
        tables: serde_json::Value::Object(tables),
        bands: serde_json::json!(bands),
        stretch,
        questions: serde_json::Value::Object(questions),
        cold,
        gates,
    }
}

impl Report {
    /// The report as Markdown, for the PR description.
    pub fn markdown(&self) -> String {
        let mut out = format!(
            "Benchmark on {} users, n = revealed ratings. The Goodreads ratings counts and shelf \
             tags behind popularity, quality and aspects are aggregates that include the test \
             users' own ratings (the files have no per-user split): a small optimistic bias for \
             every method that uses them.\n",
            self.users
        );
        let header = |out: &mut String, first: &str| {
            out.push_str(&format!("\n| {first} |"));
            for n in &self.revealed {
                out.push_str(&format!(" n={n} |"));
            }
            out.push_str("\n|---|");
            out.push_str(&"---|".repeat(self.revealed.len()));
            out.push('\n');
        };
        let fmt = |v: &serde_json::Value| v.as_f64().map_or("–".into(), |x| format!("{x:.4}"));
        if let serde_json::Value::Object(tables) = &self.tables {
            for (metric, rows) in tables {
                out.push_str(&format!("\n**{metric}**\n"));
                header(&mut out, "method");
                for m in Method::ALL {
                    out.push_str(&format!("| {} |", m.name()));
                    for v in rows[m.name()].as_array().into_iter().flatten() {
                        out.push_str(&format!(" {} |", fmt(v)));
                    }
                    out.push('\n');
                }
            }
        }
        out.push_str("\n**Band calibration** (share of future works with 4–5 stars, FULL)\n");
        header(&mut out, "band");
        for band in ["strong_fit", "good_fit", "not_sure", "probably_not"] {
            out.push_str(&format!("| {band} |"));
            for b in self.bands.as_array().into_iter().flatten() {
                out.push_str(&format!(" {} |", fmt(&b[band])));
            }
            out.push('\n');
        }
        out.push_str(&format!(
            "\n**Question value (n = 20)**: {}\n",
            self.questions
        ));
        out.push_str(&format!(
            "\n**Cold books (first published from the cold year on)**: {}\n",
            self.cold
        ));
        out.push_str(&format!("\n**Stretch pick**: {}\n", self.stretch));
        out.push_str("\n**Gates**\n\n| gate | | passed | numbers |\n|---|---|---|---|\n");
        for g in &self.gates {
            out.push_str(&format!(
                "| {} | {} | {} | {} |\n",
                g.id,
                g.what,
                if g.passed { "yes" } else { "**no**" },
                g.detail
            ));
        }
        out
    }
}

/// The meta-block prior's fit (04-benchmark.md §3): one logistic regression over the tuning
/// users' revealed verdicts (their first 20) with the verdict terms of 03-engine.md §4, using only the four meta numbers.
/// Runs `taste::fit` itself on a view of the vectors in which every other block is zero.
pub fn fit_prior(run: &Run) -> anyhow::Result<()> {
    let layout = vectors::layout(run);
    let space = Space::load(&run.model, false, layout)?;
    let users = data::users(run.duck, Users::Tuning, run.config.interactions.seed)?;
    let prior = prior_from(
        &space,
        &users,
        &run.config.model,
        run.config.bench.max_prior_terms,
        run.config.interactions.seed,
    );
    tracing::info!(m0 = ?prior.m0_meta, "prior mean of the meta block fitted");
    std::fs::write(prior_path(&run.model), serde_json::to_vec_pretty(&prior)?)?;
    Ok(())
}

/// A view of a space with only the bias and the meta block.
struct MetaOnly {
    layout: taste::Layout,
    rows: Matrix,
}

impl taste::Vectors for MetaOnly {
    fn layout(&self) -> taste::Layout {
        self.layout
    }
    fn phi(&self, work: Row) -> Option<&[f32]> {
        ((work.0 as usize) < self.rows.len()).then(|| self.rows.row(work.0 as usize))
    }
    fn text_sourced(&self, _: Row) -> bool {
        false
    }
}

pub fn prior_from(
    space: &Space,
    users: &[User],
    hyper: &Hyper,
    max_terms: usize,
    seed: u64,
) -> Prior {
    // Only the bias and the meta block: a layout without collaborative and aspect numbers. The
    // fit is the same as on the full vectors with those blocks zero (their weights would stay at
    // the prior mean, uncoupled), at a fraction of the memory.
    let full = space.layout;
    let layout = taste::Layout { k: 0, a: 0 };
    let mut all: Vec<(u32, u8)> = users
        .iter()
        .flat_map(|u| {
            u.ratings[..u.ratings.len().min(N_MAIN)]
                .iter()
                .filter_map(|r| space.row_of(r.work).map(|row| (row, r.stars)))
        })
        .collect();
    // Each verdict is three terms.
    let keep = max_terms / 3;
    if all.len() > keep {
        let mut g = rng(seed, &[400]);
        for i in 0..keep {
            let j = g.random_range(i..all.len());
            all.swap(i, j);
        }
        all.truncate(keep);
    }
    let mut rows = Matrix::new(layout.d());
    let mut events = Vec::with_capacity(all.len());
    all.sort_unstable();
    let mut index: HashMap<u32, u32> = HashMap::new();
    for &(row, stars) in &all {
        let next = index.len() as u32;
        let r = *index.entry(row).or_insert_with(|| {
            let mut v = vec![0f32; layout.d()];
            v[layout.bias()] = 1.0;
            v[layout.m()].copy_from_slice(&space.vector(row)[full.m()]);
            rows.push(u64::from(next), &v);
            next
        });
        events.push(Event::Read {
            work: Row(r),
            verdict: Some(data::verdict(stars)),
            stopped: None,
            nth_read: 1,
            latest: true,
            age_years: 0.0,
        });
    }
    let view = MetaOnly { layout, rows };
    let fit = taste::fit(&events, &view, hyper, &Prior::default());
    let w = fit.weights();
    let m = layout.m();
    Prior {
        m0_meta: [w[m.start], w[m.start + 1], w[m.start + 2], w[m.start + 3]],
    }
}

/// What the monthly gate stage keeps.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GateResult {
    pub ndcg10_n20: f64,
    pub users: usize,
    pub confidence_cuts: [f64; 2],
}

/// Stage bench: NDCG@10 at n = 20 on the gate subset (verify compares it with the accepted
/// value), and the confidence words' cut points: terciles of `sqrt(var)` over recommendable works
/// for gate users at n = 20 (03-engine.md §8).
pub fn gate(run: &Run) -> anyhow::Result<()> {
    let layout = vectors::layout(run);
    let space = Space::load(&run.model, false, layout)?;
    let prior: Prior = serde_json::from_slice(&std::fs::read(prior_path(&run.model))?)?;
    let users = data::users(run.duck, Users::Gate, run.config.interactions.seed)?;
    let lists = RowLists::new(
        &space,
        &crate::neighbours::read(&crate::neighbours::path(&run.model, false))?,
    );
    let setup = Setup {
        space: &space,
        without_c: None,
        without_a: None,
        cold_space: None,
        cold_rows: HashSet::new(),
        content: None,
        neighbours: None,
        lists: Some(&lists),
        cold_lists: None,
        hyper: run.config.model.clone(),
        prior,
        revealed: vec![N_MAIN],
        seed: run.config.interactions.seed,
    };
    let sums = measure(&setup, &users, Scope::FullAtMain);
    let ndcg = sums
        .main
        .get(&(Method::Full, 0))
        .map_or(0.0, Acc::mean_ndcg);
    let cuts = confidence_cuts(&setup, &users);
    let result = GateResult {
        ndcg10_n20: ndcg,
        users: users.len(),
        confidence_cuts: cuts,
    };
    tracing::info!(ndcg10_n20 = ndcg, users = users.len(), cuts = ?cuts, "benchmark gate measured");
    std::fs::write(gate_path(&run.model), serde_json::to_vec_pretty(&result)?)?;
    Ok(())
}

fn confidence_cuts(setup: &Setup, users: &[User]) -> [f64; 2] {
    let space = setup.space;
    // The first users in their fixed order: the same cut points every run.
    let sample: Vec<&User> = users
        .iter()
        .filter(|u| u.ratings.len() > N_MAIN)
        .take(CUT_USERS)
        .collect();
    let mut sds: Vec<f64> = sample
        .par_iter()
        .flat_map_iter(|u| {
            let shown = placed(space, &u.ratings[..N_MAIN]);
            let now = u.ratings[N_MAIN - 1].day;
            let mut g = rng(setup.seed, &[u.id as u64, 500]);
            let events = replay(
                space,
                &setup.hyper,
                &setup.prior,
                &shown,
                Rule::Model,
                now,
                &mut g,
            );
            let fit = taste::fit(&events, space, &setup.hyper, &setup.prior);
            (0..CUT_WORKS.min(space.cand.len()))
                .map(|_| space.cand[g.random_range(0..space.cand.len())])
                .map(|r| {
                    fit.variance(
                        space.vector(r),
                        taste::Vectors::text_sourced(space, Row(r)),
                        &setup.hyper,
                    )
                    .sqrt()
                })
                .collect::<Vec<_>>()
        })
        .collect();
    if sds.is_empty() {
        return [0.0, 0.0];
    }
    sds.sort_by(f64::total_cmp);
    [sds[sds.len() / 3], sds[2 * sds.len() / 3]]
}

/// `kindred-world bench`: the full report on the test users, or the hyperparameter search on
/// the tuning users (04-benchmark.md §3). Reads the scratch volume of a finished build.
pub fn run_full(
    duck: &Duck,
    config: &Config,
    aspects: usize,
    model: &Path,
    which: Users,
    report_tuning: bool,
) -> anyhow::Result<String> {
    let layout = taste::Layout {
        k: config.factors.k,
        a: aspects,
    };
    let space = Space::load(model, false, layout).context("run a build first: no vectors")?;
    let prior: Prior = serde_json::from_slice(&std::fs::read(prior_path(model))?)?;
    let users = data::users(duck, which, config.interactions.seed)?;
    tracing::info!(users = users.len(), which = ?which, "benchmark users loaded");
    let neighbours = crate::neighbours::read(&crate::neighbours::path(model, false))?;
    let lists = RowLists::new(&space, &neighbours);
    if which == Users::Tuning && !report_tuning {
        return tune(&space, &users, config, prior, lists);
    }
    let cold_space = Space::load(model, true, layout)?;
    let embeddings = Matrix::read(&crate::embed::path(model))?;
    let cold_lists = RowLists::new(
        &cold_space,
        &crate::neighbours::read(&crate::neighbours::path(model, true))?,
    );
    let cold_years: HashSet<u64> = {
        let mut statement = duck
            .conn()
            .prepare("SELECT work FROM catalogue WHERE year >= ?")?;
        let rows = statement.query_map([config.factors.cold_from_year], |r| r.get::<_, i64>(0))?;
        rows.map(|r| r.map(|k| k as u64))
            .collect::<Result<_, _>>()?
    };
    let cold_rows: HashSet<u32> = cold_years.iter().filter_map(|&k| space.row_of(k)).collect();
    let agreement = {
        let (mut sum, mut n) = (0.0, 0.0);
        for &row in &cold_rows {
            let key = space.phi.keys[row as usize];
            let real = space.flags[row as usize] & vectors::C_FROM_TEXT == 0;
            if let Some(cr) = cold_space.row_of(key).filter(|_| real)
                && cold_space.flags[cr as usize] & vectors::C_FROM_TEXT != 0
            {
                sum += taste::cosine(cold_space.c(cr), space.c(row));
                n += 1.0;
            }
        }
        (n > 0.0).then(|| sum / n)
    };
    let setup = Setup {
        space: &space,
        without_c: Some(space.without(layout.c())),
        without_a: Some(space.without(layout.a())),
        cold_space: Some(&cold_space),
        cold_rows,
        content: Some(Content::new(&space, &embeddings)),
        neighbours: Some(neighbours),
        lists: Some(&lists),
        cold_lists: Some(&cold_lists),
        hyper: config.model.clone(),
        prior,
        revealed: config.bench.revealed.clone(),
        seed: config.interactions.seed,
    };
    let sums = measure(&setup, &users, Scope::Report);
    let report = report(&setup, users.len(), &sums, agreement);
    std::fs::write(
        model.join(format!("bench-{which:?}.json").to_lowercase()),
        serde_json::to_vec_pretty(&report)?,
    )?;
    Ok(report.markdown())
}

/// The hyperparameter search of 04-benchmark.md §3: one at a time over its grid, keeping the best
/// FULL NDCG@10 at n = 20 on the tuning users; then one more pass. Returns the chosen values as
/// the `[model]` section of world.toml.
fn tune(
    space: &Space,
    users: &[User],
    config: &Config,
    prior: Prior,
    lists: RowLists,
) -> anyhow::Result<String> {
    type Setter = fn(&mut Hyper, f64);
    // The pick-up weights (03-engine.md §8) first: they set the scale the others work against.
    let grid: [(&str, &[f64], Setter); 10] = [
        (
            "pickup",
            &[0.0, 0.25, 0.5, 1.0, 2.0, 4.0, 8.0, 16.0],
            |h, v| h.pickup = v,
        ),
        ("pickup_pop", &[0.0, 0.25, 0.5, 1.0, 2.0], |h, v| {
            h.pickup_pop = v
        }),
        ("var_c", &[0.5, 1.0, 2.25, 4.0], |h, v| h.var_c = v),
        ("var_a", &[0.005, 0.0225, 0.1], |h, v| h.var_a = v),
        ("var_m", &[0.05, 0.25, 1.0], |h, v| h.var_m = v),
        ("tau_v", &[0.5, 1.0, 2.0], |h, v| h.tau_v = v),
        ("tau_c", &[0.25, 0.5, 1.0], |h, v| h.tau_c = v),
        ("omega_v", &[0.33, 0.5, 1.0], |h, v| h.omega_v = v),
        ("text_shrink", &[0.5, 0.7, 1.0], |h, v| h.text_shrink = v),
        ("text_var", &[0.0, 0.25, 1.0], |h, v| h.text_var = v),
    ];
    let built = config.model.text_shrink;
    let mut hyper = config.model.clone();
    let mut cache: HashMap<String, f64> = HashMap::new();
    let mut evaluate = |h: &Hyper| -> f64 {
        let key = serde_json::to_string(h).expect("serialisable");
        if let Some(&v) = cache.get(&key) {
            return v;
        }
        let shrunk = (h.text_shrink != built).then(|| space.with_text_shrink(built, h.text_shrink));
        let setup = Setup {
            space: shrunk.as_ref().unwrap_or(space),
            without_c: None,
            without_a: None,
            cold_space: None,
            cold_rows: HashSet::new(),
            content: None,
            neighbours: None,
            lists: Some(&lists),
            cold_lists: None,
            hyper: h.clone(),
            prior,
            revealed: vec![N_MAIN],
            seed: config.interactions.seed,
        };
        let v = measure(&setup, users, Scope::FullAtMain)
            .main
            .get(&(Method::Full, 0))
            .map_or(0.0, Acc::mean_ndcg);
        tracing::info!(ndcg10_n20 = v, hyper = %key, "tuning step");
        cache.insert(key, v);
        v
    };
    let mut log = String::new();
    for pass in 1..=2 {
        for (name, values, set) in &grid {
            let mut best = (f64::NEG_INFINITY, 0.0);
            for &v in *values {
                let mut h = hyper.clone();
                set(&mut h, v);
                let score = evaluate(&h);
                log.push_str(&format!("pass {pass} {name} = {v}: NDCG@10 {score:.5}\n"));
                if score > best.0 {
                    best = (score, v);
                }
            }
            set(&mut hyper, best.1);
        }
    }
    let final_score = evaluate(&hyper);
    Ok(format!(
        "{log}\nchosen (NDCG@10 at n = 20 on the tuning users: {final_score:.5}):\n\n[model]\n{}",
        toml::to_string(&hyper)?
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ndcg_and_pairwise() {
        let relevant: HashSet<u32> = [1, 2].into_iter().collect();
        assert!((ndcg(&[1, 2, 3], &relevant, 10) - 1.0).abs() < 1e-12);
        let swapped = ndcg(&[3, 1, 2], &relevant, 10);
        assert!(swapped < 1.0 && swapped > 0.0);
        assert_eq!(ndcg(&[3, 4], &relevant, 10), 0.0);
        assert_eq!(pairwise(&[(1.0, 5), (0.5, 3), (0.7, 4)]), Some(1.0));
        assert_eq!(pairwise(&[(0.0, 5), (1.0, 3)]), Some(0.0));
        assert_eq!(pairwise(&[(1.0, 4), (1.0, 3)]), Some(0.5));
        assert_eq!(pairwise(&[(1.0, 4), (2.0, 4)]), None);
    }
}
