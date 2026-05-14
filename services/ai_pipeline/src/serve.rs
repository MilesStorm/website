use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use burn::backend::Cuda;
use burn::backend::cuda::CudaDevice;
use burn::tensor::bf16;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;

use crate::model::inferance::{Detection, DicePipeline};

type InferBackend = Cuda<bf16>;

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
            let device = CudaDevice::new(0);
            let pipeline = DicePipeline::<InferBackend>::new(device, &head_dir);
            tracing::info!("inference pipeline ready");

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
                        format!(r#"{{"error":"{}"}}"#, e)
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
/// Frames are processed serially; excess frames are silently dropped so latency stays low.
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
    let (mut sink, mut stream) = ws.split();

    while let Some(msg) = stream.next().await {
        match msg? {
            Message::Binary(frame_bytes) => {
                if let Some(json) = handle.infer(frame_bytes.to_vec()).await {
                    sink.send(Message::text(json)).await?;
                }
            }
            Message::Close(_) => break,
            // Ignore ping/pong/text; tungstenite handles pings automatically.
            _ => {}
        }
    }

    Ok(())
}
