#!/usr/bin/env python3
"""Build the dice head's ONNX model from its weights: head_onnx.py <in.safetensors> <out.onnx>.

The weights are the PyTorch state dict `train_head_torch.py --all` exports: torchvision's
ResNet18 without the stem max-pool, with a 21-class classifier. The graph written here is
that network, with the ImageNet normalisation inside it as in training:

  input  `crops`  [N, 3, 64, 64] f32, RGB in [0, 1]
  output `logits` [N, 21] f32

The image build runs this (Dockerfile, stage `models`); the result is not committed.
`tools/head_onnx_parity.py` checks it against PyTorch.

Needs: pip install onnx
"""
import json
import sys

import numpy as np
import onnx
from onnx import TensorProto, helper, numpy_helper

MEAN = [0.485, 0.456, 0.406]
STD = [0.229, 0.224, 0.225]
CLASSES = 21
SIZE = 64


def read_safetensors(path: str) -> dict[str, np.ndarray]:
    """8-byte LE header length, JSON header, then raw little-endian f32 data."""
    with open(path, "rb") as f:
        header = json.loads(f.read(int.from_bytes(f.read(8), "little")))
        data = f.read()
    tensors = {}
    for name, info in header.items():
        if name == "__metadata__":
            continue
        assert info["dtype"] == "F32", f"{name}: {info['dtype']}"
        start, end = info["data_offsets"]
        tensors[name] = np.frombuffer(data[start:end], dtype="<f4").reshape(info["shape"])
    return tensors


class Graph:
    def __init__(self, weights: dict[str, np.ndarray]):
        self.weights = weights
        self.used: set[str] = set()
        self.nodes: list[onnx.NodeProto] = []
        self.initializers: list[onnx.TensorProto] = []

    def weight(self, name: str) -> str:
        """Adds the named weight to the graph once and returns its name."""
        if name not in self.used:
            self.used.add(name)
            self.initializers.append(numpy_helper.from_array(self.weights[name], name))
        return name

    def constant(self, name: str, values: list[float], shape: list[int]) -> str:
        self.initializers.append(numpy_helper.from_array(np.array(values, dtype="<f4").reshape(shape), name))
        return name

    def node(self, op: str, inputs: list[str], name: str, **attrs) -> str:
        self.nodes.append(helper.make_node(op, inputs, [name], name=name, **attrs))
        return name

    def conv_bn(self, x: str, conv: str, bn: str, kernel: int, stride: int, pad: int) -> str:
        y = self.node("Conv", [x, self.weight(f"{conv}.weight")], conv,
                      kernel_shape=[kernel, kernel], strides=[stride, stride], pads=[pad] * 4)
        stats = [self.weight(f"{bn}.{part}") for part in ("weight", "bias", "running_mean", "running_var")]
        # torch.nn.BatchNorm2d's default eps.
        return self.node("BatchNormalization", [y, *stats], bn, epsilon=1e-5)

    def block(self, x: str, name: str, stride: int, downsample: bool) -> str:
        y = self.conv_bn(x, f"{name}.conv1", f"{name}.bn1", 3, stride, 1)
        y = self.node("Relu", [y], f"{name}.relu1")
        y = self.conv_bn(y, f"{name}.conv2", f"{name}.bn2", 3, 1, 1)
        if downsample:
            x = self.conv_bn(x, f"{name}.downsample.0", f"{name}.downsample.1", 1, stride, 0)
        return self.node("Relu", [self.node("Add", [y, x], f"{name}.add")], f"{name}.relu2")


def build(weights: dict[str, np.ndarray]) -> onnx.ModelProto:
    g = Graph(weights)
    x = g.node("Sub", ["crops", g.constant("mean", MEAN, [1, 3, 1, 1])], "centered")
    x = g.node("Div", [x, g.constant("std", STD, [1, 3, 1, 1])], "normalized")
    # No max-pool after the stem: 64x64 crops would shrink to 2x2 otherwise.
    x = g.node("Relu", [g.conv_bn(x, "m.conv1", "m.bn1", 7, 2, 3)], "m.relu")
    for layer, stride in ((1, 1), (2, 2), (3, 2), (4, 2)):
        x = g.block(x, f"m.layer{layer}.0", stride, downsample=stride != 1)
        x = g.block(x, f"m.layer{layer}.1", 1, downsample=False)
    x = g.node("Flatten", [g.node("GlobalAveragePool", [x], "m.avgpool")], "pooled", axis=1)
    # The classifier is Sequential(Dropout, Linear): the Linear is index 1.
    g.nodes.append(helper.make_node(
        "Gemm", [x, g.weight("m.fc.1.weight"), g.weight("m.fc.1.bias")], ["logits"], name="m.fc.1", transB=1))

    # A weight the graph does not use means this is not the network the file was trained as.
    unused = sorted(set(weights) - g.used)
    assert not unused, f"weights not used by the graph: {unused}"

    graph = helper.make_graph(
        g.nodes, "dice_head_resnet18",
        [helper.make_tensor_value_info("crops", TensorProto.FLOAT, ["n", 3, SIZE, SIZE])],
        [helper.make_tensor_value_info("logits", TensorProto.FLOAT, ["n", CLASSES])],
        g.initializers,
    )
    model = helper.make_model(graph, opset_imports=[helper.make_opsetid("", 17)], producer_name="head_onnx.py")
    model.ir_version = 8
    onnx.checker.check_model(model)
    return model


if __name__ == "__main__":
    source, target = sys.argv[1], sys.argv[2]
    onnx.save(build(read_safetensors(source)), target)
