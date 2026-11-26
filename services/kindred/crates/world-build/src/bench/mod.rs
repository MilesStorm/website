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

pub mod batch;
pub mod data;
#[cfg(feature = "gpu")]
pub mod gpu;
pub mod methods;

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Mutex;

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
/// stars is better, equal stars a skip). Returns the partner asked about, if any.
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
    last_fit: &mut Option<(usize, taste::Fit)>,
) -> Option<u32> {
    let other = match rule {
        Rule::None => None,
        Rule::Random => {
            let open: Vec<&(u32, Rating)> = earlier
                .iter()
                .filter(|(row, r)| {
                    *row != new.0
                        && data::verdict(r.stars) == data::verdict(new.1.stars)
                        && !asked.contains(&pair_key(new.0, *row))
                })
                .collect();
            (!open.is_empty()).then(|| open[rng.random_range(0..open.len())].0)
        }
        Rule::Model => {
            // Events only grow, and a skip adds none: the same count is the same fit.
            if last_fit.as_ref().is_none_or(|(n, _)| *n != events.len()) {
                *last_fit = Some((events.len(), taste::fit(events, space, hyper, prior)));
            }
            let fit = &last_fit.as_ref().expect("just set").1;
            taste::next_comparison(
                fit,
                space,
                hyper,
                Row(new.0),
                data::verdict(new.1.stars),
                &rated(earlier, now),
                &|a, b| asked.contains(&pair_key(a.0, b.0)),
            )
            .map(|r| r.0)
        }
    };
    let other = other?;
    answer(events, asked, new, earlier, other, now);
    Some(other)
}

/// A pair of works, either way round.
fn pair_key(a: u32, b: u32) -> (u32, u32) {
    (a.min(b), a.max(b))
}

/// The earlier works as `taste::next_comparison` reads them; recent: logged within three years
/// of `now`, or of unknown date.
fn rated(earlier: &[(u32, Rating)], now: f64) -> Vec<Rated> {
    earlier
        .iter()
        .map(|(row, r)| Rated {
            work: Row(*row),
            verdict: data::verdict(r.stars),
            recent: (now - r.day).is_nan() || now - r.day <= RECENT_DAYS,
        })
        .collect()
}

/// The simulated answer about `new` against `other` (04-benchmark.md §2): the work with more
/// stars was liked more; equal stars are a skip, which adds no event. The pair counts as asked
/// either way.
fn answer(
    events: &mut Vec<Event>,
    asked: &mut HashSet<(u32, u32)>,
    new: (u32, Rating),
    earlier: &[(u32, Rating)],
    other: u32,
    now: f64,
) {
    asked.insert(pair_key(new.0, other));
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
    let mut last_fit = None;
    for (i, item) in revealed.iter().enumerate() {
        events.push(read_event(item.0, &item.1, now));
        if rule == Rule::None || i == 0 {
            continue;
        }
        for _ in 0..QUESTIONS_PER_LOG {
            if ask(
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
                &mut last_fit,
            )
            .is_none()
            {
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
    let mut last_fit = None;
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
                &mut last_fit,
            )
            .is_some()
            {
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
            Method::FullWithoutCollaborative => {
                "FULL without collaborative blocks (c, r, star bias)"
            }
            Method::FullWithoutAspects => "FULL without aspect block",
            Method::FullWithoutComparisons => "FULL without comparisons",
        }
    }
}

/// What ranking and pairwise accuracy read from a fitted reader (03-engine.md §8): `mu` of one
/// work and of every candidate. The CPU path reads taste's own fit; the batched path
/// ([`batch`]) the weights it fitted on a device.
trait Mu {
    /// `mu(i)` without the bias number.
    fn score(&self, phi: &[f32]) -> f64;
    /// [`Mu::score`] in f32 of every row of `rows` (`d` numbers each) into `out`.
    fn score_rows(&self, rows: &[f32], out: &mut [f32]);
}

impl Mu for taste::Fit {
    fn score(&self, phi: &[f32]) -> f64 {
        taste::Fit::score(self, phi)
    }

    fn score_rows(&self, rows: &[f32], out: &mut [f32]) {
        taste::Fit::score_rows(self, rows, out)
    }
}

impl<T: Mu + ?Sized> Mu for &T {
    fn score(&self, phi: &[f32]) -> f64 {
        (**self).score(phi)
    }

    fn score_rows(&self, rows: &[f32], out: &mut [f32]) {
        (**self).score_rows(rows, out)
    }
}

/// One method's scores for one user.
enum Scorer<'a> {
    /// The taste model: `mu(i)` plus the pick-up term (03-engine.md §8).
    Model(Box<dyn Mu + 'a>, Personal),
    Content(Option<Vec<f32>>),
    Sparse(HashMap<u32, f32>),
    Factor(Option<Vec<f32>>),
}

impl Scorer<'_> {
    fn score(&self, space: &Space, content: &Content, hyper: &Hyper, row: u32) -> f32 {
        match self {
            Scorer::Model(fit, pickup) => model_score(&**fit, pickup, hyper, space, row) as f32,
            Scorer::Content(q) => q.as_ref().map_or(0.0, |q| content.score(q, row)),
            Scorer::Sparse(s) => s.get(&row).copied().unwrap_or(0.0),
            Scorer::Factor(x) => x.as_ref().map_or(0.0, |x| methods::dot(x, space.c(row))),
        }
    }

    /// How much the method says the reader would like `row`: for the taste model `mu(row)`
    /// alone, without the pick-up term, which says whether they would read it (03-engine.md §8);
    /// pairwise accuracy orders works the reader has read.
    fn liking(&self, space: &Space, content: &Content, hyper: &Hyper, row: u32) -> f32 {
        match self {
            Scorer::Model(fit, personal) => liking(&**fit, personal, hyper, space, row) as f32,
            _ => self.score(space, content, hyper, row),
        }
    }

    /// Scores of every recommendable row, in `space.cand` order.
    fn fill(&self, space: &Space, content: &Content, hyper: &Hyper, out: &mut [f32]) {
        match self {
            Scorer::Model(fit, pickup) => model_rank(&**fit, pickup, hyper, space, out),
            _ => {
                for (o, &row) in out.iter_mut().zip(&space.cand) {
                    *o = self.score(space, content, hyper, row);
                }
            }
        }
    }
}

/// The reader's terms beside `mu(i)` (03-engine.md §8): whether they would pick a work up, and
/// how they judged its nearest works.
#[derive(Clone, Debug, Default)]
struct Personal {
    pickup: taste::Pickup,
    agree: taste::Agreement,
}

/// How much the reader would like `row`: `mu` (bias dropped) plus the neighbourhood term.
fn liking(
    fit: &(impl Mu + ?Sized),
    personal: &Personal,
    hyper: &Hyper,
    space: &Space,
    row: u32,
) -> f64 {
    fit.score(space.vector(row)) + hyper.agree * personal.agree.get(Row(row), hyper.agree_shrink)
}

/// The taste model's ranking score of one work: how much the reader would like it plus the
/// pick-up term.
fn model_score(
    fit: &(impl Mu + ?Sized),
    personal: &Personal,
    hyper: &Hyper,
    space: &Space,
    row: u32,
) -> f64 {
    let phi = space.vector(row);
    liking(fit, personal, hyper, space, row)
        + taste::bonus(
            hyper,
            personal.pickup.get(Row(row)),
            taste::popularity_z(space.layout, phi),
        )
}

/// [`model_score`] of every recommendable row, in `space.cand` order.
fn model_rank(
    fit: &(impl Mu + ?Sized),
    personal: &Personal,
    hyper: &Hyper,
    space: &Space,
    out: &mut [f32],
) {
    fit.score_rows(&space.cand_phi, out);
    if hyper.agree != 0.0 {
        for (row, n) in personal.agree.iter(hyper.agree_shrink) {
            if let Ok(i) = space.cand.binary_search(&row.0) {
                out[i] += (hyper.agree * n) as f32;
            }
        }
    }
    if hyper.pickup == 0.0 {
        return;
    }
    let d = space.layout.d();
    for (o, phi) in out.iter_mut().zip(space.cand_phi.chunks_exact(d)) {
        *o += taste::bonus(hyper, 0.0, taste::popularity_z(space.layout, phi)) as f32;
    }
    for (row, p) in personal.pickup.iter() {
        if let Ok(i) = space.cand.binary_search(&row.0) {
            out[i] += (hyper.pickup * p) as f32;
        }
    }
}

/// The reader's terms from their revealed works (03-engine.md §8): for the pick-up sums liked
/// or loved count for and not for me against; the neighbourhood term reads every verdict.
fn personal(lists: Option<&RowLists>, shown: &[(u32, Rating)]) -> Personal {
    let Some(lists) = lists else {
        return Personal::default();
    };
    let reads: Vec<(Row, taste::Lean)> = shown
        .iter()
        .filter_map(|(row, r)| match r.stars {
            4 | 5 => Some((Row(*row), taste::Lean::Toward)),
            1 | 2 => Some((Row(*row), taste::Lean::Away)),
            _ => None,
        })
        .collect();
    let judged: Vec<(Row, taste::Verdict)> = shown
        .iter()
        .map(|(row, r)| (Row(*row), data::verdict(r.stars)))
        .collect();
    Personal {
        pickup: taste::Pickup::new(&reads, |w| lists.get(w).iter().copied()),
        agree: taste::Agreement::new(&judged, |w| lists.get(w).iter().copied()),
    }
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
    /// Sum of squares of the per-user NDCG@10, for its standard error.
    ndcg_sq: f64,
    recall: f64,
    ranked_users: f64,
    pairwise: f64,
    pairwise_sq: f64,
    pairwise_users: f64,
    novelty: f64,
    diversity: f64,
    top_users: f64,
    covered: HashSet<u32>,
    /// NDCG@10 against the same future at every n: the relevant works after the largest n,
    /// ranking without any of the works before it. Separates "more ratings" from "a different,
    /// later, smaller set of books to find".
    ndcg_same: f64,
    same_users: f64,
    /// Relevant future works per ranked user.
    relevant: f64,
}

impl Acc {
    fn merge(&mut self, o: Acc) {
        self.ndcg += o.ndcg;
        self.ndcg_sq += o.ndcg_sq;
        self.recall += o.recall;
        self.ranked_users += o.ranked_users;
        self.pairwise += o.pairwise;
        self.pairwise_sq += o.pairwise_sq;
        self.pairwise_users += o.pairwise_users;
        self.novelty += o.novelty;
        self.diversity += o.diversity;
        self.top_users += o.top_users;
        self.covered.extend(o.covered);
        self.ndcg_same += o.ndcg_same;
        self.same_users += o.same_users;
        self.relevant += o.relevant;
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
    /// P and FULL on the cold vectors (text-placed), FULL and P on the real vectors:
    /// (NDCG@10 sum, users).
    cold: [[f64; 2]; 4],
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
        for i in 0..4 {
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
    /// Where the per-reader work runs: `None` on the CPU, one user at a time (the reference);
    /// a device batches it across users ([`batch`]).
    pub device: Option<&'a Mutex<Box<dyn batch::Device>>>,
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
    /// FULL's pairwise accuracy at n = 20 only (tuning the settings of `mu`): no ranking.
    PairwiseAtMain,
    /// P, FULL and FULL without comparisons at n = 20: NDCG@10 and pairwise accuracy (the quick
    /// check while developing, `bench --quick`).
    Quick,
}

/// One user's ratings split at `n` (04-benchmark.md §2) and the sets the measurements read.
struct Split<'u> {
    revealed: &'u [Rating],
    /// The day of the last revealed rating: "now" for ages and the recent flag.
    now: f64,
    /// The revealed works that have a vector.
    shown: Vec<(u32, Rating)>,
    revealed_rows: HashSet<u32>,
    /// Recommendable future works with 4 or 5 stars.
    relevant: HashSet<u32>,
    /// The same future at every n (Report only): the works before the largest n, and the
    /// relevant ones after it.
    before_rows: HashSet<u32>,
    relevant_same: HashSet<u32>,
    /// Future works with a vector and their stars.
    future_rows: Vec<(u32, u8)>,
}

impl<'u> Split<'u> {
    /// `None` when the user has no future at `n`.
    fn new(setup: &Setup, user: &'u User, n: usize) -> Option<Split<'u>> {
        let space = setup.space;
        if user.ratings.len() <= n {
            return None;
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
        let last = setup.revealed.iter().copied().max().unwrap_or(0);
        let (before_last, after_last) = user.ratings.split_at(last.min(user.ratings.len()));
        let before_rows: HashSet<u32> = before_last
            .iter()
            .filter_map(|r| space.row_of(r.work))
            .collect();
        let relevant_same: HashSet<u32> = after_last
            .iter()
            .filter(|r| r.stars >= 4)
            .filter_map(|r| space.row_of(r.work))
            .filter(|&row| space.recommendable(row) && !before_rows.contains(&row))
            .collect();
        let future_rows: Vec<(u32, u8)> = future
            .iter()
            .filter_map(|r| space.row_of(r.work).map(|row| (row, r.stars)))
            .filter(|(row, _)| !revealed_rows.contains(row))
            .collect();
        Some(Split {
            revealed,
            now,
            shown,
            revealed_rows,
            relevant,
            before_rows,
            relevant_same,
            future_rows,
        })
    }

    /// The cold-book test's candidates (cold works the cold vectors can rank, not revealed) and
    /// its relevant works; `None` when no relevant work is among them (the user is not counted).
    fn cold(&self, setup: &Setup, cold_space: &Space) -> Option<(Vec<u32>, HashSet<u32>)> {
        let space = setup.space;
        let cold_cands: Vec<u32> = space
            .cand
            .iter()
            .copied()
            .filter(|r| setup.cold_rows.contains(r) && !self.revealed_rows.contains(r))
            .filter(|&r| cold_space.row_of(space.phi.keys[r as usize]).is_some())
            .collect();
        // Only works the cold vectors can rank count toward the ideal.
        let cold_relevant: HashSet<u32> = self
            .relevant
            .iter()
            .copied()
            .filter(|r| setup.cold_rows.contains(r))
            .filter(|r| cold_space.row_of(space.phi.keys[*r as usize]).is_some())
            .collect();
        (!cold_relevant.is_empty()).then_some((cold_cands, cold_relevant))
    }
}

/// The (index of n, n) a scope measures.
fn ns(setup: &Setup, scope: Scope) -> Vec<(usize, usize)> {
    match scope {
        Scope::Report => setup.revealed.iter().copied().enumerate().collect(),
        Scope::FullAtMain | Scope::PairwiseAtMain | Scope::Quick => vec![(0, N_MAIN)],
    }
}

/// The methods a scope measures.
fn methods(scope: Scope) -> &'static [Method] {
    match scope {
        Scope::Report => &Method::ALL,
        Scope::FullAtMain | Scope::PairwiseAtMain => &[Method::Full],
        Scope::Quick => &[Method::P, Method::Full, Method::FullWithoutComparisons],
    }
}

/// Whether the cold-book test runs at `n` (the report runs it at the main n only).
fn cold_at(setup: &Setup, scope: Scope, n: usize) -> bool {
    setup.cold_space.is_some() && (scope != Scope::Report || n == N_MAIN)
}

/// One user's measurements. `pre`: the histories and fits the batched path computed ahead
/// ([`batch`]); without it every replay and fit runs here, on the CPU.
fn measure_user(setup: &Setup, user: &User, scope: Scope, pre: Option<&batch::UserSims>) -> Sums {
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
    // FULL's fit with its covariance: the bands, the stretch pick and the cold test read it.
    let full_fit_needed = scope == Scope::Report || setup.cold_space.is_some();
    for (ni, n) in ns(setup, scope) {
        let Some(split) = Split::new(setup, user, n) else {
            continue;
        };
        let Split {
            revealed,
            now,
            shown,
            revealed_rows,
            relevant,
            before_rows,
            relevant_same,
            future_rows,
        } = &split;
        let now = *now;
        let simulated = |sim: batch::Sim| pre.map(|p| &p[&(sim, ni)]);
        let mut full_fit = None;
        let full_pickup = personal(setup.lists, shown);
        for &method in methods(scope) {
            let mut g = rng(setup.seed, &[user.id as u64, n as u64, method as u64]);
            let scorer = match method {
                Method::P => Scorer::Model(
                    Box::new(taste::fit(&[], space, &setup.hyper, &setup.prior)),
                    Personal::default(),
                ),
                Method::Full | Method::FullWithoutCollaborative | Method::FullWithoutAspects => {
                    let view = setup.view(method);
                    // Without the collaborative block there are no co-likes either.
                    let pickup = if method == Method::FullWithoutCollaborative {
                        Personal::default()
                    } else {
                        full_pickup.clone()
                    };
                    let fitted: Box<dyn Mu> = match simulated(batch::Sim::Method(method)) {
                        Some(s) => {
                            if method == Method::Full && full_fit_needed {
                                full_fit =
                                    Some(taste::fit(&s.events, view, &setup.hyper, &setup.prior));
                            }
                            Box::new(s)
                        }
                        None => {
                            let events = replay(
                                view,
                                &setup.hyper,
                                &setup.prior,
                                shown,
                                Rule::Model,
                                now,
                                &mut g,
                            );
                            let fit = taste::fit(&events, view, &setup.hyper, &setup.prior);
                            if method == Method::Full && scope != Scope::PairwiseAtMain {
                                full_fit = Some(fit.clone());
                            }
                            Box::new(fit)
                        }
                    };
                    Scorer::Model(fitted, pickup)
                }
                Method::FullWithoutComparisons => {
                    let fitted: Box<dyn Mu> = match simulated(batch::Sim::Method(method)) {
                        Some(s) => Box::new(s),
                        None => {
                            let events = replay(
                                space,
                                &setup.hyper,
                                &setup.prior,
                                shown,
                                Rule::None,
                                now,
                                &mut g,
                            );
                            Box::new(taste::fit(&events, space, &setup.hyper, &setup.prior))
                        }
                    };
                    Scorer::Model(fitted, full_pickup.clone())
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
            if scope == Scope::PairwiseAtMain {
                let scored: Vec<(f32, u8)> = future_rows
                    .iter()
                    .map(|&(row, stars)| (scorer.liking(view, content, &setup.hyper, row), stars))
                    .collect();
                let acc = sums.main.entry((method, ni)).or_default();
                if let Some(p) = pairwise(&scored) {
                    acc.pairwise += p;
                    acc.pairwise_sq += p * p;
                    acc.pairwise_users += 1.0;
                }
                continue;
            }
            scorer.fill(view, content, &setup.hyper, &mut buf);
            let ranked = top(space, &buf, revealed_rows, 50);
            let acc = sums.main.entry((method, ni)).or_default();
            if !relevant.is_empty() {
                let v = ndcg(&ranked, relevant, 10);
                acc.ndcg += v;
                acc.ndcg_sq += v * v;
                acc.recall += ranked.iter().filter(|r| relevant.contains(r)).count() as f64
                    / relevant.len() as f64;
                acc.ranked_users += 1.0;
                acc.relevant += relevant.len() as f64;
            }
            if scope == Scope::Quick {
                let scored: Vec<(f32, u8)> = future_rows
                    .iter()
                    .map(|&(row, stars)| (scorer.liking(view, content, &setup.hyper, row), stars))
                    .collect();
                if let Some(p) = pairwise(&scored) {
                    acc.pairwise += p;
                    acc.pairwise_sq += p * p;
                    acc.pairwise_users += 1.0;
                }
            }
            if scope == Scope::FullAtMain || scope == Scope::Quick {
                continue;
            }
            if !relevant_same.is_empty() {
                let ranked_same = top(space, &buf, before_rows, 10);
                acc.ndcg_same += ndcg(&ranked_same, relevant_same, 10);
                acc.same_users += 1.0;
            }
            let scored: Vec<(f32, u8)> = future_rows
                .iter()
                .map(|&(row, stars)| (scorer.liking(view, content, &setup.hyper, row), stars))
                .collect();
            if let Some(p) = pairwise(&scored) {
                acc.pairwise += p;
                acc.pairwise_sq += p * p;
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
        }
        let Some(fit) = full_fit else { continue };
        if scope == Scope::Report {
            // Band calibration: each future work with a vector, in its band.
            let bands = sums.bands.entry(ni).or_default();
            for &(row, stars) in future_rows {
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
                if let Some(pick) =
                    taste::stretch(&fit, space, &setup.hyper, &candidates, 0.5, &mut g)
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
                    let fit: Box<dyn Mu> = match simulated(batch::Sim::Question(rule, count)) {
                        Some(s) => Box::new(s),
                        None => {
                            let mut g = rng(
                                setup.seed,
                                &[user.id as u64, 200 + count as u64, rule as u64],
                            );
                            let events = ask_after(
                                space,
                                &setup.hyper,
                                &setup.prior,
                                shown,
                                count,
                                rule,
                                now,
                                &mut g,
                            );
                            Box::new(taste::fit(&events, space, &setup.hyper, &setup.prior))
                        }
                    };
                    model_rank(&*fit, &full_pickup, &setup.hyper, space, &mut buf);
                    let ranked = top(space, &buf, revealed_rows, 10);
                    let scored: Vec<(f32, u8)> = future_rows
                        .iter()
                        .map(|&(row, stars)| {
                            (
                                liking(&*fit, &full_pickup, &setup.hyper, space, row) as f32,
                                stars,
                            )
                        })
                        .collect();
                    let e = sums.questions.entry((rule, count)).or_default();
                    if !relevant.is_empty() {
                        e[0] += ndcg(&ranked, relevant, 10);
                        e[1] += 1.0;
                    }
                    if let Some(p) = pairwise(&scored) {
                        e[2] += p;
                        e[3] += 1.0;
                    }
                }
            }
        }
        // The cold-book test: rank only cold candidates.
        if let Some(cold_space) = setup.cold_space {
            let Some((cold_cands, cold_relevant)) = split.cold(setup, cold_space) else {
                continue;
            };
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
            let cold_row = |r: u32| {
                cold_space
                    .row_of(space.phi.keys[r as usize])
                    .expect("filtered above")
            };
            // P sees what the cold vectors hold: the catalogue facts, but no star bias for a
            // cold work, since that comes from the star factorization the cold works are left
            // out of.
            let none = Personal::default();
            let cold_prior_fit = taste::fit(&[], cold_space, &setup.hyper, &setup.prior);
            sums.cold[0][0] += rank(&|r| {
                model_score(
                    &cold_prior_fit,
                    &none,
                    &setup.hyper,
                    cold_space,
                    cold_row(r),
                ) as f32
            });
            let prior_fit = taste::fit(&[], space, &setup.hyper, &setup.prior);
            sums.cold[3][0] +=
                rank(&|r| model_score(&prior_fit, &none, &setup.hyper, space, r) as f32);
            let cold_shown = placed(cold_space, revealed);
            let text_fit: Box<dyn Mu> = match simulated(batch::Sim::Cold) {
                Some(s) => Box::new(s),
                None => {
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
                    Box::new(taste::fit(&events, cold_space, &setup.hyper, &setup.prior))
                }
            };
            // Co-likes without the cold works: they get no pick-up from them.
            let cold_pickup = personal(setup.cold_lists, &cold_shown);
            sums.cold[1][0] += rank(&|r| {
                model_score(
                    &*text_fit,
                    &cold_pickup,
                    &setup.hyper,
                    cold_space,
                    cold_row(r),
                ) as f32
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
    if let Some(device) = setup.device {
        let mut device = device
            .lock()
            .expect("no measurement panicked holding the device");
        return batch::measure(setup, users, scope, &mut **device);
    }
    // Users in parallel, then summed in user order: the same floating-point sums every run.
    let per_user: Vec<Sums> = users
        .par_iter()
        .map(|u| measure_user(setup, u, scope, None))
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
        "ndcg@10, standard error".into(),
        metric(&|a| {
            (a.ranked_users > 0.0).then(|| Estimate::of(a.ndcg, a.ndcg_sq, a.ranked_users).se)
        }),
    );
    tables.insert(
        "ndcg@10, same future at every n (works after the first 50)".into(),
        metric(&|a| per(a.ndcg_same, a.same_users)),
    );
    tables.insert(
        "relevant future works per user".into(),
        metric(&|a| per(a.relevant, a.ranked_users)),
    );
    tables.insert(
        "recall@50".into(),
        metric(&|a| per(a.recall, a.ranked_users)),
    );
    tables.insert(
        "pairwise accuracy".into(),
        metric(&|a| per(a.pairwise, a.pairwise_users)),
    );
    tables.insert(
        "pairwise accuracy, standard error".into(),
        metric(&|a| {
            (a.pairwise_users > 0.0)
                .then(|| Estimate::of(a.pairwise, a.pairwise_sq, a.pairwise_users).se)
        }),
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
        "P, real vectors (with the cold works' star bias)": per(sums.cold[3][0], sums.cold[3][1]),
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
            (a.pairwise_users > 0.0)
                .then(|| Estimate::of(a.pairwise, a.pairwise_sq, a.pairwise_users))
        })
    };
    let none = Estimate { mean: 0.0, se: 0.0 };
    let (f, p) = (
        pw(Method::Full).unwrap_or(none),
        pw(Method::P).unwrap_or(none),
    );
    // Restated 2026-11-26 (04-benchmark §6): "better than the prior" means a margin of more than
    // two standard errors of the difference, not a fixed 0.03 set before P gained the star bias.
    let threshold = 2.0 * (f.se * f.se + p.se * p.se).sqrt();
    gate(
        "G4",
        "FULL's pairwise accuracy at n = 20 exceeds P's by more than two standard errors",
        f.mean - p.mean > threshold,
        format!(
            "{:.4} vs {:.4} (margin {:+.4}, threshold {threshold:.4})",
            f.mean,
            p.mean,
            f.mean - p.mean
        ),
    );
    // G5 compares both n against the same future works (those after the 50th rating, the first
    // 50 out of the candidates), so that only the number of revealed ratings changes.
    let same_at = |m: Method, n: usize| {
        ns.iter().position(|&x| x == n).and_then(|ni| {
            let a = get(m, ni);
            per(a.ndcg_same, a.same_users)
        })
    };
    let (a, b) = (
        same_at(Method::Full, 50).unwrap_or(0.0),
        same_at(Method::Full, 5).unwrap_or(0.0),
    );
    gate(
        "G5",
        "FULL's NDCG@10 at n = 50 is higher than at n = 5, against the same future works",
        a > b,
        format!("{a:.4} vs {b:.4}"),
    );
    let (a, b) = (
        ndcg_at(Method::Full, 50).unwrap_or(0.0),
        ndcg_at(Method::Full, 5).unwrap_or(0.0),
    );
    gate(
        "info: G5 as first written",
        "each n against its own future works (the future shrinks and moves later with n)",
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

/// The meta-block prior's fit (04-benchmark.md §3): an ordinal logistic regression over the
/// tuning users' verdicts (all of them), with the verdict terms of 03-engine.md §4,
/// on the meta numbers only, with an intercept per user.
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

const META: usize = taste::Layout::META;

/// One verdict of the prior's fit: its user (index), the work's meta numbers, how many of the
/// thresholds `b1 < b2 < b3` it lies above.
struct MetaVerdict {
    user: usize,
    meta: [f64; META],
    level: usize,
}

/// The prior mean `m0` of the meta block: the shared meta weights of a fit in which every
/// tuning user has their own intercept `b_u` (prior N(0, var_bias)) and the meta weights have
/// prior N(0, var_m). Why per-user intercepts: readers differ in how many stars they give, and a
/// shared intercept would credit those differences to the kinds of books each one reads.
///
/// The verdict terms are those of 03-engine.md §4 for a latest, current read (weight `omega_v`,
/// noise `tau_v`, thresholds `b1..b3`). The posterior mode by exact Newton steps with a
/// backtracking line search: the Hessian is an arrowhead (each intercept couples only to the
/// meta weights), solved by its Schur complement on the meta block.
pub fn prior_from(
    space: &Space,
    users: &[User],
    hyper: &Hyper,
    max_terms: usize,
    seed: u64,
) -> Prior {
    let m = space.layout.m();
    let mut all: Vec<MetaVerdict> = users
        .iter()
        .enumerate()
        .flat_map(|(u, user)| {
            user.ratings
                .iter()
                .filter_map(move |r| space.row_of(r.work).map(|row| (u, row, r.stars)))
        })
        .map(|(user, row, stars)| {
            let phi = &space.vector(row)[m.clone()];
            MetaVerdict {
                user,
                meta: std::array::from_fn(|j| f64::from(phi[j])),
                level: data::verdict(stars).level(),
            }
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
    Prior {
        m0_meta: fit_meta_with_intercepts(&all, users.len(), hyper),
    }
}

fn fit_meta_with_intercepts(obs: &[MetaVerdict], users: usize, hyper: &Hyper) -> [f64; META] {
    let thresholds = [hyper.b1, hyper.b2, hyper.b3];
    let (omega, tau) = (hyper.omega_v, hyper.tau_v);
    // The negative log posterior at (b, w).
    let objective = |b: &[f64], w: &[f64; META]| -> f64 {
        let mut f = b.iter().map(|x| x * x).sum::<f64>() / (2.0 * hyper.var_bias)
            + w.iter().map(|x| x * x).sum::<f64>() / (2.0 * hyper.var_m);
        for o in obs {
            let z = b[o.user] + dot_meta(w, &o.meta);
            for (k, &t) in thresholds.iter().enumerate() {
                let y = if o.level > k { 1.0 } else { -1.0 };
                f -= omega * taste::ln_sigmoid(y * (z - t) / tau);
            }
        }
        f
    };
    let mut b = vec![0.0; users];
    let mut w = [0.0; META];
    let mut f = objective(&b, &w);
    for _ in 0..100 {
        // Gradient and Hessian blocks: per user g_u, h_uu, h_uw; shared g_w, h_ww.
        let mut g_u = vec![0.0; users];
        let mut h_uu = vec![0.0; users];
        let mut h_uw = vec![[0.0; META]; users];
        let mut g_w = [0.0; META];
        let mut h_ww = [[0.0; META]; META];
        for o in obs {
            let z = b[o.user] + dot_meta(&w, &o.meta);
            let (mut dz, mut hz) = (0.0, 0.0);
            for (k, &t) in thresholds.iter().enumerate() {
                let y = if o.level > k { 1.0 } else { -1.0 };
                let s = taste::sigmoid(y * (z - t) / tau);
                dz -= omega * (1.0 - s) * y / tau;
                hz += omega * s * (1.0 - s) / (tau * tau);
            }
            g_u[o.user] += dz;
            h_uu[o.user] += hz;
            for p in 0..META {
                g_w[p] += dz * o.meta[p];
                h_uw[o.user][p] += hz * o.meta[p];
                for q in 0..META {
                    h_ww[p][q] += hz * o.meta[p] * o.meta[q];
                }
            }
        }
        for u in 0..users {
            g_u[u] += b[u] / hyper.var_bias;
            h_uu[u] += 1.0 / hyper.var_bias;
        }
        for p in 0..META {
            g_w[p] += w[p] / hyper.var_m;
            h_ww[p][p] += 1.0 / hyper.var_m;
        }
        // Schur complement on the meta block: S = H_ww − Σ_u h_uw h_uwᵀ / h_uu.
        let mut schur = nalgebra::SMatrix::<f64, META, META>::from_fn(|p, q| h_ww[p][q]);
        let mut rhs = nalgebra::SVector::<f64, META>::from_fn(|p, _| g_w[p]);
        for u in 0..users {
            for p in 0..META {
                rhs[p] -= h_uw[u][p] * g_u[u] / h_uu[u];
                for q in 0..META {
                    schur[(p, q)] -= h_uw[u][p] * h_uw[u][q] / h_uu[u];
                }
            }
        }
        let step_w = schur
            .cholesky()
            .expect("positive definite: the posterior is strictly convex")
            .solve(&rhs);
        let step_b: Vec<f64> = (0..users)
            .map(|u| (g_u[u] - (0..META).map(|p| h_uw[u][p] * step_w[p]).sum::<f64>()) / h_uu[u])
            .collect();
        // The Newton decrement gᵀ H⁻¹ g: small means converged.
        let decrease = (0..META).map(|p| g_w[p] * step_w[p]).sum::<f64>()
            + (0..users).map(|u| g_u[u] * step_b[u]).sum::<f64>();
        // Relative: with ~10⁶ terms an absolute test stays above rounding noise.
        if decrease.abs() < 1e-10 * f.abs().max(1.0) {
            break;
        }
        let mut alpha = 1.0;
        loop {
            let nb: Vec<f64> = b.iter().zip(&step_b).map(|(x, s)| x - alpha * s).collect();
            let nw: [f64; META] = std::array::from_fn(|p| w[p] - alpha * step_w[p]);
            let nf = objective(&nb, &nw);
            if nf <= f - 0.25 * alpha * decrease {
                b = nb;
                w = nw;
                f = nf;
                break;
            }
            alpha *= 0.5;
            if alpha < 1e-10 {
                // No step decreases the objective: at the optimum to rounding.
                return w;
            }
        }
    }
    w
}

fn dot_meta(w: &[f64; META], m: &[f64; META]) -> f64 {
    w.iter().zip(m).map(|(a, b)| a * b).sum()
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
        device: None,
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

/// The cold vectors, their co-like lists, and the cold works' rows in `space`.
fn cold(
    duck: &Duck,
    config: &Config,
    model: &Path,
    space: &Space,
    layout: taste::Layout,
) -> anyhow::Result<(Space, RowLists, HashSet<u32>)> {
    let cold_space = Space::load(model, true, layout)?;
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
    let cold_rows = cold_years.iter().filter_map(|&k| space.row_of(k)).collect();
    Ok((cold_space, cold_lists, cold_rows))
}

/// Where `kindred-world bench` runs the per-reader work (replays, fits, ranking).
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Compute {
    /// On the CPU, one user at a time: taste's own code (04-benchmark.md §1).
    Cpu,
    /// Batched across users on an NVIDIA GPU ([`batch`], [`gpu`]; the `gpu` feature).
    Cuda,
}

fn open_device(compute: Compute) -> anyhow::Result<Option<Mutex<Box<dyn batch::Device>>>> {
    match compute {
        Compute::Cpu => Ok(None),
        #[cfg(feature = "gpu")]
        Compute::Cuda => Ok(Some(Mutex::new(Box::new(gpu::Cuda::new()?)))),
        #[cfg(not(feature = "gpu"))]
        Compute::Cuda => {
            anyhow::bail!("--device cuda: this binary was built without the `gpu` feature")
        }
    }
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
    quick_tuning: bool,
    only: &[String],
    compute: Compute,
) -> anyhow::Result<String> {
    let layout = taste::Layout {
        k: config.factors.k,
        r: config.factors.k_stars,
        a: aspects,
    };
    let space = Space::load(model, false, layout).context("run a build first: no vectors")?;
    let prior: Prior = serde_json::from_slice(&std::fs::read(prior_path(model))?)?;
    let users = data::users(duck, which, config.interactions.seed)?;
    tracing::info!(users = users.len(), which = ?which, "benchmark users loaded");
    let neighbours = crate::neighbours::read(&crate::neighbours::path(model, false))?;
    let lists = RowLists::new(&space, &neighbours);
    anyhow::ensure!(
        which == Users::Tuning || (!quick_tuning && only.is_empty()),
        "--quick and --only are for the tuning users"
    );
    let device = open_device(compute)?;
    if which == Users::Tuning && quick_tuning {
        let (cold_space, cold_lists, cold_rows) = cold(duck, config, model, &space, layout)?;
        return Ok(quick(
            &space,
            &users,
            config,
            &lists,
            (&cold_space, &cold_lists, cold_rows),
            device.as_ref(),
        ));
    }
    if which == Users::Tuning && !report_tuning {
        return tune(&space, &users, config, lists, only, device.as_ref());
    }
    let (cold_space, cold_lists, cold_rows) = cold(duck, config, model, &space, layout)?;
    let embeddings = Matrix::read(&crate::embed::path(model))?;
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
        // Everything learned from other readers: the implicit and star factors, and the star
        // bias (the meta block's fifth number).
        without_c: Some(space.without(&[
            layout.c(),
            layout.r(),
            layout.m().start + 4..layout.m().start + 5,
        ])),
        without_a: Some(space.without(&[layout.a()])),
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
        device: device.as_ref(),
    };
    let sums = measure(&setup, &users, Scope::Report);
    let report = report(&setup, users.len(), &sums, agreement);
    std::fs::write(
        model.join(format!("bench-{which:?}.json").to_lowercase()),
        serde_json::to_vec_pretty(&report)?,
    )?;
    Ok(report.markdown())
}

/// The hyperparameter search of 04-benchmark.md §3, one setting at a time over its grid, then
/// one more pass; returns the chosen values as the `[model]` section of world.toml.
///
/// - The settings of `mu(i)` (var_c, var_r, var_a, var_m, tau_v, omega_v) maximise FULL's
///   pairwise accuracy at n = 20 over all tuning users (how well `mu` orders what the reader
///   read), among the values whose NDCG@10 at n = 20 is at most one standard error below the best
///   NDCG@10 seen so far (a better `mu` must not cost the ranking).
/// - The pick-up and text settings maximise NDCG@10 at n = 20 over a fixed seeded subset of
///   [`TUNE_NDCG_USERS`] tuning users.
/// - `tau_c` is not searched: the simulation answers comparisons from the same stars as the
///   verdicts, so it cannot value them (it stays at the 03-engine.md default).
///
/// A value chosen at the edge of its grid extends the grid once in that direction (the next
/// step of the grid's ratio; never below 0, and text_shrink never above 1).
fn tune(
    space: &Space,
    users: &[User],
    config: &Config,
    lists: RowLists,
    only: &[String],
    device: Option<&Mutex<Box<dyn batch::Device>>>,
) -> anyhow::Result<String> {
    type Setter = fn(&mut Hyper, f64);
    // (name, grid, setter, what it is chosen on, largest allowed value).
    let grid: [(&str, &[f64], Setter, Objective, f64); 12] = [
        (
            "agree",
            &[0.0, 0.25, 0.5, 1.0, 2.0],
            |h, v| h.agree = v,
            Objective::Pairwise,
            f64::INFINITY,
        ),
        (
            "agree_shrink",
            &[0.25, 1.0, 4.0],
            |h, v| h.agree_shrink = v,
            Objective::Pairwise,
            f64::INFINITY,
        ),
        (
            "var_c",
            &[0.5, 1.0, 2.25, 4.0],
            |h, v| h.var_c = v,
            Objective::Pairwise,
            f64::INFINITY,
        ),
        (
            "var_r",
            &[0.25, 0.5, 1.0, 2.0, 4.0],
            |h, v| h.var_r = v,
            Objective::Pairwise,
            f64::INFINITY,
        ),
        (
            "var_a",
            &[0.005, 0.0225, 0.1],
            |h, v| h.var_a = v,
            Objective::Pairwise,
            f64::INFINITY,
        ),
        (
            "var_m",
            &[0.05, 0.25, 1.0],
            |h, v| h.var_m = v,
            Objective::Pairwise,
            f64::INFINITY,
        ),
        (
            "tau_v",
            &[0.5, 1.0, 2.0, 4.0],
            |h, v| h.tau_v = v,
            Objective::Pairwise,
            f64::INFINITY,
        ),
        (
            "omega_v",
            &[0.33, 0.5, 1.0],
            |h, v| h.omega_v = v,
            Objective::Pairwise,
            f64::INFINITY,
        ),
        (
            "text_shrink",
            &[0.5, 0.7, 1.0],
            |h, v| h.text_shrink = v,
            Objective::Ndcg,
            1.0,
        ),
        (
            "text_var",
            &[0.0, 0.25, 1.0],
            |h, v| h.text_var = v,
            Objective::Ndcg,
            f64::INFINITY,
        ),
        (
            "pickup",
            &[0.0, 0.25, 0.5, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0],
            |h, v| h.pickup = v,
            Objective::Ndcg,
            f64::INFINITY,
        ),
        (
            "pickup_pop",
            &[0.0, 0.25, 0.5, 1.0, 2.0],
            |h, v| h.pickup_pop = v,
            Objective::Ndcg,
            f64::INFINITY,
        ),
    ];
    // NDCG ranks every candidate for every user: a fixed seeded subset of the tuning users keeps
    // a step to about a minute.
    let ndcg_users = tuning_subset(users, config.interactions.seed);
    let built = config.model.text_shrink;
    let mut hyper = config.model.clone();
    for name in only {
        anyhow::ensure!(
            grid.iter().any(|g| g.0 == name),
            "--only {name}: not a searched setting"
        );
    }
    let mut cache: HashMap<(String, Objective), Estimate> = HashMap::new();
    // The prior depends on the settings its fit reads; refit it only when one of them changes.
    let mut priors: HashMap<String, Prior> = HashMap::new();
    let mut evaluate = |h: &Hyper, objective: Objective| -> Estimate {
        let key = serde_json::to_string(h).expect("serialisable");
        if let Some(&v) = cache.get(&(key.clone(), objective)) {
            return v;
        }
        let shrunk = (h.text_shrink != built).then(|| space.with_text_shrink(built, h.text_shrink));
        let prior_key = format!(
            "{:?}",
            [h.b1, h.b2, h.b3, h.omega_v, h.tau_v, h.var_bias, h.var_m]
        );
        let prior = priors
            .entry(prior_key)
            .or_insert_with(|| {
                prior_from(
                    space,
                    users,
                    h,
                    config.bench.max_prior_terms,
                    config.interactions.seed,
                )
            })
            .clone();
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
            device,
        };
        let (scope, who) = match objective {
            Objective::Ndcg => (Scope::FullAtMain, &ndcg_users[..]),
            Objective::Pairwise => (Scope::PairwiseAtMain, users),
        };
        let a = measure(&setup, who, scope)
            .main
            .remove(&(Method::Full, 0))
            .unwrap_or_default();
        let v = match objective {
            Objective::Ndcg => Estimate::of(a.ndcg, a.ndcg_sq, a.ranked_users),
            Objective::Pairwise => Estimate::of(a.pairwise, a.pairwise_sq, a.pairwise_users),
        };
        tracing::info!(mean = v.mean, se = v.se, objective = ?objective, hyper = %key, "tuning step");
        cache.insert((key, objective), v);
        v
    };
    let mut log = String::new();
    // The best NDCG@10 seen so far, with its standard error.
    let mut best_ndcg = Estimate {
        mean: f64::NEG_INFINITY,
        se: 0.0,
    };
    let mut grids: Vec<Vec<f64>> = grid.iter().map(|g| g.1.to_vec()).collect();
    let mut extended = [false; 12];
    for pass in 1..=2 {
        for (g, &(name, _, set, objective, cap)) in grid.iter().enumerate() {
            if !only.is_empty() && !only.iter().any(|o| o == name) {
                continue;
            }
            let mut pending = grids[g].clone();
            let mut seen: Vec<(f64, Estimate, Option<Estimate>)> = Vec::new();
            let choice = loop {
                for v in pending.drain(..) {
                    let mut h = hyper.clone();
                    set(&mut h, v);
                    let ndcg = evaluate(&h, Objective::Ndcg);
                    let pairwise = (objective == Objective::Pairwise)
                        .then(|| evaluate(&h, Objective::Pairwise));
                    if ndcg.mean > best_ndcg.mean {
                        best_ndcg = ndcg;
                    }
                    log.push_str(&format!(
                        "pass {pass} {name} = {v}: NDCG@10 {:.5} ± {:.5}",
                        ndcg.mean, ndcg.se
                    ));
                    if let Some(p) = pairwise {
                        log.push_str(&format!(", pairwise {:.5} ± {:.5}", p.mean, p.se));
                    }
                    log.push('\n');
                    seen.push((v, ndcg, pairwise));
                }
                // Pairwise accuracy among the values within one standard error of the best
                // NDCG@10 so far (the best NDCG@10 if none is); NDCG@10 for the others.
                let floor = best_ndcg.mean - best_ndcg.se;
                let allowed: Vec<(f64, f64)> = seen
                    .iter()
                    .filter_map(|&(v, ndcg, pairwise)| match pairwise {
                        Some(p) => (ndcg.mean >= floor).then_some((p.mean, v)),
                        None => Some((ndcg.mean, v)),
                    })
                    .collect();
                let allowed = if allowed.is_empty() {
                    seen.iter().map(|&(v, ndcg, _)| (ndcg.mean, v)).collect()
                } else {
                    allowed
                };
                let Some(v) = allowed
                    .iter()
                    .fold(None, |best: Option<(f64, f64)>, &(s, v)| match best {
                        Some((b, _)) if b >= s => best,
                        _ => Some((s, v)),
                    })
                    .map(|(_, v)| v)
                else {
                    break None;
                };
                if !extended[g]
                    && let Some(next) = beyond_edge(&grids[g], v, cap)
                {
                    extended[g] = true;
                    log.push_str(&format!(
                        "pass {pass} {name}: {v} is at the grid's edge, adding {next}\n"
                    ));
                    grids[g].push(next);
                    grids[g].sort_by(f64::total_cmp);
                    pending.push(next);
                    continue;
                }
                break Some(v);
            };
            if let Some(v) = choice {
                set(&mut hyper, v);
            }
        }
    }
    let ndcg = evaluate(&hyper, Objective::Ndcg);
    let pw = evaluate(&hyper, Objective::Pairwise);
    Ok(format!(
        "{log}\nchosen (tuning users, n = 20: NDCG@10 {:.5} ± {:.5} on {} users, pairwise accuracy {:.5} ± {:.5}):\n\n[model]\n{}",
        ndcg.mean,
        ndcg.se,
        ndcg_users.len(),
        pw.mean,
        pw.se,
        toml::to_string(&hyper)?
    ))
}

/// The value one grid step beyond the edge `v` of `values` (the ratio of the last two steps;
/// double when the step before is 0), when `v` is an edge and the new value is allowed: above 0
/// and at most `cap`.
fn beyond_edge(values: &[f64], v: f64, cap: f64) -> Option<f64> {
    let n = values.len();
    if n < 2 {
        return None;
    }
    if v == values[n - 1] {
        let a = values[n - 2];
        let next = if a > 0.0 { v * v / a } else { 2.0 * v };
        (next > v && next <= cap).then_some(next)
    } else if v == values[0] && v > 0.0 {
        Some(v * v / values[1])
    } else {
        None
    }
}

/// What a tuning step maximises.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Objective {
    Ndcg,
    Pairwise,
}

/// A mean over users and its standard error.
#[derive(Clone, Copy, Debug)]
struct Estimate {
    mean: f64,
    se: f64,
}

impl Estimate {
    /// From the sum, the sum of squares and the count of per-user values.
    fn of(sum: f64, sq: f64, n: f64) -> Estimate {
        if n < 1.0 {
            return Estimate { mean: 0.0, se: 0.0 };
        }
        let mean = sum / n;
        let var = if n > 1.0 {
            ((sq - n * mean * mean) / (n - 1.0)).max(0.0)
        } else {
            0.0
        };
        Estimate {
            mean,
            se: (var / n).sqrt(),
        }
    }
}

/// A fixed seeded subset of [`TUNE_NDCG_USERS`] tuning users.
fn tuning_subset(users: &[User], seed: u64) -> Vec<User> {
    let mut keyed: Vec<(u64, &User)> = users
        .iter()
        .map(|u| (rng(seed, &[u.id as u64, 600]).random(), u))
        .collect();
    keyed.sort_by_key(|&(k, u)| (k, u.id));
    keyed
        .into_iter()
        .take(TUNE_NDCG_USERS)
        .map(|(_, u)| u.clone())
        .collect()
}

/// `kindred-world bench --users tuning --quick`: P, FULL and FULL without comparisons at
/// n = 20 on the tuning subset, NDCG@10 and pairwise accuracy with standard errors, with the
/// prior refitted from `[model]` (minutes, for development).
fn quick(
    space: &Space,
    users: &[User],
    config: &Config,
    lists: &RowLists,
    (cold_space, cold_lists, cold_rows): (&Space, &RowLists, HashSet<u32>),
    device: Option<&Mutex<Box<dyn batch::Device>>>,
) -> String {
    let prior = prior_from(
        space,
        users,
        &config.model,
        config.bench.max_prior_terms,
        config.interactions.seed,
    );
    let who = tuning_subset(users, config.interactions.seed);
    let setup = Setup {
        space,
        without_c: None,
        without_a: None,
        cold_space: Some(cold_space),
        cold_rows,
        content: None,
        neighbours: None,
        lists: Some(lists),
        cold_lists: Some(cold_lists),
        hyper: config.model.clone(),
        prior,
        revealed: vec![N_MAIN],
        seed: config.interactions.seed,
        device,
    };
    let sums = measure(&setup, &who, Scope::Quick);
    let mut out = format!(
        "quick check: tuning subset of {} users, n = {N_MAIN}; m0 = {:?}\n\n| method | NDCG@10 | pairwise accuracy |\n|---|---|---|\n",
        who.len(),
        prior.m0_meta
    );
    for m in [Method::P, Method::Full, Method::FullWithoutComparisons] {
        let a = sums.main.get(&(m, 0)).cloned().unwrap_or_default();
        let n = Estimate::of(a.ndcg, a.ndcg_sq, a.ranked_users);
        let p = Estimate::of(a.pairwise, a.pairwise_sq, a.pairwise_users);
        out += &format!(
            "| {} | {:.4} ± {:.4} | {:.4} ± {:.4} |\n",
            m.name(),
            n.mean,
            n.se,
            p.mean,
            p.se
        );
    }
    let per = |i: usize| sums.cold[i][0] / sums.cold[i][1].max(1.0);
    out += &format!(
        "\ncold works, NDCG@10 at n = 20 only (the report pools every n; {} users): P {:.4}, FULL text-placed {:.4}, FULL real {:.4}, P real {:.4}\n",
        sums.cold[0][1],
        per(0),
        per(1),
        per(2),
        per(3)
    );
    out
}

/// Tuning users whose NDCG@10 a tuning step measures.
const TUNE_NDCG_USERS: usize = 2000;

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

    /// Readers with different rating levels, and the higher raters read books with a higher
    /// fourth meta number, which itself does not matter: the fit with an intercept per user
    /// recovers the planted meta weights; one shared intercept credits the level to that number.
    #[test]
    fn prior_with_intercepts_recovers_planted_meta_weights() {
        use rand_distr::{Distribution as _, StandardNormal};
        let planted = [0.5, -0.3, 0.2, 0.0, 0.6];
        let hyper = Hyper {
            var_m: 100.0,
            ..Hyper::default()
        };
        let mut g = ChaCha8Rng::seed_from_u64(7);
        let mut normal = || -> f64 { StandardNormal.sample(&mut g) };
        let users = 600;
        let mut obs = Vec::new();
        let mut draws = Vec::new();
        for user in 0..users {
            let level = 0.8 * normal();
            for _ in 0..40 {
                let mut meta: [f64; META] = std::array::from_fn(|_| normal());
                meta[3] = level + 0.5 * normal();
                draws.push((user, level, meta));
            }
        }
        let mut g = ChaCha8Rng::seed_from_u64(8);
        for (user, level, meta) in draws {
            // Logistic noise around the utility, cut at b1 < b2 < b3.
            let u: f64 = g.random_range(1e-12..1.0 - 1e-12);
            let latent = level + dot_meta(&planted, &meta) + (u / (1.0 - u)).ln();
            let stars = [hyper.b1, hyper.b2, hyper.b3]
                .iter()
                .filter(|&&t| latent > t)
                .count();
            obs.push(MetaVerdict {
                user,
                meta,
                level: stars,
            });
        }
        let w = fit_meta_with_intercepts(&obs, users, &hyper);
        for (p, (&got, &want)) in w.iter().zip(&planted).enumerate() {
            assert!(
                (got - want).abs() < 0.1,
                "weight {p}: {got} vs {want} ({w:?})"
            );
        }
        let pooled: Vec<MetaVerdict> = obs
            .iter()
            .map(|o| MetaVerdict {
                user: 0,
                meta: o.meta,
                level: o.level,
            })
            .collect();
        let shared = fit_meta_with_intercepts(&pooled, 1, &hyper);
        assert!(shared[3] > 0.3, "one intercept: {shared:?}");
    }
}
