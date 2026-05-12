use burn::tensor::grid::affine_grid_2d;
use burn::tensor::module::interpolate;
use burn::tensor::ops::GridSampleOptions;
use burn::tensor::{Distribution, ElementConversion};

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
    _device: B::Device,
}

impl<B: Backend> DiceBatcher<B> {
    pub fn new(device: B::Device) -> Self {
        Self { _device: device }
    }
}

impl<B: Backend> Batcher<B, DiceBatch<B>, DiceBatch<B>> for DiceBatcher<B> {
    fn batch(&self, items: Vec<DiceBatch<B>>, _device: &B::Device) -> DiceBatch<B> {
        if items.is_empty() {
            panic!("Cannot batch empty items");
        }
        if items.len() == 1 {
            return items.into_iter().next().unwrap();
        }

        let images: Vec<Tensor<B, 4>> = items.iter().map(|b| b.images.clone()).collect();
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
            let crop = crop_dice(sample.image.clone(), ann, [128, 128], device);

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

/// Expand a flat list of single-item DiceBatches by appending `augment_factor`
/// augmented copies of each entry. Used by the folder loader, which already
/// produces one DiceBatch per pre-cropped image.
pub fn augment_batches<B: Backend>(
    batches: &[DiceBatch<B>],
    device: &B::Device,
    augment_factor: usize,
) -> Vec<DiceBatch<B>> {
    let mut out = Vec::with_capacity(batches.len() * (1 + augment_factor));
    for batch in batches {
        out.push(batch.clone());
        for _ in 0..augment_factor {
            let aug = augment_crop(batch.images.clone(), device);
            out.push(DiceBatch {
                images: aug,
                targets: batch.targets.clone(),
            });
        }
    }
    out
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
    let mut x = small_rotate(crop, device);
    x = color_jitter(x, device);
    x
}

/// Stochastic augmentation chain. Each transform is applied with some
/// probability; all are label-preserving for numeric D&D dice (no 90deg
/// rotations, no flips).
pub fn augment_crop<B: Backend>(crop: Tensor<B, 4>, device: &B::Device) -> Tensor<B, 4> {
    let mut x = crop;
    if bernoulli::<B>(0.7, device) {
        x = small_rotate(x, device);
    }
    if bernoulli::<B>(0.5, device) {
        x = random_crop_resize(x, device);
    }
    if bernoulli::<B>(0.7, device) {
        x = color_jitter(x, device);
    }
    if bernoulli::<B>(0.3, device) {
        x = add_noise(x, device);
    }
    x
}

fn bernoulli<B: Backend>(p: f64, device: &B::Device) -> bool {
    Tensor::<B, 1>::random([1], Distribution::Bernoulli(p), device)
        .into_scalar()
        .elem::<f32>()
        > 0.5
}

/// Rotate by a uniform random angle in [-15deg, +15deg] around the image
/// center using an affine grid + bilinear grid sample. Out-of-bound samples
/// are zero-padded.
fn small_rotate<B: Backend>(image: Tensor<B, 4>, device: &B::Device) -> Tensor<B, 4> {
    let [batch, _channels, height, width] = image.dims();

    const MAX_DEG: f32 = 15.0;
    let angle_deg = Tensor::<B, 1>::random(
        [1],
        Distribution::Uniform(-MAX_DEG as f64, MAX_DEG as f64),
        device,
    )
    .into_scalar()
    .elem::<f32>();
    let theta = angle_deg.to_radians();
    let (s, c) = (theta.sin(), theta.cos());

    // 2x3 rotation around the centre. The affine_grid convention follows
    // torch.nn.functional.affine_grid; the matrix maps output normalised
    // coords to input normalised coords.
    let transform = Tensor::<B, 1>::from_data(
        TensorData::new(vec![c, -s, 0.0_f32, s, c, 0.0_f32], [6]),
        device,
    )
    .reshape([1, 2, 3])
    .expand([batch, 2, 3]);

    let grid = affine_grid_2d(transform, [batch, 3, height, width]);

    image.grid_sample_2d(grid, GridSampleOptions::default())
}

/// Random zoom: crop a 95-100% box of the input and resize back to the
/// original spatial dims. At 128x128 with whole-die framing a tighter range
/// is needed since a 90% crop can shave the top face entirely if the die
/// isn't centered.
fn random_crop_resize<B: Backend>(image: Tensor<B, 4>, device: &B::Device) -> Tensor<B, 4> {
    let [batch, channels, h, w] = image.dims();

    let scale = Tensor::<B, 1>::random([1], Distribution::Uniform(0.95f64, 1.0f64), device)
        .into_scalar()
        .elem::<f32>();

    let new_h = ((h as f32 * scale) as usize).max(1);
    let new_w = ((w as f32 * scale) as usize).max(1);

    if new_h >= h && new_w >= w {
        return image;
    }

    let y_offset = if h > new_h {
        Tensor::<B, 1, Int>::random(
            [1],
            Distribution::Uniform(0.0, (h - new_h) as f64),
            device,
        )
        .into_scalar()
        .elem::<i64>() as usize
    } else {
        0
    };
    let x_offset = if w > new_w {
        Tensor::<B, 1, Int>::random(
            [1],
            Distribution::Uniform(0.0, (w - new_w) as f64),
            device,
        )
        .into_scalar()
        .elem::<i64>() as usize
    } else {
        0
    };

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
    let brightness = Tensor::<B, 1>::random([1], Distribution::Uniform(0.8, 1.2), device)
        .into_scalar()
        .elem::<f32>();
    let contrast = Tensor::<B, 1>::random([1], Distribution::Uniform(0.8, 1.2), device)
        .into_scalar()
        .elem::<f32>();

    let adjusted = (image - 0.5) * contrast + 0.5;
    let adjusted = adjusted * brightness;
    adjusted.clamp(0.0, 1.0)
}

fn add_noise<B: Backend>(image: Tensor<B, 4>, device: &B::Device) -> Tensor<B, 4> {
    let noise = Tensor::<B, 4>::random(image.shape(), Distribution::Normal(0.0, 0.05), device);
    (image + noise).clamp(0.0, 1.0)
}
