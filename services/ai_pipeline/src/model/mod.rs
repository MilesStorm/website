pub mod head;
pub mod training;
use burn::optim::decay::WeightDecayConfig;
pub use head::DiceHead;

use burn::module::AutodiffModule;
use burn::nn::loss::CrossEntropyLossConfig;
use burn::optim::{AdamConfig, GradientsParams, Optimizer};
use burn::tensor::ElementConversion;
use burn::tensor::backend::AutodiffBackend;

use crate::datasets::DiceBatch;

pub mod my_model {
    #![allow(warnings)]
    include!(concat!(env!("OUT_DIR"), "/model/yolo26n.rs"));
}

pub struct DiceHeadTrainer<B: AutodiffBackend> {
    model: DiceHead<B>,
    optim: AdamConfig,
}

impl<B: AutodiffBackend> DiceHeadTrainer<B> {
    pub fn model(&self) -> &DiceHead<B> {
        &self.model
    }

    pub fn new(device: B::Device) -> Self {
        let model = DiceHead::new(&device);

        Self {
            model,
            optim: AdamConfig::new().with_weight_decay(Some(WeightDecayConfig::new(1e-4))),
        }
    }

    pub fn train_step(&mut self, batch: DiceBatch<B>) -> f32 {
        let output = self.model.forward(batch.images);

        let loss = CrossEntropyLossConfig::new()
            .init(&output.device())
            .forward(output.clone(), batch.targets.clone());

        let grads = loss.backward();
        let grads = GradientsParams::from_grads(grads, &self.model);

        let lr = 3e-4f64;
        self.model = self
            .optim
            .init::<B, DiceHead<B>>()
            .step(lr, self.model.clone(), grads);

        loss.into_scalar().elem::<f32>()
    }

    pub fn train_epoch(&mut self, batches: &[DiceBatch<B>]) -> f32 {
        let mut total_loss = 0.0;

        for batch in batches.iter() {
            let loss = self.train_step(batch.clone());
            total_loss += loss;
        }

        total_loss / batches.len() as f32
    }
}

pub fn validate<B: AutodiffBackend>(model: &DiceHead<B>, batches: &[DiceBatch<B>]) -> f32 {
    let model_valid = model.valid(); // InnerBackend tensors

    let mut total_loss = 0.0;

    for batch in batches {
        let images = batch.images.clone().inner();
        let targets = batch.targets.clone().inner();

        let output = model_valid.forward(images);
        // let loss = CrossEntropyLoss::new(None, &device).forward(output, targets);
        let loss = CrossEntropyLossConfig::new()
            .init(&output.device())
            .forward(output.clone(), targets.clone());

        total_loss += loss.into_scalar().elem::<f32>();
    }

    total_loss / batches.len() as f32
}
