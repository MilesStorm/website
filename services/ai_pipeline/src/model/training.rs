use std::fs::File;
use std::io::Write;
use std::path::Path;

use crate::datasets::dataset::{DatasetType, load_dataset, load_dataset_folder};
use crate::datasets::{DiceBatch, DiceBatcher, augment_crop, prepare_training_data_augmented};
use crate::model::head::evaluate_mode;

use burn::grad_clipping::GradientClippingConfig;
use burn::lr_scheduler::cosine::CosineAnnealingLrSchedulerConfig;
use burn::module::AutodiffModule;
use burn::optim::decay::WeightDecayConfig;
use burn::prelude::Backend;
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

const AUG_FACTOR: usize = 1;

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

    let (train_batches, val_batches) = match folder_type {
        DatasetType::YOLO => {
            let samples = load_dataset::<B>(root, device.clone()).expect("Failed to load dataset");

            println!("Loaded {} YOLOv1.1 samples", samples.len());

            let split = (samples.len() as f32 * 0.8) as usize;
            let (train_samples, val_samples) = samples.split_at(split);

            let train_batches =
                prepare_training_data_augmented::<B>(train_samples, &device, AUG_FACTOR);
            let val_batches = prepare_training_data_augmented::<B>(val_samples, &device, 1);

            (train_batches, val_batches)
        }
        DatasetType::Folder => {
            let mut batches =
                load_dataset_folder(root, device.clone()).expect("Failed to load dataset");

            // Shuffle before splitting
            use rand::SeedableRng;
            use rand::seq::SliceRandom;
            let mut rng = rand::rngs::StdRng::seed_from_u64(config.seed);
            batches.shuffle(&mut rng);

            println!("Loaded {} folder batches", batches.len());

            let split = (batches.len() as f32 * 0.8) as usize;
            let (train_batches, val_batches) = batches.split_at(split);

            // Apply augmentation to training data only
            let mut train_augmented = Vec::new();
            for batch in train_batches {
                // Add original
                train_augmented.push(batch.clone());
                // Add 3 augmentations
                for i in 0..AUG_FACTOR {
                    let aug = augment_crop(batch.images.clone(), &device, i);
                    train_augmented.push(DiceBatch {
                        images: aug,
                        targets: batch.targets.clone(),
                    });
                }
            }

            (train_augmented, val_batches.to_vec())
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

    let model = crate::model::DiceHead::new(&device);

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
            StoppingCondition::NoImprovementSince { n_epochs: 15 },
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

    //save
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

    match load_res {
        Ok(_) => {
            let matrix = evaluate_mode(model.valid(), &batches, 20);
            let mut names = vec![];
            for i in 1..=20 {
                names.push(format!("{i}"));
            }

            print_confusion_matrix(&matrix, &names, artifact_dir);
        }

        Err(e) => {
            eprintln!("could not load model: {}", e);
            panic!("");
        }
    }
}

pub fn inferance<B: Backend>(device: B::Device) {}

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
