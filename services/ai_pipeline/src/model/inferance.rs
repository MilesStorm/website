use std::path::Path;
use std::sync::Mutex;
use std::time::Instant;

use burn::module::{ModuleMapper, Param};
use burn::prelude::Backend;
use serde::Serialize;
use burn::tensor::activation::softmax;
use burn::tensor::{DType, Tensor, TensorData};

use crate::model::resnet::ResNet18Head;

/// The YOLO model in the source tree, for runs outside Docker. The image sets
/// YOLO_ONNX_PATH to its copy.
const YOLO_ONNX_DEFAULT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/model/yolo26s.onnx");

fn yolo_onnx_path() -> String {
    std::env::var("YOLO_ONNX_PATH").unwrap_or_else(|_| YOLO_ONNX_DEFAULT.to_string())
}

/// The half-precision copy of the model (`tools/yolo_fp16.py`; the image builds it). Its
/// input and output stay f32, so nothing else changes. None outside the image.
fn yolo_fp16_onnx_path() -> Option<String> {
    std::env::var("YOLO_FP16_ONNX_PATH").ok()
}

/// The model name of the GPU this container was given, e.g. "NVIDIA GeForce RTX 2060
/// SUPER". Kubernetes hands the pod either of the node's cards, so it is read at start:
/// the NVIDIA runtime names the card's UUID in NVIDIA_VISIBLE_DEVICES, and the driver
/// lists every card in /proc. Reading /proc opens no GPU device.
fn gpu_model() -> Option<String> {
    let uuid = std::env::var("NVIDIA_VISIBLE_DEVICES").ok()?;
    let cards = std::fs::read_dir("/proc/driver/nvidia/gpus").ok()?;
    cards
        .flatten()
        .filter_map(|card| std::fs::read_to_string(card.path().join("information")).ok())
        .find_map(|information| model_of(&information, &uuid))
}

/// The `Model:` of a card's /proc `information`, if the card's `GPU UUID:` is `uuid`.
fn model_of(information: &str, uuid: &str) -> Option<String> {
    let field = |name: &str| {
        information.lines().find_map(|line| line.strip_prefix(name)).map(|value| value.trim().to_string())
    };
    (field("GPU UUID:")? == uuid).then(|| field("Model:")).flatten()
}

/// Whether half precision pays on this card. RTX cards have tensor cores, which are
/// built for half-precision convolutions. GTX 16-series cards have none, and are widely
/// reported to return NaN from half-precision convolutions, so they and any card not
/// recognized keep the f32 model.
fn has_tensor_cores(model: &str) -> bool {
    model.contains("RTX")
}

/// The YOLO detector, run by ONNX Runtime on the GPU (its CUDA provider uses cuDNN).
/// burn's CUDA backend ran the same model in ~100 ms a frame on a GTX 1660 SUPER
/// (production 2026-10-11): it has a fixed cost per layer, and YOLO has hundreds.
///
/// ONNX Runtime is loaded at run time from the library ORT_DYLIB_PATH names (the image
/// ships Microsoft's CUDA 12 build). It runs on the GPU or not at all: `load` fails if
/// the CUDA provider can't load. Only the test build can run it on the CPU.
struct Yolo {
    /// `run` needs `&mut`; one thread infers, so the lock is never contended.
    session: Mutex<ort::session::Session>,
}

impl Yolo {
    fn load() -> anyhow::Result<Self> {
        Self::load_on(true)
    }

    fn load_on(gpu: bool) -> anyhow::Result<Self> {
        let card = if gpu { gpu_model() } else { None };
        let fp16 = yolo_fp16_onnx_path().filter(|_| card.as_deref().is_some_and(has_tensor_cores));
        let precision = if fp16.is_some() { "fp16" } else { "fp32" };
        let path = fp16.unwrap_or_else(yolo_onnx_path);
        tracing::info!(gpu = card.as_deref().unwrap_or("unknown"), precision, model = %path, "loading YOLO");
        let context = |what: &str, e: &dyn std::fmt::Display| anyhow::anyhow!("YOLO ({path}): {what}: {e}");
        let mut builder = ort::session::Session::builder().map_err(|e| context("onnxruntime", &e))?;
        // Outside the test build the GPU provider is always required.
        if gpu || !cfg!(test) {
            let cuda = ort::ep::CUDA::default()
                // Take what a run needs instead of doubling: the card is shared.
                .with_arena_extend_strategy(ort::ep::ArenaExtendStrategy::SameAsRequested)
                .build()
                .error_on_failure();
            builder = builder.with_execution_providers([cuda]).map_err(|e| context("CUDA provider", &e))?;
        }
        let session = builder.commit_from_file(&path).map_err(|e| context("loading the model", &e))?;
        Ok(Self { session: Mutex::new(session) })
    }

    /// `input` is the letterboxed frame, [1, 3, 640, 640]. Returns the [1, 300, 6] output:
    /// each row = [x1, y1, x2, y2, conf, class], in letterboxed pixels.
    fn forward(&self, input: Vec<f32>) -> Vec<f32> {
        let input = ort::value::Tensor::from_array(([1usize, 3, YOLO_INPUT, YOLO_INPUT], input))
            .expect("the letterboxed frame has the model's input shape");
        let mut session = self.session.lock().unwrap();
        let outputs = session.run(ort::inputs![input]).expect("YOLO forward pass");
        let (_, raw) = outputs[0].try_extract_tensor::<f32>().expect("YOLO's output is f32");
        raw.to_vec()
    }
}
/// YOLO model expects 640×640 input (Ultralytics default).
const YOLO_INPUT: usize = 640;
/// DiceHead input crop size. The head is a compact glyph recognizer that only
/// sees the tight top-number crop, so 64×64 is plenty (and ~4× cheaper than the
/// old 128). MUST match `HEAD_INPUT`/`TARGET_SIZE` everywhere the head's crops
/// are produced (dataset loader + crop exporter) so train == serve.
pub const HEAD_INPUT: usize = 64;
/// Symmetric margin added around the YOLO number box before cropping, as a
/// fraction of box size per side. Gives the head a little surrounding context
/// and absorbs YOLO localization error. Applied identically at serve time and in
/// the offline crop exporter, so the head trains on exactly what it is served.
pub const HEAD_CROP_MARGIN: f32 = 0.12;
/// Head batch sizes `warm_up` runs: one die, and a handful rolled together.
const WARM_UP_DICE: [usize; 2] = [1, 6];
/// Default YOLO confidence threshold.
const DEFAULT_CONF: f32 = 0.25;
/// Default minimum head probability for a value to count as read. Below it the
/// pipeline reports the die as unreadable instead of guessing. 0.70 was ~97.4%
/// correct while answering ~93% of crops in the out-of-fold test (ROADMAP.md).
pub const DEFAULT_DICE_THRESHOLD: f32 = 0.70;

/// Head class index -> printed face value ("1".."20"; "0" is the d10 zero).
/// Order matches obj.names: 1..9, 0, 10..20.
pub fn class_value(class: usize) -> &'static str {
    const VALUES: [&str; 21] = [
        "1", "2", "3", "4", "5", "6", "7", "8", "9", "0", "10", "11", "12", "13", "14", "15",
        "16", "17", "18", "19", "20",
    ];
    VALUES.get(class).copied().unwrap_or("?")
}

#[derive(Debug, Clone, Serialize)]
pub struct Detection {
    /// Bounding box in normalized [0, 1] coordinates within the input frame.
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
    /// YOLO max-class confidence for this detection.
    pub yolo_conf: f32,
    /// YOLO class index (COCO 0-based, or fine-tuned class).
    pub yolo_class: u32,
    /// DiceHead predicted class index (0..=20, matching obj.names ordering).
    pub dice_class: u32,
    /// Head softmax probability for the winning class.
    pub dice_conf: f32,
    /// Printed face value of `dice_class` ("0" is the d10 zero).
    pub value: &'static str,
    /// `dice_conf >= threshold`: when false, clients must not treat `value` as read.
    pub confident: bool,
    /// Full softmax over the 21 classes, used for multi-frame voting.
    #[serde(skip)]
    pub probs: Vec<f32>,
}

/// When each stage of `infer_frame` finished, for the frame's trace (`unit_trace.rs`).
/// Each is read after the stage's GPU work was done: ONNX Runtime returns YOLO's output
/// when it has it, and the head's `into_data` reads its result back (burn's CUDA backend
/// queues operations, and a clock read before the sync would charge them to the next
/// stage). Cropping has no sync of its own: it is CPU work, and the crop tensors it
/// queues are uploaded as part of the head's pass.
#[derive(Debug, Clone, Copy)]
pub struct StageEnds {
    /// The frame resized and letterboxed into YOLO's input (CPU).
    pub prepare: Instant,
    /// YOLO forward pass (upload, GPU, read-back) and duplicate suppression.
    pub yolo: Instant,
    /// Crop and head are None when YOLO found no dice: nothing was cropped or classified.
    pub crop: Option<Instant>,
    pub head: Option<Instant>,
}

pub struct DicePipeline<B: Backend> {
    yolo: Yolo,
    /// None for `yolo_only` (crop export), which never classifies.
    head: Option<ResNet18Head<B>>,
    device: B::Device,
    conf_threshold: f32,
    dice_threshold: f32,
}

/// Widens every float parameter to f32. Heads trained before the switch to f32
/// training (`Autodiff<Cuda<bf16>>`) store bf16 weights in their burnpack, and burn 0.21's
/// padded-conv path builds the zero-pad fill tensor at f32 and slice-assigns the
/// activation into it — a `DTypeMismatch` panic for any bf16 input. Casting the
/// loaded weights to f32 (lossless widening) makes the whole head run in f32, so
/// pad fill, input, and weights agree. Inference-only; training is untouched.
pub(crate) struct CastToF32;

impl<B: Backend> ModuleMapper<B> for CastToF32 {
    fn map_float<const D: usize>(&mut self, param: Param<Tensor<B, D>>) -> Param<Tensor<B, D>> {
        param.map(|tensor| tensor.cast(DType::F32))
    }
}

impl<B: Backend> DicePipeline<B> {
    /// Load YOLO and the ResNet18 head (`head_path`: safetensors from
    /// `tools/train_head_torch.py --all`). Fails if the weights don't load completely.
    pub fn new(device: B::Device, head_path: &Path, dice_threshold: f32) -> anyhow::Result<Self> {
        let yolo = Yolo::load()?;
        let head = ResNet18Head::<B>::load(head_path, &device)?;
        Ok(Self { yolo, head: Some(head), device, conf_threshold: DEFAULT_CONF, dice_threshold })
    }

    /// Load only the YOLO detector for offline crop export, where no classification is performed.
    pub fn yolo_only(device: B::Device, conf_threshold: f32) -> Self {
        let yolo = Yolo::load().unwrap_or_else(|e| panic!("{e:#}"));
        Self { yolo, head: None, device, conf_threshold, dice_threshold: DEFAULT_DICE_THRESHOLD }
    }

    /// Runs both models once on a blank frame, as `infer_frame` would for `WARM_UP_DICE`
    /// dice. The first pass is slow in both: cuDNN times its convolution algorithms for
    /// YOLO, and burn's CUDA backend compiles each of the head's kernels on first use (3 s,
    /// production 2026-10-11). Without this the first camera frame waited for that.
    pub fn warm_up(&self) {
        let side = YOLO_INPUT as u32;
        let blank = image::RgbImage::from_pixel(side, side, image::Rgb([114, 114, 114]));
        self.detect_boxes(&blank);
        let Some(head) = &self.head else { return };
        for dice in WARM_UP_DICE {
            let crops: Vec<Tensor<B, 4>> =
                (0..dice).map(|_| crop_for_head::<B>(&blank, 0.4, 0.4, 0.6, 0.6, &self.device)).collect();
            softmax(head.forward(Tensor::cat(crops, 0)), 1).into_data();
        }
    }

    /// Run YOLO bbox detection then classify every detected die (one batched head pass).
    ///
    /// `rgb` is packed R,G,B bytes in row-major (HWC) order — the format produced
    /// by most webcam APIs and image libraries. Length must equal `width * height * 3`.
    /// Also returns when each stage finished.
    pub fn infer_frame(&self, rgb: &[u8], width: usize, height: usize) -> (Vec<Detection>, StageEnds) {
        assert_eq!(rgb.len(), width * height * 3, "rgb buffer length mismatch");
        let head = self.head.as_ref().expect("infer_frame needs a head (not yolo_only)");

        // Wrap bytes once for per-detection cropping later.
        let img = image::RgbImage::from_raw(width as u32, height as u32, rgb.to_vec())
            .expect("rgb dimensions inconsistent with width/height");

        let (boxes, prepare_end) = self.detect_boxes_timed(&img);
        let yolo_end = Instant::now();
        if boxes.is_empty() {
            return (Vec::new(), StageEnds { prepare: prepare_end, yolo: yolo_end, crop: None, head: None });
        }
        // Crop via the shared margin-aware path (train == serve), then classify all at once.
        let crops: Vec<Tensor<B, 4>> = boxes
            .iter()
            .map(|b| crop_for_head::<B>(&img, b.x1, b.y1, b.x2, b.y2, &self.device))
            .collect();
        let crop_end = Instant::now();
        let probs: Vec<f32> = softmax(head.forward(Tensor::cat(crops, 0)), 1)
            .into_data()
            .convert::<f32>()
            .to_vec()
            .unwrap();
        let head_end = Instant::now();

        let detections = boxes
            .iter()
            .zip(probs.as_chunks::<{ crate::model::head::NUM_CLASSES }>().0)
            .map(|(b, p)| {
                let (class, conf) = p
                    .iter()
                    .enumerate()
                    .max_by(|a, c| a.1.total_cmp(c.1))
                    .map(|(i, &p)| (i, p))
                    .unwrap();
                Detection {
                    x1: b.x1,
                    y1: b.y1,
                    x2: b.x2,
                    y2: b.y2,
                    yolo_conf: b.conf,
                    yolo_class: b.class,
                    dice_class: class as u32,
                    dice_conf: conf,
                    value: class_value(class),
                    confident: conf >= self.dice_threshold,
                    probs: p.to_vec(),
                }
            })
            .collect();
        let stages =
            StageEnds { prepare: prepare_end, yolo: yolo_end, crop: Some(crop_end), head: Some(head_end) };
        (detections, stages)
    }

    /// Run only the YOLO detector on a frame and return accepted boxes in
    /// original-frame normalized coords. Shared by `infer_frame` and the offline
    /// crop exporter so both consume identical detector geometry.
    pub fn detect_boxes(&self, img: &image::RgbImage) -> Vec<BoxDet> {
        self.detect_boxes_timed(img).0
    }

    /// `detect_boxes`, and when the frame's preparation ended (the forward pass began).
    fn detect_boxes_timed(&self, img: &image::RgbImage) -> (Vec<BoxDet>, Instant) {
        // Letterbox to 640×640 (aspect-preserving + 114 pad), matching the
        // Ultralytics preprocessing the model was trained/exported with. `lb`
        // carries the scale + padding needed to map boxes back to the frame.
        let (yolo_input, lb) = letterbox_to_chw(img);
        let prepare_end = Instant::now();

        // Output shape: [1, 300, 6], each row = [x1, y1, x2, y2, conf, class_f32].
        // Coords are absolute pixels in the letterboxed YOLO_INPUT × YOLO_INPUT space.
        let raw = self.yolo.forward(yolo_input);

        let mut boxes = Vec::new();
        for row in raw.as_chunks::<6>().0 {
            let conf = row[4];
            if conf < self.conf_threshold {
                continue;
            }
            // Undo letterbox: pixel→original-frame normalized coords, clamped.
            let x1 = lb.unpad_x(row[0]);
            let y1 = lb.unpad_y(row[1]);
            let x2 = lb.unpad_x(row[2]);
            let y2 = lb.unpad_y(row[3]);
            if x2 <= x1 || y2 <= y1 {
                continue;
            }
            boxes.push(BoxDet { x1, y1, x2, y2, conf, class: row[5] as u32 });
        }
        (suppress_duplicates(boxes), prepare_end)
    }
}

/// Boxes overlapping a more confident box by more than this IoU are the same die.
const DUPLICATE_IOU: f32 = 0.5;

/// Class-agnostic non-maximum suppression. yolo26 is exported "end-to-end"
/// (NMS-free), but it still emits near-identical boxes for one die now and then,
/// which would count that die twice in a roll.
fn suppress_duplicates(mut boxes: Vec<BoxDet>) -> Vec<BoxDet> {
    let iou = |a: &BoxDet, b: &BoxDet| {
        let iw = (a.x2.min(b.x2) - a.x1.max(b.x1)).max(0.0);
        let ih = (a.y2.min(b.y2) - a.y1.max(b.y1)).max(0.0);
        let inter = iw * ih;
        let union = (a.x2 - a.x1) * (a.y2 - a.y1) + (b.x2 - b.x1) * (b.y2 - b.y1) - inter;
        if union > 0.0 { inter / union } else { 0.0 }
    };
    boxes.sort_by(|a, b| b.conf.total_cmp(&a.conf));
    let mut kept: Vec<BoxDet> = Vec::with_capacity(boxes.len());
    for b in boxes {
        if kept.iter().all(|k| iou(k, &b) <= DUPLICATE_IOU) {
            kept.push(b);
        }
    }
    kept
}

/// A single accepted YOLO detection in original-frame normalized coords.
#[derive(Debug, Clone, Copy)]
pub struct BoxDet {
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
    pub conf: f32,
    pub class: u32,
}

/// Letterbox parameters for mapping YOLO outputs back to original-frame coords.
/// The model sees a 640×640 image built by scaling the frame by `scale` (uniform,
/// aspect-preserving) and centering it with `pad_x`/`pad_y` pixels of 114-grey
/// border. `w`/`h` are the original frame dimensions.
struct Letterbox {
    scale: f32,
    pad_x: f32,
    pad_y: f32,
    w: f32,
    h: f32,
}

impl Letterbox {
    /// 640px x-coordinate → original-frame normalized x in [0, 1].
    fn unpad_x(&self, px: f32) -> f32 {
        (((px - self.pad_x) / self.scale) / self.w).clamp(0.0, 1.0)
    }
    /// 640px y-coordinate → original-frame normalized y in [0, 1].
    fn unpad_y(&self, py: f32) -> f32 {
        (((py - self.pad_y) / self.scale) / self.h).clamp(0.0, 1.0)
    }
}

/// Aspect-preserving resize of `img` to fit YOLO_INPUT×YOLO_INPUT, centered on a
/// 114-grey canvas (Ultralytics letterbox), returned as [1, 3, 640, 640] CHW values
/// normalized to [0, 1] plus the `Letterbox` mapping back to the frame.
fn letterbox_to_chw(img: &image::RgbImage) -> (Vec<f32>, Letterbox) {
    let (w, h) = img.dimensions();
    let side = YOLO_INPUT as u32;
    let scale = (YOLO_INPUT as f32 / w as f32).min(YOLO_INPUT as f32 / h as f32);
    let nw = ((w as f32 * scale).round() as u32).clamp(1, side);
    let nh = ((h as f32 * scale).round() as u32).clamp(1, side);
    let pad_x = (side - nw) / 2;
    let pad_y = (side - nh) / 2;

    let resized =
        image::imageops::resize(img, nw, nh, image::imageops::FilterType::Triangle);

    // The grey canvas, then the resized frame's rows written straight into each plane.
    let plane = YOLO_INPUT * YOLO_INPUT;
    let mut data = vec![114.0 / 255.0; 3 * plane];
    let (red, rest) = data.split_at_mut(plane);
    let (green, blue) = rest.split_at_mut(plane);
    for (y, row) in resized.as_raw().chunks_exact(nw as usize * 3).enumerate() {
        let at = (pad_y as usize + y) * YOLO_INPUT + pad_x as usize;
        for (x, px) in row.as_chunks::<3>().0.iter().enumerate() {
            red[at + x] = px[0] as f32 / 255.0;
            green[at + x] = px[1] as f32 / 255.0;
            blue[at + x] = px[2] as f32 / 255.0;
        }
    }

    (
        data,
        Letterbox {
            scale,
            pad_x: pad_x as f32,
            pad_y: pad_y as f32,
            w: w as f32,
            h: h as f32,
        },
    )
}

/// Crop the (margin-expanded) number box from an RgbImage and resize to
/// HEAD_INPUT² with Lanczos3, returning the head's input image. This is the
/// single source of truth for the head's preprocessing: `crop_for_head` packs
/// its output into a tensor at serve time, and the offline exporter saves its
/// output to disk for training, so train == serve by construction.
pub fn crop_region_to_image(
    img: &image::RgbImage,
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
) -> image::RgbImage {
    let (w, h) = img.dimensions();

    // Expand the box by HEAD_CROP_MARGIN of its size on each side, then clamp.
    let bw = (x2 - x1).max(0.0);
    let bh = (y2 - y1).max(0.0);
    let mx = bw * HEAD_CROP_MARGIN;
    let my = bh * HEAD_CROP_MARGIN;
    let ex1 = (x1 - mx).clamp(0.0, 1.0);
    let ey1 = (y1 - my).clamp(0.0, 1.0);
    let ex2 = (x2 + mx).clamp(0.0, 1.0);
    let ey2 = (y2 + my).clamp(0.0, 1.0);

    let px0 = (ex1 * w as f32) as u32;
    let py0 = (ey1 * h as f32) as u32;
    let px1 = ((ex2 * w as f32) as u32).min(w);
    let py1 = ((ey2 * h as f32) as u32).min(h);
    let crop_w = px1.saturating_sub(px0).max(1);
    let crop_h = py1.saturating_sub(py0).max(1);

    let cropped = image::imageops::crop_imm(img, px0, py0, crop_w, crop_h).to_image();
    image::imageops::resize(
        &cropped,
        HEAD_INPUT as u32,
        HEAD_INPUT as u32,
        image::imageops::FilterType::Lanczos3,
    )
}

/// Pack an RGB image into planar CHW order (all R, then all G, then all B),
/// scaled to [0, 1]: the layout `Tensor<B, 4>` of shape [N, 3, H, W] expects and
/// the layout PyTorch trains on. (Interleaved R,G,B pixel order would scramble
/// the image: the old burn head was trained and served that way.)
pub fn pack_chw(img: &image::RgbImage) -> Vec<f32> {
    let (w, h) = img.dimensions();
    let plane = (w * h) as usize;
    let mut data = vec![0f32; 3 * plane];
    for (i, pixel) in img.pixels().enumerate() {
        for c in 0..3 {
            data[c * plane + i] = pixel[c] as f32 / 255.0;
        }
    }
    data
}

/// Crop a normalized bbox from an RgbImage and build the head's input tensor
/// through the shared `crop_region_to_image` path and planar `pack_chw` packing.
fn crop_for_head<B: Backend>(
    img: &image::RgbImage,
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    device: &B::Device,
) -> Tensor<B, 4> {
    let resized = crop_region_to_image(img, x1, y1, x2, y2);
    Tensor::<B, 4>::from_data(
        TensorData::new(pack_chw(&resized), [1, 3, HEAD_INPUT, HEAD_INPUT]),
        device,
    )
}

/// Offline crop exporter: build the head's training set from the labeled
/// `dice_face` photos so that train == serve.
///
/// For every image under `src_root/<value>/`, run the real YOLO detector, take
/// the single highest-confidence number box (each photo holds exactly one die),
/// crop it through the shared `crop_region_to_image` path, and write the result
/// to `dst_root/<value>/<stem>.png`. The value folder name is preserved verbatim
/// so the existing folder-name → label mapping still applies.
///
/// Images where YOLO finds no box above `conf` are dropped (and counted); they
/// would have no usable crop at serve time either.
pub fn export_yolo_crops<B: Backend>(
    device: B::Device,
    src_root: &Path,
    dst_root: &Path,
    conf: f32,
) {
    let pipe = DicePipeline::<B>::yolo_only(device, conf);

    let exts = ["jpg", "jpeg", "png", "webp", "JPG"];
    let is_img = |p: &Path| {
        p.extension()
            .and_then(|e| e.to_str())
            .map(|e| exts.contains(&e))
            .unwrap_or(false)
    };

    let mut class_dirs: Vec<std::path::PathBuf> = std::fs::read_dir(src_root)
        .unwrap_or_else(|e| panic!("cannot read src_root {src_root:?}: {e}"))
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    class_dirs.sort();

    let (mut total_kept, mut total_dropped) = (0usize, 0usize);

    for class_dir in class_dirs {
        let value = class_dir.file_name().unwrap().to_string_lossy().to_string();
        let out_dir = dst_root.join(&value);
        std::fs::create_dir_all(&out_dir).expect("create out dir");

        let (mut kept, mut dropped) = (0usize, 0usize);
        let images: Vec<std::path::PathBuf> = std::fs::read_dir(&class_dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| is_img(p))
            .collect();

        for path in images {
            let img = match image::open(&path) {
                Ok(i) => i.to_rgb8(),
                Err(e) => {
                    eprintln!("  skip unreadable {path:?}: {e}");
                    dropped += 1;
                    continue;
                }
            };

            let boxes = pipe.detect_boxes(&img);
            // Exactly one die per photo: keep the highest-confidence box.
            let Some(best) = boxes
                .into_iter()
                .max_by(|a, b| a.conf.partial_cmp(&b.conf).unwrap())
            else {
                dropped += 1;
                continue;
            };

            let crop = crop_region_to_image(&img, best.x1, best.y1, best.x2, best.y2);
            let stem = path.file_stem().unwrap().to_string_lossy();
            crop.save(out_dir.join(format!("{stem}.png")))
                .expect("save crop");
            kept += 1;
        }

        println!("  {value:>4}: kept {kept}, dropped {dropped}");
        total_kept += kept;
        total_dropped += dropped;
    }

    println!(
        "\nexport complete: kept {total_kept}, dropped {total_dropped} (no box >= {conf}) -> {}",
        dst_root.display()
    );
}

#[cfg(test)]
mod tests {
    use super::{BoxDet, YOLO_INPUT, Yolo, has_tensor_cores, letterbox_to_chw, model_of, suppress_duplicates};

    /// eta's two cards, as the driver lists them in /proc/driver/nvidia/gpus/*/information.
    const RTX_2060: &str = "Model: \t\t NVIDIA GeForce RTX 2060 SUPER\nIRQ:   \t\t 136\nGPU UUID: \t GPU-4061671a-fa52-81d1-d007-ce786b0e13de\nVideo BIOS: \t 90.06.44.80.56\n";
    const GTX_1660: &str = "Model: \t\t NVIDIA GeForce GTX 1660 SUPER\nIRQ:   \t\t 137\nGPU UUID: \t GPU-5520ea75-84bd-7884-58cc-96bf86457f88\n";

    #[test]
    fn the_card_the_container_was_given_decides_the_precision() {
        let model = |uuid| [RTX_2060, GTX_1660].iter().find_map(|information| model_of(information, uuid));

        let rtx = model("GPU-4061671a-fa52-81d1-d007-ce786b0e13de").unwrap();
        assert_eq!(rtx, "NVIDIA GeForce RTX 2060 SUPER");
        assert!(has_tensor_cores(&rtx));

        let gtx = model("GPU-5520ea75-84bd-7884-58cc-96bf86457f88").unwrap();
        assert_eq!(gtx, "NVIDIA GeForce GTX 1660 SUPER");
        assert!(!has_tensor_cores(&gtx));

        // No UUID to match ("all", an index): the card is unknown, and stays on f32.
        assert_eq!(model("all"), None);
    }

    #[test]
    fn a_wide_frame_is_letterboxed_between_grey_bars() {
        let frame = image::RgbImage::from_pixel(1280, 720, image::Rgb([255, 0, 51]));
        let (data, lb) = letterbox_to_chw(&frame);
        assert_eq!(data.len(), 3 * YOLO_INPUT * YOLO_INPUT);
        assert_eq!((lb.scale, lb.pad_x, lb.pad_y), (0.5, 0.0, 140.0));

        let at = |plane: usize, y: usize, x: usize| data[plane * YOLO_INPUT * YOLO_INPUT + y * YOLO_INPUT + x];
        let grey = 114.0 / 255.0;
        // 360 rows of frame, with 140 grey rows above and below.
        for (y, pixel) in [(0, [grey; 3]), (139, [grey; 3]), (140, [1.0, 0.0, 0.2]), (499, [1.0, 0.0, 0.2]), (500, [grey; 3])] {
            for x in [0, 320, 639] {
                assert_eq!([at(0, y, x), at(1, y, x), at(2, y, x)], pixel, "row {y}, column {x}");
            }
        }
    }

    /// The real model in ONNX Runtime, on the CPU. CI runs it (checks.yml downloads the
    /// library): `ORT_DYLIB_PATH=<libonnxruntime.so> cargo test -- --ignored yolo`.
    #[test]
    #[ignore = "needs ONNX Runtime: set ORT_DYLIB_PATH"]
    fn yolo_runs_in_onnx_runtime_and_finds_nothing_in_a_blank_frame() {
        let yolo = Yolo::load_on(false).unwrap();
        let blank = image::RgbImage::from_pixel(640, 480, image::Rgb([114, 114, 114]));
        let raw = yolo.forward(letterbox_to_chw(&blank).0);
        assert_eq!(raw.len(), 300 * 6);
        let most_confident = raw.as_chunks::<6>().0.iter().map(|row| row[4]).fold(0.0, f32::max);
        assert!(most_confident < super::DEFAULT_CONF, "a die at {most_confident} in a blank frame");
    }

    fn b(x1: f32, conf: f32) -> BoxDet {
        BoxDet { x1, y1: 0.4, x2: x1 + 0.06, y2: 0.55, conf, class: 0 }
    }

    #[test]
    fn duplicate_boxes_for_one_die_collapse_to_the_most_confident() {
        let kept = suppress_duplicates(vec![b(0.2369, 0.80), b(0.2367, 0.90), b(0.69, 0.95)]);
        assert_eq!(kept.len(), 2);
        assert!(kept.iter().any(|k| (k.conf - 0.90).abs() < 1e-6));
    }

    #[test]
    fn neighbouring_dice_are_kept() {
        assert_eq!(suppress_duplicates(vec![b(0.20, 0.9), b(0.27, 0.9)]).len(), 2);
    }
}
