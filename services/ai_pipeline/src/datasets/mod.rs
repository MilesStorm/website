use burn::tensor::grid::affine_grid_2d;
use burn::tensor::module::interpolate;
use burn::tensor::ops::{GridSampleOptions, GridSamplePaddingMode, InterpolateMode};
use burn::tensor::Distribution;
use rand::RngExt;

use burn::data::dataloader::batcher::Batcher;

use burn::tensor::ops::InterpolateOptions;
use burn::{
    Tensor,
    prelude::Backend,
    tensor::{Int, TensorData},
};

use crate::datasets::dataset::{Annotation, Sample};

pub mod dataset;

#[derive(Clone, Debug)]
pub struct DiceBatch<B: Backend> {
    pub images: Tensor<B, 4>,
    pub targets: Tensor<B, 1, Int>,
}

pub struct DiceBatcher<B: Backend> {
    device: B::Device,
    /// When set, each image is freshly augmented as it is batched. Because the
    /// dataloader re-runs the batcher every epoch, this yields different crops
    /// each epoch — far stronger regularization than baking one augmented copy
    /// up front. Off for validation/eval.
    augment: bool,
}

impl<B: Backend> DiceBatcher<B> {
    /// Non-augmenting batcher (validation / eval).
    pub fn new(device: B::Device) -> Self {
        Self { device, augment: false }
    }

    /// Augmenting batcher (training): applies `augment_crop` per image, fresh
    /// every epoch.
    pub fn augmenting(device: B::Device) -> Self {
        Self { device, augment: true }
    }
}

impl<B: Backend> Batcher<B, DiceBatch<B>, DiceBatch<B>> for DiceBatcher<B> {
    fn batch(&self, items: Vec<DiceBatch<B>>, _device: &B::Device) -> DiceBatch<B> {
        if items.is_empty() {
            panic!("Cannot batch empty items");
        }

        let images: Vec<Tensor<B, 4>> = items
            .iter()
            .map(|b| {
                if self.augment {
                    augment_crop(b.images.clone(), &self.device)
                } else {
                    b.images.clone()
                }
            })
            .collect();
        let images = Tensor::cat(images, 0);

        let targets: Vec<Tensor<B, 1, Int>> = items.iter().map(|b| b.targets.clone()).collect();
        let targets = Tensor::cat(targets, 0);

        DiceBatch { images, targets }
    }
}

/// Build one DiceBatch (size 1) per annotation, optionally appending
/// `augment_factor` additional augmented copies of each crop. Augmentation
/// runs on the main thread so that worker threads only need thread-safe
/// `Tensor::cat`.
pub fn prepare_crops<B: Backend>(
    samples: &[Sample<B>],
    device: &B::Device,
    augment_factor: usize,
) -> Vec<DiceBatch<B>> {
    let mut batches = Vec::new();

    for sample in samples {
        for ann in &sample.annotations {
            let crop = crop_dice(
                sample.image.clone(),
                ann,
                [crate::model::inferance::HEAD_INPUT, crate::model::inferance::HEAD_INPUT],
                device,
            );

            let target_data = TensorData::new(vec![ann.class as i64], vec![1]);
            let target = Tensor::<B, 1, Int>::from_data(target_data, device);

            batches.push(DiceBatch {
                images: crop.clone(),
                targets: target.clone(),
            });

            for _ in 0..augment_factor {
                let aug = augment_crop(crop.clone(), device);
                batches.push(DiceBatch {
                    images: aug,
                    targets: target.clone(),
                });
            }
        }
    }

    batches
}

pub fn crop_dice<B: Backend>(
    image: Tensor<B, 4>,
    ann: &Annotation,
    target_size: [usize; 2],
    device: &B::Device,
) -> Tensor<B, 4> {
    let [_, _, h, w] = image.dims();

    let cx = (ann.x * w as f32) as usize;
    let cy = (ann.y * h as f32) as usize;
    let bw = (ann.w * w as f32) as usize;
    let hh = ann.h;
    let bh = (hh * h as f32) as usize;

    let x0 = cx.saturating_sub(bw / 2).min(w.saturating_sub(1));
    let y0 = cy.saturating_sub(bh / 2).min(h.saturating_sub(1));
    let x1 = (cx + bw / 2).min(w);
    let y1 = (cy + bh / 2).min(h);

    if x0 >= x1 || y0 >= y1 {
        return Tensor::zeros([1, 3, target_size[0], target_size[1]], device);
    }

    let crop = image.clone().slice([0..1, 0..3, y0..y1, x0..x1]);

    interpolate(
        crop,
        [target_size[0], target_size[1]],
        InterpolateOptions::new(burn::tensor::ops::InterpolateMode::Bilinear),
    )
}

/// Mild augmentation for test-time augmentation: rotation + color jitter
/// only, no spatial crop. Used to generate variants for logit averaging at
/// eval time.
pub fn augment_crop_tta<B: Backend>(crop: Tensor<B, 4>, device: &B::Device) -> Tensor<B, 4> {
    let mut x = random_affine(crop, device);
    x = color_jitter(x, device);
    x
}

/// Stochastic augmentation chain for the tight top-number crop the head now
/// classifies. Each transform is applied with some probability; all are
/// label-preserving for numeric D&D dice. Rotation covers the full circle: dice
/// land at any orientation in the tray (the dataset already holds upside-down
/// 12s and 20s), and 6/9 are told apart by their dot/underline marker, which
/// rotates with the glyph. No mirror flips — those are never label-preserving.
///
/// The distribution this models changed completely from the old whole-die
/// design. The head's input is now a margined YOLO number box, so the only real
/// train/serve gaps are: (1) YOLO localization jitter — box slightly tighter or
/// off-center (`bbox_jitter`); (2) in-plane + out-of-plane viewing angle of the
/// glyph on a 3-D face (`random_affine`: rotation + shear + mild anisotropic
/// scale); (3) webcam exposure/white-balance and sensor noise (`color_jitter`,
/// `add_noise`). Colour jitter is deliberately kept: glyph colour correlates
/// with die *type*, not *value*, so jittering it forces the head to read shape
/// instead of taking a colour→value shortcut.
pub fn augment_crop<B: Backend>(crop: Tensor<B, 4>, device: &B::Device) -> Tensor<B, 4> {
    let mut x = crop;
    if bernoulli(0.8) {
        x = random_affine(x, device);
    }
    // Localization jitter: simulate YOLO boxes that land tighter/off-center than
    // the margined export crop. Milder than the old framing zoom, which spanned
    // the now-obsolete tight-bbox-to-loose-studio range.
    if bernoulli(0.7) {
        x = bbox_jitter(x);
    }
    if bernoulli(0.7) {
        x = color_jitter(x, device);
    }
    if bernoulli(0.4) {
        x = add_noise(x, device);
    }
    x
}

/// Coin flip drawn on the host. All augmentation randomness is generated on the
/// CPU (rather than via `Tensor::random(...).into_scalar()`) because pulling a
/// scalar back off the GPU forces a blocking device sync that flushes the CUDA
/// stream — at batch 128 with ~10 draws per image that was ~1k stalls per step,
/// starving the GPU and dominating training time.
fn bernoulli(p: f64) -> bool {
    rand::rng().random_bool(p)
}

/// Random affine warp around the image center: in-plane rotation plus a small
/// shear and mild anisotropic scale, via an affine grid + bilinear grid sample.
/// Out-of-bound samples repeat the edge pixel (border padding), so rotated
/// corners show die-coloured surface rather than black wedges that never occur
/// at serve time.
///
/// Rotation is uniform over the full circle (a die's resting orientation is
/// arbitrary); shear + anisotropic scale cheaply approximate the out-of-plane
/// perspective of a glyph painted on an angled 3-D die face (a full projective
/// warp isn't available, but this covers most of the variation).
fn random_affine<B: Backend>(image: Tensor<B, 4>, device: &B::Device) -> Tensor<B, 4> {
    let [batch, _channels, height, width] = image.dims();

    let mut rng = rand::rng();
    let theta = rng.random_range(-std::f32::consts::PI..std::f32::consts::PI);
    let (s, c) = (theta.sin(), theta.cos());
    // Shear and per-axis scale jitter for the perspective approximation.
    let shx: f32 = rng.random_range(-0.12f32..0.12f32);
    let shy: f32 = rng.random_range(-0.12f32..0.12f32);
    let sx: f32 = rng.random_range(0.9f32..1.1f32);
    let sy: f32 = rng.random_range(0.9f32..1.1f32);

    // Compose rotation R, shear H and scale S as the 2x3 matrix that maps output
    // normalised coords to input normalised coords (torch affine_grid convention).
    // M = R * H * diag(sx, sy); translation column is zero (centered).
    let a = (c - s * shy) * sx;
    let b = (c * shx - s) * sy;
    let d = (s + c * shy) * sx;
    let e = (s * shx + c) * sy;
    let transform = Tensor::<B, 1>::from_data(
        TensorData::new(vec![a, b, 0.0_f32, d, e, 0.0_f32], [6]),
        device,
    )
    .reshape([1, 2, 3])
    .expand([batch, 2, 3]);

    let grid = affine_grid_2d(transform, [batch, 3, height, width]);

    image.grid_sample_2d(
        grid,
        GridSampleOptions::new(InterpolateMode::Bilinear)
            .with_padding_mode(GridSamplePaddingMode::Border),
    )
}

/// Localization-jitter augmentation. Crops a `[0.85, 1.0]` box of the input and
/// resizes it back to the original spatial dims, simulating YOLO number boxes
/// that land slightly tighter or off-center than the 12%-margin export crop.
///
/// This replaces the old framing zoom (which spanned a wide 0.62–1.0 range to
/// cover the now-obsolete tight-bbox-to-loose-studio gap). The export margin
/// already provides the "loose" end, so we only need a mild tighten-and-shift
/// here. The window stays near center (the number fills the frame) with a small
/// jitter so the glyph never leaves the crop.
fn bbox_jitter<B: Backend>(image: Tensor<B, 4>) -> Tensor<B, 4> {
    let [batch, channels, h, w] = image.dims();

    let scale: f32 = rand::rng().random_range(0.85f32..1.0f32);

    let new_h = ((h as f32 * scale) as usize).max(1);
    let new_w = ((w as f32 * scale) as usize).max(1);

    if new_h >= h && new_w >= w {
        return image;
    }

    // Center the crop window, then jitter it by up to ±25% of the available
    // margin so the die can shift off-center but never leaves the frame.
    let offset = |max: usize| -> usize {
        if max == 0 {
            return 0;
        }
        let center = max as f32 / 2.0;
        let span = max as f32 * 0.25;
        let jitter: f32 = rand::rng().random_range(-1.0f32..1.0f32);
        (center + jitter * span).round().clamp(0.0, max as f32) as usize
    };
    let y_offset = offset(h - new_h);
    let x_offset = offset(w - new_w);

    let cropped = image.slice([
        0..batch,
        0..channels,
        y_offset..y_offset + new_h,
        x_offset..x_offset + new_w,
    ]);

    interpolate(
        cropped,
        [h, w],
        InterpolateOptions::new(burn::tensor::ops::InterpolateMode::Bilinear),
    )
}

fn color_jitter<B: Backend>(image: Tensor<B, 4>, device: &B::Device) -> Tensor<B, 4> {
    // Wider than studio framing: webcams vary a lot in exposure and white
    // balance, so brightness/contrast swing harder and each channel gets an
    // independent gain to simulate colour-temperature shifts.
    let brightness: f32 = rand::rng().random_range(0.65f32..1.35f32);
    let contrast: f32 = rand::rng().random_range(0.65f32..1.35f32);

    let adjusted = (image - 0.5) * contrast + 0.5;
    let adjusted = adjusted * brightness;

    // Per-channel gain (white-balance jitter): shape [1, 3, 1, 1] broadcasts
    // over the spatial dims.
    let tint = Tensor::<B, 4>::random([1, 3, 1, 1], Distribution::Uniform(0.85, 1.15), device);
    let adjusted = adjusted * tint;

    adjusted.clamp(0.0, 1.0)
}

fn add_noise<B: Backend>(image: Tensor<B, 4>, device: &B::Device) -> Tensor<B, 4> {
    let noise = Tensor::<B, 4>::random(image.shape(), Distribution::Normal(0.0, 0.07), device);
    (image + noise).clamp(0.0, 1.0)
}
