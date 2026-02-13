mod datasets;
mod helper;
mod inferance;
pub mod model;

use std::{env, path::Path};

#[cfg(target_os = "macos")]
use burn::backend::Metal;
use burn::{
    backend::{
        Autodiff, Cuda, autodiff::checkpoint::strategy::BalancedCheckpointing, cuda::CudaDevice,
    },
    optim::AdamConfig,
};

use crate::{
    datasets::dataset::DatasetType,
    helper::{latest_experiment_dir, next_experiment_dir},
    model::training::{TrainingConfig, eval, train},
};
const ART_ROOT: &str = "./art";
// Add a quick test in main to verify dataset loading.

fn main() -> anyhow::Result<()> {
    type MyBackend = Autodiff<Cuda, BalancedCheckpointing>;

    let args: Vec<String> = env::args().collect();

    println!("creating device..");

    #[cfg(target_os = "linux")]
    let device = CudaDevice::new(0);

    #[cfg(target_os = "macos")]
    let device = Metal::default();

    println!("Done.");

    let config = TrainingConfig::new(AdamConfig::new())
        .with_num_epochs(150)
        .with_batch_size(32)
        .with_num_workers(4)
        .with_seed(42)
        .with_learning_rate(5e-4)
        .with_weight_decay(5e-5);

    if args.contains(&String::from("yolo")) {
        let data_path = std::path::Path::new("./data/dice_bbox");
        train::<MyBackend>("./art", config, device, data_path, DatasetType::YOLO);
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
    }

    Ok(())
}
