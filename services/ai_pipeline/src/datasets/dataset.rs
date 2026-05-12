use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Result;
use burn::{
    prelude::Backend,
    tensor::{Int, Tensor, TensorData},
};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use rand::seq::SliceRandom;

use crate::datasets::DiceBatch;
use crate::model::head::NUM_CLASSES;

pub enum DatasetType {
    #[allow(clippy::upper_case_acronyms)]
    YOLO,
    Folder,
}

const TARGET_SIZE: usize = 128;

/// Map a face-value folder name (e.g. "1", "0", "10") to the canonical label
/// index used by the model, matching the order in `obj.names`:
///   "1".."9"   -> 0..8
///   "0"        -> 9   (the d10 "0" glyph, semantically face value 10)
///   "10".."20" -> 10..20
fn folder_name_to_label(name: &str) -> Option<u32> {
    match name {
        "0" => Some(9),
        n => {
            let v: u32 = n.parse().ok()?;
            match v {
                1..=9 => Some(v - 1),
                10..=20 => Some(v),
                _ => None,
            }
        }
    }
}

/// Annotation for a single bounding box.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Annotation {
    /// Class id (should be 0‑9 to match number of dice faces).
    pub class: u32,
    /// Normalized center x coordinate (0‑1).
    pub x: f32,
    /// Normalized center y coordinate.
    pub y: f32,
    /// Normalized width.
    pub w: f32,
    /// Normalized height.
    pub h: f32,
}

/// Sample containing an image tensor and its annotations.
#[derive(Debug)]
pub struct Sample<B: Backend> {
    pub image: Tensor<B, 4>,
    pub annotations: Vec<Annotation>,
}

pub fn load_dataset_folder<B: Backend>(
    root: &Path,
    device: B::Device,
) -> Result<Vec<DiceBatch<B>>> {
    let mut batches = Vec::new();

    let class_dirs: Vec<(String, u32, PathBuf)> = std::fs::read_dir(root)?
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_dir() {
                return None;
            }
            let name = path.file_name()?.to_str()?.to_string();
            let label = folder_name_to_label(&name)?;
            Some((name, label, path))
        })
        .collect();

    let m = Arc::new(MultiProgress::new());
    let sty = ProgressStyle::with_template("{bar:40.green/yellow} {pos:>7}/{len:7}").unwrap();

    let pb = m.add(ProgressBar::new(class_dirs.len() as u64));
    pb.set_style(sty.clone());

    for (_name, label, class_path) in class_dirs {
        let entries: Vec<_> = std::fs::read_dir(&class_path)?.flatten().collect();
        let pb2 = m.add(ProgressBar::new(entries.len() as u64));

        for entry in entries {
            let path = entry.path();
            if !path
                .extension()
                .map(|e| e == "jpg" || e == "png")
                .unwrap_or(false)
            {
                continue;
            }

            let img = image::open(&path).unwrap().to_rgb8();
            let img = image::imageops::resize(
                &img,
                TARGET_SIZE as u32,
                TARGET_SIZE as u32,
                image::imageops::FilterType::Lanczos3,
            );
            let mut data = Vec::with_capacity(TARGET_SIZE * TARGET_SIZE * 3);
            for pixel in img.pixels() {
                data.push(pixel[0] as f32 / 255.);
                data.push(pixel[1] as f32 / 255.);
                data.push(pixel[2] as f32 / 255.);
            }

            let image = Tensor::<B, 4>::from_data(
                TensorData::new(data, [1, 3, TARGET_SIZE, TARGET_SIZE]),
                &device,
            );

            let target = Tensor::<B, 1, Int>::from_data(
                TensorData::new(vec![label as i64], [1]),
                &device,
            );

            batches.push(DiceBatch {
                images: image,
                targets: target,
            });
            pb2.inc(1);
        }
        pb.inc(1);
    }

    Ok(batches)
}

/// Load all samples from the dice dataset.
/// Expects `root/dice_images/*.jpg` and corresponding
/// `root/dice_annotations/*.txt` files.
pub fn load_dataset<B: Backend>(root: &Path, device: B::Device) -> Result<Vec<Sample<B>>> {
    let dice_path = root.join("obj_train_data");

    let mut samples = Vec::new();

    let mut img_root: Option<PathBuf> = None;
    let mut anno_root: Option<PathBuf> = None;

    for folder in std::fs::read_dir(&dice_path)? {
        let folder = folder?.path();

        if folder.to_str().unwrap().contains("anno") {
            anno_root = Some(dice_path.join(folder.file_name().unwrap().to_str().unwrap()));
        } else {
            img_root = Some(dice_path.join(folder.file_name().unwrap().to_str().unwrap()));
        }
    }

    for dice_folder in std::fs::read_dir(img_root.unwrap())?
        .filter_map(|dir| dir.ok())
        .map(|x| x.path())
        .collect::<Vec<PathBuf>>()
    {
        let Some(dice_type) = dice_folder.file_name().and_then(|x| x.to_str()) else {
            continue;
        };

        let Some(anno_folder) = anno_root.clone().map(|x| x.join(dice_type)) else {
            eprintln!("Could not construct annotations folder");
            continue;
        };

        if !&anno_folder.is_dir() {
            eprintln!("No annotation folder for {dice_type:?}");
            continue;
        }

        for entry in std::fs::read_dir(&dice_folder)?
            .filter_map(|dir| dir.ok())
            .map(|x| x.path())
            .collect::<Vec<PathBuf>>()
        {
            // Only consider jpg images.
            let supported_filetype = matches!(
                entry.extension().and_then(|s| s.to_str()),
                Some("jpg") | Some("JPG") | Some("png") | Some("webp")
            );

            if !supported_filetype {
                continue;
            }

            let filename_stem = if let Some(s) = entry.file_stem().and_then(|s| s.to_str()) {
                s
            } else {
                eprintln!("Image file has no stem: {entry:?}");
                continue;
            };

            let ann_path = anno_folder.join(format!("{}.txt", filename_stem));

            // Read annotations.
            let ann_contents = match std::fs::read_to_string(&ann_path) {
                Ok(val) => val,
                Err(e) => {
                    eprintln!("failed to read annotation file {ann_path:?} with error: {e}");
                    continue;
                }
            };

            let annotations: Vec<Annotation> = ann_contents
                .lines()
                .filter_map(|line| {
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    if parts.len() != 5 {
                        return None;
                    }
                    let class = parts[0].parse::<u32>().ok()?;
                    if class as usize >= NUM_CLASSES {
                        eprintln!(
                            "warning: dropping annotation with class {class} >= NUM_CLASSES ({NUM_CLASSES}) in {ann_path:?}"
                        );
                        return None;
                    }
                    let x = parts[1].parse::<f32>().ok()?;
                    let y = parts[2].parse::<f32>().ok()?;
                    let w = parts[3].parse::<f32>().ok()?;
                    let h = parts[4].parse::<f32>().ok()?;
                    Some(Annotation { class, x, y, w, h })
                })
                .collect();

            // Load image using the image crate.
            let img = match image::open(&entry) {
                Ok(img) => img,
                Err(e) => {
                    eprintln!("failed to open image {entry:?}, with error: {e}");
                    match e {
                        image::ImageError::Decoding(_) => {
                            let pathstring = entry.as_path().to_string_lossy();
                            let pathstub = entry.parent().unwrap().to_string_lossy();
                            println!("renamed {filename_stem}");
                            std::fs::rename(&entry, format!("{pathstub}/{filename_stem}.webp"))
                                .unwrap_or_else(|_| panic!("Could not rename {pathstring}"));
                        }
                        _ => todo!(),
                    };
                    continue;
                }
            };

            let img_rgb = img.to_rgb8();
            let (w, h) = img_rgb.dimensions();
            let mut data: Vec<f32> = Vec::with_capacity((w * h * 3) as usize);
            for pixel in img_rgb.pixels() {
                data.push(pixel[0] as f32 / 255.);
                data.push(pixel[1] as f32 / 255.);
                data.push(pixel[2] as f32 / 255.);
            }

            // Shape: [1, 3, h, w]
            let tensor = Tensor::<B, 4>::from_data(
                TensorData::new(data, [1, 3, h as usize, w as usize]),
                &device,
            );

            samples.push(Sample {
                image: tensor,
                annotations,
            });
        }
    }

    samples.shuffle(&mut rand::rng());
    Ok(samples)
}
