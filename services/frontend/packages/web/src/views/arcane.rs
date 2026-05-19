use api::LoginStatus;
use dioxus::prelude::*;

use crate::{LOGIN_STATUS, PERMISSIONS};

#[component]
pub fn Arcane() -> Element {
    let has_perm = PERMISSIONS.read().contains_key("arcane");

    match LOGIN_STATUS() {
        LoginStatus::LoggedOut => rsx! {
            div { class: "flex h-screen items-center justify-center",
                p { "Please log in to access the dice recognizer." }
            }
        },
        LoginStatus::LoggedIn(_) if !has_perm => rsx! {
            div { class: "flex h-screen items-center justify-center",
                p { "You do not have permission to access the dice recognizer." }
            }
        },
        LoginStatus::LoggedIn(_) => rsx! { ArcaneIsland {} },
    }
}

// ── shared types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum WsState {
    Connecting,
    Connected,
    Disconnected,
    Error(String),
}

#[cfg(target_arch = "wasm32")]
use serde::Deserialize;

#[cfg(target_arch = "wasm32")]
#[derive(Debug, Clone, Deserialize)]
struct Detection {
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    yolo_conf: f32,
    yolo_class: u32,
    dice_class: u32,
    dice_conf: f32,
}

#[cfg(target_arch = "wasm32")]
#[derive(Debug, Clone, Deserialize)]
struct InferResult {
    detections: Vec<Detection>,
    frame_ms: u64,
}

#[cfg(target_arch = "wasm32")]
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum ServerMsg {
    Result(InferResult),
    Error { error: String },
}

// ── component ─────────────────────────────────────────────────────────────────

#[component]
fn ArcaneIsland() -> Element {
    let mut ws_state = use_signal(|| WsState::Connecting);
    let mut frame_ms = use_signal(|| 0u64);
    let mut debug_mode = use_signal(|| false);
    let mut detect_count = use_signal(|| 0usize);
    let mut server_error = use_signal(|| Option::<String>::None);
    let mut debug_text = use_signal(|| String::new());

    use_coroutine(move |_: UnboundedReceiver<()>| async move {
        #[cfg(target_arch = "wasm32")]
        run_arcane(ws_state, frame_ms, debug_mode, detect_count, server_error, debug_text).await;
    });

    rsx! {
        div { class: "flex flex-col items-center gap-4 p-4 min-h-[calc(100vh-5rem)]",
            div { class: "relative w-full max-w-2xl",
                // Live camera feed
                video {
                    id: "arcane-video",
                    class: "w-full rounded-xl bg-base-300",
                    autoplay: "true",
                    playsinline: "true",
                    muted: "true",
                }
                // Detection overlay (absolutely positioned, pointer-events disabled so
                // the video controls remain accessible)
                canvas {
                    id: "arcane-overlay",
                    class: "absolute inset-0 w-full h-full pointer-events-none rounded-xl",
                }
                // Status badge — top-left corner
                div { class: "absolute top-2 left-2",
                    match ws_state() {
                        WsState::Connecting => rsx! {
                            span { class: "badge badge-warning badge-sm", "connecting" }
                        },
                        WsState::Connected => rsx! {
                            span { class: "badge badge-success badge-sm", "{frame_ms()}ms" }
                        },
                        WsState::Disconnected => rsx! {
                            span { class: "badge badge-error badge-sm", "disconnected" }
                        },
                        WsState::Error(e) => rsx! {
                            span { class: "badge badge-error badge-sm", "{e}" }
                        },
                    }
                }
                // Debug toggle — top-right corner
                button {
                    class: "absolute top-2 right-2 btn btn-xs btn-ghost bg-base-300/60 hover:bg-base-300",
                    onclick: move |_| debug_mode.set(!debug_mode()),
                    if debug_mode() { "✕ debug" } else { "debug" }
                }
            }

            // Debug panel — only visible when debug_mode is on
            if debug_mode() {
                div { class: "w-full max-w-2xl rounded-xl bg-base-200 p-3 font-mono text-xs space-y-2",
                    // Summary row
                    div { class: "flex flex-wrap gap-4 items-center",
                        span { class: "text-base-content/50", "frame_ms" }
                        span { class: "font-bold tabular-nums", "{frame_ms()}ms" }
                        span { class: "text-base-content/50", "detections" }
                        span { class: "font-bold tabular-nums", "{detect_count()}" }
                        span { class: "text-base-content/50", "ws" }
                        span { class: "font-bold",
                            match ws_state() {
                                WsState::Connecting => "connecting",
                                WsState::Connected => "open",
                                WsState::Disconnected => "closed",
                                WsState::Error(_) => "error",
                            }
                        }
                    }
                    // Server error (if any)
                    if let Some(err) = server_error() {
                        div { class: "text-error break-all", "server: {err}" }
                    }
                    // Per-detection detail
                    if !debug_text().is_empty() {
                        pre { class: "whitespace-pre-wrap text-base-content/80 overflow-x-auto leading-5",
                            "{debug_text()}"
                        }
                    } else if matches!(ws_state(), WsState::Connected) {
                        div { class: "text-base-content/40 italic", "no detections yet" }
                    }
                }
            }
        }
        // Hidden off-screen canvas used only for JPEG capture; never displayed.
        canvas { id: "arcane-capture", style: "display:none;" }
    }
}

// ── WASM implementation ───────────────────────────────────────────────────────

#[cfg(target_arch = "wasm32")]
async fn run_arcane(
    mut ws_state: Signal<WsState>,
    mut frame_ms: Signal<u64>,
    debug_mode: Signal<bool>,
    mut detect_count: Signal<usize>,
    mut server_error: Signal<Option<String>>,
    mut debug_text: Signal<String>,
) {
    use std::{cell::RefCell, rc::Rc};
    use wasm_bindgen::{closure::Closure, JsCast, JsValue};
    use wasm_bindgen_futures::JsFuture;
    use web_sys::{
        CanvasRenderingContext2d, HtmlCanvasElement, HtmlMediaElement, HtmlVideoElement,
        MediaStreamConstraints, WebSocket,
    };

    macro_rules! bail {
        ($msg:expr) => {{
            ws_state.set(WsState::Error($msg.into()));
            return;
        }};
    }

    let window = match web_sys::window() {
        Some(w) => w,
        None => bail!("no window"),
    };
    let document = window.document().unwrap();

    // ── Camera ───────────────────────────────────────────────────────────
    let media_devices = match window.navigator().media_devices() {
        Ok(m) => m,
        Err(_) => bail!("camera unavailable"),
    };

    let mut constraints = MediaStreamConstraints::new();
    constraints.video(&JsValue::TRUE);

    let stream_js = match JsFuture::from(
        media_devices
            .get_user_media_with_constraints(&constraints)
            .unwrap(),
    )
    .await
    {
        Ok(s) => s,
        Err(_) => bail!("camera permission denied"),
    };
    let stream: web_sys::MediaStream = match stream_js.dyn_into() {
        Ok(s) => s,
        Err(_) => bail!("stream cast failed"),
    };

    let video = document
        .get_element_by_id("arcane-video")
        .unwrap()
        .dyn_into::<HtmlVideoElement>()
        .unwrap();

    // set_src_object lives on HtmlMediaElement, which HtmlVideoElement derefs to.
    video
        .unchecked_ref::<HtmlMediaElement>()
        .set_src_object(Some(&stream));

    // Wait until the browser knows the video dimensions.
    JsFuture::from(js_sys::Promise::new(&mut |resolve, _| {
        let cb = Closure::once(move || {
            resolve.call0(&JsValue::NULL).ok();
        });
        video.set_onloadedmetadata(Some(cb.as_ref().unchecked_ref()));
        cb.forget();
    }))
    .await
    .ok();

    let vid_w = video.video_width().max(1);
    let vid_h = video.video_height().max(1);

    // ── Canvas setup ─────────────────────────────────────────────────────
    let capture_canvas = document
        .get_element_by_id("arcane-capture")
        .unwrap()
        .dyn_into::<HtmlCanvasElement>()
        .unwrap();
    capture_canvas.set_width(vid_w);
    capture_canvas.set_height(vid_h);
    let capture_ctx = capture_canvas
        .get_context("2d")
        .unwrap()
        .unwrap()
        .dyn_into::<CanvasRenderingContext2d>()
        .unwrap();

    let overlay_canvas = document
        .get_element_by_id("arcane-overlay")
        .unwrap()
        .dyn_into::<HtmlCanvasElement>()
        .unwrap();
    overlay_canvas.set_width(vid_w);
    overlay_canvas.set_height(vid_h);
    let overlay_ctx = overlay_canvas
        .get_context("2d")
        .unwrap()
        .unwrap()
        .dyn_into::<CanvasRenderingContext2d>()
        .unwrap();

    // ── WebSocket ────────────────────────────────────────────────────────
    let location = window.location();
    let scheme = if location.protocol().unwrap_or_default() == "https:" {
        "wss"
    } else {
        "ws"
    };
    let host = location.host().unwrap_or_default();

    let ws = match WebSocket::new(&format!("{scheme}://{host}/ws/arcane")) {
        Ok(ws) => Rc::new(ws),
        Err(e) => bail!(format!("ws: {e:?}")),
    };
    ws.set_binary_type(web_sys::BinaryType::Arraybuffer);

    // Wait for the WS handshake.
    JsFuture::from(js_sys::Promise::new(&mut |resolve, _| {
        let ws2 = ws.clone();
        let cb = Closure::once(move |_: JsValue| {
            resolve.call0(&JsValue::NULL).ok();
        });
        ws2.set_onopen(Some(cb.as_ref().unchecked_ref()));
        cb.forget();
    }))
    .await
    .ok();

    ws_state.set(WsState::Connected);

    // Shared slot for the latest inference result — written by the onmessage
    // callback, read by the draw loop.
    let latest: Rc<RefCell<Option<InferResult>>> = Rc::new(RefCell::new(None));
    {
        let latest = latest.clone();
        let mut frame_ms = frame_ms;
        let cb = Closure::<dyn FnMut(_)>::new(move |e: web_sys::MessageEvent| {
            if let Some(text) = e.data().as_string() {
                match serde_json::from_str::<ServerMsg>(&text) {
                    Ok(ServerMsg::Result(result)) => {
                        frame_ms.set(result.frame_ms);
                        detect_count.set(result.detections.len());
                        server_error.set(None);

                        // Build per-detection debug summary
                        let mut dbg = String::new();
                        for (i, d) in result.detections.iter().enumerate() {
                            dbg.push_str(&format!(
                                "det[{i}] dice={} ({:.1}%)  yolo_cls={} yolo_conf={:.1}%  bbox=[{:.3},{:.3} → {:.3},{:.3}]\n",
                                dice_value(d.dice_class),
                                d.dice_conf * 100.0,
                                d.yolo_class,
                                d.yolo_conf * 100.0,
                                d.x1, d.y1, d.x2, d.y2,
                            ));
                        }
                        debug_text.set(dbg);
                        *latest.borrow_mut() = Some(result);
                    }
                    Ok(ServerMsg::Error { error }) => {
                        server_error.set(Some(error));
                        detect_count.set(0);
                        debug_text.set(String::new());
                    }
                    Err(_) => {}
                }
            }
        });
        ws.set_onmessage(Some(cb.as_ref().unchecked_ref()));
        cb.forget();
    }

    // ── Frame capture loop ──────────────────────────────────────────────
    loop {
        // ~15 fps keeps the GPU queue comfortable without saturating it.
        sleep_ms(66).await;

        if ws.ready_state() != WebSocket::OPEN {
            ws_state.set(WsState::Disconnected);
            break;
        }

        // Stamp the current video frame onto the hidden capture canvas.
        capture_ctx
            .draw_image_with_html_video_element(&video, 0.0, 0.0)
            .ok();

        // Encode as JPEG (via browser's dataURL API) and ship as binary.
        if let Ok(url) = capture_canvas.to_data_url_with_type("image/jpeg") {
            if let Some(b64) = url.strip_prefix("data:image/jpeg;base64,") {
                if let Ok(binary) = window.atob(b64) {
                    // atob returns a Latin-1 string; each char's code point is one byte.
                    let bytes: Vec<u8> = binary.chars().map(|c| c as u8).collect();
                    let ta = js_sys::Uint8Array::new_with_length(bytes.len() as u32);
                    ta.copy_from(&bytes);
                    ws.send_with_array_buffer(&ta.buffer()).ok();
                }
            }
        }

        // Redraw detection boxes from the latest inference result.
        let dets = latest
            .borrow()
            .as_ref()
            .map(|r| r.detections.clone())
            .unwrap_or_default();
        draw_overlay(&overlay_ctx, &dets, vid_w as f64, vid_h as f64, debug_mode());
    }
}

/// Tick the async event loop for `ms` milliseconds using a JS setTimeout.
#[cfg(target_arch = "wasm32")]
async fn sleep_ms(ms: i32) {
    let _ = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::new(
        &mut |resolve, _| {
            web_sys::window()
                .unwrap()
                .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms)
                .unwrap();
        },
    ))
    .await;
}

/// Clear the overlay canvas and draw bounding boxes + labels for each detection.
/// In debug mode the label includes YOLO confidence and boxes are colour-coded by
/// dice confidence: green ≥80 %, yellow ≥50 %, red below that.
#[cfg(target_arch = "wasm32")]
fn draw_overlay(ctx: &web_sys::CanvasRenderingContext2d, dets: &[Detection], w: f64, h: f64, debug: bool) {
    use wasm_bindgen::JsValue;

    ctx.clear_rect(0.0, 0.0, w, h);

    for det in dets {
        let x = det.x1 as f64 * w;
        let y = det.y1 as f64 * h;
        let bw = (det.x2 - det.x1) as f64 * w;
        let bh = (det.y2 - det.y1) as f64 * h;

        let color = if debug {
            if det.dice_conf >= 0.8 {
                "rgba(52,211,153,0.95)"
            } else if det.dice_conf >= 0.5 {
                "rgba(251,191,36,0.95)"
            } else {
                "rgba(239,68,68,0.95)"
            }
        } else {
            "rgba(52,211,153,0.95)"
        };

        // Box
        ctx.set_stroke_style(&JsValue::from_str(color));
        ctx.set_line_width(2.5);
        ctx.stroke_rect(x, y, bw, bh);

        // Label
        let label = if debug {
            format!(
                "{} {:.0}% | y:{:.0}%",
                dice_value(det.dice_class),
                det.dice_conf * 100.0,
                det.yolo_conf * 100.0,
            )
        } else {
            format!("{} {:.0}%", dice_value(det.dice_class), det.dice_conf * 100.0)
        };

        ctx.set_font("bold 13px monospace");
        // Rough text width estimate: ~8px per character
        let pill_w = label.len() as f64 * 8.0 + 6.0;
        ctx.set_fill_style(&JsValue::from_str("rgba(0,0,0,0.55)"));
        ctx.fill_rect(x, y - 18.0, pill_w, 17.0);
        ctx.set_fill_style(&JsValue::from_str(color));
        ctx.fill_text(&label, x + 3.0, y - 4.0).ok();
    }
}

/// Map the model's class index back to the die face label.
/// Training used: class 0-8 → faces "1"-"9", class 9 → "0", class 10-20 → "10"-"20"
#[cfg(target_arch = "wasm32")]
fn dice_value(class: u32) -> String {
    match class {
        0..=8 => (class + 1).to_string(),
        9 => "0".to_string(),
        10..=20 => class.to_string(),
        _ => "?".to_string(),
    }
}
