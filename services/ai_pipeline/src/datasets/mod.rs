use burn::tensor::module::interpolate;
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
        let batch_size = items.len();
        if batch_size == 0 {
            panic!("Cannot batch empty items");
        }
        if batch_size == 1 {
            return items.into_iter().next().unwrap();
        }

        // Stack images: [B, 3, 64, 64]
        let images: Vec<Tensor<B, 4>> = items.iter().map(|b| b.images.clone()).collect();
        let images = Tensor::cat(images, 0); // Stack along batch dim (dim 0)

        // Stack targets: [B]
        let targets: Vec<Tensor<B, 1, Int>> = items.iter().map(|b| b.targets.clone()).collect();
        let targets = Tensor::cat(targets, 0);

        DiceBatch { images, targets }
    }
}

pub fn prepare_training_data_augmented<B: Backend>(
    samples: &[Sample<B>],
    device: &B::Device,
    augment_factor: usize, // 4 = 4x data
) -> Vec<DiceBatch<B>> {
    let mut batches = Vec::new();

    for sample in samples {
        for ann in &sample.annotations {
            // Original crop
            let crop = crop_dice(sample.image.clone(), ann, [64, 64], device);

            let target_data = TensorData::new(vec![ann.class as i64], vec![1]);
            let target = Tensor::<B, 1, Int>::from_data(target_data, device);

            // Add original
            batches.push(DiceBatch {
                images: crop.clone(),
                targets: target.clone(),
            });

            for i in 0..augment_factor {
                let aug_crop = augment_crop(crop.clone(), device, i);
                batches.push(DiceBatch {
                    images: aug_crop,
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

    // Ensure valid slice
    if x0 >= x1 || y0 >= y1 {
        // Return blank crop if invalid bbox
        return Tensor::zeros([1, 3, target_size[0], target_size[1]], device);
    }

    let crop = image.clone().slice([0..1, 0..3, y0..y1, x0..x1]);

    // Resize using interpolate - adjust for your Burn version
    interpolate(
        crop,
        [target_size[0], target_size[1]],
        InterpolateOptions::new(burn::tensor::ops::InterpolateMode::Bilinear),
    )
}

/// Apply all augmentations
pub fn augment_crop<B: Backend>(
    crop: Tensor<B, 4>,
    device: &B::Device,
    variant: usize,
) -> Tensor<B, 4> {
    // Cycle through 4 different augmentation combinations
    match variant % 4 {
        0 => {
            // Variant 1: Rotation + Flip
            let crop = random_rotate(crop, device);
            random_flip(crop, device)
        }
        1 => {
            // Variant 2: Zoom + Color jitter
            let crop = random_crop_resize(crop, device);
            color_jitter(crop, device)
        }
        2 => {
            // Variant 3: Rotation + Noise + Cutout
            let crop = random_rotate(crop, device);
            add_noise(crop, device)
        }
        _ => {
            // Variant 4: Flip + Zoom + Jitter
            let crop = random_flip(crop, device);
            let crop = random_crop_resize(crop, device);
            color_jitter(crop, device)
        }
    }
}

/// Add random Gaussian noise
fn add_noise<B: Backend>(image: Tensor<B, 4>, device: &B::Device) -> Tensor<B, 4> {
    let noise = Tensor::<B, 4>::random(image.shape(), Distribution::Normal(0.0, 0.05), device);
    (image + noise).clamp(0.0, 1.0)
}

/// Random 90-degree rotation
fn random_rotate<B: Backend>(image: Tensor<B, 4>, device: &B::Device) -> Tensor<B, 4> {
    use burn::tensor::Int;

    let rotation = Tensor::<B, 1, Int>::random([1], Distribution::Uniform(0.0, 4.0), device)
        .into_scalar()
        .elem::<i32>()
        % 4;

    match rotation {
        1 => image.transpose().flip([3]), // 90°
        2 => image.flip([2, 3]),          // 180°
        3 => image.transpose().flip([2]), // 270°
        _ => image,                       // 0°
    }
}

/// Random crop and resize (zoom)
fn random_crop_resize<B: Backend>(image: Tensor<B, 4>, device: &B::Device) -> Tensor<B, 4> {
    let [batch, channels, h, w] = image.dims();

    let scale = Tensor::<B, 1>::random([1], Distribution::Uniform(0.8f64, 1.0f64), device)
        .into_scalar()
        .elem::<f32>();

    let new_h = (h as f32 * scale) as usize;
    let new_w = (w as f32 * scale) as usize;

    if new_h == h && new_w == w {
        return image;
    }

    let y_offset =
        Tensor::<B, 1, Int>::random([1], Distribution::Uniform(0.0, (h - new_h) as f64), device)
            .into_scalar()
            .elem::<i64>() as usize;
    let x_offset =
        Tensor::<B, 1, Int>::random([1], Distribution::Uniform(0.0, (w - new_w) as f64), device)
            .into_scalar()
            .elem::<i64>() as usize;

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
    // Brightness: multiply by random factor [0.8, 1.2]
    let brightness = Tensor::<B, 1>::random([1], Distribution::Uniform(0.8, 1.2), device)
        .into_scalar()
        .elem::<f32>();

    // Contrast: adjust around mean (0.5)
    let contrast = Tensor::<B, 1>::random([1], Distribution::Uniform(0.8, 1.2), device)
        .into_scalar()
        .elem::<f32>();

    let adjusted = (image.clone() - 0.5) * contrast + 0.5;
    let adjusted = adjusted * brightness;
    adjusted.clamp(0.0, 1.0)
}

/// Random horizontal flip
///
fn random_flip<B: Backend>(image: Tensor<B, 4>, device: &B::Device) -> Tensor<B, 4> {
    let flip = Tensor::<B, 1>::random([1], Distribution::Bernoulli(0.5), device)
        .into_scalar()
        .elem::<f32>()
        > 0.5;
    if flip {
        image.flip([3]) // Flip along width dimension (axis 3)
    } else {
        image
    }
}
