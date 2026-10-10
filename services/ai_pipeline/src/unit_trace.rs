//! Traces of a camera session (TRACING.md).
//!
//! A session lasts minutes, and a span is exported only when it ends, so the session is
//! not one span. It is a short `session open` span (the handshake, in the caller's
//! trace), one trace per unit of work while it runs, and a `session close` span with the
//! totals. Every unit and the close span link to `session open`, and all of them carry
//! the same `session.id`.
//!
//! A unit is one frame: `roll.settle` for every frame that settled a roll, `frame.infer`
//! for one in `FRAME_TRACE_EVERY` of the others. Whether a frame settles a roll is only
//! known after it was inferred, so its spans are made after the fact: the stages record
//! `Instant`s as they run (`FrameTimes`), and the spans are created with those times once
//! the frame's fate is known. A frame that is not traced costs a few clock reads.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use opentelemetry::global::BoxedTracer;
use opentelemetry::propagation::TextMapPropagator as _;
use opentelemetry::trace::{Link, Span as _, SpanContext, SpanKind, Status, TraceContextExt as _, Tracer as _};
use opentelemetry::{Context, KeyValue};
use opentelemetry_sdk::propagation::TraceContextPropagator;

use crate::model::inferance::StageEnds;
use crate::roll::RollEvent;

/// The session span these replace was named `arcane.ws_connection`.
const SESSION_KIND: &str = "arcane.ws_connection";
/// Default for `FRAME_TRACE_EVERY`: at ~140 ms per inference, a frame trace every ~1.4 s.
const DEFAULT_FRAME_EVERY: u64 = 10;
/// A failed frame is always traced when the session's last traced failure is this long
/// ago (or there is none), so an error is never invisible and a long run of failures has
/// a trace at least this often; the failures in between are sampled like other frames.
const FAILURE_QUIET: Duration = Duration::from_secs(10);

/// Converts a unit's `Instant`s to wall-clock time through one reading of both clocks, so
/// its spans keep their exact distances and nesting whatever the wall clock does meanwhile.
struct Anchor {
    at: Instant,
    wall: SystemTime,
}

impl Anchor {
    fn now() -> Self {
        Self { at: Instant::now(), wall: SystemTime::now() }
    }

    fn wall(&self, t: Instant) -> SystemTime {
        match self.at.checked_duration_since(t) {
            Some(ago) => self.wall - ago,
            None => self.wall + (t - self.at),
        }
    }
}

/// When a frame passed each point on its way through the service.
#[derive(Debug, Clone, Copy)]
pub struct FrameTimes {
    /// Read from the socket.
    pub received: Instant,
    /// Handed to the inference thread's queue.
    pub submitted: Instant,
    /// Taken from the queue by the inference thread.
    pub dequeued: Instant,
    /// Image decoding ended, whether it worked or not. None when the pipeline never
    /// loaded, so nothing ran.
    pub decoded: Option<Instant>,
    /// The model's stages. None when decoding failed.
    pub stages: Option<StageEnds>,
    /// The inference thread sent its reply.
    pub done: Instant,
}

impl FrameTimes {
    /// The stages that ran, in order, as (span name, start, end). They are back to back,
    /// so a frame's trace has no unexplained time before the reply.
    fn spans(&self) -> impl Iterator<Item = (&'static str, Instant, Instant)> {
        let stages = self.stages;
        [
            // In the session's newest-wins slot while its previous frame was inferred.
            Some(("frame.pending", self.received, self.submitted)),
            // In the inference thread's queue while it worked on another session's frame.
            Some(("infer.queue", self.submitted, self.dequeued)),
            self.decoded.map(|decoded| ("infer.decode", self.dequeued, decoded)),
            self.decoded.zip(stages).map(|(decoded, s)| ("infer.yolo", decoded, s.yolo)),
            stages.and_then(|s| s.crop.map(|crop| ("infer.crop", s.yolo, crop))),
            stages.and_then(|s| s.crop.zip(s.head)).map(|(crop, head)| ("infer.head", crop, head)),
        ]
        .into_iter()
        .flatten()
    }
}

/// What became of an inferred frame.
pub enum Outcome<'a> {
    /// Inferred, and no roll settled on it.
    Frame { detections: usize },
    /// The frame that settled a roll: always traced.
    Roll { detections: usize, roll: &'a RollEvent },
    /// Decoding or the pipeline failed.
    Failed(&'a str),
}

/// Makes the session and unit spans. One per process, shared by all sessions.
pub struct UnitTracer {
    tracer: BoxedTracer,
    /// Trace one in this many inferred frames; 0 traces only the frames that settle a roll.
    frame_every: u64,
}

impl UnitTracer {
    /// The global tracer (a no-op without an OTLP endpoint) and `FRAME_TRACE_EVERY`.
    pub fn from_env() -> anyhow::Result<Self> {
        let frame_every = match std::env::var("FRAME_TRACE_EVERY") {
            Ok(v) => v.parse().map_err(|e| anyhow::anyhow!("FRAME_TRACE_EVERY={v}: {e}"))?,
            Err(_) => DEFAULT_FRAME_EVERY,
        };
        Ok(Self::new(opentelemetry::global::tracer("ai-pipeline"), frame_every))
    }

    pub fn new(tracer: BoxedTracer, frame_every: u64) -> Self {
        Self { tracer, frame_every }
    }

    /// Records the `session open` span: the WebSocket handshake, from `accept_started` to
    /// now, as a child of the span the handshake's `traceparent` named. `error` is why
    /// the handshake failed, if it did (no session follows then).
    pub fn open_session(
        self: &Arc<Self>,
        accept_started: Instant,
        parent: &Context,
        model: &str,
        error: Option<&str>,
    ) -> SessionTrace {
        let anchor = Anchor::now();
        let id = format!("{:016x}", rand::random::<u64>());
        let mut span = self
            .tracer
            .span_builder("session open")
            .with_kind(SpanKind::Server)
            .with_start_time(anchor.wall(accept_started))
            .with_attributes([
                KeyValue::new("session.kind", SESSION_KIND),
                KeyValue::new("session.id", id.clone()),
                KeyValue::new("model", model.to_string()),
            ])
            .start_with_context(&self.tracer, parent);
        if let Some(error) = error {
            span.set_status(Status::error(error.to_string()));
        }
        let open = span.span_context().clone();
        span.end_with_timestamp(anchor.wall);

        SessionTrace {
            tracer: Arc::clone(self),
            open,
            opened: anchor.at,
            id,
            model: model.to_string(),
            last_seq: 0,
            inferred: 0,
            failed: 0,
            last_failed_trace: None,
            traced: 0,
            rolls: 0,
            superseded: 0,
            refused: 0,
            unreported_superseded: 0,
            unreported_refused: 0,
        }
    }
}

/// One camera session's counters and the context its units link to.
pub struct SessionTrace {
    tracer: Arc<UnitTracer>,
    /// The `session open` span.
    open: SpanContext,
    opened: Instant,
    id: String,
    model: String,
    /// Number of the last frame taken for inference.
    last_seq: u64,
    inferred: u64,
    failed: u64,
    /// When the last failed frame that was traced sent its reply.
    last_failed_trace: Option<Instant>,
    traced: u64,
    rolls: u64,
    /// Frames replaced by a newer one before they were inferred.
    superseded: u64,
    /// Frames the inference thread didn't take: its queue was full, or it is gone.
    refused: u64,
    /// Drops since the last unit trace, reported on the next one.
    unreported_superseded: u64,
    unreported_refused: u64,
}

impl SessionTrace {
    /// The `session open` span as a context: current for the session's tasks, so their
    /// log lines and `Carried` messages belong to the session.
    pub fn cx(&self) -> Context {
        Context::new().with_remote_span_context(self.open.clone())
    }

    /// Frame number `seq` was taken for inference; the numbers skipped since the last
    /// one were replaced by newer frames.
    pub fn took(&mut self, seq: u64) {
        let skipped = seq.saturating_sub(self.last_seq + 1);
        self.superseded += skipped;
        self.unreported_superseded += skipped;
        self.last_seq = seq;
    }

    /// The frame just taken was not inferred: the inference thread's queue was full.
    pub fn refused(&mut self) {
        self.refused += 1;
        self.unreported_refused += 1;
    }

    /// Counts an inferred frame and, if it is to be traced, records its spans up to the
    /// inference thread's reply. The returned unit is still open: call `sent` once the
    /// replies are written.
    pub fn frame(&mut self, seq: u64, times: &FrameTimes, outcome: Outcome<'_>) -> Option<UnitTrace> {
        self.frame_at(Anchor::now(), seq, times, outcome)
    }

    fn frame_at(&mut self, anchor: Anchor, seq: u64, times: &FrameTimes, outcome: Outcome<'_>) -> Option<UnitTrace> {
        self.inferred += 1;
        // Always traced: a frame that settles a roll, and a failure when none was traced lately.
        let always = match outcome {
            Outcome::Frame { .. } => false,
            Outcome::Roll { .. } => {
                self.rolls += 1;
                true
            }
            Outcome::Failed(_) => {
                self.failed += 1;
                self.last_failed_trace
                    .is_none_or(|last| times.done.saturating_duration_since(last) >= FAILURE_QUIET)
            }
        };
        // The first frame, then every `frame_every`th, so a short session has one too.
        let every = self.tracer.frame_every;
        let sampled = every != 0 && (self.inferred - 1).is_multiple_of(every);
        if !sampled && !always {
            return None;
        }
        self.traced += 1;
        if matches!(outcome, Outcome::Failed(_)) {
            self.last_failed_trace = Some(times.done);
        }

        let dropped = self.unreported_superseded + self.unreported_refused;
        let mut attributes = vec![
            KeyValue::new("session.id", self.id.clone()),
            KeyValue::new("model", self.model.clone()),
            KeyValue::new("frame_seq", seq as i64),
            KeyValue::new("frame_ms", times.done.duration_since(times.dequeued).as_millis() as i64),
            KeyValue::new("frames_dropped", dropped as i64),
        ];
        let name = match outcome {
            Outcome::Frame { detections } => {
                attributes.push(KeyValue::new("detections", detections as i64));
                "frame.infer"
            }
            Outcome::Roll { detections, roll } => {
                attributes.extend([
                    KeyValue::new("detections", detections as i64),
                    KeyValue::new("roll.id", roll.roll_id.clone()),
                    KeyValue::new("roll.dice", roll.dice.len() as i64),
                    KeyValue::new("roll.complete", roll.complete),
                ]);
                "roll.settle"
            }
            Outcome::Failed(_) => "frame.infer",
        };

        // A root of its own, whatever context is current: a unit is one trace.
        let tracer = &self.tracer.tracer;
        let start = anchor.wall(times.received);
        let mut root = tracer
            .span_builder(name)
            .with_start_time(start)
            .with_links(vec![Link::with_context(self.open.clone())])
            .with_attributes(attributes)
            .start_with_context(tracer, &Context::new());
        if dropped > 0 {
            root.add_event_with_timestamp(
                "frames_dropped",
                start,
                vec![
                    KeyValue::new("superseded", self.unreported_superseded as i64),
                    KeyValue::new("refused", self.unreported_refused as i64),
                ],
            );
            self.unreported_superseded = 0;
            self.unreported_refused = 0;
        }
        if let Outcome::Failed(error) = outcome {
            root.set_status(Status::error(error.to_string()));
        }

        let cx = Context::new().with_span(root);
        for (stage, from, to) in times.spans() {
            let mut span = tracer.span_builder(stage).with_start_time(anchor.wall(from)).start_with_context(tracer, &cx);
            // Decoding is the one stage that returns an error.
            if let (Outcome::Failed(error), "infer.decode") = (&outcome, stage) {
                span.set_status(Status::error(error.to_string()));
            }
            span.end_with_timestamp(anchor.wall(to));
        }
        Some(UnitTrace { tracer: Arc::clone(&self.tracer), cx, anchor, send: None })
    }

    /// Records the `session close` span, from `close_started` to now, with the session's
    /// totals. `received` is the number of the last frame read from the socket.
    pub fn close(self, close_started: Instant, received: u64, reason: &'static str) {
        let anchor = Anchor::now();
        // Frames that arrived after the last one taken were never inferred either.
        let superseded = self.superseded + received.saturating_sub(self.last_seq);
        let tracer = &self.tracer.tracer;
        let mut span = tracer
            .span_builder("session close")
            .with_start_time(anchor.wall(close_started))
            .with_links(vec![Link::with_context(self.open)])
            .with_attributes([
                KeyValue::new("session.kind", SESSION_KIND),
                KeyValue::new("session.id", self.id),
                KeyValue::new("model", self.model),
                KeyValue::new("session.close_reason", reason),
                KeyValue::new("session.duration_ms", close_started.duration_since(self.opened).as_millis() as i64),
                KeyValue::new("frames_received", received as i64),
                KeyValue::new("frames_inferred", self.inferred as i64),
                KeyValue::new("frames_failed", self.failed as i64),
                KeyValue::new("frames_traced", self.traced as i64),
                KeyValue::new("frames_dropped", (superseded + self.refused) as i64),
                KeyValue::new("frames_dropped.superseded", superseded as i64),
                KeyValue::new("frames_dropped.refused", self.refused as i64),
                KeyValue::new("rolls", self.rolls as i64),
            ])
            .start_with_context(tracer, &Context::new());
        span.end_with_timestamp(anchor.wall);
    }
}

/// A frame's trace, recorded up to the inference reply and still open while the replies
/// are sent, so what the frontend does with a roll message can be its child.
pub struct UnitTrace {
    tracer: Arc<UnitTracer>,
    /// Holds the root span (`roll.settle` or `frame.infer`).
    cx: Context,
    anchor: Anchor,
    /// The `ws.send` span, when it was started before the send ([`Self::roll_send`]).
    send: Option<opentelemetry::global::BoxedSpan>,
}

impl UnitTrace {
    pub fn cx(&self) -> &Context {
        &self.cx
    }

    pub fn span_context(&self) -> SpanContext {
        self.cx.span().span_context().clone()
    }

    /// Starts the unit's `ws.send` span as the PRODUCER of roll `roll_id`'s message and
    /// returns it as a W3C `traceparent` for the message to carry: the frontend's span for
    /// receiving the roll is its child (OTel messaging: send, then process). Started before
    /// the send because the message has to name it. None when spans aren't exported (no
    /// OTLP endpoint): there is nothing to continue then.
    pub fn roll_send(&mut self, roll_id: &str) -> Option<String> {
        if !self.cx.span().span_context().is_valid() {
            return None;
        }
        let tracer = &self.tracer.tracer;
        let span = tracer
            .span_builder("ws.send")
            .with_kind(SpanKind::Producer)
            .with_start_time(self.anchor.wall(Instant::now()))
            .with_attributes([
                KeyValue::new("messaging.system", "websocket"),
                KeyValue::new("messaging.operation.name", "send"),
                KeyValue::new("messaging.operation.type", "send"),
                KeyValue::new("messaging.message.id", roll_id.to_string()),
            ])
            .start_with_context(tracer, &self.cx);
        let mut carrier = HashMap::new();
        let cx = Context::new().with_remote_span_context(span.span_context().clone());
        TraceContextPropagator::new().inject_context(&cx, &mut carrier);
        self.send = Some(span);
        carrier.remove("traceparent")
    }

    /// Records the `ws.send` span for the replies (`sent`: whether the client took them
    /// all) and ends the unit with it.
    pub fn sent(mut self, send_started: Instant, send_ended: Instant, sent: bool) {
        let tracer = &self.tracer.tracer;
        let end = self.anchor.wall(send_ended);
        let mut span = self.send.take().unwrap_or_else(|| {
            tracer
                .span_builder("ws.send")
                .with_start_time(self.anchor.wall(send_started))
                .start_with_context(tracer, &self.cx)
        });
        if !sent {
            span.set_status(Status::error("client disconnected"));
        }
        span.end_with_timestamp(end);
        self.cx.span().end_with_timestamp(end);
    }
}

#[cfg(test)]
pub mod tests {
    use opentelemetry::trace::{SpanId, TraceFlags, TraceId, TraceState, TracerProvider as _};
    use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider, SpanData};

    use super::*;
    use crate::model::inferance::class_value;
    use crate::roll::{Observation, RollTracker};

    /// A tracer whose finished spans can be read back from the exporter, for as long as
    /// the tracer lives: dropping its last reference shuts the provider down, which
    /// empties the exporter.
    pub fn in_memory(frame_every: u64) -> (Arc<UnitTracer>, InMemorySpanExporter) {
        let exporter = InMemorySpanExporter::default();
        let provider = SdkTracerProvider::builder().with_simple_exporter(exporter.clone()).build();
        let tracer = BoxedTracer::new(Box::new(provider.tracer("test")));
        (Arc::new(UnitTracer::new(tracer, frame_every)), exporter)
    }

    pub fn named<'a>(spans: &'a [SpanData], name: &str) -> Vec<&'a SpanData> {
        spans.iter().filter(|s| s.name == name).collect()
    }

    pub fn attribute(span: &SpanData, key: &str) -> Option<opentelemetry::Value> {
        span.attributes.iter().find(|kv| kv.key.as_str() == key).map(|kv| kv.value.clone())
    }

    /// The context a caller's `traceparent` would give.
    pub fn caller() -> Context {
        Context::new().with_remote_span_context(SpanContext::new(
            TraceId::from(0x4bf9_2f35_77b3_4da6_a3ce_929d_0e0e_4736_u128),
            SpanId::from(0x00f0_67aa_0ba9_02b7_u64),
            TraceFlags::SAMPLED,
            true,
            TraceState::default(),
        ))
    }

    /// Stage timings as the inference thread would record them, without the model:
    /// `t0` plus fixed offsets.
    fn times(t0: Instant) -> FrameTimes {
        let at = |ms| t0 + Duration::from_millis(ms);
        FrameTimes {
            received: at(0),
            submitted: at(3),
            dequeued: at(5),
            decoded: Some(at(9)),
            stages: Some(StageEnds { yolo: at(100), crop: Some(at(104)), head: Some(at(140)) }),
            done: at(141),
        }
    }

    /// An anchor at a fixed wall-clock time, so spans' timestamps can be compared exactly.
    fn anchor(t0: Instant) -> (Anchor, impl Fn(u64) -> SystemTime) {
        let wall = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let anchor = Anchor { at: t0 + Duration::from_millis(141), wall };
        (anchor, move |ms| wall - Duration::from_millis(141) + Duration::from_millis(ms))
    }

    #[test]
    fn spans_made_after_the_fact_carry_the_recorded_times() {
        let (tracer, exporter) = in_memory(1);
        let mut session = tracer.open_session(Instant::now(), &caller(), "m", None);
        let t0 = Instant::now();
        let (anchor, wall) = anchor(t0);

        session.took(1);
        let unit = session.frame_at(anchor, 1, &times(t0), Outcome::Frame { detections: 2 }).unwrap();
        // Later than the anchor: the replies are sent after the spans were started.
        unit.sent(t0 + Duration::from_millis(142), t0 + Duration::from_millis(150), true);

        let spans = exporter.get_finished_spans().unwrap();
        let open = named(&spans, "session open")[0];
        let root = named(&spans, "frame.infer")[0];
        assert_eq!((root.start_time, root.end_time), (wall(0), wall(150)));
        // Its own root, linked to the session.
        assert_eq!(root.parent_span_id, SpanId::INVALID);
        assert_ne!(root.span_context.trace_id(), open.span_context.trace_id());
        assert_eq!(root.links.links[0].span_context, open.span_context);
        assert_eq!(attribute(root, "frame_seq"), Some(1.into()));
        assert_eq!(attribute(root, "frame_ms"), Some(136.into()));
        assert_eq!(attribute(root, "detections"), Some(2.into()));

        let expected = [
            ("frame.pending", 0, 3),
            ("infer.queue", 3, 5),
            ("infer.decode", 5, 9),
            ("infer.yolo", 9, 100),
            ("infer.crop", 100, 104),
            ("infer.head", 104, 140),
            ("ws.send", 142, 150),
        ];
        for (name, from, to) in expected {
            let span = named(&spans, name)[0];
            assert_eq!((span.start_time, span.end_time), (wall(from), wall(to)), "{name}");
            assert_eq!(span.parent_span_id, root.span_context.span_id(), "{name}");
            assert_eq!(span.span_context.trace_id(), root.span_context.trace_id(), "{name}");
        }
        assert_eq!(spans.len(), 1 + 1 + expected.len());
    }

    #[test]
    fn a_frame_with_no_dice_has_no_crop_or_head_span() {
        let (tracer, exporter) = in_memory(1);
        let mut session = tracer.open_session(Instant::now(), &Context::new(), "m", None);
        let t0 = Instant::now();
        let mut times = times(t0);
        times.stages = Some(StageEnds { yolo: t0 + Duration::from_millis(100), crop: None, head: None });

        session.frame(1, &times, Outcome::Frame { detections: 0 }).unwrap().sent(times.done, times.done, true);

        let spans = exporter.get_finished_spans().unwrap();
        assert_eq!(named(&spans, "infer.yolo").len(), 1);
        assert!(named(&spans, "infer.crop").is_empty() && named(&spans, "infer.head").is_empty());
    }

    #[test]
    fn a_failed_decode_is_an_error_on_the_frame_and_its_decode_span() {
        let (tracer, exporter) = in_memory(1);
        let mut session = tracer.open_session(Instant::now(), &Context::new(), "m", None);
        let mut times = times(Instant::now());
        times.stages = None;

        session.frame(1, &times, Outcome::Failed("decode: bad image")).unwrap().sent(times.done, times.done, true);
        let close_started = Instant::now();
        session.close(close_started, 1, "client");

        let spans = exporter.get_finished_spans().unwrap();
        let error = Status::error("decode: bad image");
        assert_eq!(named(&spans, "frame.infer")[0].status, error);
        assert_eq!(named(&spans, "infer.decode")[0].status, error);
        assert!(named(&spans, "infer.yolo").is_empty());
        assert_eq!(attribute(named(&spans, "session close")[0], "frames_failed"), Some(1.into()));
    }

    /// One die lying still: the tracker settles a roll on frame `SETTLE_FRAMES + 1`.
    fn steady_die() -> Vec<Observation> {
        let mut probs = vec![0.005; 21];
        probs[16] = 0.9;
        vec![Observation { bbox: [0.4, 0.4, 0.5, 0.5], probs }]
    }

    /// Feeds `frames` frames of a steady die through the tracker and the session, and
    /// returns the frame numbers traced as (`frame.infer`, `roll.settle`).
    fn traced_frames(frame_every: u64, frames: u64) -> (Vec<i64>, Vec<i64>) {
        let (tracer, exporter) = in_memory(frame_every);
        let mut session = tracer.open_session(Instant::now(), &Context::new(), "m", None);
        let mut tracker = RollTracker::new(0.7, class_value);
        for seq in 1..=frames {
            let times = times(Instant::now());
            session.took(seq);
            let roll = tracker.update(&steady_die(), seq);
            let outcome = match &roll {
                Some(roll) => Outcome::Roll { detections: 1, roll },
                None => Outcome::Frame { detections: 1 },
            };
            if let Some(unit) = session.frame(seq, &times, outcome) {
                unit.sent(times.done, times.done, true);
            }
        }
        let spans = exporter.get_finished_spans().unwrap();
        let seqs = |name| {
            named(&spans, name)
                .into_iter()
                .map(|s| match attribute(s, "frame_seq") {
                    Some(opentelemetry::Value::I64(seq)) => seq,
                    other => panic!("frame_seq: {other:?}"),
                })
                .collect()
        };
        (seqs("frame.infer"), seqs("roll.settle"))
    }

    #[test]
    fn sampling_traces_one_frame_in_n_and_every_frame_that_settles_a_roll() {
        let settles = crate::roll::SETTLE_FRAMES as i64 + 1;
        assert_eq!(traced_frames(10, 25), (vec![1, 11, 21], vec![settles]));
        // A settling frame that is also the Nth is one trace, not two.
        assert_eq!(traced_frames(5, 12), (vec![1, 11], vec![settles]));
        // 0 turns frame sampling off; rolls are still traced.
        assert_eq!(traced_frames(0, 25), (vec![], vec![settles]));
    }

    #[test]
    fn a_failed_frame_is_always_traced_ten_seconds_after_the_last_traced_one() {
        // Frame sampling off, so only the failure rule traces anything.
        let (tracer, exporter) = in_memory(0);
        let mut session = tracer.open_session(Instant::now(), &Context::new(), "m", None);
        let t0 = Instant::now();
        // (frame, seconds into the session, failed)
        let frames = [
            (1, 0, false),
            (2, 1, true),  // the session's first failure
            (3, 2, true),  // 1 s after the traced one
            (4, 11, true), // 10 s after it: a run of failures has a trace every 10 s
            (5, 20, true), // 9 s after the one traced at 11 s
            (6, 25, false),
            (7, 30, true), // 19 s after it
            (8, 31, true),
        ];
        for (seq, at, failed) in frames {
            let mut times = times(t0 + Duration::from_secs(at));
            let outcome = if failed {
                times.stages = None;
                Outcome::Failed("decode: bad image")
            } else {
                Outcome::Frame { detections: 0 }
            };
            session.took(seq);
            if let Some(unit) = session.frame(seq, &times, outcome) {
                unit.sent(times.done, times.done, true);
            }
        }
        session.close(Instant::now(), 8, "client");

        let spans = exporter.get_finished_spans().unwrap();
        let traced: Vec<_> = named(&spans, "frame.infer").iter().map(|s| attribute(s, "frame_seq").unwrap()).collect();
        assert_eq!(traced, vec![2.into(), 4.into(), 7.into()]);
        assert!(named(&spans, "frame.infer").iter().all(|s| s.status == Status::error("decode: bad image")));
        assert_eq!(attribute(named(&spans, "session close")[0], "frames_failed"), Some(6.into()));
    }

    #[test]
    fn roll_settle_carries_the_roll_and_its_message_names_the_send_span() {
        let (tracer, exporter) = in_memory(0);
        let mut session = tracer.open_session(Instant::now(), &Context::new(), "m", None);
        let mut tracker = RollTracker::new(0.7, class_value);
        let roll = (1..=10).find_map(|now| tracker.update(&steady_die(), now)).unwrap();
        let times = times(Instant::now());

        let mut unit = session.frame(6, &times, Outcome::Roll { detections: 1, roll: &roll }).unwrap();
        let traceparent = unit.roll_send(&roll.roll_id).unwrap();
        let send_started = Instant::now();
        unit.sent(send_started, Instant::now(), true);

        let spans = exporter.get_finished_spans().unwrap();
        let settle = named(&spans, "roll.settle")[0];
        // The message names its send span: a PRODUCER under roll.settle, begun before the
        // send and ended with it.
        let [send] = named(&spans, "ws.send")[..] else { panic!("one ws.send span") };
        assert_eq!(traceparent, format!("00-{}-{}-01", settle.span_context.trace_id(), send.span_context.span_id()));
        assert_eq!(send.span_kind, SpanKind::Producer);
        assert_eq!(send.parent_span_id, settle.span_context.span_id());
        assert_eq!(attribute(send, "messaging.operation.type"), Some("send".into()));
        assert_eq!(attribute(send, "messaging.message.id"), Some(roll.roll_id.clone().into()));
        assert!(settle.start_time <= send.start_time && send.start_time <= send.end_time);
        assert_eq!(send.end_time, settle.end_time);
        assert_eq!(attribute(settle, "roll.id"), Some(roll.roll_id.clone().into()));
        assert_eq!(attribute(settle, "roll.dice"), Some(1.into()));
        assert_eq!(attribute(settle, "roll.complete"), Some(true.into()));
    }

    #[test]
    fn without_an_exporter_there_is_no_traceparent() {
        let noop = BoxedTracer::new(Box::new(opentelemetry::trace::noop::NoopTracer::new()));
        let tracer = Arc::new(UnitTracer::new(noop, 1));
        let mut session = tracer.open_session(Instant::now(), &Context::new(), "m", None);
        let mut unit = session.frame(1, &times(Instant::now()), Outcome::Frame { detections: 0 }).unwrap();
        assert_eq!(unit.roll_send("r1"), None);
    }

    #[test]
    fn session_open_and_close_are_linked_and_close_has_the_totals() {
        let (tracer, exporter) = in_memory(1);
        let accept_started = Instant::now();
        let mut session = tracer.open_session(accept_started, &caller(), "dice-head-v1", None);
        // Units link to the open span through this context.
        let session_cx = session.cx();
        for seq in 1..=3 {
            let times = times(Instant::now());
            session.took(seq);
            session.frame(seq, &times, Outcome::Frame { detections: 0 }).unwrap().sent(times.done, times.done, true);
        }
        let close_started = Instant::now();
        session.close(close_started, 3, "client");

        let spans = exporter.get_finished_spans().unwrap();
        let open = named(&spans, "session open")[0];
        let close = named(&spans, "session close")[0];

        // Open: the handshake, in the caller's trace.
        let caller = caller().span().span_context().clone();
        assert_eq!(open.span_kind, SpanKind::Server);
        assert_eq!(open.span_context.trace_id(), caller.trace_id());
        assert_eq!(open.parent_span_id, caller.span_id());
        assert_eq!(session_cx.span().span_context(), &open.span_context);

        // Close: a root of its own, linked to open, with the same session id.
        assert_eq!(close.parent_span_id, SpanId::INVALID);
        assert_eq!(close.links.links.len(), 1);
        assert_eq!(close.links.links[0].span_context, open.span_context);
        assert!(attribute(open, "session.id").is_some());
        for span in [open, close, named(&spans, "frame.infer")[0]] {
            assert_eq!(attribute(span, "session.id"), attribute(open, "session.id"));
            assert_eq!(attribute(span, "model"), Some("dice-head-v1".into()));
        }
        assert_eq!(attribute(open, "session.kind"), Some("arcane.ws_connection".into()));
        assert_eq!(attribute(close, "session.kind"), Some("arcane.ws_connection".into()));
        assert_eq!(attribute(close, "session.close_reason"), Some("client".into()));
        let duration = close_started.duration_since(accept_started).as_millis() as i64;
        match attribute(close, "session.duration_ms") {
            // Counted from the end of the handshake, so at most the whole time.
            Some(opentelemetry::Value::I64(ms)) => assert!((0..=duration).contains(&ms), "{ms} of {duration}"),
            other => panic!("session.duration_ms: {other:?}"),
        }
        for (key, value) in
            [("frames_received", 3), ("frames_inferred", 3), ("frames_traced", 3), ("frames_dropped", 0), ("rolls", 0)]
        {
            assert_eq!(attribute(close, key), Some(value.into()), "{key}");
        }
    }

    #[test]
    fn dropped_frames_are_reported_on_the_next_unit_and_totalled_at_close() {
        let (tracer, exporter) = in_memory(1);
        let mut session = tracer.open_session(Instant::now(), &Context::new(), "m", None);
        let infer = |session: &mut SessionTrace, seq| {
            let times = times(Instant::now());
            session.took(seq);
            session.frame(seq, &times, Outcome::Frame { detections: 0 }).unwrap().sent(times.done, times.done, true);
        };

        infer(&mut session, 1);
        // Frames 2 and 3 were replaced by 4, which the busy inference thread refused.
        session.took(4);
        session.refused();
        infer(&mut session, 5);
        infer(&mut session, 6);
        // Frames 7 and 8 arrived but the session ended before they were taken.
        session.close(Instant::now(), 8, "client");

        let spans = exporter.get_finished_spans().unwrap();
        let frames = named(&spans, "frame.infer");
        let dropped: Vec<_> = frames.iter().map(|s| attribute(s, "frames_dropped").unwrap()).collect();
        assert_eq!(dropped, vec![0.into(), 3.into(), 0.into()]);
        let events: Vec<_> = frames.iter().map(|s| s.events.events.len()).collect();
        assert_eq!(events, vec![0, 1, 0]);
        let event = &frames[1].events.events[0];
        assert_eq!(event.name, "frames_dropped");
        assert_eq!(event.attributes, vec![KeyValue::new("superseded", 2), KeyValue::new("refused", 1)]);

        let close = named(&spans, "session close")[0];
        for (key, value) in [
            ("frames_received", 8),
            ("frames_inferred", 3),
            ("frames_dropped", 5),
            ("frames_dropped.superseded", 4),
            ("frames_dropped.refused", 1),
        ] {
            assert_eq!(attribute(close, key), Some(value.into()), "{key}");
        }
    }
}
