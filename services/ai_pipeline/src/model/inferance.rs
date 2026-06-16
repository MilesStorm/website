use std::path::Path;

use burn::module::{Module, ModuleMapper, Param};
use burn::prelude::Backend;
use serde::Serialize;
use burn::tensor::activation::softmax;
use burn::tensor::{DType, Tensor, TensorData};
use burn_store::{BurnpackStore, ModuleSnapshot};

use crate::model::{my_model, DiceHead};

/// Compile-time fallback path to the YOLO burnpack produced by build.rs.
/// Override at runtime with the YOLO_MODEL_PATH env var (required in Docker).
const YOLO_BPK_DEFAULT: &str = concat!(env!("OUT_DIR"), "/model/yolo26s.bpk");

fn yolo_bpk_path() -> String {
    std::env::var("YOLO_MODEL_PATH").unwrap_or_else(|_| YOLO_BPK_DEFAULT.to_string())
}
/// YOLO model expects 640×640 input (Ultralytics default).
const YOLO_INPUT: usize = 640;
/// DiceHead was trained on 128×128 crops.
const HEAD_INPUT: usize = 128;
/// Default YOLO confidence threshold.
const DEFAULT_CONF: f32 = 0.25;

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
    /// DiceHead softmax probability for the winning class.
    pub dice_conf: f32,
}

pub struct DicePipeline<B: Backend> {
    yolo: my_model::Model<B>,
    head: DiceHead<B>,
    device: B::Device,
    conf_threshold: f32,
}

/// Widens every float parameter to f32. The head is trained in bf16
/// (`Autodiff<Cuda<bf16>>`) so its burnpack stores bf16 weights, but burn 0.21's
/// padded-conv path builds the zero-pad fill tensor at f32 and slice-assigns the
/// activation into it — a `DTypeMismatch` panic for any bf16 input. Casting the
/// loaded weights to f32 (lossless widening) makes the whole head run in f32, so
/// pad fill, input, and weights agree. Inference-only; training is untouched.
struct CastToF32;

impl<B: Backend> ModuleMapper<B> for CastToF32 {
    fn map_float<const D: usize>(&mut self, param: Param<Tensor<B, D>>) -> Param<Tensor<B, D>> {
        param.map(|tensor| tensor.cast(DType::F32))
    }
}

impl<B: Backend> DicePipeline<B> {
    /// Load both models. `head_dir` is an experiment artifact directory
    /// (e.g. `art/experiment_32`) containing `model/model.bpk`.
    pub fn new(device: B::Device, head_dir: &Path) -> Self {
        Self::with_conf(device, head_dir, DEFAULT_CONF)
    }

    pub fn with_conf(device: B::Device, head_dir: &Path, conf_threshold: f32) -> Self {
        let yolo = my_model::Model::<B>::from_file(&yolo_bpk_path(), &device);
        let mut head = DiceHead::<B>::new(&device);
        let mut store = BurnpackStore::from_file(head_dir.join("model/model"));
        head.load_from(&mut store).expect("DiceHead weights not found");
        // Loaded weights keep their stored bf16 dtype; widen to f32 for inference.
        let head = head.map(&mut CastToF32);
        Self { yolo, head, device, conf_threshold }
    }

    /// Run YOLO bbox detection then DiceHead classification on every detected die.
    ///
    /// `rgb` is packed R,G,B bytes in row-major (HWC) order — the format produced
    /// by most webcam APIs and image libraries. Length must equal `width * height * 3`.
    pub fn infer_frame(&self, rgb: &[u8], width: usize, height: usize) -> Vec<Detection> {
        assert_eq!(rgb.len(), width * height * 3, "rgb buffer length mismatch");

        // Wrap bytes once for per-detection cropping later.
        let img = image::RgbImage::from_raw(width as u32, height as u32, rgb.to_vec())
            .expect("rgb dimensions inconsistent with width/height");

        // Letterbox to 640×640 (aspect-preserving + 114 pad), matching the
        // Ultralytics preprocessing the model was trained/exported with. `lb`
        // carries the scale + padding needed to map boxes back to the frame.
        let (yolo_input, lb) = letterbox_to_chw::<B>(&img, &self.device);

        // Output shape: [1, 300, 6], each row = [x1, y1, x2, y2, conf, class_f32].
        // Coords are absolute pixels in the letterboxed YOLO_INPUT × YOLO_INPUT space.
        let raw: Vec<f32> = self
            .yolo
            .forward(yolo_input)
            .into_data()
            .convert::<f32>()
            .to_vec()
            .unwrap();

        let mut detections = Vec::new();

        for row in raw.chunks_exact(6) {
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

            // Crop using image crate and resize with Lanczos3, matching dataset.rs.
            // The head runs in f32 (see CastToF32), so the f32 crop feeds it directly.
            let crop = crop_for_head::<B>(&img, x1, y1, x2, y2, &self.device);
            let probs: Vec<f32> = softmax(self.head.forward(crop), 1)
                .into_data()
                .convert::<f32>()
                .to_vec()
                .unwrap();

            let (dice_class, dice_conf) = probs
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
                .map(|(i, &p)| (i as u32, p))
                .unwrap();

            detections.push(Detection {
                x1,
                y1,
                x2,
                y2,
                yolo_conf: conf,
                yolo_class: row[5] as u32,
                dice_class,
                dice_conf,
            });
        }

        detections
    }
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
/// 114-grey canvas (Ultralytics letterbox), returned as a [1, 3, 640, 640] CHW
/// tensor normalized to [0, 1] plus the `Letterbox` mapping back to the frame.
fn letterbox_to_chw<B: Backend>(
    img: &image::RgbImage,
    device: &B::Device,
) -> (Tensor<B, 4>, Letterbox) {
    let (w, h) = img.dimensions();
    let side = YOLO_INPUT as u32;
    let scale = (YOLO_INPUT as f32 / w as f32).min(YOLO_INPUT as f32 / h as f32);
    let nw = ((w as f32 * scale).round() as u32).clamp(1, side);
    let nh = ((h as f32 * scale).round() as u32).clamp(1, side);
    let pad_x = (side - nw) / 2;
    let pad_y = (side - nh) / 2;

    let resized =
        image::imageops::resize(img, nw, nh, image::imageops::FilterType::Triangle);
    let mut canvas = image::RgbImage::from_pixel(side, side, image::Rgb([114, 114, 114]));
    image::imageops::overlay(&mut canvas, &resized, pad_x as i64, pad_y as i64);

    // CHW, normalized to [0, 1].
    let mut data = Vec::with_capacity(3 * YOLO_INPUT * YOLO_INPUT);
    for c in 0..3usize {
        for y in 0..side {
            for x in 0..side {
                data.push(canvas.get_pixel(x, y)[c] as f32 / 255.0);
            }
        }
    }
    let tensor =
        Tensor::<B, 4>::from_data(TensorData::new(data, [1, 3, YOLO_INPUT, YOLO_INPUT]), device);

    (
        tensor,
        Letterbox {
            scale,
            pad_x: pad_x as f32,
            pad_y: pad_y as f32,
            w: w as f32,
            h: h as f32,
        },
    )
}

/// Crop a normalized bbox from an RgbImage, resize to 128×128 with Lanczos3, and
/// build a tensor using the same pixel-by-pixel R,G,B push as dataset.rs so that
/// the layout matches what DiceHead was trained on.
fn crop_for_head<B: Backend>(
    img: &image::RgbImage,
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    device: &B::Device,
) -> Tensor<B, 4> {
    let (w, h) = img.dimensions();
    let px0 = (x1 * w as f32) as u32;
    let py0 = (y1 * h as f32) as u32;
    let px1 = ((x2 * w as f32) as u32).min(w);
    let py1 = ((y2 * h as f32) as u32).min(h);
    let crop_w = (px1 - px0).max(1);
    let crop_h = (py1 - py0).max(1);

    let cropped = image::imageops::crop_imm(img, px0, py0, crop_w, crop_h).to_image();
    let resized = image::imageops::resize(
        &cropped,
        HEAD_INPUT as u32,
        HEAD_INPUT as u32,
        image::imageops::FilterType::Lanczos3,
    );

    let mut data = Vec::with_capacity(HEAD_INPUT * HEAD_INPUT * 3);
    for pixel in resized.pixels() {
        data.push(pixel[0] as f32 / 255.0);
        data.push(pixel[1] as f32 / 255.0);
        data.push(pixel[2] as f32 / 255.0);
    }
    Tensor::<B, 4>::from_data(
        TensorData::new(data, [1, 3, HEAD_INPUT, HEAD_INPUT]),
        device,
    )
}
