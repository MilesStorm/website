//! A user's rows as the model sees them (§4): each event becomes zero or more observations, each
//! a statement that a linear function of `w` lies above or below a threshold, and "Don't care"
//! answers become changes to the prior.

use crate::{Hyper, Layout, Prior, Row, Vectors};

/// The one-tap rating of a finished read, worst first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    NotForMe,
    Okay,
    Liked,
    Loved,
}

impl Verdict {
    /// How many of the thresholds `b1 < b2 < b3` the verdict lies above.
    pub fn level(self) -> usize {
        match self {
            Verdict::NotForMe => 0,
            Verdict::Okay => 1,
            Verdict::Liked => 2,
            Verdict::Loved => 3,
        }
    }

    /// Liked or loved: the verdicts that count as "this worked".
    pub fn is_positive(self) -> bool {
        matches!(self, Verdict::Liked | Verdict::Loved)
    }
}

/// Why the user stopped reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// Counts as a `not_for_me` verdict at 0.7 of its weight.
    NotForMe,
    /// Counts as nothing.
    NotNow,
}

/// The user's answer to a pick.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Feedback {
    Save,
    StartReading,
    NotForMe,
}

/// The three-way control of the taste profile (§9).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileChoice {
    /// "Yes": a strong positive statement about the aspect.
    Yes,
    /// "The opposite": a strong negative statement about the aspect.
    Opposite,
    /// "Don't care": pins the aspect's weight at zero through the prior.
    DontCare,
}

/// One row of a user's history, exactly as §4 lists them. `age_years` (≥ 0, supplied by the
/// caller, who owns the clock) is how long ago it happened; older rows count less.
///
/// Generic chips (story, characters, …) have no aspect and so no event: the caller stores them
/// and does not pass them here.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    /// One read of a work. `verdict` is what the row says now. `stopped` is set when the user
    /// stopped reading, and then decides the terms. `nth_read` counts reads of this work from 1;
    /// `latest` is true for the most recent read of the work, whose verdict counts in full while
    /// earlier ones count half.
    Read {
        work: Row,
        verdict: Option<Verdict>,
        stopped: Option<StopReason>,
        nth_read: u32,
        latest: bool,
        age_years: f64,
    },
    /// The user liked `better` more than `worse`.
    Comparison {
        better: Row,
        worse: Row,
        age_years: f64,
    },
    /// A chip on an aspect (index in `0..layout.a`) after logging a read.
    Chip {
        aspect: usize,
        positive: bool,
        age_years: f64,
    },
    /// The user's answer to a pick.
    Pick {
        work: Row,
        feedback: Feedback,
        age_years: f64,
    },
    /// The taste profile's control on an aspect. It does not age: it states the present.
    Profile {
        aspect: usize,
        choice: ProfileChoice,
    },
}

/// Weight of a second or later read, of a pick and of a profile answer (§4).
const OMEGA_REREAD: f64 = 0.3;
const OMEGA_SAVE: f64 = 0.2;
const OMEGA_START: f64 = 0.3;
const OMEGA_PICK_NOT_FOR_ME: f64 = 0.3;
const OMEGA_PROFILE: f64 = 5.0;
/// A stopped read with reason `not_for_me` counts as that verdict at this share.
const STOPPED_SHARE: f64 = 0.7;
/// An earlier read's verdict counts at this share of the latest one's.
const EARLIER_SHARE: f64 = 0.5;
/// "Don't care" divides the aspect weight's prior variance by this.
const DONT_CARE_SHRINK: f64 = 100.0;

/// `max(0.5, 0.5^(age / 10))`: tastes drift, but old favourites still count.
pub(crate) fn age_factor(age_years: f64) -> f64 {
    0.5f64.powf(age_years.max(0.0) / 10.0).max(0.5)
}

/// Which of the user's rows an observation belongs to, for the score breakdown (§8).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Source {
    Work(Row),
    /// `phi_better` is kept so the comparison's contribution can be split between its two works.
    Pair {
        better: Row,
        worse: Row,
        phi_better: Vec<f64>,
    },
    Aspect(usize),
}

/// One term `omega * ln sigma( y * (x · w - t) / tau )`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Observation {
    pub x: Vec<f64>,
    pub t: f64,
    pub y: f64,
    pub tau: f64,
    pub omega: f64,
    pub source: Source,
}

/// This user's prior: mean `m0` and the diagonal of `S0`, after "Don't care" overrides.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct UserPrior {
    pub m0: Vec<f64>,
    pub s0: Vec<f64>,
}

pub(crate) fn user_prior(
    layout: Layout,
    hyper: &Hyper,
    prior: &Prior,
    events: &[Event],
) -> UserPrior {
    let d = layout.d();
    let mut m0 = vec![0.0; d];
    let mut s0 = vec![0.0; d];
    s0[layout.bias()] = hyper.var_bias;
    s0[layout.c()].fill(hyper.var_c);
    s0[layout.r()].fill(hyper.var_r);
    s0[layout.a()].fill(hyper.var_a);
    s0[layout.m()].fill(hyper.var_m);
    m0[layout.m()].copy_from_slice(&prior.m0_meta);
    for event in events {
        if let Event::Profile {
            aspect,
            choice: ProfileChoice::DontCare,
        } = *event
            && aspect < layout.a
        {
            let i = layout.aspect(aspect);
            s0[i] = hyper.var_a / DONT_CARE_SHRINK;
            m0[i] = 0.0;
        }
    }
    UserPrior { m0, s0 }
}

/// The work's vector in f64, or `None` when it has none or it is unusable (wrong length or a
/// non-finite number). Why the check: one bad number in the world must cost that work its terms,
/// not poison the whole fit with NaN.
pub(crate) fn vector(world: &impl Vectors, layout: Layout, work: Row) -> Option<Vec<f64>> {
    let phi = world.phi(work)?;
    (phi.len() == layout.d() && phi.iter().all(|v| v.is_finite()))
        .then(|| phi.iter().map(|&v| f64::from(v)).collect())
}

/// Every observation in the user's events, in event order. Works without a vector and aspects
/// outside the layout produce none.
pub(crate) fn observations(
    events: &[Event],
    world: &impl Vectors,
    hyper: &Hyper,
) -> Vec<Observation> {
    let layout = world.layout();
    let mut out = Vec::new();
    for event in events {
        match *event {
            Event::Read {
                work,
                verdict,
                stopped,
                nth_read,
                latest,
                age_years,
            } => {
                let Some(phi) = vector(world, layout, work) else {
                    continue;
                };
                let age = age_factor(age_years);
                let counted = match (stopped, verdict) {
                    (Some(StopReason::NotNow), _) => None,
                    (Some(StopReason::NotForMe), _) => Some((Verdict::NotForMe, STOPPED_SHARE)),
                    (None, Some(v)) => Some((v, 1.0)),
                    (None, None) => None,
                };
                if let Some((v, share)) = counted {
                    let recency = if latest { 1.0 } else { EARLIER_SHARE };
                    let omega = hyper.omega_v * share * recency * age;
                    for (k, t) in [hyper.b1, hyper.b2, hyper.b3].into_iter().enumerate() {
                        out.push(Observation {
                            x: phi.clone(),
                            t,
                            y: if v.level() > k { 1.0 } else { -1.0 },
                            tau: hyper.tau_v,
                            omega,
                            source: Source::Work(work),
                        });
                    }
                }
                // Choosing to read a work again says it worked; a reread the user stopped says
                // nothing of the kind (its stop reason above already counts).
                if nth_read >= 2 && stopped.is_none() {
                    out.push(utility_term(phi, work, 1.0, OMEGA_REREAD * age, hyper));
                }
            }
            Event::Comparison {
                better,
                worse,
                age_years,
            } => {
                let (Some(p), Some(q)) =
                    (vector(world, layout, better), vector(world, layout, worse))
                else {
                    continue;
                };
                out.push(Observation {
                    x: p.iter().zip(&q).map(|(a, b)| a - b).collect(),
                    t: 0.0,
                    y: 1.0,
                    tau: hyper.tau_c,
                    omega: age_factor(age_years),
                    source: Source::Pair {
                        better,
                        worse,
                        phi_better: p,
                    },
                });
            }
            Event::Chip {
                aspect,
                positive,
                age_years,
            } => {
                if aspect < layout.a {
                    let omega = hyper.omega_h * age_factor(age_years);
                    out.push(aspect_term(layout, aspect, positive, omega));
                }
            }
            Event::Pick {
                work,
                feedback,
                age_years,
            } => {
                let Some(phi) = vector(world, layout, work) else {
                    continue;
                };
                let (y, omega) = match feedback {
                    Feedback::Save => (1.0, OMEGA_SAVE),
                    Feedback::StartReading => (1.0, OMEGA_START),
                    Feedback::NotForMe => (-1.0, OMEGA_PICK_NOT_FOR_ME),
                };
                out.push(utility_term(
                    phi,
                    work,
                    y,
                    omega * age_factor(age_years),
                    hyper,
                ));
            }
            Event::Profile { aspect, choice } => {
                if aspect < layout.a {
                    match choice {
                        ProfileChoice::Yes => {
                            out.push(aspect_term(layout, aspect, true, OMEGA_PROFILE))
                        }
                        ProfileChoice::Opposite => {
                            out.push(aspect_term(layout, aspect, false, OMEGA_PROFILE))
                        }
                        ProfileChoice::DontCare => {}
                    }
                }
            }
        }
    }
    out
}

/// "The utility of this work is above (or below) `b2`": picks and rereads. Same noise as a
/// verdict, because it is the same kind of statement.
fn utility_term(phi: Vec<f64>, work: Row, y: f64, omega: f64, hyper: &Hyper) -> Observation {
    Observation {
        x: phi,
        t: hyper.b2,
        y,
        tau: hyper.tau_v,
        omega,
        source: Source::Work(work),
    }
}

/// "The weight of aspect `k` is positive (or negative)": chips and profile answers.
fn aspect_term(layout: Layout, aspect: usize, positive: bool, omega: f64) -> Observation {
    let mut x = vec![0.0; layout.d()];
    x[layout.aspect(aspect)] = 1.0;
    Observation {
        x,
        t: 0.0,
        y: if positive { 1.0 } else { -1.0 },
        tau: 1.0,
        omega,
        source: Source::Aspect(aspect),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two works with simple vectors and one without.
    struct Tiny {
        rows: Vec<Option<Vec<f32>>>,
    }

    impl Vectors for Tiny {
        fn layout(&self) -> Layout {
            Layout { k: 2, r: 0, a: 3 }
        }
        fn phi(&self, work: Row) -> Option<&[f32]> {
            self.rows.get(work.0 as usize)?.as_deref()
        }
        fn text_sourced(&self, _: Row) -> bool {
            false
        }
    }

    fn tiny() -> Tiny {
        // d = 1 + 2 + 0 + 3 + 5 = 11
        let a: Vec<f32> = (0..11)
            .map(|i| if i == 0 { 1.0 } else { i as f32 / 10.0 })
            .collect();
        let b: Vec<f32> = (0..11)
            .map(|i| if i == 0 { 1.0 } else { -(i as f32) / 10.0 })
            .collect();
        Tiny {
            rows: vec![Some(a), Some(b), None, Some(vec![f32::NAN; 11])],
        }
    }

    fn read(work: u32, verdict: Option<Verdict>, stopped: Option<StopReason>) -> Event {
        Event::Read {
            work: Row(work),
            verdict,
            stopped,
            nth_read: 1,
            latest: true,
            age_years: 0.0,
        }
    }

    fn obs(events: &[Event]) -> Vec<Observation> {
        observations(events, &tiny(), &Hyper::default())
    }

    #[test]
    fn verdict_gives_three_threshold_terms() {
        let h = Hyper::default();
        for (v, ys) in [
            (Verdict::NotForMe, [-1.0, -1.0, -1.0]),
            (Verdict::Okay, [1.0, -1.0, -1.0]),
            (Verdict::Liked, [1.0, 1.0, -1.0]),
            (Verdict::Loved, [1.0, 1.0, 1.0]),
        ] {
            let o = obs(&[read(0, Some(v), None)]);
            assert_eq!(o.len(), 3, "{v:?}");
            for (n, (term, t)) in o.iter().zip([h.b1, h.b2, h.b3]).enumerate() {
                assert_eq!(term.t, t);
                assert_eq!(term.y, ys[n], "{v:?} threshold {n}");
                assert_eq!(term.tau, h.tau_v);
                assert_eq!(term.omega, h.omega_v);
                assert_eq!(term.source, Source::Work(Row(0)));
                assert_eq!(term.x[0], 1.0);
            }
        }
    }

    #[test]
    fn stopped_reads() {
        let h = Hyper::default();
        let o = obs(&[read(0, None, Some(StopReason::NotForMe))]);
        assert_eq!(o.len(), 3);
        assert!(
            o.iter()
                .all(|t| t.y == -1.0 && (t.omega - h.omega_v * 0.7).abs() < 1e-15)
        );
        assert!(obs(&[read(0, None, Some(StopReason::NotNow))]).is_empty());
        assert!(obs(&[read(0, Some(Verdict::Loved), Some(StopReason::NotNow))]).is_empty());
        // In progress, no verdict yet: nothing.
        assert!(obs(&[read(0, None, None)]).is_empty());
    }

    #[test]
    fn earlier_reads_count_half_and_rereads_add_a_term() {
        let h = Hyper::default();
        let earlier = Event::Read {
            work: Row(0),
            verdict: Some(Verdict::Liked),
            stopped: None,
            nth_read: 1,
            latest: false,
            age_years: 0.0,
        };
        let latest = Event::Read {
            work: Row(0),
            verdict: Some(Verdict::Loved),
            stopped: None,
            nth_read: 2,
            latest: true,
            age_years: 0.0,
        };
        let o = obs(&[earlier, latest]);
        assert_eq!(o.len(), 7);
        assert!(o[..3].iter().all(|t| t.omega == h.omega_v * 0.5));
        assert!(o[3..6].iter().all(|t| t.omega == h.omega_v));
        let reread = &o[6];
        assert_eq!((reread.t, reread.y, reread.omega), (h.b2, 1.0, 0.3));
    }

    #[test]
    fn comparison_term() {
        let h = Hyper::default();
        let o = obs(&[Event::Comparison {
            better: Row(0),
            worse: Row(1),
            age_years: 0.0,
        }]);
        assert_eq!(o.len(), 1);
        let c = &o[0];
        assert_eq!((c.t, c.y, c.tau, c.omega), (0.0, 1.0, h.tau_c, 1.0));
        assert_eq!(c.x[0], 0.0, "the bias cancels");
        assert!((c.x[3] - 0.6).abs() < 1e-6);
        assert!(matches!(
            c.source,
            Source::Pair {
                better: Row(0),
                worse: Row(1),
                ..
            }
        ));
    }

    #[test]
    fn chip_terms() {
        let h = Hyper::default();
        let l = tiny().layout();
        for positive in [true, false] {
            let o = obs(&[Event::Chip {
                aspect: 1,
                positive,
                age_years: 0.0,
            }]);
            assert_eq!(o.len(), 1);
            let c = &o[0];
            let mut x = vec![0.0; l.d()];
            x[l.aspect(1)] = 1.0;
            assert_eq!(c.x, x);
            assert_eq!((c.t, c.tau, c.omega), (0.0, 1.0, h.omega_h));
            assert_eq!(c.y, if positive { 1.0 } else { -1.0 });
            assert_eq!(c.source, Source::Aspect(1));
        }
        // Out of range: nothing.
        assert!(
            obs(&[Event::Chip {
                aspect: 3,
                positive: true,
                age_years: 0.0
            }])
            .is_empty()
        );
    }

    #[test]
    fn pick_terms() {
        let h = Hyper::default();
        for (feedback, y, omega) in [
            (Feedback::Save, 1.0, 0.2),
            (Feedback::StartReading, 1.0, 0.3),
            (Feedback::NotForMe, -1.0, 0.3),
        ] {
            let o = obs(&[Event::Pick {
                work: Row(1),
                feedback,
                age_years: 0.0,
            }]);
            assert_eq!(o.len(), 1);
            let p = &o[0];
            assert_eq!(
                (p.t, p.y, p.tau, p.omega),
                (h.b2, y, h.tau_v, omega),
                "{feedback:?}"
            );
            assert_eq!(p.source, Source::Work(Row(1)));
        }
    }

    #[test]
    fn profile_answers() {
        let l = tiny().layout();
        let yes = obs(&[Event::Profile {
            aspect: 2,
            choice: ProfileChoice::Yes,
        }]);
        assert_eq!((yes[0].y, yes[0].omega), (1.0, 5.0));
        let opposite = obs(&[Event::Profile {
            aspect: 2,
            choice: ProfileChoice::Opposite,
        }]);
        assert_eq!((opposite[0].y, opposite[0].omega), (-1.0, 5.0));
        let dont = [Event::Profile {
            aspect: 2,
            choice: ProfileChoice::DontCare,
        }];
        assert!(obs(&dont).is_empty());
        let h = Hyper::default();
        let prior = Prior {
            m0_meta: [0.3, 0.2, 0.0, -0.1, 0.4],
        };
        let p = user_prior(l, &h, &prior, &dont);
        assert_eq!(p.s0[l.aspect(2)], h.var_a / 100.0);
        assert_eq!(p.s0[l.aspect(1)], h.var_a);
        assert_eq!(p.m0[l.aspect(2)], 0.0);
        assert_eq!(&p.m0[l.m()], &prior.m0_meta);
        assert_eq!(p.s0[0], h.var_bias);
        assert!(p.s0[l.c()].iter().all(|&v| v == h.var_c));
        assert!(p.s0[l.r()].iter().all(|&v| v == h.var_r));
        assert!(p.s0[l.m()].iter().all(|&v| v == h.var_m));
    }

    #[test]
    fn age_rule() {
        assert_eq!(age_factor(0.0), 1.0);
        assert!((age_factor(5.0) - 0.5f64.sqrt()).abs() < 1e-15);
        assert_eq!(age_factor(10.0), 0.5);
        assert_eq!(age_factor(40.0), 0.5);
        assert_eq!(age_factor(-3.0), 1.0);
        let o = obs(&[Event::Pick {
            work: Row(0),
            feedback: Feedback::Save,
            age_years: 5.0,
        }]);
        assert!((o[0].omega - 0.2 * 0.5f64.sqrt()).abs() < 1e-15);
    }

    #[test]
    fn works_without_a_usable_vector_give_nothing() {
        for work in [2, 3, 9] {
            assert!(obs(&[read(work, Some(Verdict::Loved), None)]).is_empty());
            assert!(
                obs(&[Event::Comparison {
                    better: Row(0),
                    worse: Row(work),
                    age_years: 0.0
                }])
                .is_empty()
            );
        }
    }
}
