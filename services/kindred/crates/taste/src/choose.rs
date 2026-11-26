//! Choosing with a fit: the next comparison question and "Have you read…?" (§7), the three
//! picks with their Thompson-sampled stretch, novelty, the context term and the steers (§8),
//! and the seed that makes picks stable across reloads.

use nalgebra::DVector;
use rand::SeedableRng as _;
use rand_distr::{Distribution as _, Normal};

use crate::observe::vector;
use crate::{Fit, Hyper, Layout, Row, Vectors, Verdict, sigmoid};

/// `memory(B)` for an earlier work the user may remember less well (§7).
pub const FADED_MEMORY: f64 = 0.6;
/// Weight of the "unlike the first likely fit" term of the second likely fit (§8).
const SECOND_PICK_SPREAD: f64 = 0.5;
/// The stretch pick's novelty requirement is at least this quantile of the candidates' novelty.
const STRETCH_QUANTILE: f64 = 0.5;
/// Aspect scores count up to this in the context term (§8).
const CTX_CAP: f64 = 2.0;
/// How many of the last work's aspects stand in for chips the user did not tap (§8).
const CTX_ASPECTS: usize = 3;
/// The Shorter steer keeps works up to `max(SHORTER_FLOOR, SHORTER_SHARE * median pages)`.
const SHORTER_FLOOR: f64 = 250.0;
const SHORTER_SHARE: f64 = 0.7;

/// An earlier rated work of the user, for [`next_comparison`]. `recent`: logged within three
/// years.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Rated {
    pub work: Row,
    pub verdict: Verdict,
    pub recent: bool,
}

/// Which earlier rated work to compare the newly logged work `new` (with verdict `verdict`)
/// with (§7), or `None` when no pair reaches `hyper.ask_min`. Only works with the same verdict
/// are partners: for two works with different verdicts the verdicts already say which one the
/// reader liked more, so the answer would count that evidence twice. For each such `B` in `rated`
/// (not `new`, not a pair `excluded` says was already compared or skipped, and with a vector):
/// `x = phi(new) - phi(B)`, `p = sigma(x · w_hat / tau_c)`,
/// `info = p (1 - p) x^T Sigma x`, and `memory` 1.0 for a loved or not-for-me or recent `B`,
/// else 0.6. The largest `info * memory` wins; the first one on a tie. The caller refits after
/// the answer and asks again, at most twice.
pub fn next_comparison(
    fit: &Fit,
    world: &impl Vectors,
    hyper: &Hyper,
    new: Row,
    verdict: Verdict,
    rated: &[Rated],
    excluded: &dyn Fn(Row, Row) -> bool,
) -> Option<Row> {
    let layout = world.layout();
    let phi_new = vector(world, layout, new)?;
    let w = DVector::from_column_slice(fit.weights());
    let mut best: Option<(Row, f64)> = None;
    for b in rated {
        if b.work == new || b.verdict != verdict || excluded(new, b.work) {
            continue;
        }
        let Some(phi_b) = vector(world, layout, b.work) else {
            continue;
        };
        let x = DVector::from_iterator(layout.d(), phi_new.iter().zip(&phi_b).map(|(n, b)| n - b));
        let p = sigmoid(x.dot(&w) / hyper.tau_c);
        let info = p * (1.0 - p) * (fit.covariance() * &x).dot(&x);
        let memory = if b.recent || matches!(b.verdict, Verdict::Loved | Verdict::NotForMe) {
            1.0
        } else {
            FADED_MEMORY
        };
        let value = info * memory;
        if best.is_none_or(|(_, v)| value > v) {
            best = Some((b.work, value));
        }
    }
    best.filter(|&(_, v)| v >= hyper.ask_min)
        .map(|(work, _)| work)
}

/// "Have you read…?" (§7): among `candidates` (work, popularity z-score), already restricted by
/// the caller to the most popular works not logged, saved, dismissed or asked, the one with the
/// largest `sigma(popularity_z) * phi^T Sigma phi`: popular enough that the user may have read
/// it, and where an answer would teach the model the most.
pub fn have_you_read(fit: &Fit, world: &impl Vectors, candidates: &[(Row, f64)]) -> Option<Row> {
    let layout = world.layout();
    let mut best: Option<(Row, f64)> = None;
    for &(work, popularity_z) in candidates {
        let Some(phi) = vector(world, layout, work) else {
            continue;
        };
        let phi = DVector::from_vec(phi);
        let value = sigmoid(popularity_z) * (fit.covariance() * &phi).dot(&phi);
        if best.is_none_or(|(_, v)| value > v) {
            best = Some((work, value));
        }
    }
    best.map(|(work, _)| work)
}

/// A candidate for the picks, prepared by the caller: already filtered (unread, unsaved, not
/// dismissed, a language the user reads, series order), with its `novelty` ([`novelty`]),
/// quality z-score and context term ([`ctx`], 0 without a context).
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Candidate {
    pub work: Row,
    pub author: u64,
    pub novelty: f64,
    pub quality_z: f64,
    pub ctx: f64,
    /// The pick-up term's addition to the ranking ([`crate::bonus`]); 0 ranks by `mu` alone.
    pub pickup: f64,
}

/// The series pick, chosen by the caller (§8 pick 1): the next unread entry of the liked or
/// loved series touched most recently.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SeriesNext {
    pub work: Row,
    pub author: u64,
}

/// The three picks (§8). `likely` holds one work after a series pick and two without one; any
/// slot is empty when no candidate qualifies. Every pick has a different author.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Picks {
    pub series: Option<Row>,
    pub likely: Vec<Row>,
    pub stretch: Option<Row>,
}

/// The three picks (§8), reproducible from `seed` ([`seed`]).
///
/// 1. *Series:* `series`, as given.
/// 2. *Likely fit:* the candidate maximising `mu(i) + ctx(i) + pickup(i)` (bias dropped), with
///    an author not already picked. Without a series pick, a second likely fit maximising
///    `mu(i) + ctx(i) + pickup(i) - 0.5 cos(c(i), c(first))`, again a new author.
/// 3. *Stretch:* [`stretch`] among the rest, a new author too.
///
/// `novelty_quantile` is the stretch's novelty requirement as a quantile of the candidates'
/// novelty: 0.5 (the median) normally, 0.75 for the Something different steer. Above 0.5 it
/// applies to the likely fits as well, which is what that steer asks for "all three picks"; the
/// series pick is the caller's to keep or drop.
pub fn picks(
    fit: &Fit,
    world: &impl Vectors,
    hyper: &Hyper,
    series: Option<SeriesNext>,
    candidates: &[Candidate],
    novelty_quantile: f64,
    seed: u64,
) -> Picks {
    let layout = world.layout();
    let mut rng = rand_chacha::ChaCha8Rng::seed_from_u64(seed);
    let usable: Vec<(&Candidate, &[f32])> = candidates
        .iter()
        .filter_map(|c| usable_phi(world, layout, c.work).map(|phi| (c, phi)))
        .collect();
    let novelties: Vec<f64> = usable.iter().map(|(c, _)| c.novelty).collect();
    let likely_floor = if novelty_quantile > STRETCH_QUANTILE {
        quantile(&novelties, novelty_quantile).unwrap_or(f64::NEG_INFINITY)
    } else {
        f64::NEG_INFINITY
    };

    let mut taken: Vec<(Row, u64)> = series.iter().map(|s| (s.work, s.author)).collect();
    let free = |c: &Candidate, taken: &[(Row, u64)]| {
        taken.iter().all(|&(w, a)| w != c.work && a != c.author)
    };
    let mut likely = Vec::new();
    let first = argmax(
        usable
            .iter()
            .filter(|(c, _)| c.novelty >= likely_floor && free(c, &taken)),
        |(c, phi)| fit.score(phi) + c.ctx + c.pickup,
    );
    if let Some((c, phi_first)) = first {
        likely.push(c.work);
        taken.push((c.work, c.author));
        if series.is_none() {
            let c_first = &phi_first[layout.c()];
            let second = argmax(
                usable
                    .iter()
                    .filter(|(c, _)| c.novelty >= likely_floor && free(c, &taken)),
                |(c, phi)| {
                    fit.score(phi) + c.ctx + c.pickup
                        - SECOND_PICK_SPREAD * cosine(&phi[layout.c()], c_first)
                },
            );
            if let Some((c, _)) = second {
                likely.push(c.work);
                taken.push((c.work, c.author));
            }
        }
    }
    let remaining: Vec<(&Candidate, &[f32])> = usable
        .iter()
        .copied()
        .filter(|(c, _)| free(c, &taken))
        .collect();
    let floor = quantile(&novelties, novelty_quantile.max(STRETCH_QUANTILE));
    let stretch =
        floor.and_then(|floor| draw_stretch(fit, world, hyper, &remaining, floor, &mut rng));
    Picks {
        series: series.map(|s| s.work),
        likely,
        stretch,
    }
}

/// The stretch pick alone (§8): draw `w_tilde = w_hat + chol(Sigma) z` from `rng`; among
/// candidates with novelty at or above the `max(0.5, novelty_quantile)` quantile of the
/// candidates' novelty and quality z-score at or above 0, take the largest `phi(i) · w_tilde`
/// (bias dropped), plus a normal draw with variance `text_var` for a text-sourced work. This is
/// Thompson sampling: works the model is unsure about win sometimes, in proportion to how
/// plausibly they are good.
pub fn stretch(
    fit: &Fit,
    world: &impl Vectors,
    hyper: &Hyper,
    candidates: &[Candidate],
    novelty_quantile: f64,
    rng: &mut impl rand::Rng,
) -> Option<Row> {
    let layout = world.layout();
    let usable: Vec<(&Candidate, &[f32])> = candidates
        .iter()
        .filter_map(|c| usable_phi(world, layout, c.work).map(|phi| (c, phi)))
        .collect();
    let novelties: Vec<f64> = usable.iter().map(|(c, _)| c.novelty).collect();
    let floor = quantile(&novelties, novelty_quantile.max(STRETCH_QUANTILE))?;
    draw_stretch(fit, world, hyper, &usable, floor, rng)
}

fn draw_stretch(
    fit: &Fit,
    world: &impl Vectors,
    hyper: &Hyper,
    usable: &[(&Candidate, &[f32])],
    novelty_floor: f64,
    rng: &mut impl rand::Rng,
) -> Option<Row> {
    let mut w_tilde = fit.sample_weights(rng);
    w_tilde[fit.layout().bias()] = 0.0;
    let text_noise = Normal::new(0.0, hyper.text_var.max(0.0).sqrt()).ok();
    let mut best: Option<(Row, f64)> = None;
    for (c, phi) in usable {
        if c.novelty < novelty_floor || c.quality_z < 0.0 {
            continue;
        }
        let mut value: f64 = phi
            .iter()
            .zip(&w_tilde)
            .map(|(&p, w)| f64::from(p) * w)
            .sum();
        if world.text_sourced(c.work)
            && let Some(noise) = &text_noise
        {
            value += noise.sample(rng);
        }
        if best.is_none_or(|(_, v)| value > v) {
            best = Some((c.work, value));
        }
    }
    best.map(|(work, _)| work)
}

/// The work's vector when it is usable (right length, finite).
fn usable_phi(world: &impl Vectors, layout: Layout, work: Row) -> Option<&[f32]> {
    world
        .phi(work)
        .filter(|phi| phi.len() == layout.d() && phi.iter().all(|v| v.is_finite()))
}

/// The first item with the largest finite key.
fn argmax<T>(items: impl Iterator<Item = T>, key: impl Fn(&T) -> f64) -> Option<T> {
    let mut best: Option<(T, f64)> = None;
    for item in items {
        let k = key(&item);
        if k.is_finite() && best.as_ref().is_none_or(|(_, b)| k > *b) {
            best = Some((item, k));
        }
    }
    best.map(|(item, _)| item)
}

/// The `q` quantile of `values` with linear interpolation between order statistics; `None`
/// for no values.
fn quantile(values: &[f64], q: f64) -> Option<f64> {
    let mut sorted: Vec<f64> = values.iter().copied().filter(|v| !v.is_nan()).collect();
    if sorted.is_empty() {
        return None;
    }
    sorted.sort_by(f64::total_cmp);
    let pos = q.clamp(0.0, 1.0) * (sorted.len() - 1) as f64;
    let (lo, hi) = (pos.floor() as usize, pos.ceil() as usize);
    Some(sorted[lo] + (sorted[hi] - sorted[lo]) * (pos - lo as f64))
}

/// Cosine similarity in f64; 0 when either vector is zero.
pub fn cosine(a: &[f32], b: &[f32]) -> f64 {
    let (mut ab, mut aa, mut bb) = (0.0, 0.0, 0.0);
    for (&x, &y) in a.iter().zip(b) {
        let (x, y) = (f64::from(x), f64::from(y));
        ab += x * y;
        aa += x * x;
        bb += y * y;
    }
    if aa == 0.0 || bb == 0.0 {
        0.0
    } else {
        ab / (aa.sqrt() * bb.sqrt())
    }
}

/// `novelty(i) = 1 - max_j cos(c(i), c(j))` over the collaborative blocks of the user's liked
/// or loved works (§8). With nothing liked yet every work is fully novel (1).
pub fn novelty(c_i: &[f32], liked_c: &[&[f32]]) -> f64 {
    liked_c
        .iter()
        .map(|c_j| cosine(c_i, c_j))
        .max_by(f64::total_cmp)
        .map_or(1.0, |m| 1.0 - m)
}

/// The aspects the context term rewards and penalises after logging work `last` (§8):
/// `positive` = the aspects chipped positive, or, if none and the verdict is liked or loved,
/// the last work's three strongest aspects (largest `a_k`, `a_k > 0`) with `w_hat_k > 0`;
/// `negative` likewise with chips negative, verdict okay or not for me, and `w_hat_k < 0`.
pub fn context_aspects(
    fit: &Fit,
    phi_last: &[f32],
    verdict: Verdict,
    chipped_positive: &[usize],
    chipped_negative: &[usize],
) -> (Vec<usize>, Vec<usize>) {
    let layout = fit.layout();
    let strongest = |sign: f64| -> Vec<usize> {
        let mut ks: Vec<usize> = (0..layout.a)
            .filter(|&k| {
                let i = layout.aspect(k);
                phi_last[i] > 0.0 && fit.weights()[i] * sign > 0.0
            })
            .collect();
        ks.sort_by(|&x, &y| {
            phi_last[layout.aspect(y)]
                .total_cmp(&phi_last[layout.aspect(x)])
                .then(x.cmp(&y))
        });
        ks.truncate(CTX_ASPECTS);
        ks
    };
    let positive = if !chipped_positive.is_empty() {
        chipped_positive.to_vec()
    } else if verdict.is_positive() {
        strongest(1.0)
    } else {
        Vec::new()
    };
    let negative = if !chipped_negative.is_empty() {
        chipped_negative.to_vec()
    } else if verdict.is_positive() {
        Vec::new()
    } else {
        strongest(-1.0)
    };
    (positive, negative)
}

/// The context term of a candidate after logging a work (§8):
/// `beta * ( mean_{k in positive} min(a_k(i), 2) - mean_{k in negative} clamp(a_k(i), 0, 2)
///           + s * cos(c(i), c(last)) )`.
/// `s` is 1 after a liked or loved verdict and for More like my last one, else 0; `last` is the
/// last work's vector. An empty aspect list contributes 0.
pub fn ctx(
    layout: Layout,
    phi: &[f32],
    positive: &[usize],
    negative: &[usize],
    last: Option<&[f32]>,
    s: f64,
    beta: f64,
) -> f64 {
    let a = |k: usize| f64::from(phi[layout.aspect(k)]);
    let pos = mean(positive.iter().map(|&k| a(k).min(CTX_CAP)));
    let neg = mean(negative.iter().map(|&k| a(k).clamp(0.0, CTX_CAP)));
    let similar = last.map_or(0.0, |l| s * cosine(&phi[layout.c()], &l[layout.c()]));
    beta * (pos - neg + similar)
}

/// The Lighter steer's addition to `ctx` (§8): `beta * (mean of the aspects with steer "light"
/// - mean of those with steer "dark")`, from `aspects.toml`.
pub fn lighter(layout: Layout, phi: &[f32], light: &[usize], dark: &[usize], beta: f64) -> f64 {
    let a = |k: &usize| f64::from(phi[layout.aspect(*k)]);
    beta * (mean(light.iter().map(a)) - mean(dark.iter().map(a)))
}

/// The Shorter steer's page limit (§8): `max(250, 0.7 × the user's median pages)`; 250 when the
/// user's reads have no page counts.
pub fn shorter_page_limit(user_median_pages: Option<f64>) -> f64 {
    user_median_pages.map_or(SHORTER_FLOOR, |m| (SHORTER_SHARE * m).max(SHORTER_FLOOR))
}

fn mean(values: impl Iterator<Item = f64>) -> f64 {
    let (sum, n) = values.fold((0.0, 0usize), |(s, n), v| (s + v, n + 1));
    if n == 0 { 0.0 } else { sum / n as f64 }
}

/// The picks' seed (§8): a hash of the user, the world version, the user's change counter and
/// the "Show others" count. Reloads show the same picks; any real event or "Show others"
/// changes them. FNV-1a over a fixed little-endian encoding, then the SplitMix64 finaliser:
/// stable across Rust versions and platforms, unlike `std`'s hasher, so a seed means the same
/// thing in the benchmark and in every service release.
pub fn seed(user: u64, world_version: &str, change_counter: u64, show_others: u32) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = OFFSET;
    let mut feed = |bytes: &[u8]| {
        for &b in bytes {
            h ^= u64::from(b);
            h = h.wrapping_mul(PRIME);
        }
    };
    feed(&user.to_le_bytes());
    feed(&(world_version.len() as u64).to_le_bytes());
    feed(world_version.as_bytes());
    feed(&change_counter.to_le_bytes());
    feed(&show_others.to_le_bytes());
    let mut z = h.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Event, Prior, fit};

    #[test]
    fn seed_is_stable() {
        // Reference values computed independently of this code (a Python transcription of the
        // same FNV-1a + SplitMix64). A change here changes every user's picks.
        assert_eq!(seed(0, "", 0, 0), 0x36f3_7059_8e03_0e07);
        assert_eq!(seed(42, "2026-11", 7, 1), 0x7c09_e637_cf61_8441);
        assert_ne!(seed(42, "2026-11", 7, 1), seed(42, "2026-11", 7, 2));
        assert_ne!(seed(42, "2026-11", 7, 1), seed(42, "2026-11", 8, 1));
        assert_ne!(seed(42, "2026-11", 7, 1), seed(43, "2026-11", 7, 1));
        assert_ne!(seed(42, "2026-11", 7, 1), seed(42, "2026-12", 7, 1));
    }

    #[test]
    fn quantiles() {
        assert_eq!(quantile(&[], 0.5), None);
        assert_eq!(quantile(&[3.0], 0.75), Some(3.0));
        assert_eq!(quantile(&[4.0, 1.0, 3.0, 2.0], 0.5), Some(2.5));
        assert_eq!(quantile(&[1.0, 2.0, 3.0, 4.0, 5.0], 0.75), Some(4.0));
    }

    #[test]
    fn novelty_and_cosine() {
        let a = [1.0f32, 0.0];
        let b = [0.0f32, 1.0];
        let c = [1.0f32, 1.0];
        assert_eq!(novelty(&a, &[]), 1.0);
        assert!((novelty(&a, &[&b]) - 1.0).abs() < 1e-12);
        assert!(novelty(&a, &[&b, &a]).abs() < 1e-12);
        assert!((cosine(&a, &c) - 0.5f64.sqrt()).abs() < 1e-7);
        assert_eq!(cosine(&a, &[0.0, 0.0]), 0.0);
    }

    #[test]
    fn steers() {
        assert_eq!(shorter_page_limit(None), 250.0);
        assert_eq!(shorter_page_limit(Some(300.0)), 250.0);
        assert!((shorter_page_limit(Some(500.0)) - 350.0).abs() < 1e-9);
        let layout = Layout { k: 2, r: 0, a: 3 };
        // bias, c = (1, 0), a = (3, -1, 0.5), meta
        let phi = [1.0f32, 1.0, 0.0, 3.0, -1.0, 0.5, 0.0, 0.0, 0.0, 0.0, 0.0];
        assert!((lighter(layout, &phi, &[0], &[1, 2], 0.5) - 0.5 * (3.0 - (-0.25))).abs() < 1e-9);
        // positive aspect 0 capped at 2; negative aspects 1 (clamped to 0) and 2 (0.5).
        let last = [1.0f32, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let v = ctx(layout, &phi, &[0], &[1, 2], Some(&last), 1.0, 0.5);
        assert!((v - 0.5 * (2.0 - 0.25 + 0.0)).abs() < 1e-9);
        let same = ctx(layout, &phi, &[], &[], Some(&phi), 1.0, 0.5);
        assert!((same - 0.5).abs() < 1e-9);
        assert_eq!(ctx(layout, &phi, &[], &[], Some(&phi), 0.0, 0.5), 0.0);
    }

    struct Three;
    const THREE: [[f32; 11]; 3] = [
        [1.0, 1.0, 0.0, 3.0, 0.0, 1.5, 0.0, 0.0, 0.0, 0.0, 0.0],
        [1.0, 0.0, 1.0, 0.0, 2.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        [1.0, 0.6, 0.8, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0],
    ];
    impl Vectors for Three {
        fn layout(&self) -> Layout {
            Layout { k: 2, r: 0, a: 3 }
        }
        fn phi(&self, work: Row) -> Option<&[f32]> {
            THREE.get(work.0 as usize).map(|r| r.as_slice())
        }
        fn text_sourced(&self, _: Row) -> bool {
            false
        }
    }

    #[test]
    fn context_aspects_fall_back_to_the_last_work() {
        let events = [
            Event::Chip {
                aspect: 0,
                positive: true,
                age_years: 0.0,
            },
            Event::Chip {
                aspect: 2,
                positive: true,
                age_years: 0.0,
            },
            Event::Chip {
                aspect: 1,
                positive: false,
                age_years: 0.0,
            },
        ];
        let f = fit(&events, &Three, &Hyper::default(), &Prior::default());
        let last = &THREE[0];
        assert_eq!(
            context_aspects(&f, last, Verdict::Loved, &[], &[]),
            (vec![0, 2], vec![])
        );
        assert_eq!(
            context_aspects(&f, last, Verdict::Loved, &[1], &[]),
            (vec![1], vec![])
        );
        // Aspect 1 has a negative weight but the last work does not have it (a = 0).
        assert_eq!(
            context_aspects(&f, last, Verdict::Okay, &[], &[]),
            (vec![], vec![])
        );
        assert_eq!(
            context_aspects(&f, &THREE[1], Verdict::NotForMe, &[], &[]),
            (vec![], vec![1])
        );
    }

    #[test]
    fn comparison_question_prefers_uncertain_memorable_pairs() {
        let h = Hyper {
            ask_min: 0.0,
            ..Hyper::default()
        };
        let f = fit(&[], &Three, &h, &Prior::default());
        let rated = [
            Rated {
                work: Row(1),
                verdict: Verdict::Liked,
                recent: false,
            },
            Rated {
                work: Row(2),
                verdict: Verdict::Liked,
                recent: true,
            },
        ];
        let none = |_: Row, _: Row| false;
        // Work 1 is further from work 0 than work 2, but faded memory (0.6) may cost it.
        let x1: f64 = THREE[0]
            .iter()
            .zip(&THREE[1])
            .map(|(a, b)| f64::from(a - b))
            .map(|x| x * x)
            .sum();
        let x2: f64 = THREE[0]
            .iter()
            .zip(&THREE[2])
            .map(|(a, b)| f64::from(a - b))
            .map(|x| x * x)
            .sum();
        assert!(x1 > x2);
        let pick = next_comparison(&f, &Three, &h, Row(0), Verdict::Liked, &rated, &none);
        assert!(pick.is_some());
        // Excluding a pair removes it; `new` itself is never a partner.
        let only_2 = |_: Row, b: Row| b == Row(1);
        assert_eq!(
            next_comparison(&f, &Three, &h, Row(0), Verdict::Liked, &rated, &only_2),
            Some(Row(2))
        );
        let self_only = [Rated {
            work: Row(0),
            verdict: Verdict::Loved,
            recent: true,
        }];
        assert_eq!(
            next_comparison(&f, &Three, &h, Row(0), Verdict::Loved, &self_only, &none),
            None
        );
        // A partner with another verdict is never asked about.
        assert_eq!(
            next_comparison(&f, &Three, &h, Row(0), Verdict::Loved, &rated, &none),
            None
        );
        // Below ask_min: no question.
        let strict = Hyper {
            ask_min: 1e9,
            ..Hyper::default()
        };
        assert_eq!(
            next_comparison(&f, &Three, &strict, Row(0), Verdict::Liked, &rated, &none),
            None
        );
    }

    #[test]
    fn have_you_read_weighs_popularity() {
        let h = Hyper::default();
        let f = fit(&[], &Three, &h, &Prior::default());
        // Under the prior, work 0 is the more uncertain one; enough popularity outweighs that.
        let (v0, v1) = (
            f.variance(&THREE[0], false, &h),
            f.variance(&THREE[1], false, &h),
        );
        assert!(v0 > v1);
        assert_eq!(
            have_you_read(&f, &Three, &[(Row(0), 0.0), (Row(1), 0.0)]),
            Some(Row(0))
        );
        assert_eq!(
            have_you_read(&f, &Three, &[(Row(0), -5.0), (Row(1), 5.0)]),
            Some(Row(1))
        );
        assert_eq!(have_you_read(&f, &Three, &[]), None);
        assert_eq!(have_you_read(&f, &Three, &[(Row(9), 1.0)]), None);
    }
}
