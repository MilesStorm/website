#!/usr/bin/env python3
"""Export the fine-tuned detector to ONNX with the exact same recipe as the
original src/model/yolo26n.onnx, then verify the contract and install it.

The export args below were recovered from the original file's embedded
metadata (ultralytics 8.4.7 / pytorch 2.9.1 / opset 22):
    {'batch': 1, 'half': False, 'dynamic': False, 'simplify': True, 'opset': None}

Usage:
    python export_detector.py runs/dice_detector/weights/best.pt
    python export_detector.py best.pt --out ../src/model/yolo26n.onnx --image die.jpg
"""

from __future__ import annotations

import argparse
import shutil
import sys
from pathlib import Path

from verify_export import smoke_infer, verify

DEFAULT_OUT = Path(__file__).resolve().parent.parent / "src" / "model" / "yolo26n.onnx"


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("weights", type=Path, help="fine-tuned checkpoint (best.pt)")
    ap.add_argument("--out", type=Path, default=DEFAULT_OUT,
                    help=f"where to install the ONNX (default: {DEFAULT_OUT})")
    ap.add_argument("--image", type=Path, help="optional die photo for smoke inference")
    args = ap.parse_args()

    from ultralytics import YOLO

    model = YOLO(str(args.weights))
    exported = Path(model.export(
        format="onnx",
        imgsz=640,
        batch=1,
        half=False,
        dynamic=False,
        simplify=True,
        device="cpu",
    ))
    print(f"\nexported: {exported}")

    print(f"\nverifying {exported}")
    if not verify(exported, expect_classes=1):
        sys.exit("\ncontract check FAILED — not installing. The export does not "
                 "match the [1,300,6] end-to-end contract; check ultralytics/torch "
                 "versions against training/requirements.txt")

    if args.image:
        smoke_infer(exported, args.image)

    if args.out.exists():
        backup = args.out.with_suffix(".onnx.bak")
        shutil.copy2(args.out, backup)
        print(f"\nbacked up previous model to {backup}")
    args.out.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(exported, args.out)
    print(f"installed -> {args.out}")
    print("\nnext: cargo build --release   (build.rs regenerates the Burn graph "
          "and .bpk from the new ONNX)")


if __name__ == "__main__":
    main()
