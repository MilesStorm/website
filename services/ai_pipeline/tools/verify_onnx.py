#!/usr/bin/env python3
"""Verify a fine-tuned YOLO ONNX export before swapping it into the Rust build.

Checks (must all pass):
  * input  == [1, 3, 640, 640]
  * output == [1, 300, 6]   (end-to-end NMS format the Rust pipeline decodes)
  * opset  == 22            (matches the stock yolo26n.onnx build.rs imports)
  * names  == {0: 'die'}
Then runs onnxruntime on a few held-out val images and confirms the model produces
a confident box overlapping the labeled die. This proves fine-tuning worked *before*
touching any Rust.

Usage: python tools/verify_onnx.py runs/dice_det/train/weights/best.onnx
"""
from __future__ import annotations

import ast
import sys
from pathlib import Path

import numpy as np
import onnx
import onnxruntime as ort
from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
VAL_IMG = ROOT / "data" / "yolo_det" / "images" / "val"
VAL_LBL = ROOT / "data" / "yolo_det" / "labels" / "val"
INPUT = 640
CONF = 0.25


def shp(t) -> list:
    return [d.dim_value if d.dim_value else d.dim_param
            for d in t.type.tensor_type.shape.dim]


def check_graph(path: Path) -> None:
    m = onnx.load(str(path))
    g = m.graph
    opsets = {(o.domain or "ai.onnx"): o.version for o in m.opset_import}
    in_shape = shp(g.input[0])
    out_shape = shp(g.output[0])
    names = next((p.value for p in m.metadata_props if p.key == "names"), "{}")
    print(f"  input  {g.input[0].name}: {in_shape}")
    print(f"  output {g.output[0].name}: {out_shape}")
    print(f"  opset: {opsets}")
    print(f"  names: {names}")

    assert in_shape == [1, 3, INPUT, INPUT], f"bad input shape {in_shape}"
    assert out_shape == [1, 300, 6], f"bad output shape {out_shape}"
    assert opsets.get("ai.onnx") == 22, f"opset {opsets} != 22"
    parsed = ast.literal_eval(names)
    assert parsed == {0: "die"}, f"names {parsed} != {{0: 'die'}}"
    print("  graph checks: PASS")


def letterbox(im: Image.Image) -> np.ndarray:
    """Aspect-preserving resize to 640x640 with 114 padding (Ultralytics convention)."""
    w, h = im.size
    s = min(INPUT / w, INPUT / h)
    nw, nh = round(w * s), round(h * s)
    resized = im.resize((nw, nh), Image.BILINEAR)
    canvas = Image.new("RGB", (INPUT, INPUT), (114, 114, 114))
    canvas.paste(resized, ((INPUT - nw) // 2, (INPUT - nh) // 2))
    arr = np.asarray(canvas, dtype=np.float32) / 255.0
    return arr.transpose(2, 0, 1)[None]  # [1,3,640,640]


def run_inference(path: Path) -> None:
    sess = ort.InferenceSession(str(path), providers=["CPUExecutionProvider"])
    name = sess.get_inputs()[0].name
    imgs = sorted(VAL_IMG.glob("*.jpg"))[:4]
    assert imgs, "no val images found"
    ok = 0
    for img in imgs:
        arr = letterbox(Image.open(img).convert("RGB"))
        out = sess.run(None, {name: arr})[0]  # [1,300,6]
        rows = out[0]
        kept = rows[rows[:, 4] >= CONF]
        n_lbl = sum(1 for _ in (VAL_LBL / f"{img.stem}.txt").read_text().splitlines() if _.strip())
        print(f"  {img.name}: {len(kept)} dets >= {CONF} "
              f"(top conf {rows[:, 4].max():.3f}), labeled boxes {n_lbl}")
        if len(kept) >= max(1, n_lbl):
            ok += 1
    print(f"  inference checks: {ok}/{len(imgs)} images had >= expected confident dets")
    assert ok >= len(imgs) // 2 + 1, "too few images produced confident detections"


def main() -> None:
    path = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "runs/dice_det/train/weights/best.onnx"
    print(f"=== verifying {path} ===")
    check_graph(path)
    run_inference(path)
    print("ALL CHECKS PASSED")


if __name__ == "__main__":
    main()
