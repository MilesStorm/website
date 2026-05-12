mod datasets;
mod helper;
pub mod model;

use std::{env, path::Path};

use burn::{
    backend::{Autodiff, Cuda, cuda::CudaDevice},
    optim::AdamConfig,
    tensor::bf16,
};

use crate::{
    datasets::dataset::DatasetType,
    helper::{latest_experiment_dir, next_experiment_dir},
    model::training::{TrainingConfig, eval, train},
};
const ART_ROOT: &str = "./art";
// Add a quick test in main to verify dataset loading.

fn main() -> anyhow::Result<()> {
    type MyBackend = Autodiff<Cuda<bf16>>;

    let args: Vec<String> = env::args().collect();

    println!("creating device..");

    let device = CudaDevice::new(0);

    println!("Done.");

    let config = TrainingConfig::new(AdamConfig::new())
        .with_num_epochs(80)
        .with_batch_size(128)
        .with_num_workers(0)
        .with_seed(42)
        .with_learning_rate(1e-3)
        .with_weight_decay(5e-5);

    if args.contains(&String::from("yolo")) {
        anyhow::bail!(
            "`yolo` is the production inference path (camera -> YOLO bbox -> DiceHead) and \
             is not implemented yet. Use `folder` to train the head, or `eval` to score it \
             against data/dice_face."
        );
    } else if args.contains(&String::from("eval")) {
        let exp_dir = latest_experiment_dir(Path::new(ART_ROOT))
            .unwrap_or_else(|| panic!("No experiment_* directories found in {}", ART_ROOT));
        let exp_dir_str = exp_dir.to_string_lossy().to_string();

        let data_path = std::path::Path::new("./data/dice_face");
        eval::<MyBackend>(&exp_dir_str, device, data_path, DatasetType::Folder);
    } else if args.contains(&String::from("folder")) {
        let exp_dir = next_experiment_dir(Path::new(ART_ROOT));
        let exp_dir_str = exp_dir.to_string_lossy().to_string();

        let data_path = std::path::Path::new("./data/dice_face");
        train::<MyBackend>(&exp_dir_str, config, device, data_path, DatasetType::Folder);
    } else {
        // evaluation pipeline should go here
    }

    Ok(())
}
