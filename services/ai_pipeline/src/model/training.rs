use std::fs::File;
use std::io::Write;
use std::path::Path;

use crate::datasets::dataset::{DatasetType, load_dataset, load_dataset_folder};
use crate::datasets::{DiceBatch, DiceBatcher, augment_batches, prepare_crops};
use crate::model::head::{NUM_CLASSES, evaluate_with_tta};

use burn::grad_clipping::GradientClippingConfig;
use burn::lr_scheduler::cosine::CosineAnnealingLrSchedulerConfig;
use burn::module::AutodiffModule;
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
}

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

    const AUG_FACTOR: usize = 1;

    let (train_batches, val_batches) = match folder_type {
        DatasetType::YOLO => {
            let samples = load_dataset::<B>(root, device.clone()).expect("Failed to load dataset");

            println!("Loaded {} YOLOv1.1 samples", samples.len());

            let split = (samples.len() as f32 * 0.8) as usize;
            let (train_samples, val_samples) = samples.split_at(split);

            let train_batches = prepare_crops::<B>(train_samples, &device, AUG_FACTOR);
            let val_batches = prepare_crops::<B>(val_samples, &device, 0);

            (train_batches, val_batches)
        }
        DatasetType::Folder => {
            let mut batches =
                load_dataset_folder(root, device.clone()).expect("Failed to load dataset");

            use rand::SeedableRng;
            use rand::seq::SliceRandom;
            let mut rng = rand::rngs::StdRng::seed_from_u64(config.seed);
            batches.shuffle(&mut rng);

            println!("Loaded {} folder batches", batches.len());

            let split = (batches.len() as f32 * 0.8) as usize;
            let (train_raw, val_raw) = batches.split_at(split);

            let train_batches = augment_batches(train_raw, &device, AUG_FACTOR);
            let val_batches = val_raw.to_vec();

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

    let batcher_train = DiceBatcher::<B>::new(device.clone());
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

    let training = SupervisedTraining::new(artifact_dir, dataloader_train, dataloader_valid)
        .num_epochs(config.num_epochs)
        .metric_train_numeric(AccuracyMetric::new())
        .metric_valid_numeric(AccuracyMetric::new())
        .metric_train(CudaMetric::new())
        .metric_valid(CudaMetric::new())
        .metric_train_numeric(LossMetric::new())
        .early_stopping(MetricEarlyStoppingStrategy::new(
            &valid_loss,
            Aggregate::Mean,
            Direction::Lowest,
            Split::Valid,
            StoppingCondition::NoImprovementSince { n_epochs: 10 },
        ))
        .metric_valid_numeric(valid_loss)
        .summary();

    // let lr_scheduler = ExponentialLrSchedulerConfig::new(config.learning_rate, 0.998)
    //     .init()
    //     .unwrap();
    let lr_scheduler =
        CosineAnnealingLrSchedulerConfig::new(config.learning_rate, config.num_epochs)
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

    // KNOWN LIMITATION: burn-train 0.21 SupervisedTraining::launch returns the
    // final-epoch model, not the best-valid-loss epoch. The MetricEarlyStoppingStrategy
    // wired above only halts training; it does not restore the best weights, and no
    // "restore best" API exists on the Learner / SupervisedTraining builder. Per-epoch
    // checkpoints are also not written because we do not call .with_file_checkpointer(),
    // so we cannot reload a better epoch post-hoc. To capture the best-valid-loss
    // epoch in the future, install .with_file_checkpointer(CompactRecorder::new()) +
    // .with_checkpointing_strategy(MetricCheckpointingStrategy::new(...)) and reload
    // the surviving checkpoint here.
    let mut store = BurnpackStore::from_file(format!("{}/model/model", artifact_dir));

    result.model.save_into(&mut store).unwrap_or_else(|e| {
        println!("Cannot write burnpack: {}", e);
        result
            .model
            .save_file(format!("{}/model", artifact_dir), &CompactRecorder::new())
            .expect("Trained model should be saved");
    });
}

pub fn eval<B: AutodiffBackend>(
    artifact_dir: &str,
    device: B::Device,
    root: &Path,
    folder_type: DatasetType,
) {
    let batches = match folder_type {
        DatasetType::YOLO => {
            unimplemented!("unsupported for now");
        }
        DatasetType::Folder => {
            load_dataset_folder(root, device.clone()).expect("Failed to load dataset")
        }
    };

    let mut store = BurnpackStore::from_file(format!("{}/model/model", artifact_dir));

    let mut model = crate::model::DiceHead::<B>::new(&device);

    let load_res = model.load_from(&mut store);

    // load_dataset_folder emits one [1,3,64,64] DiceBatch per image. Running
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
