use burn::{
    Tensor,
    module::Module,
    nn::{
        BatchNorm, BatchNormConfig, Dropout, DropoutConfig, Linear, LinearConfig,
        PaddingConfig2d,
        conv::{Conv2d, Conv2dConfig},
        pool::{AdaptiveAvgPool2d, AdaptiveAvgPool2dConfig},
    },
    prelude::Backend,
    tensor::activation::relu,
    train::{ClassificationOutput, InferenceStep, TrainOutput, TrainStep},
};

use crate::datasets::DiceBatch;

pub const NUM_CLASSES: usize = 21;

/// One residual block: two stride-1 3x3 convs with BN+GELU, identity add.
/// Channels stay constant inside the block; the preceding stride-2 down-conv
/// already brought the activation to the block's channel/spatial shape.
#[derive(Module, Debug)]
struct ResBlock<B: Backend> {
    conv1: Conv2d<B>,
    bn1: BatchNorm<B>,
    conv2: Conv2d<B>,
    bn2: BatchNorm<B>,
}

impl<B: Backend> ResBlock<B> {
    fn new(channels: usize, device: &B::Device) -> Self {
        Self {
            conv1: Conv2dConfig::new([channels, channels], [3, 3])
                .with_padding(PaddingConfig2d::Same)
                .init(device),
            bn1: BatchNormConfig::new(channels).init(device),
            conv2: Conv2dConfig::new([channels, channels], [3, 3])
                .with_padding(PaddingConfig2d::Same)
                .init(device),
            bn2: BatchNormConfig::new(channels).init(device),
        }
    }

    fn forward(&self, x: Tensor<B, 4>) -> Tensor<B, 4> {
        let residual = x.clone();
        let y = self.bn1.forward(self.conv1.forward(x));
        let y = relu(y);
        let y = self.bn2.forward(self.conv2.forward(y));
        relu(y + residual)
    }
}

/// Spatial size the head's final feature map is pooled to before the FC.
///
/// Crucially this is NOT 1x1. The head now classifies a tight crop of the die's
/// top number (1-2 digits filling the frame) rather than a whole die in context.
/// Global-average-pooling to 1x1 would wash out digit-count/layout information,
/// collapsing "1" and the two-digit "11"/"17" family toward the same pooled
/// vector. Keeping a small 4x4 grid lets the FC see that there are two glyph
/// blobs side by side, which is exactly what separates single- from double-digit
/// values.
const POOL_GRID: usize = 4;

/// Compact SVHN-style digit/glyph recognizer for the die's top number.
///
/// Input is a 64x64 RGB crop (see `HEAD_INPUT` in `inferance.rs`) produced by the
/// YOLO number detector. Colour is kept (3 channels, no desaturation) because
/// glyph colour and font vary by die type/style and carry signal; the net is
/// sized to model several font renderings rather than memorising one.
///
/// Stem keeps full resolution for thin strokes, then three stride-2 stages with
/// one residual block each take 64 -> 32 -> 16 -> 8 while widening 32 -> 64 ->
/// 128 -> 128. The 8x8 map is adaptive-avg-pooled to POOL_GRID and flattened.
/// ~0.9M params: far lighter and faster than the previous 128-input / 384-channel
/// design, which was built for full-die context the head no longer receives.
#[derive(Module, Debug)]
pub struct DiceHead<B: Backend> {
    // Stem: 3 -> 32, stride 1. 64 -> 64 (preserve stroke detail).
    stem_conv: Conv2d<B>,
    stem_bn: BatchNorm<B>,

    // Stage 1: 32 -> 64, stride 2. 64 -> 32.
    s1_down: Conv2d<B>,
    s1_bn: BatchNorm<B>,
    s1_block: ResBlock<B>,

    // Stage 2: 64 -> 128, stride 2. 32 -> 16.
    s2_down: Conv2d<B>,
    s2_bn: BatchNorm<B>,
    s2_block: ResBlock<B>,

    // Stage 3: 128 -> 128, stride 2. 16 -> 8.
    s3_down: Conv2d<B>,
    s3_bn: BatchNorm<B>,
    s3_block: ResBlock<B>,

    pool: AdaptiveAvgPool2d,
    dropout: Dropout,
    fc: Linear<B>,

    #[module(skip)]
    class_weights: Option<Vec<f32>>,
}

impl<B: Backend> DiceHead<B> {
    pub fn new(device: &B::Device) -> Self {
        let down = |in_c: usize, out_c: usize| -> Conv2dConfig {
            Conv2dConfig::new([in_c, out_c], [3, 3])
                .with_stride([2, 2])
                .with_padding(PaddingConfig2d::Same)
        };

        Self {
            stem_conv: Conv2dConfig::new([3, 32], [3, 3])
                .with_padding(PaddingConfig2d::Same)
                .init(device),
            stem_bn: BatchNormConfig::new(32).init(device),

            s1_down: down(32, 64).init(device),
            s1_bn: BatchNormConfig::new(64).init(device),
            s1_block: ResBlock::new(64, device),

            s2_down: down(64, 128).init(device),
            s2_bn: BatchNormConfig::new(128).init(device),
            s2_block: ResBlock::new(128, device),

            s3_down: down(128, 128).init(device),
            s3_bn: BatchNormConfig::new(128).init(device),
            s3_block: ResBlock::new(128, device),

            pool: AdaptiveAvgPool2dConfig::new([POOL_GRID, POOL_GRID]).init(),
            dropout: DropoutConfig::new(0.3).init(),
            fc: LinearConfig::new(128 * POOL_GRID * POOL_GRID, NUM_CLASSES).init(device),
            class_weights: None,
        }
    }

    pub fn with_class_weights(mut self, weights: Vec<f32>) -> Self {
        assert_eq!(
            weights.len(),
            NUM_CLASSES,
            "class_weights len must equal NUM_CLASSES"
        );
        self.class_weights = Some(weights);
        self
    }

    pub fn forward(&self, x: Tensor<B, 4>) -> Tensor<B, 2> {
        let x = relu(self.stem_bn.forward(self.stem_conv.forward(x)));

        let x = relu(self.s1_bn.forward(self.s1_down.forward(x)));
        let x = self.s1_block.forward(x);

        let x = relu(self.s2_bn.forward(self.s2_down.forward(x)));
        let x = self.s2_block.forward(x);

        let x = relu(self.s3_bn.forward(self.s3_down.forward(x)));
        let x = self.s3_block.forward(x);

        let x = self.pool.forward(x);
        let x = x.flatten(1, 3);
        let x = self.dropout.forward(x);
        self.fc.forward(x)
    }
}

impl<B: burn::tensor::backend::AutodiffBackend> TrainStep for DiceHead<B> {
    type Input = DiceBatch<B>;
    type Output = ClassificationOutput<B>;

    fn step(&self, batch: Self::Input) -> TrainOutput<Self::Output> {
        let item = self.forward_classification(batch.images, batch.targets);
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
            .with_weights(self.class_weights.clone())
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
    evaluate_with_tta(model, val_batches, num_classes, 0)
}

/// Evaluate with test-time augmentation. Averages logits across the
/// original crop plus `num_tta` mildly-augmented copies (rotation + color
/// jitter only; no spatial crop). `num_tta = 0` reduces to the plain
/// single-pass evaluator.
pub fn evaluate_with_tta<B: Backend>(
    model: DiceHead<B>,
    val_batches: &[DiceBatch<B>],
    num_classes: usize,
    num_tta: usize,
) -> Vec<Vec<usize>> {
    use crate::datasets::augment_crop_tta;

    let mut matrix = vec![vec![0usize; num_classes]; num_classes];

    for batch in val_batches {
        let device = batch.images.device();
        let mut output = model.forward(batch.images.clone());

        for _ in 0..num_tta {
            let augmented = augment_crop_tta(batch.images.clone(), &device);
            output = output + model.forward(augmented);
        }

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
