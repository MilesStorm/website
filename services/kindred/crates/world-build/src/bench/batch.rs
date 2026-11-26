//! The benchmark's per-reader work batched across readers on a device (`bench --device cuda`):
//! every user's replay (03-engine.md §7: log, refit, ask), the fit of the final events (§6) and
//! `mu` of every candidate (§8), for a whole group of users at once. The measurements themselves
//! (top-k, NDCG, pairwise accuracy, the personal terms, bands, stretch) stay in `measure_user`
//! on the CPU, which reads what this module computed instead of replaying and fitting itself.
//!
//! The model is unchanged: the same terms (§4), Newton's method from `m0` with step halving and
//! the stopping rule of `taste::fit`, a real refit whenever a reader's events change, and the
//! product's question rule (`taste::next_comparison`: same-verdict partners, `info * memory`,
//! `ask_min`). What a device changes is arithmetic. The objective, the gradient and `w` are f64;
//! the Hessian and its Cholesky factor are f32 in the prior's scale,
//! `S0^½ H S0^½ = I + sum_n h_n (S0^½ x_n)(S0^½ x_n)^T`, whose eigenvalues are all at least 1.
//! An f32 Newton step only slows the last rounds a little (the fixed point is where the f64
//! gradient vanishes, so `w_hat` is the f64 optimum), and `x^T Sigma x` comes from a
//! well-conditioned f32 factor. The tests hold this path to taste's own results.
//!
//! All readers of a group advance in lockstep: at each step every reader that waits for a
//! question is refitted if its events changed (one batched Newton iteration, converged readers
//! dropped round by round), then every candidate partner's `x^T Sigma x` is computed in one
//! batch, and each reader's choice is made here with taste's formula.

use std::collections::{HashMap, HashSet};

use rayon::prelude::*;
use taste::{Event, Hyper, Row};

use super::data::{self, Rating, Space, User};
use super::{
    Method, Mu, N_MAIN, Rule, Scope, Setup, Split, Sums, answer, ask_after, cold_at, measure_user,
    methods, ns, pair_key, placed, rated, read_event, replay, rng,
};

/// Newton's stopping rule and step halving, as `taste::fit` (fit.rs): the largest absolute
/// gradient entry below `GRAD_TOL`, at most `MAX_ROUNDS` rounds, at most `MAX_HALVINGS` halvings.
const GRAD_TOL: f64 = 1e-9;
const MAX_ROUNDS: u32 = 50;
const MAX_HALVINGS: u32 = 60;
/// Readers in one lockstep group: bounds the device's per-reader factors (`d * d` f32 each).
const MAX_READERS: usize = 4096;
/// Candidate-score rows held at once (each `cand.len()` f32): bounds host and device memory.
const MU_ROWS: usize = 256;
/// The second index of a term with one work: `x = phi(a)` rather than `phi(a) - phi(b)`.
pub const ONE: u32 = u32::MAX;
/// A preconditioned Hessian whose f32 Cholesky fails (rounding, never in exact arithmetic: its
/// eigenvalues are at least 1) is retried with this diagonal jitter, times 100 each time.
pub const JITTER: f32 = 1e-6;
pub const JITTER_TRIES: usize = 6;

/// The primitives a device offers the batched path. A device holds one group of readers at a
/// time: the work vectors their terms read, the prior, and per reader `w`, the last trial point,
/// the last gradient and Newton step, and the last Cholesky factor. Readers are indices into
/// the group. Device errors are fatal (a benchmark run cannot continue without its device), so
/// the methods panic on them.
pub trait Device: Send {
    /// Starts a group of `readers` readers. `phi`: the work vectors their terms read, `d`
    /// numbers per row; `m0`, `s0`: the prior mean and variances (03-engine.md §6).
    fn load(&mut self, d: usize, phi: &[f32], m0: &[f64], s0: &[f64], readers: usize);
    /// Replaces every reader's terms.
    fn terms(&mut self, terms: &Terms);
    /// `w = m0` for the listed readers.
    fn reset(&mut self, readers: &[u32]);
    /// The objective `L` (§6) of each listed reader: at `w`, or with `alpha` (one per reader)
    /// at the trial point `w + alpha * step`, which the device keeps.
    fn objective(&mut self, readers: &[u32], alpha: Option<&[f64]>) -> Vec<f64>;
    /// `w =` the trial point of the last [`Device::objective`] with `alpha`.
    fn accept(&mut self, readers: &[u32]);
    /// The gradient at `w` and each term's curvature there, kept for [`Device::step`] and
    /// [`Device::factor`]; returns the largest absolute gradient entry of each reader.
    fn gradient(&mut self, readers: &[u32]) -> Vec<f64>;
    /// The Newton step `H^-1 g` from the last gradient, kept for the trial points.
    fn step(&mut self, readers: &[u32]);
    /// The Cholesky factor of `S0^½ H S0^½` with the curvatures of the last gradient, kept per
    /// reader for [`Device::quad`].
    fn factor(&mut self, readers: &[u32]);
    /// `w` of the listed readers, `d` numbers each.
    fn weights(&mut self, readers: &[u32]) -> Vec<f64>;
    /// `x^T Sigma x` for each `(reader, a, b)` with `x = phi(a) - phi(b)`, from the reader's
    /// last factor.
    fn quad(&mut self, pairs: &[(u32, u32, u32)]) -> Vec<f64>;
    /// Keeps a view's packed candidate vectors (`d` numbers each) on the device; returns a
    /// handle for [`Device::scores`].
    fn candidates(&mut self, d: usize, cand_phi: &[f32]) -> usize;
    /// For each row of `w` (`d` f32 numbers, bias weight zeroed), its score of every candidate
    /// of `handle`: rows × candidates, row after row.
    fn scores(&mut self, handle: usize, w: &[f32]) -> Vec<f32>;
    /// Frees what [`Device::candidates`] kept.
    fn release(&mut self, handle: usize);
}

/// Every reader's terms (03-engine.md §4), `term_n = omega_n ln sigma(y_n (x_n · w - t_n) /
/// tau_n)` with `x_n = phi(a_n) - phi(b_n)` (`b_n` = [`ONE`]: `phi(a_n)`), rows of the group's
/// work vectors. Reader `r`'s terms are `off[r]..off[r + 1]`.
#[derive(Clone, Debug, Default)]
pub struct Terms {
    pub off: Vec<u32>,
    pub a: Vec<u32>,
    pub b: Vec<u32>,
    pub t: Vec<f64>,
    pub y: Vec<f64>,
    pub tau: Vec<f64>,
    pub omega: Vec<f64>,
}

impl Terms {
    fn push(&mut self, a: u32, b: u32, t: f64, y: f64, tau: f64, omega: f64) {
        self.a.push(a);
        self.b.push(b);
        self.t.push(t);
        self.y.push(y);
        self.tau.push(tau);
        self.omega.push(omega);
    }

    /// The most terms of one reader.
    pub fn widest(&self) -> usize {
        self.off
            .windows(2)
            .map(|w| (w[1] - w[0]) as usize)
            .max()
            .unwrap_or(0)
    }
}

/// Which simulated history of a user `measure_user` reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Sim {
    /// A method's replay (FULL, its ablations, FULL without comparisons).
    Method(Method),
    /// The question-value table: (rule, questions) after revealing.
    Question(Rule, usize),
    /// The cold-book test's replay on the cold vectors.
    Cold,
}

/// The vectors a history is simulated on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum View {
    Main,
    WithoutC,
    WithoutA,
    Cold,
}

fn view<'a>(setup: &'a Setup, v: View) -> &'a Space {
    match v {
        View::Main => setup.space,
        View::WithoutC => setup.view(Method::FullWithoutCollaborative),
        View::WithoutA => setup.view(Method::FullWithoutAspects),
        View::Cold => setup.cold_space.expect("cold jobs only with cold vectors"),
    }
}

/// How a history unfolds: the product's flow (log each work, then ask), or all verdicts first
/// and then a number of questions (04-benchmark.md §4, question value).
#[derive(Clone, Copy, Debug)]
enum Plan {
    Replay(Rule),
    After(usize, Rule),
}

/// One history to simulate and fit.
struct Job {
    /// The key `measure_user` looks it up by: what, at which index of n.
    key: (Sim, usize),
    view: View,
    shown: Vec<(u32, Rating)>,
    now: f64,
    plan: Plan,
    /// The parts of the seeded generator `measure_user` would use for it.
    seed: Vec<u64>,
    /// Whether it ranks every candidate (its `mu` is computed on the device).
    rank: bool,
}

/// What a job produced: its events, the weights `w_hat` of their fit and, when it ranks, `mu` of
/// every candidate of its view.
pub(super) struct Simulated {
    pub events: Vec<Event>,
    pub w: Vec<f64>,
    mu: Option<Vec<f32>>,
    view: View,
    rank: bool,
    bias: usize,
    /// Each question point of a Model-rule history: the new work and the partner asked about
    /// (the parity test compares them with taste's choices).
    #[cfg_attr(not(test), expect(dead_code, reason = "read by the parity test"))]
    pub questions: Vec<(u32, Option<u32>)>,
}

impl Mu for Simulated {
    /// As `taste::Fit::score`: `phi · w_hat` without the bias number.
    fn score(&self, phi: &[f32]) -> f64 {
        let utility: f64 = phi
            .iter()
            .zip(&self.w)
            .map(|(&p, w)| f64::from(p) * w)
            .sum();
        utility - f64::from(phi[self.bias]) * self.w[self.bias]
    }

    fn score_rows(&self, rows: &[f32], out: &mut [f32]) {
        if let Some(mu) = self.mu.as_ref().filter(|mu| mu.len() == out.len()) {
            out.copy_from_slice(mu);
            return;
        }
        let d = self.w.len();
        let mut w: Vec<f32> = self.w.iter().map(|&v| v as f32).collect();
        w[self.bias] = 0.0;
        for (row, o) in rows.chunks_exact(d).zip(out.iter_mut()) {
            *o = row.iter().zip(&w).map(|(a, b)| a * b).sum();
        }
    }
}

/// One user's simulated histories.
pub(super) type UserSims = HashMap<(Sim, usize), Simulated>;

/// The histories `measure_user` reads for `user` in `scope`, in the order it reads them.
fn jobs(setup: &Setup, user: &User, scope: Scope) -> Vec<Job> {
    let id = user.id as u64;
    let mut out = Vec::new();
    for (ni, n) in ns(setup, scope) {
        let Some(split) = Split::new(setup, user, n) else {
            continue;
        };
        let rank = scope != Scope::PairwiseAtMain;
        for &method in methods(scope) {
            let (view, rule) = match method {
                Method::Full => (View::Main, Rule::Model),
                Method::FullWithoutCollaborative => (View::WithoutC, Rule::Model),
                Method::FullWithoutAspects => (View::WithoutA, Rule::Model),
                Method::FullWithoutComparisons => (View::Main, Rule::None),
                Method::P | Method::C | Method::I | Method::MF => continue,
            };
            out.push(Job {
                key: (Sim::Method(method), ni),
                view,
                shown: split.shown.clone(),
                now: split.now,
                plan: Plan::Replay(rule),
                seed: vec![id, n as u64, method as u64],
                rank,
            });
        }
        if scope == Scope::Report && n == N_MAIN {
            for rule in [Rule::None, Rule::Random, Rule::Model] {
                for count in [0usize, 10, 20] {
                    if rule == Rule::None && count > 0 {
                        continue;
                    }
                    out.push(Job {
                        key: (Sim::Question(rule, count), ni),
                        view: View::Main,
                        shown: split.shown.clone(),
                        now: split.now,
                        plan: Plan::After(count, rule),
                        seed: vec![id, 200 + count as u64, rule as u64],
                        rank: true,
                    });
                }
            }
        }
        if let Some(cold_space) = setup.cold_space
            && cold_at(setup, scope, n)
            && split.cold(setup, cold_space).is_some()
        {
            out.push(Job {
                key: (Sim::Cold, ni),
                view: View::Cold,
                shown: placed(cold_space, split.revealed),
                now: split.now,
                plan: Plan::Replay(Rule::Model),
                seed: vec![id, 300],
                rank: false,
            });
        }
    }
    out
}

/// Where a Model-rule history stands, as the loops of `replay` and `ask_after`.
enum At {
    /// Logging `shown[i]` (`logged`: its read is in the events), `q` questions asked about it.
    Replay {
        i: usize,
        q: usize,
        logged: bool,
    },
    /// Asking about `shown[i]` against the works before it, `done` questions asked of `count`,
    /// `before` of them before this pass.
    After {
        count: usize,
        i: usize,
        done: usize,
        before: usize,
    },
    Done,
}

struct Reader {
    events: Vec<Event>,
    asked: HashSet<(u32, u32)>,
    at: At,
    /// The event count `w` was fitted at: the same count is the same fit (events only grow).
    fitted: Option<usize>,
    w: Vec<f64>,
    questions: Vec<(u32, Option<u32>)>,
}

impl Reader {
    /// A history about to start; one without fits inside (rules None and Random) is simulated
    /// here at once, by `replay` and `ask_after` themselves.
    fn new(setup: &Setup, job: &Job) -> Reader {
        let space = view(setup, job.view);
        let mut reader = Reader {
            events: Vec::new(),
            asked: HashSet::new(),
            at: At::Done,
            fitted: None,
            w: Vec::new(),
            questions: Vec::new(),
        };
        let mut g = rng(setup.seed, &job.seed);
        let (h, p) = (&setup.hyper, &setup.prior);
        match job.plan {
            Plan::Replay(Rule::Model) => {
                reader.at = At::Replay {
                    i: 0,
                    q: 0,
                    logged: false,
                }
            }
            Plan::After(count, Rule::Model) => {
                reader.events = job
                    .shown
                    .iter()
                    .map(|(row, r)| read_event(*row, r, job.now))
                    .collect();
                reader.at = At::After {
                    count,
                    i: 1,
                    done: 0,
                    before: 0,
                };
            }
            Plan::Replay(rule) => {
                reader.events = replay(space, h, p, &job.shown, rule, job.now, &mut g);
            }
            Plan::After(count, rule) => {
                reader.events = ask_after(space, h, p, &job.shown, count, rule, job.now, &mut g);
            }
        }
        reader
    }

    /// Advances to the next question: the index of the work to ask about, or `None` when the
    /// history is complete.
    fn next(&mut self, job: &Job) -> Option<usize> {
        loop {
            match &mut self.at {
                At::Done => return None,
                At::Replay { i, q, logged } => {
                    if !*logged {
                        if *i >= job.shown.len() {
                            self.at = At::Done;
                            continue;
                        }
                        let (row, r) = &job.shown[*i];
                        self.events.push(read_event(*row, r, job.now));
                        *logged = true;
                        *q = 0;
                        if *i == 0 {
                            *i += 1;
                            *logged = false;
                            continue;
                        }
                    }
                    if *q < super::QUESTIONS_PER_LOG {
                        return Some(*i);
                    }
                    *i += 1;
                    *logged = false;
                }
                At::After {
                    count,
                    i,
                    done,
                    before,
                } => {
                    if *done >= *count {
                        self.at = At::Done;
                    } else if *i >= job.shown.len() {
                        if *done == *before {
                            self.at = At::Done;
                        } else {
                            *before = *done;
                            *i = 1;
                        }
                    } else {
                        return Some(*i);
                    }
                }
            }
        }
    }

    /// After the question [`Reader::next`] returned: whether a partner was asked about.
    fn answered(&mut self, asked: bool) {
        match &mut self.at {
            At::Replay { q, .. } => *q = if asked { *q + 1 } else { usize::MAX },
            At::After { i, done, .. } => {
                *done += usize::from(asked);
                *i += 1;
            }
            At::Done => unreachable!("answered only after a question"),
        }
    }
}

/// Whether the work has a vector the model can use (as taste's `observe::vector`).
fn usable(space: &Space, row: u32) -> bool {
    taste::Vectors::phi(space, Row(row))
        .is_some_and(|p| p.len() == space.layout.d() && p.iter().all(|v| v.is_finite()))
}

/// The terms of every reader's events (03-engine.md §4) over the group's work vectors. The
/// benchmark's events are first reads with a verdict and comparisons; their terms are those of
/// taste's `observe::observations`.
fn terms(jobs: &[Job], readers: &[Reader], local: &HashMap<(View, u32), u32>, h: &Hyper) -> Terms {
    let mut out = Terms::default();
    for (job, reader) in jobs.iter().zip(readers) {
        out.off.push(out.a.len() as u32);
        let at = |row: Row| local.get(&(job.view, row.0)).copied();
        for event in &reader.events {
            match *event {
                Event::Read {
                    work,
                    verdict: Some(v),
                    stopped: None,
                    nth_read: 1,
                    latest: true,
                    age_years,
                } => {
                    let Some(a) = at(work) else { continue };
                    // omega_v * share (1) * recency (1) * age, as observe.rs.
                    let omega = h.omega_v * 1.0 * 1.0 * taste::age_factor(age_years);
                    for (k, t) in [h.b1, h.b2, h.b3].into_iter().enumerate() {
                        let y = if v.level() > k { 1.0 } else { -1.0 };
                        out.push(a, ONE, t, y, h.tau_v, omega);
                    }
                }
                Event::Comparison {
                    better,
                    worse,
                    age_years,
                } => {
                    let (Some(a), Some(b)) = (at(better), at(worse)) else {
                        continue;
                    };
                    out.push(a, b, 0.0, 1.0, h.tau_c, taste::age_factor(age_years));
                }
                ref other => {
                    panic!("the benchmark simulates first reads and comparisons, not {other:?}")
                }
            }
        }
    }
    out.off.push(out.a.len() as u32);
    out
}

/// Newton's method for the listed readers from `w = m0` (03-engine.md §6, as `taste::fit`):
/// solve `H step = g`, halve the step until `L` does not decrease, stop when the largest
/// gradient entry is below [`GRAD_TOL`] or after [`MAX_ROUNDS`] rounds. All readers advance
/// together; a converged reader drops out. Leaves each reader's factor of `H(w_hat)` on the
/// device.
fn newton(dev: &mut dyn Device, readers: &[u32]) {
    dev.reset(readers);
    let mut value = dev.objective(readers, None);
    // Positions in `readers` still iterating.
    let mut active: Vec<usize> = (0..readers.len()).collect();
    let mut rounds = 0;
    while rounds < MAX_ROUNDS && !active.is_empty() {
        let ids: Vec<u32> = active.iter().map(|&p| readers[p]).collect();
        let largest = dev.gradient(&ids);
        active = active
            .into_iter()
            .zip(largest)
            .filter(|&(_, g)| g >= GRAD_TOL || g.is_nan())
            .map(|(p, _)| p)
            .collect();
        if active.is_empty() {
            break;
        }
        rounds += 1;
        let ids: Vec<u32> = active.iter().map(|&p| readers[p]).collect();
        dev.step(&ids);
        // Near the optimum the gain of a full step is below the rounding of L; accept it then
        // rather than halving into noise (as taste).
        let mut alpha = vec![1.0; active.len()];
        let mut moved = vec![false; active.len()];
        let mut pending: Vec<usize> = (0..active.len()).collect();
        for _ in 0..=MAX_HALVINGS {
            if pending.is_empty() {
                break;
            }
            let ids: Vec<u32> = pending.iter().map(|&q| readers[active[q]]).collect();
            let a: Vec<f64> = pending.iter().map(|&q| alpha[q]).collect();
            let trial = dev.objective(&ids, Some(&a));
            let mut accepted = Vec::new();
            let mut rest = Vec::new();
            for (k, &q) in pending.iter().enumerate() {
                let current = value[active[q]];
                let noise = 1e-12 * (1.0 + current.abs());
                if trial[k].is_finite() && trial[k] >= current - noise {
                    value[active[q]] = trial[k];
                    moved[q] = true;
                    accepted.push(ids[k]);
                } else {
                    alpha[q] *= 0.5;
                    rest.push(q);
                }
            }
            dev.accept(&accepted);
            pending = rest;
        }
        active = active
            .into_iter()
            .zip(moved)
            .filter(|&(_, m)| m)
            .map(|(p, _)| p)
            .collect();
    }
    // The covariance is evaluated at w_hat.
    dev.gradient(readers);
    dev.factor(readers);
}

/// Simulates and fits every job (all users of a group) in lockstep.
fn simulate(dev: &mut dyn Device, setup: &Setup, jobs: &[Job]) -> Vec<Simulated> {
    if jobs.is_empty() {
        return Vec::new();
    }
    let layout = setup.space.layout;
    let d = layout.d();
    let h = &setup.hyper;
    // The prior as taste builds it (no "Don't care" answers in the benchmark).
    let prior = taste::fit(&[], setup.space, h, &setup.prior);
    // The work vectors the terms read: every job's revealed works, on its view.
    let mut local: HashMap<(View, u32), u32> = HashMap::new();
    let mut phi: Vec<f32> = Vec::new();
    for job in jobs {
        let space = view(setup, job.view);
        for &(row, _) in &job.shown {
            if usable(space, row) {
                local.entry((job.view, row)).or_insert_with(|| {
                    phi.extend_from_slice(space.vector(row));
                    (phi.len() / d - 1) as u32
                });
            }
        }
    }
    dev.load(
        d,
        &phi,
        prior.prior_mean(),
        prior.prior_variance(),
        jobs.len(),
    );
    let mut readers: Vec<Reader> = jobs.iter().map(|job| Reader::new(setup, job)).collect();
    let refit = |dev: &mut dyn Device, readers: &mut [Reader], list: &[u32]| {
        dev.terms(&terms(jobs, readers, &local, h));
        newton(dev, list);
        let w = dev.weights(list);
        for (&k, w) in list.iter().zip(w.chunks_exact(d)) {
            let reader = &mut readers[k as usize];
            reader.w = w.to_vec();
            reader.fitted = Some(reader.events.len());
        }
    };
    loop {
        let asking: Vec<(usize, usize)> = readers
            .iter_mut()
            .zip(jobs)
            .enumerate()
            .filter_map(|(k, (reader, job))| reader.next(job).map(|i| (k, i)))
            .collect();
        if asking.is_empty() {
            break;
        }
        let stale: Vec<u32> = asking
            .iter()
            .filter(|&&(k, _)| readers[k].fitted != Some(readers[k].events.len()))
            .map(|&(k, _)| k as u32)
            .collect();
        if !stale.is_empty() {
            refit(dev, &mut readers, &stale);
        }
        // Each asking reader's candidate partners (as taste::next_comparison, in order):
        // (partner, x · w_hat, memory), and their (reader, new, partner) rows for the device.
        let mut pairs = Vec::new();
        let mut candidates: Vec<Vec<(u32, f64, f64)>> = Vec::with_capacity(asking.len());
        for &(k, i) in &asking {
            let job = &jobs[k];
            let space = view(setup, job.view);
            let (new, new_rating) = job.shown[i];
            let verdict = data::verdict(new_rating.stars);
            let mut c = Vec::new();
            if let Some(&ln) = local.get(&(job.view, new)) {
                let phi_new = space.vector(new);
                for b in rated(&job.shown[..i], job.now) {
                    let other = b.work.0;
                    if other == new
                        || b.verdict != verdict
                        || readers[k].asked.contains(&pair_key(new, other))
                    {
                        continue;
                    }
                    let Some(&lb) = local.get(&(job.view, other)) else {
                        continue;
                    };
                    let xw: f64 = phi_new
                        .iter()
                        .zip(space.vector(other))
                        .zip(&readers[k].w)
                        .map(|((&n, &b), w)| (f64::from(n) - f64::from(b)) * w)
                        .sum();
                    let memory = if b.recent
                        || matches!(b.verdict, taste::Verdict::Loved | taste::Verdict::NotForMe)
                    {
                        1.0
                    } else {
                        taste::FADED_MEMORY
                    };
                    pairs.push((k as u32, ln, lb));
                    c.push((other, xw, memory));
                }
            }
            candidates.push(c);
        }
        let quad = if pairs.is_empty() {
            Vec::new()
        } else {
            dev.quad(&pairs)
        };
        let mut quad = quad.into_iter();
        for (&(k, i), c) in asking.iter().zip(&candidates) {
            // info = p (1 - p) x^T Sigma x, times memory; the largest wins, the first on a tie;
            // only at ask_min or above (03-engine.md §7).
            let mut best: Option<(u32, f64)> = None;
            for &(other, xw, memory) in c {
                let p = taste::sigmoid(xw / h.tau_c);
                let value = p * (1.0 - p) * quad.next().expect("one per pair") * memory;
                if best.is_none_or(|(_, v)| value > v) {
                    best = Some((other, value));
                }
            }
            let other = best.filter(|&(_, v)| v >= h.ask_min).map(|(o, _)| o);
            let job = &jobs[k];
            let reader = &mut readers[k];
            if let Some(other) = other {
                answer(
                    &mut reader.events,
                    &mut reader.asked,
                    job.shown[i],
                    &job.shown[..i],
                    other,
                    job.now,
                );
            }
            reader.questions.push((job.shown[i].0, other));
            reader.answered(other.is_some());
        }
    }
    // The fit of every history's final events, unless its last refit already was.
    let stale: Vec<u32> = (0..readers.len())
        .filter(|&k| readers[k].fitted != Some(readers[k].events.len()))
        .map(|k| k as u32)
        .collect();
    if !stale.is_empty() {
        refit(dev, &mut readers, &stale);
    }
    readers
        .into_iter()
        .zip(jobs)
        .map(|(reader, job)| Simulated {
            events: reader.events,
            w: reader.w,
            mu: None,
            view: job.view,
            rank: job.rank,
            bias: layout.bias(),
            questions: reader.questions,
        })
        .collect()
}

/// `mu` of every candidate for every ranking history of `sims`, one batch per view.
fn rank(
    dev: &mut dyn Device,
    setup: &Setup,
    sims: &mut [UserSims],
    handles: &mut HashMap<View, usize>,
) {
    let d = setup.space.layout.d();
    for v in [View::Main, View::WithoutC, View::WithoutA, View::Cold] {
        let mut targets: Vec<&mut Simulated> = sims
            .iter_mut()
            .flat_map(|m| m.values_mut())
            .filter(|s| s.rank && s.view == v)
            .collect();
        if targets.is_empty() {
            continue;
        }
        let space = view(setup, v);
        let handle = *handles
            .entry(v)
            .or_insert_with(|| dev.candidates(d, &space.cand_phi));
        // w_hat in f32 with the bias weight zeroed, as taste's score_rows.
        let mut w = Vec::with_capacity(targets.len() * d);
        for s in &targets {
            let start = w.len();
            w.extend(s.w.iter().map(|&x| x as f32));
            w[start + s.bias] = 0.0;
        }
        let mu = dev.scores(handle, &w);
        for (s, m) in targets.iter_mut().zip(mu.chunks_exact(space.cand.len())) {
            s.mu = Some(m.to_vec());
        }
    }
}

/// `measure` on a device: users in groups of at most [`MAX_READERS`] histories, each group
/// simulated and fitted in lockstep, then measured user by user on the CPU (in parallel),
/// summed in user order as the CPU path does.
pub(super) fn measure(setup: &Setup, users: &[User], scope: Scope, dev: &mut dyn Device) -> Sums {
    let mut handles = HashMap::new();
    let mut total = Sums::default();
    let mut start = 0;
    while start < users.len() {
        let mut group: Vec<Job> = Vec::new();
        let mut owner: Vec<usize> = Vec::new();
        let mut end = start;
        while end < users.len() {
            let more = jobs(setup, &users[end], scope);
            if end > start && group.len() + more.len() > MAX_READERS {
                break;
            }
            owner.extend(std::iter::repeat_n(end - start, more.len()));
            group.extend(more);
            end += 1;
        }
        let simulated = simulate(dev, setup, &group);
        let mut per_user: Vec<UserSims> = (start..end).map(|_| HashMap::new()).collect();
        for ((job, sim), &u) in group.iter().zip(simulated).zip(&owner) {
            per_user[u].insert(job.key, sim);
        }
        tracing::debug!(
            users = end - start,
            histories = group.len(),
            "batched group simulated"
        );
        // Ranked and measured in parts of at most MU_ROWS candidate-score rows.
        let mut from = 0;
        while from < per_user.len() {
            let mut rows = 0;
            let mut to = from;
            while to < per_user.len() {
                let r = per_user[to].values().filter(|s| s.rank).count();
                if to > from && rows + r > MU_ROWS {
                    break;
                }
                rows += r;
                to += 1;
            }
            rank(dev, setup, &mut per_user[from..to], &mut handles);
            let part: Vec<Sums> = (from..to)
                .into_par_iter()
                .map(|k| measure_user(setup, &users[start + k], scope, Some(&per_user[k])))
                .collect();
            for s in part {
                total = total.merge(s);
            }
            for m in &mut per_user[from..to] {
                m.clear();
            }
            from = to;
        }
        start = end;
    }
    for (_, handle) in handles {
        dev.release(handle);
    }
    total
}

#[cfg(test)]
pub(super) mod tests {
    use std::sync::Mutex;

    use nalgebra::{Cholesky, DMatrix, Dyn};
    use rand::{Rng as _, SeedableRng as _};
    use rand_chacha::ChaCha8Rng;
    use rand_distr::{Distribution as _, StandardNormal};

    use super::*;
    use crate::bench::{Acc, ask, measure};
    use crate::matrix::Matrix;
    use crate::vectors;

    /// The device semantics on the CPU, in the device's precisions (f64 objective, gradient
    /// and `w`; f32 preconditioned Hessian and Cholesky factor): the batched path's driver
    /// checked without a GPU.
    #[derive(Default)]
    pub struct Emulated {
        d: usize,
        phi: Vec<f32>,
        m0: Vec<f64>,
        s0: Vec<f64>,
        w: Vec<f64>,
        trial: Vec<f64>,
        g: Vec<f64>,
        step: Vec<f64>,
        curvature: Vec<f64>,
        terms: Terms,
        /// Every term's `x_n`, `d` numbers each.
        xs: Vec<f64>,
        factors: Vec<Option<DMatrix<f32>>>,
        cands: Vec<Option<Vec<f32>>>,
    }

    impl Emulated {
        fn x(&self, n: usize) -> &[f64] {
            &self.xs[n * self.d..(n + 1) * self.d]
        }

        fn z(&self, n: usize, w: &[f64]) -> f64 {
            let xw: f64 = self.x(n).iter().zip(w).map(|(x, w)| x * w).sum();
            self.terms.y[n] * (xw - self.terms.t[n]) / self.terms.tau[n]
        }

        fn range(&self, r: u32) -> std::ops::Range<usize> {
            self.terms.off[r as usize] as usize..self.terms.off[r as usize + 1] as usize
        }

        fn row(v: &[f64], d: usize, r: u32) -> &[f64] {
            &v[r as usize * d..(r as usize + 1) * d]
        }

        /// `I + M M^T` with the columns of `M` the terms' `sqrt(h_n) S0^½ x_n`, in f32.
        fn preconditioned(&self, r: u32) -> DMatrix<f32> {
            let d = self.d;
            let range = self.range(r);
            let m = DMatrix::<f32>::from_fn(d, range.len().max(1), |j, c| {
                let n = range.start + c;
                if n < range.end {
                    (self.curvature[n] * self.x(n)[j] * self.s0[j].sqrt()) as f32
                } else {
                    0.0
                }
            });
            &m * m.transpose() + DMatrix::<f32>::identity(d, d)
        }

        fn cholesky(h: DMatrix<f32>) -> Cholesky<f32, Dyn> {
            let mut jitter = 0.0;
            for _ in 0..=JITTER_TRIES {
                let mut j = h.clone();
                for i in 0..j.nrows() {
                    j[(i, i)] += jitter;
                }
                if let Some(c) = Cholesky::new(j) {
                    return c;
                }
                jitter = if jitter == 0.0 {
                    JITTER
                } else {
                    jitter * 100.0
                };
            }
            panic!("no f32 Cholesky factor");
        }
    }

    impl Device for Emulated {
        fn load(&mut self, d: usize, phi: &[f32], m0: &[f64], s0: &[f64], readers: usize) {
            self.d = d;
            self.phi = phi.to_vec();
            self.m0 = m0.to_vec();
            self.s0 = s0.to_vec();
            self.w = m0.repeat(readers);
            self.trial = vec![0.0; readers * d];
            self.g = vec![0.0; readers * d];
            self.step = vec![0.0; readers * d];
            self.factors = vec![None; readers];
        }

        fn terms(&mut self, terms: &Terms) {
            self.terms = terms.clone();
            self.curvature = vec![0.0; terms.a.len()];
            let d = self.d;
            self.xs = (0..terms.a.len())
                .flat_map(|n| {
                    let a = &self.phi[terms.a[n] as usize * d..][..d];
                    let b = (terms.b[n] != ONE).then(|| &self.phi[terms.b[n] as usize * d..][..d]);
                    (0..d).map(move |j| f64::from(a[j]) - b.map_or(0.0, |b| f64::from(b[j])))
                })
                .collect();
        }

        fn reset(&mut self, readers: &[u32]) {
            let d = self.d;
            for &r in readers {
                self.w[r as usize * d..(r as usize + 1) * d].copy_from_slice(&self.m0);
            }
        }

        fn objective(&mut self, readers: &[u32], alpha: Option<&[f64]>) -> Vec<f64> {
            let d = self.d;
            readers
                .iter()
                .enumerate()
                .map(|(k, &r)| {
                    let mut w = Self::row(&self.w, d, r).to_vec();
                    if let Some(alpha) = alpha {
                        let step = Self::row(&self.step, d, r);
                        for j in 0..d {
                            w[j] += alpha[k] * step[j];
                        }
                        self.trial[r as usize * d..(r as usize + 1) * d].copy_from_slice(&w);
                    }
                    let likelihood: f64 = self
                        .range(r)
                        .map(|n| self.terms.omega[n] * taste::ln_sigmoid(self.z(n, &w)))
                        .sum();
                    let prior: f64 = (0..d)
                        .map(|j| (w[j] - self.m0[j]).powi(2) / self.s0[j])
                        .sum();
                    likelihood - 0.5 * prior
                })
                .collect()
        }

        fn accept(&mut self, readers: &[u32]) {
            let d = self.d;
            for &r in readers {
                let s = r as usize * d..(r as usize + 1) * d;
                let t = self.trial[s.clone()].to_vec();
                self.w[s].copy_from_slice(&t);
            }
        }

        fn gradient(&mut self, readers: &[u32]) -> Vec<f64> {
            let d = self.d;
            readers
                .iter()
                .map(|&r| {
                    let w = Self::row(&self.w, d, r).to_vec();
                    let mut g: Vec<f64> =
                        (0..d).map(|j| -(w[j] - self.m0[j]) / self.s0[j]).collect();
                    for n in self.range(r) {
                        let z = self.z(n, &w);
                        let s = taste::sigmoid(z);
                        let (omega, y, tau) =
                            (self.terms.omega[n], self.terms.y[n], self.terms.tau[n]);
                        let c = omega * y * taste::sigmoid(-z) / tau;
                        self.curvature[n] = (omega * s * (1.0 - s)).sqrt() / tau;
                        for (gj, x) in g.iter_mut().zip(self.x(n)) {
                            *gj += c * x;
                        }
                    }
                    self.g[r as usize * d..(r as usize + 1) * d].copy_from_slice(&g);
                    g.iter().fold(0.0f64, |m, v| m.max(v.abs()))
                })
                .collect()
        }

        fn step(&mut self, readers: &[u32]) {
            let d = self.d;
            for &r in readers {
                let chol = Self::cholesky(self.preconditioned(r));
                let g = Self::row(&self.g, d, r);
                let rhs =
                    nalgebra::DVector::<f32>::from_fn(d, |j, _| (g[j] * self.s0[j].sqrt()) as f32);
                let s = chol.solve(&rhs);
                for j in 0..d {
                    self.step[r as usize * d + j] = self.s0[j].sqrt() * f64::from(s[j]);
                }
            }
        }

        fn factor(&mut self, readers: &[u32]) {
            for &r in readers {
                self.factors[r as usize] = Some(Self::cholesky(self.preconditioned(r)).l());
            }
        }

        fn weights(&mut self, readers: &[u32]) -> Vec<f64> {
            readers
                .iter()
                .flat_map(|&r| Self::row(&self.w, self.d, r).to_vec())
                .collect()
        }

        fn quad(&mut self, pairs: &[(u32, u32, u32)]) -> Vec<f64> {
            let d = self.d;
            pairs
                .iter()
                .map(|&(r, a, b)| {
                    let l = self.factors[r as usize].as_ref().expect("factored");
                    let mut v: Vec<f64> = (0..d)
                        .map(|j| {
                            (f64::from(self.phi[a as usize * d + j])
                                - f64::from(self.phi[b as usize * d + j]))
                                * self.s0[j].sqrt()
                        })
                        .collect();
                    let mut acc = 0.0;
                    for j in 0..d {
                        let y = v[j] / f64::from(l[(j, j)]);
                        acc += y * y;
                        for i in j + 1..d {
                            v[i] -= f64::from(l[(i, j)]) * y;
                        }
                    }
                    acc
                })
                .collect()
        }

        fn candidates(&mut self, _d: usize, cand_phi: &[f32]) -> usize {
            self.cands.push(Some(cand_phi.to_vec()));
            self.cands.len() - 1
        }

        fn scores(&mut self, handle: usize, w: &[f32]) -> Vec<f32> {
            let cand = self.cands[handle].as_ref().expect("live handle");
            let d = self.d.max(1);
            w.chunks_exact(d)
                .flat_map(move |w| {
                    cand.chunks_exact(d)
                        .map(move |c| c.iter().zip(w).map(|(a, b)| a * b).sum::<f32>())
                })
                .collect()
        }

        fn release(&mut self, handle: usize) {
            self.cands[handle] = None;
        }
    }

    /// A synthetic world: `works` works with random vectors (some recommendable, some
    /// text-placed), and readers whose stars come from a planted weight vector with logistic
    /// noise, cut at the verdict thresholds (and at -2 between one and two stars, so that
    /// not-for-me pairs can be compared).
    pub fn world(
        layout: taste::Layout,
        works: usize,
        readers: usize,
        seed: u64,
    ) -> (Space, Vec<User>) {
        let mut g = ChaCha8Rng::seed_from_u64(seed);
        fn normal(g: &mut ChaCha8Rng) -> f32 {
            StandardNormal.sample(g)
        }
        let d = layout.d();
        let mut phi = Matrix::new(d);
        let mut flags = Vec::new();
        for key in 1..=works as u64 {
            let mut v = vec![0.0f32; d];
            v[layout.bias()] = 1.0;
            let c: Vec<f32> = layout.c().map(|_| normal(&mut g)).collect();
            let len = c.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
            for (i, x) in layout.c().zip(&c) {
                v[i] = x / len;
            }
            for i in layout.r() {
                v[i] = 0.5 * normal(&mut g);
            }
            for i in layout.a().chain(layout.m()) {
                v[i] = normal(&mut g).clamp(-3.0, 3.0);
            }
            phi.push(key, &v);
            let mut f = 0u8;
            if g.random_bool(0.8) {
                f |= vectors::RECOMMENDABLE;
            }
            if g.random_bool(0.1) {
                f |= vectors::C_FROM_TEXT;
            }
            flags.push(f);
        }
        let space = Space::new(vectors::Vectors { layout, phi, flags });
        let users = (0..readers)
            .map(|id| {
                let mut w = vec![0.0f64; d];
                for i in layout.c() {
                    w[i] = 1.5 * f64::from(normal(&mut g));
                }
                for i in layout.r() {
                    w[i] = 0.8 * f64::from(normal(&mut g));
                }
                for i in layout.a() {
                    w[i] = 0.15 * f64::from(normal(&mut g));
                }
                let level = 0.5 * f64::from(normal(&mut g));
                let count = g.random_range(30..60);
                let mut seen = HashSet::new();
                let mut ratings = Vec::new();
                while ratings.len() < count {
                    let key = g.random_range(1..=works as u64);
                    if !seen.insert(key) {
                        continue;
                    }
                    let row = space.row_of(key).expect("every key has a row");
                    let u: f64 = space
                        .vector(row)
                        .iter()
                        .zip(&w)
                        .map(|(&p, w)| f64::from(p) * w)
                        .sum::<f64>()
                        + level;
                    let e: f64 = g.random_range(1e-9..1.0 - 1e-9);
                    let latent = u + (e / (1.0 - e)).ln();
                    let stars = 1 + [-2.0, -1.0, 0.0, 1.0]
                        .iter()
                        .filter(|&&t| latent > t)
                        .count();
                    // Ten days apart, a few undated; some over three years before the last.
                    let day = if g.random_bool(0.05) {
                        f64::NAN
                    } else {
                        14_000.0 + 40.0 * ratings.len() as f64
                    };
                    ratings.push(Rating {
                        work: key,
                        stars: stars as u8,
                        day,
                    });
                }
                User {
                    id: id as i32,
                    tuning: true,
                    gate: false,
                    ratings,
                }
            })
            .collect();
        (space, users)
    }

    fn setup<'a>(
        space: &'a Space,
        cold: Option<&'a Space>,
        revealed: Vec<usize>,
        device: Option<&'a Mutex<Box<dyn Device>>>,
    ) -> Setup<'a> {
        Setup {
            space,
            without_c: None,
            without_a: None,
            cold_space: cold,
            cold_rows: space.cand.iter().copied().filter(|r| r % 3 == 0).collect(),
            content: None,
            neighbours: None,
            lists: None,
            cold_lists: None,
            hyper: Hyper::default(),
            prior: taste::Prior {
                m0_meta: [0.2, 0.1, 0.0, -0.05, 0.15],
            },
            revealed,
            seed: 11,
            device,
        }
    }

    /// The CPU replay of `replay` with Rule::Model, with each question point logged.
    fn replay_logged(
        setup: &Setup,
        space: &Space,
        split: &Split,
    ) -> (Vec<Event>, Vec<(u32, Option<u32>)>) {
        let mut g = ChaCha8Rng::seed_from_u64(0);
        let mut events = Vec::new();
        let mut asked = HashSet::new();
        let mut last_fit = None;
        let mut log = Vec::new();
        for (i, item) in split.shown.iter().enumerate() {
            events.push(read_event(item.0, &item.1, split.now));
            if i == 0 {
                continue;
            }
            for _ in 0..super::super::QUESTIONS_PER_LOG {
                let other = ask(
                    space,
                    &setup.hyper,
                    &setup.prior,
                    &mut events,
                    &mut asked,
                    *item,
                    &split.shown[..i],
                    Rule::Model,
                    split.now,
                    &mut g,
                    &mut last_fit,
                );
                log.push((item.0, other));
                if other.is_none() {
                    break;
                }
            }
        }
        (events, log)
    }

    fn close(what: &str, a: &Acc, b: &Acc) {
        let (na, nb) = (
            a.ndcg / a.ranked_users.max(1.0),
            b.ndcg / b.ranked_users.max(1.0),
        );
        assert_eq!(a.ranked_users, b.ranked_users, "{what}: users ranked");
        assert!((na - nb).abs() < 1e-3, "{what}: NDCG@10 {na} vs {nb}");
        let (pa, pb) = (
            a.pairwise / a.pairwise_users.max(1.0),
            b.pairwise / b.pairwise_users.max(1.0),
        );
        assert_eq!(
            a.pairwise_users, b.pairwise_users,
            "{what}: users with pairs"
        );
        assert!((pa - pb).abs() < 1e-3, "{what}: pairwise {pa} vs {pb}");
    }

    /// The batched path on `device` against taste on the CPU, on `readers` synthetic readers:
    /// the fits of the same events agree to `w_tol` (relative), at least 99% of the question
    /// points choose the same partner, and NDCG@10 and pairwise accuracy agree to 1e-3 in the
    /// quick check, the tuning scopes and the report.
    pub fn parity(device: Box<dyn Device>, layout: taste::Layout, readers: usize, w_tol: f64) {
        let (space, users) = world(layout, 3000, readers, 5);
        let cold = space.without(&[layout.r()]);
        let device = Mutex::new(device);

        // Replays and fits.
        let cpu = setup(&space, None, vec![N_MAIN], None);
        let mut questions = 0usize;
        let mut same = 0usize;
        let mut worst = 0.0f64;
        let mut fits = 0;
        let mut compared = 0;
        {
            let mut dev = device.lock().unwrap();
            let all: Vec<Job> = users
                .iter()
                .flat_map(|u| jobs(&cpu, u, Scope::FullAtMain))
                .collect();
            let sims = simulate(&mut **dev, &cpu, &all);
            for (user, sim) in users.iter().filter(|u| u.ratings.len() > N_MAIN).zip(&sims) {
                let split = Split::new(&cpu, user, N_MAIN).expect("a future");
                let (events, log) = replay_logged(&cpu, &space, &split);
                let mut g = ChaCha8Rng::seed_from_u64(0);
                let replayed = replay(
                    &space,
                    &cpu.hyper,
                    &cpu.prior,
                    &split.shown,
                    Rule::Model,
                    split.now,
                    &mut g,
                );
                assert_eq!(events, replayed, "the logged replay is replay");
                // Question points agree until the first difference; after it they count as
                // different.
                questions += log.len();
                same += log
                    .iter()
                    .zip(&sim.questions)
                    .take_while(|(a, b)| a == b)
                    .count();
                let fit = taste::fit(&sim.events, &space, &cpu.hyper, &cpu.prior);
                for (a, b) in sim.w.iter().zip(fit.weights()) {
                    worst = worst.max((a - b).abs() / (1.0 + b.abs()));
                }
                fits += 1;
                compared += sim
                    .events
                    .iter()
                    .filter(|e| matches!(e, Event::Comparison { .. }))
                    .count();
            }
        }
        assert!(fits > readers / 2, "most readers have a future: {fits}");
        assert!(worst < w_tol, "w_hat: worst relative difference {worst}");
        let share = same as f64 / questions as f64;
        assert!(
            share >= 0.99,
            "same partner at {share} of {questions} question points"
        );
        assert!(questions > users.len(), "questions were asked: {questions}");
        assert!(compared > 0, "some answers were comparisons");

        // The measurements.
        for (scope, revealed, with_cold) in [
            (Scope::Quick, vec![N_MAIN], true),
            (Scope::FullAtMain, vec![N_MAIN], false),
            (Scope::PairwiseAtMain, vec![N_MAIN], false),
            (Scope::Report, vec![0, 5, N_MAIN], true),
        ] {
            let cold = with_cold.then_some(&cold);
            let who = if scope == Scope::Report {
                &users[..users.len().min(40)]
            } else {
                &users[..]
            };
            let a = measure(&setup(&space, cold, revealed.clone(), None), who, scope);
            let b = measure(&setup(&space, cold, revealed, Some(&device)), who, scope);
            for (key, acc) in &a.main {
                close(&format!("{scope:?} {key:?}"), acc, &b.main[key]);
            }
            for (key, v) in &a.questions {
                let w = b.questions[key];
                assert!(
                    (v[0] / v[1].max(1.0) - w[0] / w[1].max(1.0)).abs() < 1e-3
                        && (v[2] / v[3].max(1.0) - w[2] / w[3].max(1.0)).abs() < 1e-3,
                    "question value {key:?}: {v:?} vs {w:?}"
                );
            }
            for i in 0..4 {
                let (x, y) = (a.cold[i], b.cold[i]);
                assert_eq!(x[1], y[1], "{scope:?} cold users");
                assert!(
                    (x[0] / x[1].max(1.0) - y[0] / y[1].max(1.0)).abs() < 1e-3,
                    "{scope:?} cold {i}: {x:?} vs {y:?}"
                );
            }
        }
    }

    /// At a small vector size and 150 readers: the emulated device runs unoptimised in test
    /// builds (about 4 ms a fit). The GPU test runs 300 readers at the product's size.
    #[test]
    fn batched_path_matches_taste() {
        let layout = taste::Layout { k: 16, r: 8, a: 12 };
        parity(Box::new(Emulated::default()), layout, 150, 1e-6);
    }

    /// The product's vector size (d = 152); slower, for checking f32 at full size.
    #[test]
    #[ignore = "slow (minutes in a test build); the GPU test runs this size"]
    fn batched_path_matches_taste_full_size() {
        let layout = taste::Layout {
            k: 64,
            r: 32,
            a: 50,
        };
        parity(Box::new(Emulated::default()), layout, 300, 1e-6);
    }
}
