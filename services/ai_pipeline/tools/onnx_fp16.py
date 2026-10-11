#!/usr/bin/env python3
"""Make the half-precision copy of a model: onnx_fp16.py <in.onnx> <out.onnx>.

Weights and activations become float16; the input and output stay float32, so the
service feeds and reads it exactly like the f32 model. The image build runs this
(Dockerfile, stage `models`); the result is not committed.

Needs: pip install onnx onnxruntime sympy
"""
import sys

import onnx
from onnxruntime.transformers import float16
from onnxruntime.transformers.onnx_model import OnnxModel

source, target = sys.argv[1], sys.argv[2]
model = float16.convert_float_to_float16(onnx.load(source), keep_io_types=True)
# The converter appends its Cast nodes at the end: put the graph back in order.
sorted_model = OnnxModel(model)
sorted_model.topological_sort()
onnx.checker.check_model(sorted_model.model)
onnx.save(sorted_model.model, target)
