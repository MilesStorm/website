//! Serving-side dice value classifier: ImageNet-pretrained ResNet18, fine-tuned
//! in PyTorch by `tools/train_head_torch.py` and loaded here from safetensors.
//!
//! The module tree mirrors torchvision's `resnet18` field names so the PyTorch
//! state dict maps 1:1 (see `load`): conv1/bn1, layer1..layer4 of two
//! BasicBlocks, fc. Two differences from stock torchvision, matching training:
//! the stem max-pool is removed (64x64 crops would shrink to 2x2 otherwise) and
//! ImageNet normalisation happens inside `forward`, so the input is the same
//! [0,1] RGB crop the rest of the pipeline produces.

use std::path::Path;

use burn::{
    Tensor,
    module::Module,
    nn::{
        BatchNorm, BatchNormConfig, Linear, LinearConfig, PaddingConfig2d,
        conv::{Conv2d, Conv2dConfig},
        pool::{AdaptiveAvgPool2d, AdaptiveAvgPool2dConfig},
    },
    prelude::Backend,
    tensor::{TensorData, activation::relu},
};
use burn_store::{ModuleSnapshot, PyTorchToBurnAdapter, SafetensorsStore};

use crate::model::head::NUM_CLASSES;

const IMAGENET_MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const IMAGENET_STD: [f32; 3] = [0.229, 0.224, 0.225];

/// torchvision convs have no bias (BN follows) and explicit symmetric padding.
fn conv(in_c: usize, out_c: usize, k: usize, stride: usize, pad: usize) -> Conv2dConfig {
    Conv2dConfig::new([in_c, out_c], [k, k])
        .with_stride([stride, stride])
        .with_padding(PaddingConfig2d::Explicit(pad, pad, pad, pad))
        .with_bias(false)
}

/// torchvision `downsample`: 1x1 strided conv + BN on the shortcut path.
#[derive(Module, Debug)]
struct Downsample<B: Backend> {
    conv: Conv2d<B>,
    bn: BatchNorm<B>,
}

#[derive(Module, Debug)]
struct BasicBlock<B: Backend> {
    conv1: Conv2d<B>,
    bn1: BatchNorm<B>,
    conv2: Conv2d<B>,
    bn2: BatchNorm<B>,
    downsample: Option<Downsample<B>>,
}

impl<B: Backend> BasicBlock<B> {
    fn new(in_c: usize, out_c: usize, stride: usize, device: &B::Device) -> Self {
        let downsample = (stride != 1 || in_c != out_c).then(|| Downsample {
            conv: conv(in_c, out_c, 1, stride, 0).init(device),
            bn: BatchNormConfig::new(out_c).init(device),
        });
        Self {
            conv1: conv(in_c, out_c, 3, stride, 1).init(device),
            bn1: BatchNormConfig::new(out_c).init(device),
            conv2: conv(out_c, out_c, 3, 1, 1).init(device),
            bn2: BatchNormConfig::new(out_c).init(device),
            downsample,
        }
    }

    fn forward(&self, x: Tensor<B, 4>) -> Tensor<B, 4> {
        let identity = match &self.downsample {
            Some(d) => d.bn.forward(d.conv.forward(x.clone())),
            None => x.clone(),
        };
        let y = relu(self.bn1.forward(self.conv1.forward(x)));
        let y = self.bn2.forward(self.conv2.forward(y));
        relu(y + identity)
    }
}

#[derive(Module, Debug)]
pub struct ResNet18Head<B: Backend> {
    conv1: Conv2d<B>,
    bn1: BatchNorm<B>,
    layer1: Vec<BasicBlock<B>>,
    layer2: Vec<BasicBlock<B>>,
    layer3: Vec<BasicBlock<B>>,
    layer4: Vec<BasicBlock<B>>,
    avgpool: AdaptiveAvgPool2d,
    fc: Linear<B>,
}

impl<B: Backend> ResNet18Head<B> {
    /// Randomly initialised network with the trained architecture.
    pub fn new(device: &B::Device) -> Self {
        let layer = |in_c: usize, out_c: usize, stride: usize| {
            vec![BasicBlock::new(in_c, out_c, stride, device), BasicBlock::new(out_c, out_c, 1, device)]
        };
        Self {
            conv1: conv(3, 64, 7, 2, 3).init(device),
            bn1: BatchNormConfig::new(64).init(device),
            layer1: layer(64, 64, 1),
            layer2: layer(64, 128, 2),
            layer3: layer(128, 256, 2),
            layer4: layer(256, 512, 2),
            avgpool: AdaptiveAvgPool2dConfig::new([1, 1]).init(),
            fc: LinearConfig::new(512, NUM_CLASSES).init(device),
        }
    }

    /// Load weights exported by `train_head_torch.py --all`
    /// (`dice_head_resnet18.safetensors`, a PyTorch state dict).
    ///
    /// Key mapping: training wrapped torchvision's net as `m.*` and its classifier as
    /// `fc = Sequential(Dropout, Linear)`, and torchvision names the shortcut
    /// `downsample.0/1`. The PyTorch adapter transposes Linear weights and maps BN
    /// `weight/bias` to burn's `gamma/beta`. Every parameter must be matched: a
    /// partially loaded model would silently return garbage.
    pub fn load(path: &Path, device: &B::Device) -> anyhow::Result<Self> {
        let mut model = Self::new(device);
        let mut store = SafetensorsStore::from_file(path)
            .with_from_adapter(PyTorchToBurnAdapter)
            .with_key_remapping(r"^m\.", "")
            .with_key_remapping(r"^fc\.1\.", "fc.")
            .with_key_remapping(r"\.downsample\.0\.", ".downsample.conv.")
            .with_key_remapping(r"\.downsample\.1\.", ".downsample.bn.")
            .allow_partial(false);
        let result = model
            .load_from(&mut store)
            .map_err(|e| anyhow::anyhow!("loading {}: {e}", path.display()))?;
        anyhow::ensure!(
            result.missing.is_empty() && result.errors.is_empty(),
            "{}: missing {:?}, errors {:?}",
            path.display(),
            result.missing,
            result.errors
        );
        Ok(model)
    }

    /// `x`: [N, 3, 64, 64] RGB in [0, 1]. Returns [N, NUM_CLASSES] logits.
    pub fn forward(&self, x: Tensor<B, 4>) -> Tensor<B, 2> {
        let device = x.device();
        let stat = |v: [f32; 3]| Tensor::<B, 4>::from_data(TensorData::new(v.to_vec(), [1, 3, 1, 1]), &device);
        let x = (x - stat(IMAGENET_MEAN)) / stat(IMAGENET_STD);

        let mut x = relu(self.bn1.forward(self.conv1.forward(x)));
        for block in self.layer1.iter().chain(&self.layer2).chain(&self.layer3).chain(&self.layer4) {
            x = block.forward(x);
        }
        let x = self.avgpool.forward(x).flatten(1, 3);
        self.fc.forward(x)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use burn::backend::{Cuda, cuda::CudaDevice};
    use burn::tensor::{Tensor, TensorData};

    use super::ResNet18Head;

    type B = Cuda<f32>;

    fn read_f32(path: PathBuf) -> Vec<f32> {
        std::fs::read(&path)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect()
    }

    /// Burn must reproduce PyTorch's logits for the exported weights.
    /// Needs a GPU and the files from `train_head_torch.py --all`:
    ///   PARITY_DIR=runs/head_torch/resnet18_final cargo test --release parity -- --ignored
    #[test]
    #[ignore]
    fn parity_with_pytorch() {
        let dir = PathBuf::from(std::env::var("PARITY_DIR").expect("set PARITY_DIR"));
        let device = CudaDevice::new(0);
        let model =
            ResNet18Head::<B>::load(&dir.join("dice_head_resnet18.safetensors"), &device).unwrap();

        let torch_inputs = read_f32(dir.join("parity_inputs.f32"));
        let expected = read_f32(dir.join("parity_logits.f32"));
        let n = expected.len() / crate::model::head::NUM_CLASSES;
        assert_eq!(torch_inputs.len(), n * 3 * 64 * 64);

        // Pack the same crop PNGs with the serving code: this is what production
        // feeds the model, so it must equal the tensor PyTorch was given.
        let crops = PathBuf::from("data/dice_face_crops");
        let keys = std::fs::read_to_string(dir.join("parity_keys.txt")).unwrap();
        let inputs: Vec<f32> = keys
            .lines()
            .filter(|k| !k.is_empty())
            .flat_map(|k| {
                let img = image::open(crops.join(format!("{k}.png"))).unwrap().to_rgb8();
                crate::model::inferance::pack_chw(&img)
            })
            .collect();
        assert_eq!(inputs.len(), torch_inputs.len());
        let pack_diff =
            inputs.iter().zip(&torch_inputs).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
        assert!(pack_diff <= 1e-6, "serving preprocessing differs from training: max {pack_diff}");

        let x = Tensor::<B, 4>::from_data(TensorData::new(inputs, [n, 3, 64, 64]), &device);
        let got: Vec<f32> = model.forward(x).into_data().convert::<f32>().to_vec().unwrap();

        // Compare what the service acts on: class probabilities (the confidence
        // threshold) and the chosen class. Raw logits also get a relative bound;
        // GPU float rounding differs slightly between PyTorch and burn kernels.
        let softmax = |v: &[f32]| -> Vec<f32> {
            v.chunks_exact(crate::model::head::NUM_CLASSES)
                .flat_map(|r| {
                    let m = r.iter().cloned().fold(f32::MIN, f32::max);
                    let e: Vec<f32> = r.iter().map(|x| (x - m).exp()).collect();
                    let s: f32 = e.iter().sum();
                    e.into_iter().map(move |x| x / s)
                })
                .collect()
        };
        let max_abs = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0f32, f32::max);
        let logit_diff = max_abs(&got, &expected);
        let logit_scale = expected.iter().fold(0f32, |m, x| m.max(x.abs()));
        let prob_diff = max_abs(&softmax(&got), &softmax(&expected));
        let argmax = |v: &[f32]| {
            v.chunks_exact(crate::model::head::NUM_CLASSES)
                .map(|r| r.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap().0)
                .collect::<Vec<_>>()
        };
        let same = argmax(&got).iter().zip(argmax(&expected)).filter(|(a, b)| **a == *b).count();
        println!(
            "parity: {n} crops, preprocessing max diff {pack_diff:.1e}, max |logit diff| {logit_diff:.2e} \
             (logits up to {logit_scale:.1}), max |prob diff| {prob_diff:.2e}, argmax agree {same}/{n}"
        );
        assert!(prob_diff <= 1e-3, "max probability diff {prob_diff}");
        assert!(logit_diff <= 1e-3 * logit_scale.max(1.0), "max logit diff {logit_diff} vs scale {logit_scale}");
        assert_eq!(same, n, "argmax disagreement");
    }
}
