//! Turns per-frame detections into one event per settled roll.
//!
//! Single frames are noisy (glare, motion blur, a die still tumbling), so a die's
//! value is decided by averaging the head's class probabilities over the frames in
//! which it sat still, and is reported only when that average clears the
//! confidence threshold. Otherwise the die is reported as unreadable (`value:
//! null`): the service never guesses.
//!
//! One tracker per camera connection. Dice are followed across frames by box
//! overlap (IoU). A roll event is emitted once every visible die has been still for
//! `SETTLE_FRAMES` frames, and again only when the settled dice change: a new
//! roll (dice moved, added or removed) gets a new `roll_id`, while a same-position
//! re-read with changed values (e.g. a die became readable) keeps the `roll_id`.
//! Known limit: a re-roll that lands every die within `SAME_PLACE` of its old spot
//! with identical values is indistinguishable from "nothing happened".

use serde::Serialize;

/// Minimum IoU to treat a detection as the same die as an existing track.
const MATCH_IOU: f32 = 0.3;
/// A die is still when its box centre moves less than this (fraction of the frame) between frames.
const STILL_MOVE: f32 = 0.02;
/// Consecutive still frames before a die counts as settled.
pub const SETTLE_FRAMES: usize = 5;
/// Most recent still observations kept for the vote.
const VOTE_WINDOW: usize = 15;
/// Frames a die may go undetected before its track is dropped.
const MAX_MISSED: u32 = 3;
/// Dice closer than this (fraction of the frame) to their previous spot are "the same place".
const SAME_PLACE: f32 = 0.05;
/// Frames with no dice at all after which the tray counts as cleared: the next
/// settled dice are a new roll even if they land exactly like the last one.
const EMPTY_RESET_FRAMES: u32 = 5;

/// One detection as the tracker needs it.
pub struct Observation {
    /// Normalised [x1, y1, x2, y2].
    pub bbox: [f32; 4],
    /// Softmax over the head's classes.
    pub probs: Vec<f32>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RolledDie {
    /// Printed value ("1".."20", "0" = d10 zero), or null if it couldn't be read confidently.
    pub value: Option<&'static str>,
    /// Mean probability of the chosen value over the voting frames.
    pub conf: f32,
    #[serde(rename = "box")]
    pub bbox: [f32; 4],
}

#[derive(Debug, Clone, Serialize)]
pub struct RollEvent {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub roll_id: String,
    /// Left to right.
    pub dice: Vec<RolledDie>,
    /// Sum of all values (d10 "0" counts as 10); null unless every die was read.
    pub total: Option<u32>,
    pub complete: bool,
    /// Unix time in milliseconds.
    pub ts: u64,
}

struct Track {
    bbox: [f32; 4],
    still_frames: usize,
    missed: u32,
    /// Probabilities from the current still period, newest last.
    votes: Vec<Vec<f32>>,
}

pub struct RollTracker {
    tracks: Vec<Track>,
    threshold: f32,
    value_of: fn(usize) -> &'static str,
    last: Option<RollEvent>,
    empty_frames: u32,
}

fn centre(b: &[f32; 4]) -> (f32, f32) {
    ((b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0)
}

fn dist(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    let ((ax, ay), (bx, by)) = (centre(a), centre(b));
    ((ax - bx).powi(2) + (ay - by).powi(2)).sqrt()
}

fn iou(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    let iw = (a[2].min(b[2]) - a[0].max(b[0])).max(0.0);
    let ih = (a[3].min(b[3]) - a[1].max(b[1])).max(0.0);
    let inter = iw * ih;
    let area = |r: &[f32; 4]| (r[2] - r[0]).max(0.0) * (r[3] - r[1]).max(0.0);
    let union = area(a) + area(b) - inter;
    if union > 0.0 { inter / union } else { 0.0 }
}

fn numeric(value: &str) -> Option<u32> {
    match value {
        "0" => Some(10), // d10 zero face
        v => v.parse().ok(),
    }
}

impl RollTracker {
    /// `value_of` maps a class index to its printed value (see `inferance::class_value`).
    pub fn new(threshold: f32, value_of: fn(usize) -> &'static str) -> Self {
        Self { tracks: Vec::new(), threshold, value_of, last: None, empty_frames: 0 }
    }

    /// Feed one frame's detections. Returns a roll event when the settled dice changed.
    pub fn update(&mut self, obs: &[Observation], now_ms: u64) -> Option<RollEvent> {
        self.track(obs);
        if obs.is_empty() {
            self.empty_frames += 1;
            if self.empty_frames >= EMPTY_RESET_FRAMES {
                self.last = None;
            }
        } else {
            self.empty_frames = 0;
        }

        let settled = !self.tracks.is_empty()
            && self.tracks.iter().all(|t| t.still_frames >= SETTLE_FRAMES);
        if !settled {
            return None;
        }

        let mut dice: Vec<RolledDie> = self.tracks.iter().map(|t| self.decide(t)).collect();
        dice.sort_by(|a, b| centre(&a.bbox).0.total_cmp(&centre(&b.bbox).0));

        let same_place = self.last.as_ref().is_some_and(|last| {
            last.dice.len() == dice.len()
                && dice.iter().all(|d| last.dice.iter().any(|l| dist(&l.bbox, &d.bbox) < SAME_PLACE))
        });
        // Same dice in the same place: only re-send if more of them became readable.
        // A value flickering between two readings must not produce a stream of events.
        if same_place {
            let readable = |ds: &[RolledDie]| ds.iter().filter(|d| d.value.is_some()).count();
            let last = self.last.as_ref().unwrap();
            if readable(&dice) <= readable(&last.dice) {
                return None;
            }
        }

        let roll_id = match (&self.last, same_place) {
            (Some(last), true) => last.roll_id.clone(), // same dice, better reading
            _ => format!("{now_ms:x}-{:08x}", rand::random::<u32>()),
        };
        let complete = dice.iter().all(|d| d.value.is_some());
        let total = if complete {
            dice.iter().map(|d| d.value.and_then(numeric)).sum::<Option<u32>>()
        } else {
            None
        };
        let event = RollEvent { kind: "roll", roll_id, dice, total, complete, ts: now_ms };
        self.last = Some(event.clone());
        Some(event)
    }

    /// Greedy IoU matching of this frame's detections to existing tracks.
    fn track(&mut self, obs: &[Observation]) {
        let mut pairs: Vec<(f32, usize, usize)> = Vec::new();
        for (ti, t) in self.tracks.iter().enumerate() {
            for (oi, o) in obs.iter().enumerate() {
                let v = iou(&t.bbox, &o.bbox);
                if v >= MATCH_IOU {
                    pairs.push((v, ti, oi));
                }
            }
        }
        pairs.sort_by(|a, b| b.0.total_cmp(&a.0));

        let mut track_used = vec![false; self.tracks.len()];
        let mut obs_used = vec![false; obs.len()];
        for (_, ti, oi) in pairs {
            if track_used[ti] || obs_used[oi] {
                continue;
            }
            track_used[ti] = true;
            obs_used[oi] = true;
            let t = &mut self.tracks[ti];
            let o = &obs[oi];
            if dist(&t.bbox, &o.bbox) < STILL_MOVE {
                t.still_frames += 1;
            } else {
                // Still moving: earlier readings belong to a different resting face.
                t.still_frames = 0;
                t.votes.clear();
            }
            t.bbox = o.bbox;
            t.missed = 0;
            t.votes.push(o.probs.clone());
            if t.votes.len() > VOTE_WINDOW {
                t.votes.remove(0);
            }
        }

        for (t, used) in self.tracks.iter_mut().zip(&track_used) {
            if !used {
                t.missed += 1;
            }
        }
        self.tracks.retain(|t| t.missed <= MAX_MISSED);

        for (o, used) in obs.iter().zip(obs_used) {
            if !used {
                self.tracks.push(Track { bbox: o.bbox, still_frames: 0, missed: 0, votes: vec![o.probs.clone()] });
            }
        }
    }

    /// Average the still-period probabilities; report the argmax only if confident.
    fn decide(&self, t: &Track) -> RolledDie {
        let n = t.votes.len().max(1) as f32;
        let classes = t.votes.first().map_or(0, Vec::len);
        let mean: Vec<f32> = (0..classes).map(|c| t.votes.iter().map(|v| v[c]).sum::<f32>() / n).collect();
        let (class, conf) = mean
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map_or((0, 0.0), |(i, p)| (i, *p));
        RolledDie {
            value: (conf >= self.threshold).then(|| (self.value_of)(class)),
            conf,
            bbox: t.bbox,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::inferance::class_value;

    const T: f32 = 0.7;

    /// Probabilities putting `p` on `class` and spreading the rest evenly.
    fn probs(class: usize, p: f32) -> Vec<f32> {
        let mut v = vec![(1.0 - p) / 20.0; 21];
        v[class] = p;
        v
    }

    fn die(x: f32, y: f32, class: usize, p: f32) -> Observation {
        Observation { bbox: [x, y, x + 0.1, y + 0.1], probs: probs(class, p) }
    }

    /// Feed the same frame `n` times, collecting emitted events.
    fn feed(t: &mut RollTracker, frame: &[Observation], n: usize) -> Vec<RollEvent> {
        (0..n).filter_map(|i| t.update(frame, i as u64)).collect()
    }

    fn frame(dice: &[(f32, f32, usize, f32)]) -> Vec<Observation> {
        dice.iter().map(|&(x, y, c, p)| die(x, y, c, p)).collect()
    }

    #[test]
    fn steady_die_emits_exactly_once() {
        let mut t = RollTracker::new(T, class_value);
        let events = feed(&mut t, &frame(&[(0.4, 0.4, 16, 0.9)]), 30); // class 16 = "16"
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].dice[0].value, Some("16"));
        assert_eq!(events[0].total, Some(16));
        assert!(events[0].complete);
    }

    #[test]
    fn needs_settle_frames_before_emitting() {
        let mut t = RollTracker::new(T, class_value);
        let f = frame(&[(0.4, 0.4, 3, 0.9)]);
        assert!(feed(&mut t, &f, SETTLE_FRAMES).is_empty());
        assert_eq!(feed(&mut t, &f, 1).len(), 1);
    }

    #[test]
    fn two_dice_left_to_right_with_total() {
        let mut t = RollTracker::new(T, class_value);
        // "5" on the right, "0" (d10 zero = 10) on the left.
        let events = feed(&mut t, &frame(&[(0.7, 0.4, 4, 0.9), (0.1, 0.4, 9, 0.9)]), 10);
        assert_eq!(events.len(), 1);
        let values: Vec<_> = events[0].dice.iter().map(|d| d.value).collect();
        assert_eq!(values, vec![Some("0"), Some("5")]);
        assert_eq!(events[0].total, Some(15));
    }

    #[test]
    fn unsure_die_is_null_not_guessed() {
        let mut t = RollTracker::new(T, class_value);
        let events = feed(&mut t, &frame(&[(0.1, 0.4, 4, 0.9), (0.6, 0.4, 7, 0.5)]), 10);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].dice[1].value, None);
        assert!(!events[0].complete);
        assert_eq!(events[0].total, None);
    }

    #[test]
    fn moving_die_does_not_emit_until_it_rests() {
        let mut t = RollTracker::new(T, class_value);
        let rolling: Vec<RollEvent> = (0..20)
            .filter_map(|i| t.update(&frame(&[(0.05 + 0.03 * i as f32, 0.4, 2, 0.9)]), i))
            .collect();
        assert!(rolling.is_empty());
    }

    #[test]
    fn reroll_to_new_place_is_a_new_roll() {
        let mut t = RollTracker::new(T, class_value);
        let first = feed(&mut t, &frame(&[(0.1, 0.4, 2, 0.9)]), 10);
        // Die tumbles across the tray, then rests elsewhere showing another value.
        for i in 0..6 {
            t.update(&frame(&[(0.2 + 0.08 * i as f32, 0.4, 5, 0.9)]), 100 + i);
        }
        let second = feed(&mut t, &frame(&[(0.7, 0.4, 5, 0.9)]), 10);
        assert_eq!(first.len(), 1);
        assert_eq!(second.len(), 1);
        assert_ne!(first[0].roll_id, second[0].roll_id);
        assert_eq!(second[0].dice[0].value, Some("6"));
    }

    #[test]
    fn brief_dropout_does_not_re_emit() {
        let mut t = RollTracker::new(T, class_value);
        let f = frame(&[(0.4, 0.4, 11, 0.9)]);
        assert_eq!(feed(&mut t, &f, 10).len(), 1);
        assert!(t.update(&[], 50).is_none()); // YOLO misses the die for one frame
        assert!(feed(&mut t, &f, 10).is_empty());
    }

    #[test]
    fn same_place_better_reading_keeps_roll_id() {
        let mut t = RollTracker::new(T, class_value);
        let unsure = feed(&mut t, &frame(&[(0.4, 0.4, 7, 0.5)]), 6);
        assert_eq!(unsure.len(), 1);
        assert_eq!(unsure[0].dice[0].value, None);
        // Lighting improves: the averaged vote crosses the threshold.
        let sure = feed(&mut t, &frame(&[(0.4, 0.4, 7, 0.99)]), 20);
        assert_eq!(sure.len(), 1);
        assert_eq!(sure[0].dice[0].value, Some("8"));
        assert_eq!(sure[0].roll_id, unsure[0].roll_id);
    }

    #[test]
    fn cleared_tray_then_identical_roll_is_new() {
        let mut t = RollTracker::new(T, class_value);
        let f = frame(&[(0.4, 0.4, 5, 0.9)]);
        let first = feed(&mut t, &f, 10);
        assert!(feed(&mut t, &[], EMPTY_RESET_FRAMES as usize + 2).is_empty());
        let second = feed(&mut t, &f, 10);
        assert_eq!((first.len(), second.len()), (1, 1));
        assert_ne!(first[0].roll_id, second[0].roll_id);
    }

    #[test]
    fn flickering_value_does_not_spam_events() {
        let mut t = RollTracker::new(T, class_value);
        assert_eq!(feed(&mut t, &frame(&[(0.4, 0.4, 5, 0.95)]), 10).len(), 1);
        // The vote drifts to another confident value (e.g. glare): no new event.
        assert!(feed(&mut t, &frame(&[(0.4, 0.4, 8, 0.99)]), 40).is_empty());
    }

    #[test]
    fn spurious_blip_does_not_block_or_emit() {
        let mut t = RollTracker::new(T, class_value);
        let f = frame(&[(0.4, 0.4, 2, 0.9)]);
        assert_eq!(feed(&mut t, &f, 10).len(), 1);
        // A one-frame false detection elsewhere, then back to normal: no new roll.
        t.update(&frame(&[(0.4, 0.4, 2, 0.9), (0.8, 0.8, 6, 0.4)]), 99);
        assert!(feed(&mut t, &f, 10).is_empty());
    }

    #[test]
    fn empty_tray_emits_nothing() {
        let mut t = RollTracker::new(T, class_value);
        assert!(feed(&mut t, &[], 20).is_empty());
    }
}
