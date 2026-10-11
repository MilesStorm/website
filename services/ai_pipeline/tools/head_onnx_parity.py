#!/usr/bin/env python3
"""Check the dice head's ONNX model against PyTorch:
head_onnx_parity.py <weights.safetensors> <model.onnx> [<model_fp16.onnx>]

Loads the same weights into the network `train_head_torch.py` trains and compares its
logits with ONNX Runtime's on random crops. Run it after changing `head_onnx.py` or the
half-precision conversion. The f32 model must match to rounding error; the half-precision
one must pick the same class with nearly the same probability.

Needs: pip install torch torchvision onnxruntime
"""
import sys

import numpy as np
import onnx
import onnxruntime as ort
import torch
import torchvision.models as tvm
from torch import nn

from head_onnx import CLASSES, MEAN, SIZE, STD, read_safetensors

weights_path, onnx_paths = sys.argv[1], sys.argv[2:]

net = tvm.resnet18()
net.maxpool = nn.Identity()
net.fc = nn.Sequential(nn.Dropout(0.3), nn.Linear(net.fc.in_features, CLASSES))
state = {k.removeprefix("m."): torch.from_numpy(v.copy()) for k, v in read_safetensors(weights_path).items()}
net.load_state_dict(state, strict=True)
net.eval()

crops = np.random.default_rng(0).random((64, 3, SIZE, SIZE), dtype=np.float32)
with torch.no_grad():
    x = torch.from_numpy(crops)
    expected = net((x - torch.tensor(MEAN).view(1, 3, 1, 1)) / torch.tensor(STD).view(1, 3, 1, 1)).numpy()


def softmax(logits: np.ndarray) -> np.ndarray:
    e = np.exp(logits - logits.max(axis=1, keepdims=True))
    return e / e.sum(axis=1, keepdims=True)


for path in onnx_paths:
    session = ort.InferenceSession(path, providers=["CPUExecutionProvider"])
    got = session.run(["logits"], {"crops": crops})[0]
    logit_diff = np.abs(got - expected).max()
    prob_diff = np.abs(softmax(got) - softmax(expected)).max()
    agree = int((got.argmax(axis=1) == expected.argmax(axis=1)).sum())
    print(f"{path}: max |logit diff| {logit_diff:.2e}, max |prob diff| {prob_diff:.2e}, "
          f"same class {agree}/{len(crops)}")
    half = any(t.data_type == onnx.TensorProto.FLOAT16 for t in onnx.load(path).graph.initializer)
    assert prob_diff <= (2e-2 if half else 1e-4), f"{path}: probabilities differ by {prob_diff}"
    assert agree == len(crops), f"{path}: class disagreement"
