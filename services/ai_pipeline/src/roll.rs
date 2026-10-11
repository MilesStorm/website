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
//! `SETTLE_MS` (and seen still in `SETTLE_FRAMES` frames), and again only when the
//! settled dice change: a new
//! roll (dice moved, added or removed) gets a new `roll_id`, while a same-position
//! re-read with changed values (e.g. a die became readable) keeps the `roll_id`.
//! Known limit: a re-roll that lands every die within `SAME_PLACE` of its old spot
//! with identical values is indistinguishable from "nothing happened".
//!
//! Every rule here is in milliseconds, not frames: the camera's frame rate decides how
//! many looks the tracker gets, never how long dice must rest. (The rules were counted
//! in frames while the page sent ~12 a second; the times below are what those counts
//! came to.)

use serde::Serialize;

/// Minimum IoU to treat a detection as the same die as an existing track.
const MATCH_IOU: f32 = 0.3;
/// A die is still while its box centre stays within this (fraction of the frame) of where
/// it came to rest. Measured from that spot, not from the frame before: between two frames
/// a slowly rolling die moves less the faster the camera is.
const STILL_MOVE: f32 = 0.02;
/// How long a die must have been still before it counts as settled.
pub const SETTLE_MS: u64 = 400;
/// Still frames a settled die needs as well, so a slow camera can't settle on a glimpse.
pub const SETTLE_FRAMES: usize = 5;
/// Still observations older than this are left out of the vote.
const VOTE_WINDOW_MS: u64 = 1250;
/// How long a die may go undetected before its track is dropped.
const MAX_UNSEEN_MS: u64 = 300;
/// Dice closer than this (fraction of the frame) to their previous spot are "the same place".
const SAME_PLACE: f32 = 0.05;
/// With no dice at all for this long the tray counts as cleared: the next
/// settled dice are a new roll even if they land exactly like the last one.
const EMPTY_RESET_MS: u64 = 400;

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
    /// Where the die was when its current still period began.
    rest: [f32; 4],
    /// When that was (ms, as given to `update`).
    still_since: u64,
    /// Frames since then in which it was seen still.
    still_frames: usize,
    last_seen: u64,
    /// When each still observation was made and its probabilities, newest last.
    votes: Vec<(u64, Vec<f32>)>,
}

impl Track {
    fn new(o: &Observation, now_ms: u64) -> Self {
        Self {
            bbox: o.bbox,
            rest: o.bbox,
            still_since: now_ms,
            still_frames: 0,
            last_seen: now_ms,
            votes: vec![(now_ms, o.probs.clone())],
        }
    }

    fn settled(&self, now_ms: u64) -> bool {
        self.still_frames >= SETTLE_FRAMES && now_ms.saturating_sub(self.still_since) >= SETTLE_MS
    }
}

pub struct RollTracker {
    tracks: Vec<Track>,
    threshold: f32,
    value_of: fn(usize) -> &'static str,
    last: Option<RollEvent>,
    /// Since when no dice have been seen at all.
    empty_since: Option<u64>,
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
        Self { tracks: Vec::new(), threshold, value_of, last: None, empty_since: None }
    }

    /// Feed one frame's detections. `now_ms` is Unix time in milliseconds: it times the
    /// rules above and stamps the event. Returns a roll event when the settled dice changed.
    pub fn update(&mut self, obs: &[Observation], now_ms: u64) -> Option<RollEvent> {
        self.track(obs, now_ms);
        if obs.is_empty() {
            let since = *self.empty_since.get_or_insert(now_ms);
            if now_ms.saturating_sub(since) >= EMPTY_RESET_MS {
                self.last = None;
            }
        } else {
            self.empty_since = None;
        }

        let settled = !self.tracks.is_empty() && self.tracks.iter().all(|t| t.settled(now_ms));
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
    fn track(&mut self, obs: &[Observation], now_ms: u64) {
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
            if dist(&t.rest, &o.bbox) < STILL_MOVE {
                t.still_frames += 1;
            } else {
                // Still moving: earlier readings belong to a different resting face.
                t.rest = o.bbox;
                t.still_since = now_ms;
                t.still_frames = 0;
                t.votes.clear();
            }
            t.bbox = o.bbox;
            t.last_seen = now_ms;
            t.votes.push((now_ms, o.probs.clone()));
            t.votes.retain(|(at, _)| now_ms.saturating_sub(*at) <= VOTE_WINDOW_MS);
        }

        self.tracks.retain(|t| now_ms.saturating_sub(t.last_seen) <= MAX_UNSEEN_MS);

        for (o, used) in obs.iter().zip(obs_used) {
            if !used {
                self.tracks.push(Track::new(o, now_ms));
            }
        }
    }

    /// Average the still-period probabilities; report the argmax only if confident.
    fn decide(&self, t: &Track) -> RolledDie {
        let n = t.votes.len().max(1) as f32;
        let classes = t.votes.first().map_or(0, |(_, probs)| probs.len());
        let mean: Vec<f32> = (0..classes).map(|c| t.votes.iter().map(|(_, v)| v[c]).sum::<f32>() / n).collect();
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
    /// The page's old pace: a frame every 83 ms.
    const SLOW: u64 = 83;
    /// A frame every 33 ms: the camera's own 30 a second.
    const FAST: u64 = 33;

    /// Probabilities putting `p` on `class` and spreading the rest evenly.
    fn probs(class: usize, p: f32) -> Vec<f32> {
        let mut v = vec![(1.0 - p) / 20.0; 21];
        v[class] = p;
        v
    }

    fn die(x: f32, y: f32, class: usize, p: f32) -> Observation {
        Observation { bbox: [x, y, x + 0.1, y + 0.1], probs: probs(class, p) }
    }

    fn frame(dice: &[(f32, f32, usize, f32)]) -> Vec<Observation> {
        dice.iter().map(|&(x, y, c, p)| die(x, y, c, p)).collect()
    }

    /// A tracker fed by a camera that sends a frame every `every` ms.
    struct Camera {
        tracker: RollTracker,
        every: u64,
        now: u64,
    }

    impl Camera {
        fn new(every: u64) -> Self {
            Self { tracker: RollTracker::new(T, class_value), every, now: 1_000_000 }
        }

        /// One frame; the clock then moves on to the next.
        fn see(&mut self, frame: &[Observation]) -> Option<RollEvent> {
            let event = self.tracker.update(frame, self.now);
            self.now += self.every;
            event
        }

        /// The same frame `n` times, collecting emitted events.
        fn feed(&mut self, frame: &[Observation], n: usize) -> Vec<RollEvent> {
            (0..n).filter_map(|_| self.see(frame)).collect()
        }

        /// The same frame for `ms`, collecting emitted events.
        fn watch(&mut self, frame: &[Observation], ms: u64) -> Vec<RollEvent> {
            self.feed(frame, (ms / self.every) as usize)
        }
    }

    #[test]
    fn steady_die_emits_exactly_once() {
        let mut cam = Camera::new(SLOW);
        let events = cam.feed(&frame(&[(0.4, 0.4, 16, 0.9)]), 30); // class 16 = "16"
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].dice[0].value, Some("16"));
        assert_eq!(events[0].total, Some(16));
        assert!(events[0].complete);
    }

    #[test]
    fn needs_settle_frames_before_emitting() {
        let mut cam = Camera::new(SLOW);
        let f = frame(&[(0.4, 0.4, 3, 0.9)]);
        assert!(cam.feed(&f, SETTLE_FRAMES).is_empty());
        assert_eq!(cam.feed(&f, 1).len(), 1);
    }

    /// The rule is time: a camera sending 30 frames a second waits as long as a slow one,
    /// and only gets more looks at the die meanwhile.
    #[test]
    fn a_fast_camera_waits_just_as_long() {
        let mut cam = Camera::new(FAST);
        let f = frame(&[(0.4, 0.4, 3, 0.9)]);
        let started = cam.now;
        // Far more than SETTLE_FRAMES frames, but not yet SETTLE_MS.
        assert!(cam.watch(&f, SETTLE_MS).is_empty());
        let events = cam.feed(&f, 2);
        assert_eq!(events.len(), 1);
        assert!(events[0].ts - started >= SETTLE_MS);
        assert!(events[0].ts - started < SETTLE_MS + 2 * FAST);
    }

    /// A slow camera has waited long enough before it has looked often enough.
    #[test]
    fn a_slow_camera_still_needs_its_frames() {
        let mut cam = Camera::new(250);
        let f = frame(&[(0.4, 0.4, 3, 0.9)]);
        assert!(cam.feed(&f, SETTLE_FRAMES).is_empty());
        assert_eq!(cam.feed(&f, 1).len(), 1);
    }

    #[test]
    fn two_dice_left_to_right_with_total() {
        let mut cam = Camera::new(SLOW);
        // "5" on the right, "0" (d10 zero = 10) on the left.
        let events = cam.feed(&frame(&[(0.7, 0.4, 4, 0.9), (0.1, 0.4, 9, 0.9)]), 10);
        assert_eq!(events.len(), 1);
        let values: Vec<_> = events[0].dice.iter().map(|d| d.value).collect();
        assert_eq!(values, vec![Some("0"), Some("5")]);
        assert_eq!(events[0].total, Some(15));
    }

    #[test]
    fn unsure_die_is_null_not_guessed() {
        let mut cam = Camera::new(SLOW);
        let events = cam.feed(&frame(&[(0.1, 0.4, 4, 0.9), (0.6, 0.4, 7, 0.5)]), 10);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].dice[1].value, None);
        assert!(!events[0].complete);
        assert_eq!(events[0].total, None);
    }

    #[test]
    fn moving_die_does_not_emit_until_it_rests() {
        let mut cam = Camera::new(SLOW);
        let rolling: Vec<RollEvent> =
            (0..20).filter_map(|i| cam.see(&frame(&[(0.05 + 0.03 * i as f32, 0.4, 2, 0.9)]))).collect();
        assert!(rolling.is_empty());
    }

    /// The same die at the same speed, seen by a fast camera: between two frames it moves
    /// less than `STILL_MOVE`, but it is not resting.
    #[test]
    fn a_slowly_rolling_die_is_not_still_however_fast_the_camera() {
        let mut cam = Camera::new(FAST);
        let speed = 0.03 / SLOW as f32; // the frame-widths per ms of the test above
        let rolling: Vec<RollEvent> = (0..50)
            .filter_map(|i| cam.see(&frame(&[(0.05 + speed * (i * FAST) as f32, 0.4, 2, 0.9)])))
            .collect();
        assert!(rolling.is_empty());
    }

    #[test]
    fn reroll_to_new_place_is_a_new_roll() {
        let mut cam = Camera::new(SLOW);
        let first = cam.feed(&frame(&[(0.1, 0.4, 2, 0.9)]), 10);
        // Die tumbles across the tray, then rests elsewhere showing another value.
        for i in 0..6 {
            cam.see(&frame(&[(0.2 + 0.08 * i as f32, 0.4, 5, 0.9)]));
        }
        let second = cam.feed(&frame(&[(0.7, 0.4, 5, 0.9)]), 10);
        assert_eq!(first.len(), 1);
        assert_eq!(second.len(), 1);
        assert_ne!(first[0].roll_id, second[0].roll_id);
        assert_eq!(second[0].dice[0].value, Some("6"));
    }

    /// Readings from before the die came to rest are not part of the vote.
    #[test]
    fn the_vote_counts_only_the_resting_face() {
        let mut cam = Camera::new(FAST);
        // Tumbling, confidently showing "3" on the way.
        for i in 0..10 {
            cam.see(&frame(&[(0.1 + 0.05 * i as f32, 0.4, 2, 0.99)]));
        }
        let events = cam.feed(&frame(&[(0.7, 0.4, 5, 0.9)]), 30);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].dice[0].value, Some("6"));
        assert!((events[0].dice[0].conf - 0.9).abs() < 1e-4);
    }

    #[test]
    fn brief_dropout_does_not_re_emit() {
        for every in [SLOW, FAST] {
            let mut cam = Camera::new(every);
            let f = frame(&[(0.4, 0.4, 11, 0.9)]);
            assert_eq!(cam.feed(&f, 20).len(), 1);
            // YOLO misses the die for a moment.
            assert!(cam.watch(&[], MAX_UNSEEN_MS - every).is_empty());
            assert!(cam.feed(&f, 20).is_empty());
        }
    }

    #[test]
    fn same_place_better_reading_keeps_roll_id() {
        let mut cam = Camera::new(SLOW);
        let unsure = cam.feed(&frame(&[(0.4, 0.4, 7, 0.5)]), 6);
        assert_eq!(unsure.len(), 1);
        assert_eq!(unsure[0].dice[0].value, None);
        // Lighting improves: the averaged vote crosses the threshold.
        let sure = cam.feed(&frame(&[(0.4, 0.4, 7, 0.99)]), 20);
        assert_eq!(sure.len(), 1);
        assert_eq!(sure[0].dice[0].value, Some("8"));
        assert_eq!(sure[0].roll_id, unsure[0].roll_id);
    }

    #[test]
    fn cleared_tray_then_identical_roll_is_new() {
        for every in [SLOW, FAST] {
            let mut cam = Camera::new(every);
            let f = frame(&[(0.4, 0.4, 5, 0.9)]);
            let first = cam.feed(&f, 20);
            assert!(cam.watch(&[], EMPTY_RESET_MS + 2 * every).is_empty());
            let second = cam.feed(&f, 20);
            assert_eq!((first.len(), second.len()), (1, 1));
            assert_ne!(first[0].roll_id, second[0].roll_id);
        }
    }

    #[test]
    fn flickering_value_does_not_spam_events() {
        let mut cam = Camera::new(SLOW);
        assert_eq!(cam.feed(&frame(&[(0.4, 0.4, 5, 0.95)]), 10).len(), 1);
        // The vote drifts to another confident value (e.g. glare): no new event.
        assert!(cam.feed(&frame(&[(0.4, 0.4, 8, 0.99)]), 40).is_empty());
    }

    #[test]
    fn spurious_blip_does_not_block_or_emit() {
        let mut cam = Camera::new(SLOW);
        let f = frame(&[(0.4, 0.4, 2, 0.9)]);
        assert_eq!(cam.feed(&f, 10).len(), 1);
        // A one-frame false detection elsewhere, then back to normal: no new roll.
        cam.see(&frame(&[(0.4, 0.4, 2, 0.9), (0.8, 0.8, 6, 0.4)]));
        assert!(cam.feed(&f, 10).is_empty());
    }

    #[test]
    fn empty_tray_emits_nothing() {
        let mut cam = Camera::new(SLOW);
        assert!(cam.feed(&[], 20).is_empty());
    }
}
