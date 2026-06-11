#!/usr/bin/env python3
"""Verify an exported dice-detector ONNX matches the contract the Rust side expects.

Contract (recovered from the original src/model/yolo26n.onnx and relied on by
the Burn codegen + the chunks_exact(6) parser in src/model/inferance.rs):

    input  "images"  : [1, 3, 640, 640] float32, RGB, 0..1
    output "output0" : [1, 300, 6], rows of [x1, y1, x2, y2, conf, class]
                       (pixel coords in 640-space, NMS-free end-to-end topk)
    opset 22, end2end=True, batch=1, static shapes

Usage:
    python verify_export.py path/to/model.onnx [--classes 1] [--image die.jpg]

--image runs an onnxruntime smoke inference using the same preprocessing as
inferance.rs (plain squash resize to 640, /255, CHW) and prints the top rows.
"""

from __future__ import annotations

import argparse
import ast
import sys
from pathlib import Path


def _dims(value_info) -> list:
    return [d.dim_value or d.dim_param for d in value_info.type.tensor_type.shape.dim]


def verify(path: Path, expect_classes: int | None = 1) -> bool:
    import onnx

    model = onnx.load(str(path))
    meta = {p.key: p.value for p in model.metadata_props}
    opsets = {o.domain: o.version for o in model.opset_import}
    ok = True

    def check(cond: bool, label: str, detail: str) -> None:
        nonlocal ok
        print(f"  {'OK  ' if cond else 'FAIL'} {label}: {detail}")
        ok = ok and cond

    inp, out = model.graph.input[0], model.graph.output[0]
    check(_dims(inp) == [1, 3, 640, 640], "input shape",
          f"{inp.name} {_dims(inp)} (want [1, 3, 640, 640])")
    check(_dims(out) == [1, 300, 6], "output shape",
          f"{out.name} {_dims(out)} (want [1, 300, 6])")
    check(opsets.get("") == 22, "opset",
          f"{opsets} (want {{'': 22}}; a different opset may change the Burn codegen)")
    check(meta.get("end2end") == "True", "end2end", meta.get("end2end", "<missing>"))

    names = ast.literal_eval(meta.get("names", "{}"))
    if expect_classes is not None:
        check(len(names) == expect_classes, "class count",
              f"{len(names)} {names if len(names) <= 4 else ''} (want {expect_classes})")
    else:
        print(f"  INFO class names: {len(names)} classes")

    args = meta.get("args", "")
    check("'dynamic': False" in args and "'half': False" in args, "export args", args)
    return ok


def smoke_infer(path: Path, image: Path, conf: float = 0.10) -> None:
    import numpy as np
    import onnxruntime as ort
    from PIL import Image

    img = Image.open(image).convert("RGB").resize((640, 640), Image.BILINEAR)
    x = np.asarray(img, dtype=np.float32).transpose(2, 0, 1)[None] / 255.0

    sess = ort.InferenceSession(str(path), providers=["CPUExecutionProvider"])
    (out,) = sess.run(None, {sess.get_inputs()[0].name: x})
    rows = out[0]
    rows = rows[rows[:, 4].argsort()[::-1]]

    print(f"\n  smoke inference on {image.name}: output {out.shape}, "
          f"max conf {rows[0, 4]:.3f}")
    hits = rows[rows[:, 4] >= conf][:10]
    if len(hits) == 0:
        print(f"  no rows above conf {conf} — fine for a random image, "
              "worrying for a clear photo of a die")
    for r in hits:
        print(f"    box=({r[0]:6.1f},{r[1]:6.1f},{r[2]:6.1f},{r[3]:6.1f}) "
              f"conf={r[4]:.3f} class={int(r[5])}")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("onnx_path", type=Path)
    ap.add_argument("--classes", type=int, default=1,
                    help="expected class count (1 for the dice model, 80 for stock; "
                         "pass -1 to skip)")
    ap.add_argument("--image", type=Path, help="optional test image for smoke inference")
    args = ap.parse_args()

    print(f"verifying {args.onnx_path}")
    ok = verify(args.onnx_path, None if args.classes < 0 else args.classes)

    if args.image:
        smoke_infer(args.onnx_path, args.image)

    if not ok:
        sys.exit("\ncontract check FAILED — do not swap this model in")
    print("\ncontract check passed — safe to swap into src/model/yolo26n.onnx and rebuild")


if __name__ == "__main__":
    main()
