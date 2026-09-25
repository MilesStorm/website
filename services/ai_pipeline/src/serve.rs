use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use burn::backend::Cuda;
use burn::backend::cuda::CudaDevice;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;

use crate::model::inferance::{Detection, DicePipeline, class_value};
use crate::roll::{Observation, RollTracker};

// f32 inference: the ONNX-imported YOLO graph carries f32 constants that clash
// with bf16 activations (DTypeMismatch at runtime), and yolo26s + ResNet18 are
// small enough that f32 stays comfortably real-time.
type InferBackend = Cuda<f32>;

/// Detections for one frame plus how long inference took.
type InferResult = Result<(Vec<Detection>, u128), String>;

/// (jpeg/png frame bytes, channel to send the result back on)
type InferRequest = (Vec<u8>, oneshot::Sender<InferResult>);

/// Shared handle to the single inference thread that owns the GPU pipeline.
#[derive(Clone)]
struct InferHandle {
    tx: mpsc::Sender<InferRequest>,
}

impl InferHandle {
    fn new(head_path: PathBuf, dice_threshold: f32) -> Self {
        // Bound of 1: if inference is busy, new frames replace the queued one
        // rather than piling up, keeping latency low.
        let (tx, mut rx) = mpsc::channel::<InferRequest>(1);

        std::thread::spawn(move || {
            let init = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                DicePipeline::<InferBackend>::new(CudaDevice::new(0), &head_path, dice_threshold)
                    .map_err(|e| format!("{e:#}"))
            }))
            .unwrap_or_else(|e| {
                Err(e
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_else(|| "unknown panic".to_string()))
            });

            let pipeline = match init {
                Ok(p) => {
                    tracing::info!(head = %head_path.display(), dice_threshold, "inference pipeline ready");
                    p
                }
                Err(msg) => {
                    tracing::error!(error = %msg, "inference pipeline failed to initialize");
                    while let Some((_, resp_tx)) = rx.blocking_recv() {
                        let _ = resp_tx.send(Err(format!("pipeline init failed: {msg}")));
                    }
                    return;
                }
            };

            while let Some((frame_bytes, resp_tx)) = rx.blocking_recv() {
                let t = Instant::now();
                let result = decode_and_infer(&pipeline, &frame_bytes).map(|d| (d, t.elapsed().as_millis()));
                let _ = resp_tx.send(result);
            }
        });

        Self { tx }
    }

    /// Submit a frame for inference. Returns None if the inference thread has
    /// exited or the frame was dropped due to backpressure.
    async fn infer(&self, frame: Vec<u8>) -> Option<InferResult> {
        let (resp_tx, resp_rx) = oneshot::channel();
        // try_send drops the frame rather than blocking if the slot is full.
        self.tx.try_send((frame, resp_tx)).ok()?;
        resp_rx.await.ok()
    }
}

fn decode_and_infer(
    pipeline: &DicePipeline<InferBackend>,
    bytes: &[u8],
) -> Result<Vec<Detection>, String> {
    let img = image::load_from_memory(bytes)
        .map_err(|e| format!("decode: {e}"))?
        .to_rgb8();
    let (w, h) = img.dimensions();
    Ok(pipeline.infer_frame(img.as_raw(), w as usize, h as usize))
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
///   `{"type":"roll","roll_id":..,"dice":[{"value":"17"|null,"conf":..,"box":[..]}],"total":..,"complete":..,"ts":..,"model":..,"frame_seq":N}`
/// `frame_seq` is the 1-based count of binary messages received on this connection.
/// Errors are `{"type":"error","error":"..."}`.
///
/// A single inference thread (owning the GPU pipeline) is shared across all connections.
/// Each connection reads frames into a newest-wins watch channel and infers only the
/// latest, so stale frames are dropped and latency stays bounded to one inference.
pub async fn serve(addr: &str, head_path: PathBuf, dice_threshold: f32) -> anyhow::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    let model = model_id();
    tracing::info!(addr, model = %model, "WebSocket server listening");

    let handle = Arc::new(InferHandle::new(head_path, dice_threshold));

    loop {
        let (stream, peer) = listener.accept().await?;
        tracing::info!(%peer, "client connected");
        let handle = Arc::clone(&handle);
        let model = model.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, handle, dice_threshold, model).await {
                tracing::error!(%peer, error = %e, "connection error");
            }
            tracing::info!(%peer, "client disconnected");
        });
    }
}

async fn handle_connection(
    stream: tokio::net::TcpStream,
    handle: Arc<InferHandle>,
    dice_threshold: f32,
    model: String,
) -> anyhow::Result<()> {
    let ws = accept_async(stream).await?;
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
    let (frame_tx, mut frame_rx) = tokio::sync::watch::channel::<Option<(u64, Vec<u8>)>>(None);

    let reader = tokio::spawn(async move {
        let mut stream = stream;
        let mut seq = 0u64;
        while let Some(msg) = stream.next().await {
            match msg {
                // Overwrites any unprocessed frame: only the latest survives.
                Ok(Message::Binary(frame_bytes)) => {
                    seq += 1;
                    if frame_tx.send(Some((seq, frame_bytes.to_vec()))).is_err() {
                        break; // inference loop gone
                    }
                }
                Ok(Message::Close(_)) | Err(_) => break,
                // Ignore ping/pong/text; tungstenite handles pings automatically.
                _ => {}
            }
        }
    });

    // One tracker per camera connection: rolls are settled per video stream.
    let mut tracker = RollTracker::new(dice_threshold, class_value);

    'frames: loop {
        // Wait until a newer frame arrives, then take it (marking it seen so we
        // don't reprocess the same frame on the next iteration).
        if frame_rx.changed().await.is_err() {
            break; // reader task ended (client disconnected)
        }
        let Some((seq, frame)) = frame_rx.borrow_and_update().clone() else {
            continue;
        };

        let Some(result) = handle.infer(frame).await else {
            continue; // dropped under backpressure
        };
        let mut replies = Vec::with_capacity(2);
        match result {
            Ok((dets, ms)) => {
                replies.push(
                    serde_json::json!({"type": "frame", "detections": dets, "frame_ms": ms, "frame_seq": seq})
                        .to_string(),
                );
                let obs: Vec<Observation> = dets
                    .into_iter()
                    .map(|d| Observation { bbox: [d.x1, d.y1, d.x2, d.y2], probs: d.probs })
                    .collect();
                if let Some(roll) = tracker.update(&obs, now_ms()) {
                    tracing::info!(roll_id = %roll.roll_id, dice = roll.dice.len(), complete = roll.complete, "roll settled");
                    let mut roll = serde_json::to_value(&roll).unwrap();
                    roll["model"] = model.clone().into();
                    roll["frame_seq"] = seq.into();
                    replies.push(roll.to_string());
                }
            }
            Err(e) => replies.push(serde_json::json!({"type": "error", "error": e}).to_string()),
        }
        for reply in replies {
            if sink.send(Message::text(reply)).await.is_err() {
                break 'frames; // client disconnected
            }
        }
    }

    reader.abort();
    Ok(())
}
