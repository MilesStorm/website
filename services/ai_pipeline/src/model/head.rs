use burn::{
    Tensor,
    module::Module,
    nn::{
        BatchNorm, BatchNormConfig, Dropout, DropoutConfig, GroupNorm, GroupNormConfig, Linear,
        LinearConfig,
        conv::{Conv2d, Conv2dConfig},
        pool::{AdaptiveAvgPool2d, AdaptiveAvgPool2dConfig},
    },
    prelude::Backend,
    tensor::activation::gelu,
    train::{ClassificationOutput, InferenceStep, TrainOutput, TrainStep},
};

use crate::datasets::DiceBatch;

#[derive(Module, Debug)]
pub struct DiceHead<B: Backend> {
    conv1: Conv2d<B>,
    bn1: BatchNorm<B>,

    conv2: Conv2d<B>,
    bn2: BatchNorm<B>,

    conv3: Conv2d<B>,
    bn3: BatchNorm<B>,
    proj3: Conv2d<B>,

    conv4: Conv2d<B>,
    gn4: GroupNorm<B>,

    conv5: Conv2d<B>,
    bn5: BatchNorm<B>,
    proj5: Conv2d<B>,

    conv6: Conv2d<B>,
    bn6: BatchNorm<B>,

    pool: AdaptiveAvgPool2d,
    dropout: Dropout,
    fc1: Linear<B>,
    fc2: Linear<B>,
}

impl<B: Backend> DiceHead<B> {
    pub fn new(device: &B::Device) -> Self {
        Self {
            conv1: Conv2dConfig::new([3, 48], [3, 3])
                .with_padding(burn::nn::PaddingConfig2d::Same)
                .init(device),
            bn1: BatchNormConfig::new(48).init(device),

            conv2: Conv2dConfig::new([48, 48], [3, 3])
                .with_padding(burn::nn::PaddingConfig2d::Same)
                .init(device),
            bn2: BatchNormConfig::new(48).init(device),

            conv3: Conv2dConfig::new([48, 64], [3, 3])
                .with_padding(burn::nn::PaddingConfig2d::Same)
                .init(device),
            bn3: BatchNormConfig::new(64).init(device),
            proj3: Conv2dConfig::new([48, 64], [1, 1]).init(device),

            conv4: Conv2dConfig::new([64, 64], [3, 3])
                .with_padding(burn::nn::PaddingConfig2d::Same)
                .init(device),
            gn4: GroupNormConfig::new(1, 64).init(device),

            conv5: Conv2dConfig::new([64, 128], [3, 3])
                .with_padding(burn::nn::PaddingConfig2d::Same)
                .init(device),
            bn5: BatchNormConfig::new(128).init(device),
            proj5: Conv2dConfig::new([64, 128], [1, 1]).init(device),

            conv6: Conv2dConfig::new([128, 128], [3, 3])
                .with_padding(burn::nn::PaddingConfig2d::Same)
                .init(device),
            bn6: BatchNormConfig::new(128).init(device),

            pool: AdaptiveAvgPool2dConfig::new([1, 1]).init(),
            dropout: DropoutConfig::new(0.1).init(),
            fc1: LinearConfig::new(128, 128).init(device),
            fc2: LinearConfig::new(128, 20).init(device),
        }
    }

    pub fn forward(&self, x: Tensor<B, 4>) -> Tensor<B, 2> {
        let mut x = gelu(self.bn1.forward(self.conv1.forward(x)));

        let mut residual = x.clone();
        x = self.bn2.forward(self.conv2.forward(x));
        x = gelu(x + residual);

        residual = self.proj3.forward(x.clone());
        x = self.bn3.forward(self.conv3.forward(x));
        x = gelu(x + residual);

        residual = x.clone();
        x = self.gn4.forward(self.conv4.forward(x));
        x = gelu(x + residual);

        residual = self.proj5.forward(x.clone());
        x = self.bn5.forward(self.conv5.forward(x));
        x = gelu(x + residual);

        residual = x.clone();
        x = self.bn6.forward(self.conv6.forward(x));
        x = gelu(x + residual);

        x = self.pool.forward(x);
        let mut x = x.flatten(1, 3);
        x = gelu(self.fc1.forward(x));
        x = self.dropout.forward(x);

        self.fc2.forward(x)
    }
}

impl<B: burn::tensor::backend::AutodiffBackend> TrainStep for DiceHead<B> {
    type Input = DiceBatch<B>;
    type Output = ClassificationOutput<B>;

    fn step(&self, batch: Self::Input) -> TrainOutput<Self::Output> {
        let item = self.forward_classification(batch.images, batch.targets);
        // Check if gradients exist and are non-zero

        TrainOutput::new(self, item.loss.backward(), item)
    }
}

impl<B: Backend> DiceHead<B> {
    /// Forward with classification output for training/validation
    pub fn forward_classification(
        &self,
        images: Tensor<B, 4>,
        targets: Tensor<B, 1, burn::tensor::Int>,
    ) -> ClassificationOutput<B> {
        let output = self.forward(images);
        let loss = burn::nn::loss::CrossEntropyLossConfig::new()
            .with_smoothing(Some(0.05))
            .init(&output.device())
            .forward(output.clone(), targets.clone());
        ClassificationOutput::new(loss, output, targets)
    }
}

impl<B: Backend> InferenceStep for DiceHead<B> {
    type Input = DiceBatch<B>;
    type Output = ClassificationOutput<B>;

    fn step(&self, batch: Self::Input) -> Self::Output {
        self.forward_classification(batch.images, batch.targets)
    }
}

pub fn evaluate_mode<B: Backend>(
    model: DiceHead<B>,
    val_batches: &[DiceBatch<B>],
    num_classes: usize,
) -> Vec<Vec<usize>> {
    let mut matrix = vec![vec![0usize; num_classes]; num_classes];

    for batch in val_batches {
        let output = model.forward(batch.images.clone());

        let predictions = output.argmax(1);
        let targets = batch.targets.clone();

        let pred_data: Vec<i64> = predictions.into_data().convert::<i64>().to_vec().unwrap();
        let true_data: Vec<i64> = targets.into_data().convert::<i64>().to_vec().unwrap();

        for (true_label, pred_label) in true_data.iter().zip(pred_data.iter()) {
            let true_idx = *true_label as usize;
            let pred_idx = *pred_label as usize;
            matrix[true_idx][pred_idx] += 1;
        }
    }
    matrix
}
