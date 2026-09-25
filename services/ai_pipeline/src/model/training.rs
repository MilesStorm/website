use std::fs::File;
use std::io::Write;
use std::path::Path;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use crate::datasets::dataset::{DatasetType, FolderSample, load_dataset, load_dataset_folder};
use crate::datasets::{DiceBatch, DiceBatcher, prepare_crops};
use crate::model::head::{NUM_CLASSES, evaluate_with_tta};

use burn::grad_clipping::GradientClippingConfig;
use burn::lr_scheduler::cosine::CosineAnnealingLrSchedulerConfig;
use burn::module::AutodiffModule;
use burn::prelude::Backend;
use burn::record::{FullPrecisionSettings, NamedMpkFileRecorder};
use burn::tensor::{Tensor, activation::softmax};
use burn_train::checkpoint::MetricCheckpointingStrategy;
use burn::optim::decay::WeightDecayConfig;
use burn::{
    config::Config, data::dataloader::DataLoaderBuilder, data::dataset::InMemDataset,
    module::Module, optim::AdamConfig, record::CompactRecorder, tensor::backend::AutodiffBackend,
};
use burn_store::{BurnpackStore, ModuleSnapshot};
use burn_train::metric::store::{Aggregate, Direction, Split};
use burn_train::{
    Learner, SupervisedTraining,
    metric::{AccuracyMetric, CudaMetric, LossMetric},
};
use burn_train::{MetricEarlyStoppingStrategy, StoppingCondition};

#[derive(Config, Debug)]
pub struct TrainingConfig {
    pub optimizer: AdamConfig,
    #[config(default = 50)]
    pub num_epochs: usize,
    #[config(default = 32)]
    pub batch_size: usize,
    #[config(default = 4)]
    pub num_workers: usize,
    #[config(default = 42)]
    pub seed: u64,
    #[config(default = 1e-4)]
    pub learning_rate: f64,
    #[config(default = 1e-4)]
    pub weight_decay: f32,
    /// Capture-sequence groups are dealt into this many class-stratified folds.
    #[config(default = 5)]
    pub num_folds: usize,
    /// Fold held out for validation; the rest train. Training once per fold
    /// gives out-of-fold predictions for every image (see `audit`).
    #[config(default = 0)]
    pub val_fold: usize,
}

/// Full-precision recorder for per-epoch checkpoints. (`CompactRecorder` stores
/// f16, which would silently truncate the best epoch's weights before export.)
type CheckpointRecorder = NamedMpkFileRecorder<FullPrecisionSettings>;

/// Held-out sample keys (`<value-folder>/<stem>`), one per line, per experiment.
const VAL_FILES: &str = "val_files.txt";

pub fn train<B: AutodiffBackend>(
    artifact_dir: &str,
    config: TrainingConfig,
    device: B::Device,
    root: &Path,
    folder_type: DatasetType,
) {
    std::fs::create_dir_all(artifact_dir).ok();
    std::fs::create_dir_all(format!("{}/model", artifact_dir)).ok();

    config
        .save(format!("{}/config.json", artifact_dir))
        .expect("Config should be saved");

    B::seed(&device, config.seed);

    // Augmentation now runs on the fly in the training batcher (DiceBatcher::
    // augmenting), so it varies every epoch instead of being baked in once.
    // Both loaders therefore emit one un-augmented crop per image here.
    let (train_batches, val_batches) = match folder_type {
        DatasetType::YOLO => {
            let samples = load_dataset::<B>(root, device.clone()).expect("Failed to load dataset");

            println!("Loaded {} YOLOv1.1 samples", samples.len());

            let split = (samples.len() as f32 * 0.8) as usize;
            let (train_samples, val_samples) = samples.split_at(split);

            let train_batches = prepare_crops::<B>(train_samples, &device, 0);
            let val_batches = prepare_crops::<B>(val_samples, &device, 0);

            (train_batches, val_batches)
        }
        DatasetType::Folder => {
            let samples = load_dataset_folder(root, device.clone()).expect("Failed to load dataset");
            assert!(
                config.val_fold < config.num_folds,
                "val_fold {} out of range for {} folds",
                config.val_fold,
                config.num_folds
            );

            // Split on capture sequences, not images, so near-duplicate frames of
            // one roll never straddle train/val (which inflates val accuracy),
            // stratified per class so rare faces appear on both sides.
            let folds = assign_folds(&samples, config.num_folds, config.seed);
            let n_groups = folds.len();

            let mut train_batches = Vec::new();
            let mut val_batches = Vec::new();
            let mut val_keys = Vec::new();
            for sample in samples {
                if folds[&sample.group] == config.val_fold {
                    val_keys.push(sample.key);
                    val_batches.push(sample.batch);
                } else {
                    train_batches.push(sample.batch);
                }
            }
            std::fs::write(format!("{artifact_dir}/{VAL_FILES}"), val_keys.join("\n"))
                .expect("write val file list");

            println!(
                "Loaded {} folder images in {} capture groups -> {} train / {} val (fold {}/{})",
                train_batches.len() + val_batches.len(),
                n_groups,
                train_batches.len(),
                val_batches.len(),
                config.val_fold,
                config.num_folds,
            );

            (train_batches, val_batches)
        }
    };

    println!("Created {} training crops", train_batches.len());

    if train_batches.is_empty() {
        panic!("No training data generated! Check annotations.");
    }

    let val_batches_inner: Vec<DiceBatch<B::InnerBackend>> = val_batches
        .iter()
        .map(|batch: &DiceBatch<B>| DiceBatch {
            images: batch.images.clone().inner(),
            targets: batch.targets.clone().inner(),
        })
        .collect();
    // val_batches lives on the autodiff backend; the dataloader only consumes
    // val_batches_inner, so release the autodiff copy before training starts
    // to avoid carrying a duplicate of the validation set on GPU for the
    // whole run.
    drop(val_batches);

    let batcher_train = DiceBatcher::<B>::augmenting(device.clone());
    let batcher_valid = DiceBatcher::<B::InnerBackend>::new(device.clone());

    let dataloader_train = DataLoaderBuilder::new(batcher_train)
        .batch_size(config.batch_size)
        .shuffle(config.seed)
        .num_workers(config.num_workers)
        .build(InMemDataset::new(train_batches.to_vec()));

    let dataloader_valid = DataLoaderBuilder::new(batcher_valid)
        .batch_size(config.batch_size)
        .num_workers(config.num_workers)
        .build(InMemDataset::new(val_batches_inner));

    let class_weights = compute_class_weights::<B>(&train_batches);
    println!("class weights: {:?}", class_weights);
    let model = crate::model::DiceHead::new(&device).with_class_weights(class_weights);

    // Use metric_train_numeric and metric_valid_numeric for graph visualization
    let valid_loss = LossMetric::new();
    let valid_acc = AccuracyMetric::new();
    let keep_best =
        MetricCheckpointingStrategy::new(&valid_acc, Aggregate::Mean, Direction::Highest, Split::Valid);

    let training = SupervisedTraining::new(artifact_dir, dataloader_train, dataloader_valid)
        .num_epochs(config.num_epochs)
        .metric_train_numeric(AccuracyMetric::new())
        .metric_valid_numeric(valid_acc)
        .metric_train(CudaMetric::new())
        .metric_valid(CudaMetric::new())
        .metric_train_numeric(LossMetric::new())
        .early_stopping(MetricEarlyStoppingStrategy::new(
            &valid_loss,
            Aggregate::Mean,
            Direction::Lowest,
            Split::Valid,
            StoppingCondition::NoImprovementSince { n_epochs: 15 },
        ))
        // Keep only the best-val-accuracy epoch on disk; it is exported below
        // instead of whatever epoch training happened to stop on.
        .with_file_checkpointer(CheckpointRecorder::new())
        .with_checkpointing_strategy(keep_best)
        .metric_valid_numeric(valid_loss)
        .summary();

    // let lr_scheduler = ExponentialLrSchedulerConfig::new(config.learning_rate, 0.998)
    //     .init()
    //     .unwrap();
    // burn-train steps the scheduler once per *batch*, and the cosine schedule
    // wraps back to max_lr after `num_iters` steps. Passing `num_epochs` here made
    // the LR saw between 1e-3 and 1e-6 every ~41 batches for the whole run (never
    // annealing). Fixing this did NOT close the ~28-point held-out gap to the same
    // net trained in PyTorch (experiment_42: 51.5% vs 79.9%); that cause is unknown.
    let total_iters = config.num_epochs * train_batches.len().div_ceil(config.batch_size);
    let lr_scheduler =
        CosineAnnealingLrSchedulerConfig::new(config.learning_rate, total_iters)
            .with_min_lr(1e-6)
            .init()
            .unwrap();

    let result = training.launch(Learner::new(
        model,
        config
            .optimizer
            .with_grad_clipping(Some(GradientClippingConfig::Norm(1.0)))
            .with_weight_decay(Some(WeightDecayConfig::new(config.weight_decay)))
            .init(),
        lr_scheduler,
    ));

    // `launch` returns the final-epoch model; swap in the best-val-accuracy
    // checkpoint kept by MetricCheckpointingStrategy.
    let model = match best_checkpoint(artifact_dir) {
        Some((epoch, path)) => {
            match crate::model::DiceHead::<B::InnerBackend>::new(&device).load_file(
                &path,
                &CheckpointRecorder::new(),
                &device,
            ) {
                Ok(best) => {
                    println!("exporting best epoch {epoch} ({})", path.display());
                    best
                }
                Err(e) => {
                    println!("cannot load {}: {e}; exporting final epoch", path.display());
                    result.model
                }
            }
        }
        None => {
            println!("no checkpoint found; exporting final epoch");
            result.model
        }
    };

    let mut store = BurnpackStore::from_file(format!("{}/model/model", artifact_dir));
    model.save_into(&mut store).unwrap_or_else(|e| {
        println!("Cannot write burnpack: {}", e);
        model
            .clone()
            .save_file(format!("{}/model", artifact_dir), &CompactRecorder::new())
            .expect("Trained model should be saved");
    });
}

/// The surviving `checkpoint/model-<epoch>` (highest epoch if several), as
/// (epoch, path-without-extension) for `load_file`.
fn best_checkpoint(artifact_dir: &str) -> Option<(usize, PathBuf)> {
    let dir = Path::new(artifact_dir).join("checkpoint");
    std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_str()?.to_string();
            let epoch = name.strip_prefix("model-")?.strip_suffix(".mpk")?.parse().ok()?;
            Some((epoch, dir.join(format!("model-{epoch}"))))
        })
        .max_by_key(|(epoch, _)| *epoch)
}

/// Deal capture-sequence groups into `k` folds, stratified by label: within
/// each class the groups are shuffled (seeded) and each goes to whichever fold
/// currently holds the fewest images of that class. Returns group -> fold.
fn assign_folds<B: Backend>(samples: &[FolderSample<B>], k: usize, seed: u64) -> HashMap<String, usize> {
    use rand::SeedableRng;
    use rand::seq::SliceRandom;

    // label -> group -> image count. BTreeMaps keep the pre-shuffle order stable.
    let mut per_label: BTreeMap<u32, BTreeMap<&str, usize>> = BTreeMap::new();
    for s in samples {
        *per_label.entry(s.label).or_default().entry(&s.group).or_default() += 1;
    }

    let mut rng = rand::rngs::StdRng::seed_from_u64(seed);
    let mut folds = HashMap::new();
    for groups in per_label.values() {
        let mut groups: Vec<(&str, usize)> = groups.iter().map(|(g, n)| (*g, *n)).collect();
        groups.shuffle(&mut rng);
        let mut fill = vec![0usize; k];
        for (group, n) in groups {
            // A relabelled override can put one group under two labels; the
            // first assignment wins so the group stays on one side.
            if folds.contains_key(group) {
                continue;
            }
            let fold = (0..k).min_by_key(|&f| fill[f]).unwrap();
            fill[fold] += n;
            folds.insert(group.to_string(), fold);
        }
    }
    folds
}

fn read_val_keys(artifact_dir: &str) -> Option<HashSet<String>> {
    let text = std::fs::read_to_string(Path::new(artifact_dir).join(VAL_FILES)).ok()?;
    Some(text.lines().map(str::to_string).collect())
}

pub fn eval<B: AutodiffBackend>(
    artifact_dir: &str,
    device: B::Device,
    root: &Path,
    folder_type: DatasetType,
) {
    let samples = match folder_type {
        DatasetType::YOLO => {
            unimplemented!("unsupported for now");
        }
        DatasetType::Folder => {
            load_dataset_folder(root, device.clone()).expect("Failed to load dataset")
        }
    };

    // Score only the experiment's held-out images; scoring the training set too
    // reports memorisation, not accuracy.
    let batches: Vec<DiceBatch<B>> = match read_val_keys(artifact_dir) {
        Some(val) => samples
            .into_iter()
            .filter(|s| val.contains(&s.key))
            .map(|s| s.batch)
            .collect(),
        None => {
            println!("WARNING: no {VAL_FILES} in {artifact_dir}; scoring ALL images (includes training data)");
            samples.into_iter().map(|s| s.batch).collect()
        }
    };
    println!("evaluating {} images", batches.len());

    let mut store = BurnpackStore::from_file(format!("{}/model/model", artifact_dir));

    let mut model = crate::model::DiceHead::<B>::new(&device);

    let load_res = model.load_from(&mut store);

    // Each sample is one [1,3,64,64] DiceBatch. Running
    // forward at batch=1 is kernel-launch bound (GPU idle 95% of the time),
    // so concat into chunks of EVAL_BATCH first. evaluate_with_tta runs on
    // B::InnerBackend (no autodiff needed for eval), so strip the autodiff
    // wrapper as we batch.
    const EVAL_BATCH: usize = 128;
    let batcher = crate::datasets::DiceBatcher::<B::InnerBackend>::new(device.clone());
    let batched: Vec<crate::datasets::DiceBatch<B::InnerBackend>> = {
        use burn::data::dataloader::batcher::Batcher;
        batches
            .chunks(EVAL_BATCH)
            .map(|chunk: &[crate::datasets::DiceBatch<B>]| {
                let inner: Vec<crate::datasets::DiceBatch<B::InnerBackend>> = chunk
                    .iter()
                    .map(|b| crate::datasets::DiceBatch {
                        images: b.images.clone().inner(),
                        targets: b.targets.clone().inner(),
                    })
                    .collect();
                batcher.batch(inner, &device)
            })
            .collect()
    };

    match load_res {
        Ok(_) => {
            let matrix = evaluate_with_tta(model.valid(), &batched, NUM_CLASSES, 0);
            let names = class_label_names();

            print_confusion_matrix(&matrix, &names, artifact_dir);
        }

        Err(e) => {
            eprintln!("could not load model: {}", e);
            panic!("");
        }
    }
}

/// Label audit from out-of-fold predictions.
///
/// For each experiment directory, runs its model over its own held-out images
/// (`val_files.txt`) — images the model never trained on, so a confident
/// disagreement is real evidence of a bad label or a crop of the wrong face,
/// not memorisation. Training one experiment per fold (`--fold 0..K`) and
/// auditing them together covers every image.
///
/// Writes `art/audit.csv` sorted by the probability given to the labelled
/// class (most suspicious first). Review with tools/audit_sheet.py and record
/// decisions in crop_overrides.tsv; the data folders are never modified.
pub fn audit<B: Backend>(exp_dirs: &[String], device: B::Device, root: &Path, out: &Path) {
    const BATCH: usize = 256;

    let samples = load_dataset_folder::<B>(root, device.clone()).expect("Failed to load dataset");
    let names = class_label_names();
    let mut rows: Vec<(f32, String)> = Vec::new();
    let mut seen = HashSet::new();

    for exp in exp_dirs {
        let val = read_val_keys(exp)
            .unwrap_or_else(|| panic!("{exp} has no {VAL_FILES}; retrain it with this version"));
        let mut model = crate::model::DiceHead::<B>::new(&device);
        model
            .load_from(&mut BurnpackStore::from_file(format!("{exp}/model/model")))
            .unwrap_or_else(|e| panic!("load {exp}: {e}"));
        let model = model.map(&mut crate::model::inferance::CastToF32);

        let held_out: Vec<&FolderSample<B>> = samples
            .iter()
            .filter(|s| val.contains(&s.key) && seen.insert(s.key.clone()))
            .collect();

        for chunk in held_out.chunks(BATCH) {
            let images = Tensor::cat(chunk.iter().map(|s| s.batch.images.clone()).collect(), 0);
            let probs: Vec<f32> = softmax(model.forward(images), 1)
                .into_data()
                .convert::<f32>()
                .to_vec()
                .unwrap();
            for (s, p) in chunk.iter().zip(probs.chunks_exact(NUM_CLASSES)) {
                let (pred, pred_p) = p
                    .iter()
                    .enumerate()
                    .max_by(|a, b| a.1.total_cmp(b.1))
                    .unwrap();
                let label_p = p[s.label as usize];
                rows.push((
                    label_p,
                    format!(
                        "{},{},{},{},{:.4},{:.4},{}",
                        s.key,
                        s.path.display(),
                        names[s.label as usize],
                        names[pred],
                        pred_p,
                        label_p,
                        exp
                    ),
                ));
            }
        }
        println!("{exp}: audited {} held-out images", held_out.len());
    }

    rows.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut f = File::create(out).expect("create audit csv");
    writeln!(f, "key,path,label,pred,pred_prob,label_prob,experiment").unwrap();
    for (_, row) in &rows {
        writeln!(f, "{row}").unwrap();
    }

    let flagged = rows.iter().filter(|(p, _)| *p < 0.1).count();
    println!(
        "audited {} of {} images; {flagged} with label prob < 0.1 -> {}",
        rows.len(),
        samples.len(),
        out.display()
    );
}

/// Sqrt-inverse-frequency class weights, capped at 3x. Plain inverse-frequency
/// (previous behaviour, 10x cap) hurt accuracy on the abundant low-value faces
/// (classes 1-5 at 57-74%) because it gave rare classes ~25x the gradient
/// pull. Sqrt softens the curve so rare classes still get a lift but common
/// classes aren't sacrificed.
pub fn compute_class_weights<B: AutodiffBackend>(batches: &[DiceBatch<B>]) -> Vec<f32> {
    let mut counts = vec![0usize; NUM_CLASSES];
    for batch in batches {
        let data: Vec<i64> = batch
            .targets
            .clone()
            .into_data()
            .convert::<i64>()
            .to_vec()
            .unwrap();
        for t in data {
            let idx = t as usize;
            if idx < NUM_CLASSES {
                counts[idx] += 1;
            }
        }
    }
    let total: usize = counts.iter().sum();
    counts
        .iter()
        .map(|&c| {
            if c == 0 {
                1.0
            } else {
                let raw = total as f32 / (NUM_CLASSES as f32 * c as f32);
                raw.sqrt().clamp(0.3, 3.0)
            }
        })
        .collect()
}

/// Class label names ordered to match obj.names: "1".."9", "0", "10".."20".
/// Label index N corresponds to the N-th line of obj.names.
pub fn class_label_names() -> Vec<String> {
    let mut names: Vec<String> = (1..=9).map(|i| i.to_string()).collect();
    names.push("0".to_string());
    names.extend((10..=20).map(|i| i.to_string()));
    debug_assert_eq!(names.len(), NUM_CLASSES);
    names
}

pub fn print_confusion_matrix(matrix: &[Vec<usize>], class_names: &[String], experiment_dir: &str) {
    let num_classes = matrix.len();

    // Print header
    print!("{:>10} |", "True\\Pred");
    for name in class_names {
        print!("{:>8}", name);
    }
    println!("\n{}", "-".repeat(11 + num_classes * 8));

    // Print rows
    for (i, row) in matrix.iter().enumerate() {
        print!("{:>10} |", class_names[i]);
        for &count in row {
            print!("{:>8}", count);
        }
        println!();
    }

    // Compute per-class accuracy
    println!("\nPer-class accuracy:");
    for (i, row) in matrix.iter().enumerate() {
        let total: usize = row.iter().sum();
        let correct = row[i];
        let accuracy = if total > 0 {
            100.0 * correct as f32 / total as f32
        } else {
            0.0
        };
        println!(
            "  Class {}: {:.2}% ({}/{})",
            class_names[i], accuracy, correct, total
        );
    }

    // Overall accuracy
    let total_samples: usize = matrix.iter().flat_map(|r| r.iter()).sum();
    let total_correct: usize = (0..num_classes).map(|i| matrix[i][i]).sum();
    let overall = 100.0 * total_correct as f32 / total_samples as f32;
    println!(
        "\nOverall accuracy: {:.2}% ({}/{})",
        overall, total_correct, total_samples
    );
    println!("writing to csv...");
    save_confusion_matrix_csv(matrix, &format!("{}/confusion.csv", experiment_dir)).unwrap();
    println!("done");
}

pub fn save_confusion_matrix_csv(matrix: &[Vec<usize>], path: &str) -> std::io::Result<()> {
    let mut file = File::create(path)?;

    // Write header
    write!(file, "true_label,")?;
    for i in 0..matrix.len() {
        write!(file, "pred_{}", i)?;
        if i < matrix.len() - 1 {
            write!(file, ",")?;
        }
    }
    writeln!(file)?;

    // Write data
    for (i, row) in matrix.iter().enumerate() {
        write!(file, "{}", i)?;
        for count in row {
            write!(file, ",{}", count)?;
        }
        writeln!(file)?;
    }

    Ok(())
}
