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
        // Older burnpacks store bf16; widen to f32 for inference (no-op for f32 ones).
        let head = head.map(&mut CastToF32);
        Self { yolo, head, device, conf_threshold }
    }

    /// Load only the YOLO detector (with a throwaway, randomly-initialized head)
    /// for offline crop export, where no classification is performed.
    pub fn yolo_only(device: B::Device, conf_threshold: f32) -> Self {
        let yolo = my_model::Model::<B>::from_file(&yolo_bpk_path(), &device);
        let head = DiceHead::<B>::new(&device);
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

        self.detect_boxes(&img)
            .into_iter()
            .map(|b| {
                // Crop via the shared margin-aware path, then classify. The head
                // runs in f32 (see CastToF32), so the f32 crop feeds it directly.
                let crop = crop_for_head::<B>(&img, b.x1, b.y1, b.x2, b.y2, &self.device);
                let probs: Vec<f32> = softmax(self.head.forward(crop), 1)
                    .into_data()
                    .convert::<f32>()
                    .to_vec()
                    .unwrap();

                let (dice_class, dice_conf) = probs
                    .iter()
                    .enumerate()
                    .max_by(|a, c| a.1.partial_cmp(c.1).unwrap())
                    .map(|(i, &p)| (i as u32, p))
                    .unwrap();

                Detection {
                    x1: b.x1,
                    y1: b.y1,
                    x2: b.x2,
                    y2: b.y2,
                    yolo_conf: b.conf,
                    yolo_class: b.class,
                    dice_class,
                    dice_conf,
                }
            })
            .collect()
    }

    /// Run only the YOLO detector on a frame and return accepted boxes in
    /// original-frame normalized coords. Shared by `infer_frame` and the offline
    /// crop exporter so both consume identical detector geometry.
    pub fn detect_boxes(&self, img: &image::RgbImage) -> Vec<BoxDet> {
        // Letterbox to 640×640 (aspect-preserving + 114 pad), matching the
        // Ultralytics preprocessing the model was trained/exported with. `lb`
        // carries the scale + padding needed to map boxes back to the frame.
        let (yolo_input, lb) = letterbox_to_chw::<B>(img, &self.device);

        // Output shape: [1, 300, 6], each row = [x1, y1, x2, y2, conf, class_f32].
        // Coords are absolute pixels in the letterboxed YOLO_INPUT × YOLO_INPUT space.
        let raw: Vec<f32> = self
            .yolo
            .forward(yolo_input)
            .into_data()
            .convert::<f32>()
            .to_vec()
            .unwrap();

        let mut boxes = Vec::new();
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
            boxes.push(BoxDet { x1, y1, x2, y2, conf, class: row[5] as u32 });
        }
        boxes
    }
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

/// Crop a normalized bbox from an RgbImage and build the head's input tensor,
/// using the shared `crop_region_to_image` path plus the same pixel-by-pixel
/// R,G,B normalization as dataset.rs so the layout matches training.
fn crop_for_head<B: Backend>(
    img: &image::RgbImage,
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    device: &B::Device,
) -> Tensor<B, 4> {
    let resized = crop_region_to_image(img, x1, y1, x2, y2);

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
