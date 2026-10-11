use std::{
    collections::HashMap,
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
use crate::model::inferance::HEAD_INPUT;

pub enum DatasetType {
    #[allow(clippy::upper_case_acronyms)]
    YOLO,
    Folder,
}

/// Folder images are resized to the head's input size. Must equal `HEAD_INPUT`
/// (and the exporter's crop size) so train == serve.
const TARGET_SIZE: usize = HEAD_INPUT;

/// Strip leading face-value digits from a filename stem so the many
/// `<value>IMG_x` / `IMG_x` copies of one capture collapse to a single group.
/// Mirrors `norm_stem` in tools/pseudo_label_face.py.
fn norm_stem(stem: &str) -> &str {
    stem.trim_start_matches(|c: char| c.is_ascii_digit())
}

/// Map a face-value folder name (e.g. "1", "0", "10") to the canonical label
/// index used by the model, matching the order in `obj.names`:
///   "1".."9"   -> 0..8
///   "0"        -> 9   (the d10 "0" glyph, semantically face value 10)
///   "10".."20" -> 10..20
pub fn folder_name_to_label(name: &str) -> Option<u32> {
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

/// Label-override file (tracked in git, next to Cargo.toml). Lets bad crops be
/// dropped or relabelled without touching the data folders. One entry per line:
///   `<value-folder>/<file-stem>\t<drop | value-folder>`
/// Key and action are separated by a TAB (keys may contain spaces, e.g.
/// `5/Pasted image (3)`); `#` starts a comment. Built from `audit` output (see
/// tools/audit_sheet.py).
pub const OVERRIDES_PATH: &str = "./crop_overrides.tsv";

pub enum Override {
    Drop,
    Relabel(u32),
}

/// Parse `OVERRIDES_PATH`. A missing file means no overrides.
pub fn load_overrides(path: &Path) -> Result<HashMap<String, Override>> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(HashMap::new());
    };
    let mut map = HashMap::new();
    for (n, line) in text.lines().enumerate() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let Some((key, action)) = line.split_once('\t') else {
            anyhow::bail!("{}:{}: expected `<folder>/<stem>\\t<drop|value>`", path.display(), n + 1);
        };
        let (key, action) = (key.trim(), action.split('#').next().unwrap_or("").trim());
        let action = match action {
            "drop" => Override::Drop,
            v => Override::Relabel(folder_name_to_label(v).ok_or_else(|| {
                anyhow::anyhow!("{}:{}: unknown value `{v}`", path.display(), n + 1)
            })?),
        };
        map.insert(key.to_string(), action);
    }
    Ok(map)
}

/// Frames within this many frame numbers of each other are one roll.
const FRAME_GAP: u32 = 3;

/// Where an image sits in its capture sequence.
#[derive(Debug, PartialEq)]
enum SeqKey {
    /// Numbered video frame `<scene><frame>` (public-dataset `d20_wood0709`,
    /// `d4309`). Grouped into rolls by `group_frames`.
    Frame { scene: String, frame: u32 },
    /// Everything else: the key is the group.
    Fixed(String),
}

/// Identify an image's capture sequence, so every near-duplicate frame of it
/// lands on the same side of the train/val split. Filename-level grouping is
/// not enough: `d20_wood0709`, `d20_wood0710`, ... are consecutive video frames
/// of one roll, `IMG_20240306_151612_10/_11` are burst shots, and Roboflow
/// exports add `<hex>-` prefixes and `_jpg.rf.<hash>` suffixes to augmented
/// copies of one source frame.
///
/// Rules: strip the leading face-value digits and the Roboflow prefix/suffix;
/// phone shots group by the minute they were taken; `d...<frame>` names are
/// frames; anything else is its own group.
fn sequence_key(stem: &str) -> SeqKey {
    let mut s = norm_stem(stem);
    if let Some(i) = s.find(".rf.") {
        // `<name>_jpg.rf.<hash>` -> `<name>`
        s = s[..i].rsplit_once('_').map_or(&s[..i], |(head, _)| head);
    }
    // Roboflow `<hex>-` prefix. `norm_stem` may already have eaten its leading
    // digits (possibly all of it), so an empty or short remainder counts too.
    if let Some((prefix, rest)) = s.split_once('-')
        && prefix.len() <= 40
        && prefix.chars().all(|c| c.is_ascii_hexdigit())
    {
        s = rest;
    }

    // IMG_YYYYMMDD_HHMMSS[...] -> IMG_YYYYMMDD_HHMM
    if let Some(ts) = s.strip_prefix("IMG_")
        && ts.len() >= 13
        && ts.as_bytes()[..8].iter().all(u8::is_ascii_digit)
    {
        return SeqKey::Fixed(format!("IMG_{}", &ts[..13]));
    }

    let scene = s.trim_end_matches(|c: char| c.is_ascii_digit());
    if scene.starts_with('d') && let Ok(frame) = s[scene.len()..].parse::<u32>() {
        return SeqKey::Frame { scene: scene.to_string(), frame };
    }
    SeqKey::Fixed(s.to_string())
}

/// Split numbered frames into rolls: within one (folder, scene), sorted by
/// frame number, a new roll starts wherever the gap exceeds `FRAME_GAP`.
/// Returns (sample index, group) pairs.
fn group_frames(mut frames: Vec<(String, String, u32, usize)>) -> Vec<(usize, String)> {
    frames.sort();
    let mut out = Vec::with_capacity(frames.len());
    let mut current: Option<(&str, &str, u32, String)> = None;
    for (folder, scene, frame, idx) in &frames {
        let same_roll = matches!(&current, Some((f, sc, prev, _))
            if f == folder && sc == scene && frame - prev <= FRAME_GAP);
        if same_roll {
            current.as_mut().unwrap().2 = *frame;
        } else {
            current = Some((folder, scene, *frame, format!("{folder}__{scene}@{frame}")));
        }
        out.push((*idx, current.as_ref().unwrap().3.clone()));
    }
    out
}

/// One folder-dataset image.
pub struct FolderSample<B: Backend> {
    /// `<value-folder>/<file-stem>`: stable id used by overrides, split files and audit.
    pub key: String,
    pub path: PathBuf,
    /// Capture-sequence group (`<value-folder>__<sequence_key>`) for leak-free splits.
    pub group: String,
    pub label: u32,
    pub batch: DiceBatch<B>,
}

/// Load every folder image as a single-image batch, applying `OVERRIDES_PATH`
/// (dropped entries are skipped, relabelled ones get the new label but keep
/// their original key/group).
pub fn load_dataset_folder<B: Backend>(
    root: &Path,
    device: B::Device,
) -> Result<Vec<FolderSample<B>>> {
    let overrides = load_overrides(Path::new(OVERRIDES_PATH))?;
    let mut samples = Vec::new();
    let (mut dropped, mut relabelled) = (0usize, 0usize);
    let mut used_overrides = std::collections::HashSet::new();
    // (folder, scene, frame, sample index) for frames; grouped after loading.
    let mut frames = Vec::new();

    let mut class_dirs: Vec<(String, u32, PathBuf)> = std::fs::read_dir(root)?
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
    // Deterministic order so seeded splits are reproducible across runs.
    class_dirs.sort();

    let m = Arc::new(MultiProgress::new());
    let sty = ProgressStyle::with_template("{bar:40.green/yellow} {pos:>7}/{len:7}").unwrap();

    let pb = m.add(ProgressBar::new(class_dirs.len() as u64));
    pb.set_style(sty.clone());

    for (name, folder_label, class_path) in class_dirs {
        let mut entries: Vec<PathBuf> =
            std::fs::read_dir(&class_path)?.flatten().map(|e| e.path()).collect();
        entries.sort();
        let pb2 = m.add(ProgressBar::new(entries.len() as u64));

        for path in entries {
            if !path
                .extension()
                .map(|e| e == "jpg" || e == "png")
                .unwrap_or(false)
            {
                continue;
            }

            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let key = format!("{name}/{stem}");
            if overrides.contains_key(&key) {
                used_overrides.insert(key.clone());
            }
            let label = match overrides.get(&key) {
                Some(Override::Drop) => {
                    dropped += 1;
                    continue;
                }
                Some(Override::Relabel(l)) => {
                    relabelled += 1;
                    *l
                }
                None => folder_label,
            };
            let group = match sequence_key(stem) {
                SeqKey::Fixed(k) => format!("{name}__{k}"),
                SeqKey::Frame { scene, frame } => {
                    frames.push((name.clone(), scene, frame, samples.len()));
                    String::new() // filled in by group_frames below
                }
            };

            let img = image::open(&path)?.to_rgb8();
            let img = image::imageops::resize(
                &img,
                TARGET_SIZE as u32,
                TARGET_SIZE as u32,
                image::imageops::FilterType::Lanczos3,
            );
            let data = crate::model::inferance::pack_chw(&img);

            let images = Tensor::<B, 4>::from_data(
                TensorData::new(data, [1, 3, TARGET_SIZE, TARGET_SIZE]),
                &device,
            );
            let targets = Tensor::<B, 1, Int>::from_data(
                TensorData::new(vec![label as i64], [1]),
                &device,
            );

            samples.push(FolderSample {
                key,
                path,
                group,
                label,
                batch: DiceBatch { images, targets },
            });
            pb2.inc(1);
        }
        pb.inc(1);
    }

    for (idx, group) in group_frames(frames) {
        samples[idx].group = group;
    }

    for key in overrides.keys().filter(|k| !used_overrides.contains(*k)) {
        println!("WARNING: override `{key}` matches no image");
    }
    if dropped + relabelled > 0 {
        println!("overrides: dropped {dropped}, relabelled {relabelled}");
    }
    Ok(samples)
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
            let data = crate::model::inferance::pack_chw(&img_rgb);

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

#[cfg(test)]
mod tests {
    use super::{SeqKey, group_frames, sequence_key};

    fn frame(scene: &str, frame: u32) -> SeqKey {
        SeqKey::Frame { scene: scene.into(), frame }
    }

    #[test]
    fn numbered_frames_are_detected() {
        assert_eq!(sequence_key("19d20_wood0709"), frame("d20_wood", 709));
        assert_eq!(sequence_key("4d4309"), frame("d", 4309));
    }

    #[test]
    fn rolls_split_on_frame_gaps_not_fixed_buckets() {
        let f = |fr: u32, i: usize| ("19".to_string(), "d20_wood".to_string(), fr, i);
        let g: std::collections::HashMap<usize, String> =
            group_frames(vec![f(724, 0), f(725, 1), f(728, 2), f(740, 3)]).into_iter().collect();
        assert_eq!(g[&0], g[&1]); // 724/725 straddled the old /25 bucket edge
        assert_eq!(g[&1], g[&2]); // gap 3 is still the same roll
        assert_ne!(g[&2], g[&3]); // gap 12 starts a new roll
    }

    #[test]
    fn burst_shots_group_by_minute() {
        assert_eq!(
            sequence_key("7IMG_20240306_151612_10"),
            sequence_key("7IMG_20240306_151612")
        );
        assert_eq!(sequence_key("IMG_20240306_151612"), SeqKey::Fixed("IMG_20240306_1516".into()));
    }

    #[test]
    fn roboflow_copies_collapse_to_source_frame() {
        assert_eq!(
            sequence_key("0a8140a4-d20_off-angle_0782_jpg.rf.af0d37ac1b1c32f2847b20ee1bb18759"),
            frame("d20_off-angle_", 782)
        );
        // All-digit hash prefix is swallowed by norm_stem, leaving a bare `-`.
        assert_eq!(sequence_key("1512345678-d20_x_0782"), frame("d20_x_", 782));
    }

    #[test]
    fn unknown_names_are_their_own_group() {
        assert_eq!(sequence_key("Pasted image (3)"), SeqKey::Fixed("Pasted image (3)".into()));
    }
}
