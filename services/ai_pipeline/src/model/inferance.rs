use std::path::Path;

use burn::prelude::Backend;
use burn::tensor::activation::softmax;
use burn::tensor::module::interpolate;
use burn::tensor::ops::{InterpolateMode, InterpolateOptions};
use burn::tensor::{Tensor, TensorData};
use burn_store::{BurnpackStore, ModuleSnapshot};

use crate::model::{my_model, DiceHead};

/// Compile-time path to the YOLO burnpack produced by build.rs.
const YOLO_BPK: &str = concat!(env!("OUT_DIR"), "/model/yolo26n.bpk");
/// YOLO model expects 640×640 input (Ultralytics default).
const YOLO_INPUT: usize = 640;
/// DiceHead was trained on 128×128 crops.
const HEAD_INPUT: usize = 128;
/// Default YOLO confidence threshold.
const DEFAULT_CONF: f32 = 0.25;

#[derive(Debug, Clone)]
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

impl<B: Backend> DicePipeline<B> {
    /// Load both models. `head_dir` is an experiment artifact directory
    /// (e.g. `art/experiment_32`) containing `model/model.bpk`.
    pub fn new(device: B::Device, head_dir: &Path) -> Self {
        Self::with_conf(device, head_dir, DEFAULT_CONF)
    }

    pub fn with_conf(device: B::Device, head_dir: &Path, conf_threshold: f32) -> Self {
        let yolo = my_model::Model::<B>::from_file(YOLO_BPK, &device);
        let mut head = DiceHead::<B>::new(&device);
        let mut store = BurnpackStore::from_file(head_dir.join("model/model"));
        head.load_from(&mut store).expect("DiceHead weights not found");
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

        // YOLO expects proper CHW input (PyTorch/Ultralytics convention).
        let yolo_tensor = frame_to_chw_tensor::<B>(rgb, width, height, &self.device);
        let yolo_resized = interpolate(
            yolo_tensor,
            [YOLO_INPUT, YOLO_INPUT],
            InterpolateOptions::new(InterpolateMode::Bilinear),
        );

        // Output shape: [1, 300, 6], each row = [x1, y1, x2, y2, conf, class_f32].
        // Coords are absolute pixels in the YOLO_INPUT × YOLO_INPUT space.
        let raw: Vec<f32> = self
            .yolo
            .forward(yolo_resized)
            .into_data()
            .convert::<f32>()
            .to_vec()
            .unwrap();

        let scale = YOLO_INPUT as f32;
        let mut detections = Vec::new();

        for row in raw.chunks_exact(6) {
            let conf = row[4];
            if conf < self.conf_threshold {
                continue;
            }

            let x1 = (row[0] / scale).clamp(0.0, 1.0);
            let y1 = (row[1] / scale).clamp(0.0, 1.0);
            let x2 = (row[2] / scale).clamp(0.0, 1.0);
            let y2 = (row[3] / scale).clamp(0.0, 1.0);

            if x2 <= x1 || y2 <= y1 {
                continue;
            }

            // Crop using image crate and resize with Lanczos3, matching dataset.rs.
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

/// HWC bytes → [1, 3, H, W] CHW tensor normalized to [0, 1].
/// Used for YOLO which was trained with standard PyTorch CHW convention.
fn frame_to_chw_tensor<B: Backend>(
    rgb: &[u8],
    width: usize,
    height: usize,
    device: &B::Device,
) -> Tensor<B, 4> {
    let mut data = Vec::with_capacity(3 * height * width);
    for c in 0..3usize {
        for h in 0..height {
            for w in 0..width {
                data.push(rgb[(h * width + w) * 3 + c] as f32 / 255.0);
            }
        }
    }
    Tensor::<B, 4>::from_data(TensorData::new(data, [1, 3, height, width]), device)
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
