use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use burn::backend::Cuda;
use burn::backend::cuda::CudaDevice;
use futures_util::{SinkExt, StreamExt};
use opentelemetry::Context;
use opentelemetry::trace::FutureExt as _;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::accept_hdr_async;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::model::inferance::{Detection, DicePipeline, StageEnds, class_value};
use crate::roll::{Observation, RollEvent, RollTracker};
use crate::trace::{self, Carried};
use crate::unit_trace::{FrameTimes, Outcome, SessionTrace, UnitTrace, UnitTracer};

// f32 inference: the ONNX-imported YOLO graph carries f32 constants that clash
// with bf16 activations (DTypeMismatch at runtime), and yolo26s + ResNet18 are
// small enough that f32 stays comfortably real-time.
type InferBackend = Cuda<f32>;

/// On shutdown, how long sessions get to record their `session close` span.
const SESSION_CLOSE_TIMEOUT: Duration = Duration::from_secs(10);

/// One camera frame as the reader took it from the socket.
#[derive(Clone)]
struct Frame {
    /// 1-based count of binary messages on the connection.
    seq: u64,
    received: Instant,
    /// JPEG or PNG.
    bytes: Vec<u8>,
}

/// Detections for one frame (or why there are none), when decoding ended, and when the
/// model's stages ended.
type Decoded = (Result<Vec<Detection>, String>, Instant, Option<StageEnds>);

/// Decodes and infers one frame on the inference thread.
type InferFn = Box<dyn FnMut(&[u8]) -> Decoded>;

/// What the inference thread sends back for one frame.
struct Inferred {
    result: Result<Vec<Detection>, String>,
    times: FrameTimes,
}

/// A frame for the inference thread and the channel to send its result back on.
struct InferRequest {
    frame: Frame,
    submitted: Instant,
    reply: oneshot::Sender<Inferred>,
}

/// Shared handle to the single inference thread that owns the GPU pipeline.
#[derive(Clone)]
struct InferHandle {
    tx: mpsc::Sender<Carried<InferRequest>>,
}

impl InferHandle {
    fn new(head_path: PathBuf, dice_threshold: f32) -> Self {
        Self::start(move || {
            let pipeline = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                DicePipeline::<InferBackend>::new(CudaDevice::new(0), &head_path, dice_threshold)
                    .map_err(|e| format!("{e:#}"))
            }))
            .unwrap_or_else(|e| {
                Err(e
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_else(|| "unknown panic".to_string()))
            })?;
            tracing::info!(head = %head_path.display(), dice_threshold, "inference pipeline ready");
            Ok(Box::new(move |bytes: &[u8]| decode_and_infer(&pipeline, bytes)) as InferFn)
        })
    }

    /// Starts the inference thread. `init` runs on it and returns what infers a frame.
    fn start(init: impl FnOnce() -> Result<InferFn, String> + Send + 'static) -> Self {
        // Bound of 1: if inference is busy, new frames replace the queued one
        // rather than piling up, keeping latency low.
        let (tx, mut rx) = trace::channel::<InferRequest>(1);

        #[allow(
            clippy::disallowed_methods,
            reason = "the inference thread owns the GPU pipeline for the life of the process; frames reach it as Carried messages"
        )]
        std::thread::spawn(move || {
            let mut infer = match init() {
                Ok(infer) => infer,
                Err(msg) => {
                    tracing::error!(error = %msg, "inference pipeline failed to initialize");
                    while let Some(request) = rx.blocking_recv() {
                        let now = Instant::now();
                        let (_cx, InferRequest { frame, submitted, reply }) = request.enter();
                        let _ = reply.send(Inferred {
                            result: Err(format!("pipeline init failed: {msg}")),
                            times: FrameTimes {
                                received: frame.received,
                                submitted,
                                dequeued: now,
                                decoded: None,
                                stages: None,
                                done: now,
                            },
                        });
                    }
                    return;
                }
            };

            while let Some(request) = rx.blocking_recv() {
                let dequeued = Instant::now();
                // The session's context is current while its frame runs, so anything
                // logged on this thread for the frame belongs to that session.
                let (_cx, InferRequest { frame, submitted, reply }) = request.enter();
                let (result, decoded, stages) = infer(&frame.bytes);
                let times = FrameTimes {
                    received: frame.received,
                    submitted,
                    dequeued,
                    decoded: Some(decoded),
                    stages,
                    done: Instant::now(),
                };
                let _ = reply.send(Inferred { result, times });
            }
        });

        Self { tx }
    }

    /// Submit a frame for inference. Returns None if the inference thread has
    /// exited or the frame was dropped due to backpressure.
    async fn infer(&self, frame: Carried<Frame>) -> Option<Inferred> {
        #[allow(
            clippy::disallowed_methods,
            reason = "one-shot reply to the task that sent the frame; the request it answers is Carried"
        )]
        let (reply, result) = oneshot::channel();
        let request = frame.map(|frame| InferRequest { frame, submitted: Instant::now(), reply });
        // try_send drops the frame rather than blocking if the slot is full.
        self.tx.try_send(request).ok()?;
        result.await.ok()
    }
}

fn decode_and_infer(pipeline: &DicePipeline<InferBackend>, bytes: &[u8]) -> Decoded {
    let img = image::load_from_memory(bytes).map(|img| img.to_rgb8());
    let decoded = Instant::now();
    match img {
        Ok(img) => {
            let (w, h) = img.dimensions();
            let (detections, stages) = pipeline.infer_frame(img.as_raw(), w as usize, h as usize);
            (Ok(detections), decoded, Some(stages))
        }
        Err(e) => (Err(format!("decode: {e}")), decoded, None),
    }
}

/// Which dice head is serving (e.g. "dice-head-v1"), stamped on roll events so
/// saved training samples record the model that read them.
fn model_id() -> String {
    std::env::var("DICE_MODEL_ID").unwrap_or_else(|_| "dev".to_string())
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// Start the WebSocket server.
///
/// Clients send binary WebSocket messages containing a JPEG or PNG-encoded frame.
/// For every processed frame the server replies with
///   `{"type":"frame","detections":[{x1,y1,x2,y2,yolo_conf,yolo_class,dice_class,dice_conf,value,confident},...],"frame_ms":N,"frame_seq":N}`
/// and, when the dice in view have settled into a new roll (see `roll.rs`),
///   `{"type":"roll","roll_id":..,"dice":[{"value":"17"|null,"conf":..,"box":[..]}],"total":..,"complete":..,"ts":..,"model":..,"frame_seq":N,"_trace":{"traceparent":".."}}`
/// `frame_seq` is the 1-based count of binary messages received on this connection.
/// `_trace` is the roll's trace context for the frontend to continue (`roll_message`);
/// it is there only when spans are exported.
/// Errors are `{"type":"error","error":"..."}`.
///
/// A single inference thread (owning the GPU pipeline) is shared across all connections.
/// Each connection reads frames into a newest-wins watch channel and infers only the
/// latest, so stale frames are dropped and latency stays bounded to one inference.
///
/// When `shutdown` resolves the server stops accepting, tells every session to close
/// (each records its `session close` span) and returns, so the caller can flush the
/// exporters before the process exits.
pub async fn serve(
    addr: &str,
    head_path: PathBuf,
    dice_threshold: f32,
    shutdown: impl Future<Output = ()>,
) -> anyhow::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    let model = model_id();
    let tracer = Arc::new(UnitTracer::from_env()?);
    tracing::info!(addr, model = %model, "WebSocket server listening");

    let handle = Arc::new(InferHandle::new(head_path, dice_threshold));
    serve_on(listener, handle, tracer, dice_threshold, model, shutdown).await
}

/// The accept loop of `serve`, and its shutdown.
async fn serve_on(
    listener: TcpListener,
    handle: Arc<InferHandle>,
    tracer: Arc<UnitTracer>,
    dice_threshold: f32,
    model: String,
    shutdown: impl Future<Output = ()>,
) -> anyhow::Result<()> {
    let closing = CancellationToken::new();
    let sessions = TaskTracker::new();

    tokio::pin!(shutdown);
    let result = loop {
        let (stream, peer) = tokio::select! {
            () = &mut shutdown => break Ok(()),
            accepted = listener.accept() => match accepted {
                Ok(accepted) => accepted,
                Err(e) => break Err(e.into()),
            },
        };
        tracing::info!(%peer, "client connected");
        let handle = Arc::clone(&handle);
        let tracer = Arc::clone(&tracer);
        let model = model.clone();
        let closing = closing.clone();
        trace::spawn_loop(
            "camera session: each frame or roll is a trace of its own",
            sessions.track_future(async move {
                if let Err(e) = handle_connection(stream, handle, tracer, dice_threshold, model, closing).await {
                    tracing::error!(%peer, error = %e, "connection error");
                }
                tracing::info!(%peer, "client disconnected");
            }),
        );
    };

    drop(listener);
    tracing::info!(sessions = sessions.len(), "shutting down: closing sessions");
    closing.cancel();
    sessions.close();
    if tokio::time::timeout(SESSION_CLOSE_TIMEOUT, sessions.wait()).await.is_err() {
        tracing::warn!(sessions = sessions.len(), "sessions still open at shutdown; their close spans are lost");
    }
    result
}

#[expect(clippy::result_large_err, reason = "the handshake callback's error type is tungstenite's")]
async fn handle_connection<S>(
    stream: S,
    handle: Arc<InferHandle>,
    tracer: Arc<UnitTracer>,
    dice_threshold: f32,
    model: String,
    closing: CancellationToken,
) -> anyhow::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    // The frontend's camera session sends its `traceparent` on the handshake, so this
    // connection's `session open` span joins the visitor's trace.
    let accept_started = Instant::now();
    let mut parent = Context::new();
    let accepted = accept_hdr_async(stream, |req: &Request, resp: Response| {
        parent = trace_parent(req.headers());
        Ok(resp)
    })
    .await;
    let ws = match accepted {
        Ok(ws) => ws,
        Err(e) => {
            // Only a caller that sent a trace context gets a span for its failed
            // handshake: port probes and scanners would each start a trace otherwise.
            if opentelemetry::trace::TraceContextExt::has_active_span(&parent) {
                tracer.open_session(accept_started, &parent, &model, Some(&e.to_string()));
            }
            return Err(e.into());
        }
    };
    let session = tracer.open_session(accept_started, &parent, &model, None);
    let cx = session.cx();
    run_session(ws, handle, dice_threshold, model, session, closing).with_context(cx).await;
    Ok(())
}

/// The trace context the handshake's `traceparent` carries (empty without one).
fn trace_parent(headers: &tokio_tungstenite::tungstenite::http::HeaderMap) -> Context {
    opentelemetry::global::get_text_map_propagator(|p| p.extract(&HeaderExtractor(headers)))
}

struct HeaderExtractor<'a>(&'a tokio_tungstenite::tungstenite::http::HeaderMap);

impl opentelemetry::propagation::Extractor for HeaderExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|v| v.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(|k| k.as_str()).collect()
    }
}

/// The roll message for the frontend: the roll event plus the model that read it, the
/// frame it settled on, and the trace context of its `roll.settle` span as `_trace`, so
/// the frontend's publish and delivery spans continue that trace. The frontend strips
/// `_trace` before it stores or forwards the roll.
fn roll_message(roll: &RollEvent, model: &str, seq: u64, unit: Option<&UnitTrace>) -> String {
    let mut roll = serde_json::to_value(roll).unwrap();
    roll["model"] = model.into();
    roll["frame_seq"] = seq.into();
    if let Some(traceparent) = unit.and_then(UnitTrace::traceparent) {
        roll["_trace"] = serde_json::json!({ "traceparent": traceparent });
    }
    roll.to_string()
}

async fn run_session<S>(
    ws: tokio_tungstenite::WebSocketStream<S>,
    handle: Arc<InferHandle>,
    dice_threshold: f32,
    model: String,
    mut session: SessionTrace,
    closing: CancellationToken,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut sink, stream) = ws.split();

    // Decouple socket reading from inference. The previous loop awaited each
    // frame's full inference before reading the next message, so on a GPU that
    // can't keep up (e.g. GTX 1660 at ~140ms/frame) frames piled up in the
    // socket buffer and were drained FIFO — latency grew without bound
    // ("minutes behind"). Instead, a reader task drains the socket as fast as
    // frames arrive and keeps only the *newest* one in a watch channel; the
    // inference loop always grabs that latest frame and lets stale ones fall
    // away. End-to-end latency is then bounded by a single inference, no matter
    // how far behind the GPU is.
    //
    // Every binary message is numbered (1, 2, 3, ...) and replies carry the number
    // of the frame they were computed from (`frame_seq`), so a client that keeps its
    // recent frames can find the exact picture a roll was read from.
    let (frame_tx, mut frame_rx) = trace::watch_channel::<Frame>();

    let reader = trace::spawn_loop(
        "camera frame reader: keeps only the newest frame for the session",
        async move {
            let mut stream = stream;
            let mut seq = 0u64;
            while let Some(msg) = stream.next().await {
                match msg {
                    // Overwrites any unprocessed frame: only the latest survives.
                    Ok(Message::Binary(frame_bytes)) => {
                        seq += 1;
                        let frame = Frame { seq, received: Instant::now(), bytes: frame_bytes.to_vec() };
                        if frame_tx.send(Some(Carried::new(frame))).is_err() {
                            break; // inference loop gone
                        }
                    }
                    Ok(Message::Close(_)) | Err(_) => break,
                    // Ignore ping/pong/text; tungstenite handles pings automatically.
                    _ => {}
                }
            }
        }
        .with_context(session.cx()),
    );

    // One tracker per camera connection: rolls are settled per video stream.
    let mut tracker = RollTracker::new(dice_threshold, class_value);

    let close_reason = loop {
        // Wait until a newer frame arrives, then take it (marking it seen so we
        // don't reprocess the same frame on the next iteration).
        tokio::select! {
            biased;
            () = closing.cancelled() => break "shutdown",
            changed = frame_rx.changed() => {
                if changed.is_err() {
                    break "client"; // reader task ended (client disconnected)
                }
            }
        }
        let Some(frame) = frame_rx.borrow_and_update().clone() else {
            continue;
        };
        let seq = frame.msg.seq;
        session.took(seq);

        let Some(Inferred { result, times }) = handle.infer(frame).await else {
            session.refused();
            continue; // dropped under backpressure
        };
        let mut replies = Vec::with_capacity(2);
        // The frame's trace, if it gets one: only now is it known whether it settled a roll.
        let unit = match result {
            Ok(dets) => {
                let ms = times.done.duration_since(times.dequeued).as_millis();
                replies.push(
                    serde_json::json!({"type": "frame", "detections": dets, "frame_ms": ms, "frame_seq": seq})
                        .to_string(),
                );
                let detections = dets.len();
                let obs: Vec<Observation> = dets
                    .into_iter()
                    .map(|d| Observation { bbox: [d.x1, d.y1, d.x2, d.y2], probs: d.probs })
                    .collect();
                match tracker.update(&obs, now_ms()) {
                    Some(roll) => {
                        let unit = session.frame(seq, &times, Outcome::Roll { detections, roll: &roll });
                        log_roll_settled(&roll, unit.as_ref());
                        replies.push(roll_message(&roll, &model, seq, unit.as_ref()));
                        unit
                    }
                    None => session.frame(seq, &times, Outcome::Frame { detections }),
                }
            }
            Err(e) => {
                let unit = session.frame(seq, &times, Outcome::Failed(&e));
                replies.push(serde_json::json!({"type": "error", "error": e}).to_string());
                unit
            }
        };

        let send_started = Instant::now();
        let mut sent = true;
        for reply in replies {
            if sink.send(Message::text(reply)).await.is_err() {
                sent = false;
                break;
            }
        }
        if let Some(unit) = unit {
            unit.sent(send_started, Instant::now(), sent);
        }
        if !sent {
            break "send failed"; // client disconnected
        }
    };

    let close_started = Instant::now();
    reader.abort();
    let received = frame_rx.borrow().as_ref().map_or(0, |frame| frame.msg.seq);
    session.close(close_started, received, close_reason);
    if close_reason == "shutdown" {
        // Tell the client the server is going away. Bounded: a stalled client must not
        // hold up the exit, and the session's spans are already recorded.
        let going_away = Message::Close(Some(CloseFrame { code: CloseCode::Away, reason: "shutting down".into() }));
        let _ = tokio::time::timeout(Duration::from_secs(1), sink.send(going_away)).await;
    }
}

/// Logs a settled roll inside its `roll.settle` span: the OTLP log record takes the
/// trace from the current context, the stdout line from the two fields.
fn log_roll_settled(roll: &RollEvent, unit: Option<&UnitTrace>) {
    let Some(unit) = unit else {
        tracing::info!(roll_id = %roll.roll_id, dice = roll.dice.len(), complete = roll.complete, "roll settled");
        return;
    };
    let span = unit.span_context();
    let _cx = unit.cx().clone().attach();
    tracing::info!(
        roll_id = %roll.roll_id,
        dice = roll.dice.len(),
        complete = roll.complete,
        trace_id = %span.trace_id(),
        span_id = %span.span_id(),
        "roll settled"
    );
}

#[cfg(test)]
mod tests {
    use opentelemetry::propagation::TextMapPropagator as _;
    use opentelemetry::trace::{SpanId, SpanKind, TraceContextExt as _, TracerProvider as _};
    use opentelemetry_sdk::propagation::TraceContextPropagator;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
    use tracing_opentelemetry::OpenTelemetrySpanExt as _;
    use tracing_subscriber::layer::SubscriberExt as _;

    use super::*;
    use crate::unit_trace::tests::{attribute, in_memory, named};

    type Client = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

    /// An inference thread without the model: every frame shows one die lying still, so
    /// the tracker settles a roll on frame `SETTLE_FRAMES + 1`.
    fn fake_inference() -> Arc<InferHandle> {
        Arc::new(InferHandle::start(|| {
            Ok(Box::new(|_bytes: &[u8]| {
                let decoded = Instant::now();
                let mut probs = vec![0.005; 21];
                probs[16] = 0.9;
                let die = Detection {
                    x1: 0.4,
                    y1: 0.4,
                    x2: 0.5,
                    y2: 0.5,
                    yolo_conf: 0.9,
                    yolo_class: 0,
                    dice_class: 16,
                    dice_conf: 0.9,
                    value: class_value(16),
                    confident: true,
                    probs,
                };
                let yolo = Instant::now();
                let stages = StageEnds { yolo, crop: Some(Instant::now()), head: Some(Instant::now()) };
                (Ok(vec![die]), decoded, Some(stages))
            }) as InferFn)
        }))
    }

    /// A WebSocket client that sends `traceparent` on the handshake when given a context.
    async fn client(url: &str, caller: Option<&Context>) -> tokio_tungstenite::tungstenite::Result<Client> {
        let mut request = url.into_client_request().unwrap();
        if let Some(caller) = caller {
            let mut headers = std::collections::HashMap::new();
            TraceContextPropagator::new().inject_context(caller, &mut headers);
            request.headers_mut().insert("traceparent", headers["traceparent"].parse().unwrap());
        }
        #[allow(clippy::disallowed_methods, reason = "test: the client stands in for the frontend's traced connect")]
        let connected = tokio_tungstenite::connect_async(request).await;
        connected.map(|(client, _)| client)
    }

    /// Serves one connection with `handle_connection` and connects a client to it.
    async fn connect(
        tracer: Arc<UnitTracer>,
        closing: CancellationToken,
        caller: Option<&Context>,
    ) -> (Client, tokio::task::JoinHandle<()>) {
        opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/", listener.local_addr().unwrap());
        let server = trace::spawn_loop("test: one camera session", async move {
            let (stream, _) = listener.accept().await.unwrap();
            handle_connection(stream, fake_inference(), tracer, 0.7, "test-model".into(), closing).await.unwrap();
        });
        (client(&url, caller).await.unwrap(), server)
    }

    /// Sends one frame and returns the replies up to and including its `frame` reply.
    async fn send_frame(client: &mut Client, seq: u64) -> Vec<serde_json::Value> {
        client.send(Message::binary(vec![0u8; 4])).await.unwrap();
        let mut replies = Vec::new();
        loop {
            let reply: serde_json::Value =
                serde_json::from_str(client.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
            let done = reply["type"] == "roll" || (reply["type"] == "frame" && seq != SETTLES_ON);
            assert_eq!(reply["frame_seq"], seq);
            replies.push(reply);
            if done {
                return replies;
            }
        }
    }

    /// The frame on which `fake_inference`'s die settles.
    const SETTLES_ON: u64 = crate::roll::SETTLE_FRAMES as u64 + 1;

    /// A `traceparent` sent on the WebSocket handshake (as the frontend's camera session
    /// does) puts the connection's `session open` span in the sender's trace. A
    /// mismatched OTel matrix still builds but breaks this (TRACING.md, "Upgrading
    /// OpenTelemetry"): the caller's span is a `tracing` span, bridged by
    /// tracing-opentelemetry.
    #[tokio::test(flavor = "current_thread")]
    async fn handshake_traceparent_continues_the_trace() {
        let bridge = opentelemetry_sdk::trace::SdkTracerProvider::builder().build().tracer("test");
        let _guard = tracing::subscriber::set_default(
            tracing_subscriber::registry().with(tracing_opentelemetry::layer().with_tracer(bridge)),
        );
        let caller = tracing::info_span!("caller").context();
        let (tracer, exporter) = in_memory(1);

        let (client, server) = connect(Arc::clone(&tracer), CancellationToken::new(), Some(&caller)).await;
        drop(client);
        server.await.unwrap();

        let spans = exporter.get_finished_spans().unwrap();
        let open = named(&spans, "session open")[0];
        let caller = caller.span().span_context().clone();
        assert!(caller.is_valid());
        assert_eq!(open.span_context.trace_id(), caller.trace_id());
        assert_eq!(open.parent_span_id, caller.span_id());
        assert_eq!(open.span_kind, SpanKind::Server);
    }

    /// A whole session against a fake model: the roll message carries the `roll.settle`
    /// span's context and is otherwise unchanged, frames are traced one in N, and a
    /// shutdown closes the session with its `session close` span.
    #[tokio::test(flavor = "current_thread")]
    async fn session_traces_frames_and_rolls_and_closes_on_shutdown() {
        let (tracer, exporter) = in_memory(4);
        let closing = CancellationToken::new();
        let (mut client, server) = connect(Arc::clone(&tracer), closing.clone(), None).await;

        let mut rolls = Vec::new();
        for seq in 1..=10 {
            let replies = send_frame(&mut client, seq).await;
            rolls.extend(replies.into_iter().filter(|r| r["type"] == "roll"));
        }
        closing.cancel();
        let goodbye = client.next().await.unwrap().unwrap();
        server.await.unwrap();

        let spans = exporter.get_finished_spans().unwrap();
        let open = named(&spans, "session open")[0];
        let settle = named(&spans, "roll.settle");
        assert_eq!(settle.len(), 1);
        let settle = settle[0];

        // The roll message: `_trace` is the roll.settle span, nothing else is new.
        assert_eq!(rolls.len(), 1);
        let roll = rolls[0].as_object().unwrap();
        let mut keys: Vec<_> = roll.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["_trace", "complete", "dice", "frame_seq", "model", "roll_id", "total", "ts", "type"]
        );
        assert_eq!(roll["model"], "test-model");
        assert_eq!(roll["frame_seq"], SETTLES_ON);
        let trace = roll["_trace"].as_object().unwrap();
        assert_eq!(trace.len(), 1);
        let carrier = std::collections::HashMap::from([(
            "traceparent".to_string(),
            trace["traceparent"].as_str().unwrap().to_string(),
        )]);
        let continued = TraceContextPropagator::new().extract(&carrier);
        let continued = continued.span().span_context().clone();
        assert!(continued.is_valid() && continued.is_sampled());
        assert_eq!(continued.trace_id(), settle.span_context.trace_id());
        assert_eq!(continued.span_id(), settle.span_context.span_id());
        assert_eq!(attribute(settle, "roll.id"), Some(roll["roll_id"].as_str().unwrap().to_string().into()));

        // One frame in 4, and the settling frame: each a root linked to the session.
        let frames = named(&spans, "frame.infer");
        let seqs: Vec<_> = frames.iter().map(|s| attribute(s, "frame_seq").unwrap()).collect();
        assert_eq!(seqs, vec![1.into(), 5.into(), 9.into()]);
        for unit in frames.iter().chain([&settle]) {
            assert_eq!(unit.parent_span_id, SpanId::INVALID);
            assert_eq!(unit.links.links[0].span_context, open.span_context);
            let children: Vec<_> = spans
                .iter()
                .filter(|s| s.parent_span_id == unit.span_context.span_id())
                .map(|s| s.name.as_ref())
                .collect();
            assert_eq!(
                children,
                ["frame.pending", "infer.queue", "infer.decode", "infer.yolo", "infer.crop", "infer.head", "ws.send"]
            );
            // The stages lie inside the unit, which lasts until its replies are sent.
            for child in spans.iter().filter(|s| s.parent_span_id == unit.span_context.span_id()) {
                assert!(unit.start_time <= child.start_time && child.end_time <= unit.end_time, "{}", child.name);
            }
        }

        // Shutdown: the client is told, and the session's totals are recorded.
        match goodbye {
            Message::Close(Some(frame)) => assert_eq!(frame.code, CloseCode::Away),
            other => panic!("expected a close frame, got {other:?}"),
        }
        let close = named(&spans, "session close")[0];
        assert_eq!(close.links.links[0].span_context, open.span_context);
        assert_eq!(attribute(close, "session.close_reason"), Some("shutdown".into()));
        for (key, value) in [("frames_received", 10), ("frames_inferred", 10), ("frames_traced", 4), ("rolls", 1)] {
            assert_eq!(attribute(close, key), Some(value.into()), "{key}");
        }
    }

    /// What SIGTERM starts: the server stops accepting, every open session is told to go
    /// and records its `session close` span, and `serve` returns (main then flushes the
    /// exporters).
    #[tokio::test(flavor = "current_thread")]
    async fn shutdown_stops_accepting_and_closes_every_session() {
        let (tracer, exporter) = in_memory(1);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/", listener.local_addr().unwrap());
        let shutdown = CancellationToken::new();
        let server = trace::spawn_loop(
            "test: the server",
            serve_on(listener, fake_inference(), Arc::clone(&tracer), 0.7, "m".into(), shutdown.clone().cancelled_owned()),
        );
        let mut clients = [client(&url, None).await.unwrap(), client(&url, None).await.unwrap()];
        for client in &mut clients {
            send_frame(client, 1).await;
        }

        shutdown.cancel();
        for client in &mut clients {
            assert!(matches!(client.next().await, Some(Ok(Message::Close(Some(_))))));
        }
        server.await.unwrap().unwrap();

        assert!(client(&url, None).await.is_err(), "still accepting after shutdown");
        let spans = exporter.get_finished_spans().unwrap();
        let closes = named(&spans, "session close");
        assert_eq!(closes.len(), 2);
        for close in closes {
            assert_eq!(attribute(close, "session.close_reason"), Some("shutdown".into()));
            assert_eq!(attribute(close, "frames_inferred"), Some(1.into()));
        }
    }

    #[test]
    fn roll_message_without_a_trace_is_the_plain_roll() {
        let roll = RollEvent { kind: "roll", roll_id: "r1".into(), dice: vec![], total: None, complete: false, ts: 7 };
        let message: serde_json::Value = serde_json::from_str(&roll_message(&roll, "m", 3, None)).unwrap();
        assert_eq!(
            message,
            serde_json::json!({"type": "roll", "roll_id": "r1", "dice": [], "total": null, "complete": false, "ts": 7, "model": "m", "frame_seq": 3})
        );
    }
}
