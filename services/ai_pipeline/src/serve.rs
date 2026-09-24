use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use burn::backend::Cuda;
use burn::backend::cuda::CudaDevice;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;

use crate::model::inferance::{Detection, DicePipeline};

// f32 inference: the ONNX-imported YOLO graph carries f32 constants that clash
// with bf16 activations (DTypeMismatch at runtime), and yolo26s + DiceHead are
// small enough that f32 stays comfortably real-time.
type InferBackend = Cuda<f32>;

/// (jpeg/png frame bytes, channel to send JSON result back on)
type InferRequest = (Vec<u8>, oneshot::Sender<String>);

/// Shared handle to the single inference thread that owns the GPU pipeline.
#[derive(Clone)]
struct InferHandle {
    tx: mpsc::Sender<InferRequest>,
}

impl InferHandle {
    fn new(head_dir: PathBuf) -> Self {
        // Bound of 1: if inference is busy, new frames replace the queued one
        // rather than piling up, keeping latency low.
        let (tx, mut rx) = mpsc::channel::<InferRequest>(1);

        std::thread::spawn(move || {
            let init = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let device = CudaDevice::new(0);
                DicePipeline::<InferBackend>::new(device, &head_dir)
            }));

            let pipeline = match init {
                Ok(p) => {
                    tracing::info!("inference pipeline ready");
                    p
                }
                Err(e) => {
                    let msg = e
                        .downcast_ref::<String>()
                        .cloned()
                        .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                        .unwrap_or_else(|| "unknown panic".to_string());
                    tracing::error!(error = %msg, "inference pipeline failed to initialize");
                    while let Some((_, resp_tx)) = rx.blocking_recv() {
                        let _ = resp_tx.send(format!(
                            r#"{{"error":"pipeline init failed: {}"}}"#,
                            msg.replace('"', "'")
                        ));
                    }
                    return;
                }
            };

            while let Some((frame_bytes, resp_tx)) = rx.blocking_recv() {
                let t = Instant::now();
                let payload = match decode_and_infer(&pipeline, &frame_bytes) {
                    Ok(dets) => {
                        let ms = t.elapsed().as_millis();
                        format!(
                            r#"{{"detections":{},"frame_ms":{}}}"#,
                            serde_json::to_string(&dets).unwrap(),
                            ms,
                        )
                    }
                    Err(e) => {
                        format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))
                    }
                };
                let _ = resp_tx.send(payload);
            }
        });

        Self { tx }
    }

    /// Submit a frame for inference. Returns None if the inference thread has
    /// exited or the frame was dropped due to backpressure.
    async fn infer(&self, frame: Vec<u8>) -> Option<String> {
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

/// Start the WebSocket server.
///
/// Clients send binary WebSocket messages containing a JPEG or PNG-encoded frame.
/// The server replies with a text message:
///   `{"detections":[{x1,y1,x2,y2,yolo_conf,yolo_class,dice_class,dice_conf},...], "frame_ms": N}`
///
/// A single inference thread (owning the GPU pipeline) is shared across all connections.
/// Each connection reads frames into a newest-wins watch channel and infers only the
/// latest, so stale frames are dropped and latency stays bounded to one inference.
pub async fn serve(addr: &str, head_dir: PathBuf) -> anyhow::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    tracing::info!(addr, "WebSocket server listening");

    let handle = Arc::new(InferHandle::new(head_dir));

    loop {
        let (stream, peer) = listener.accept().await?;
        tracing::info!(%peer, "client connected");
        let handle = Arc::clone(&handle);
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, handle).await {
                tracing::error!(%peer, error = %e, "connection error");
            }
            tracing::info!(%peer, "client disconnected");
        });
    }
}

async fn handle_connection(
    stream: tokio::net::TcpStream,
    handle: Arc<InferHandle>,
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
    let (frame_tx, mut frame_rx) = tokio::sync::watch::channel::<Option<Vec<u8>>>(None);

    let reader = tokio::spawn(async move {
        let mut stream = stream;
        while let Some(msg) = stream.next().await {
            match msg {
                // Overwrites any unprocessed frame: only the latest survives.
                Ok(Message::Binary(frame_bytes)) => {
                    if frame_tx.send(Some(frame_bytes.to_vec())).is_err() {
                        break; // inference loop gone
                    }
                }
                Ok(Message::Close(_)) | Err(_) => break,
                // Ignore ping/pong/text; tungstenite handles pings automatically.
                _ => {}
            }
        }
    });

    loop {
        // Wait until a newer frame arrives, then take it (marking it seen so we
        // don't reprocess the same frame on the next iteration).
        if frame_rx.changed().await.is_err() {
            break; // reader task ended (client disconnected)
        }
        let Some(frame) = frame_rx.borrow_and_update().clone() else {
            continue;
        };

        if let Some(json) = handle.infer(frame).await {
            if sink.send(Message::text(json)).await.is_err() {
                break; // client disconnected
            }
        }
    }

    reader.abort();
    Ok(())
}
