#!/usr/bin/env python3
"""Fine-tune yolo26s on the single-class dice dataset and export to ONNX.

Run after tools/prepare_yolo_dataset.py. Produces:
  runs/dice_det/<run>/weights/best.pt
  runs/dice_det/<run>/weights/best.onnx   (end-to-end, output [1,300,6])

The ONNX is exported at opset 22 to match the stock src/model/yolo26n.onnx that
build.rs already imports cleanly into burn. yolo26 is natively NMS-free/end-to-end,
so the export reproduces the [x1,y1,x2,y2,conf,class] top-300 box format the Rust
pipeline (src/model/inferance.rs) decodes.

Usage:
  python tools/train_yolo.py [--name train] [--epochs 120] [--data <data.yaml>]

Round 1 (bbox only): defaults.
Round 2 (self-trained): point --data at the combined yaml produced by
tools/pseudo_label_face.py and use fewer epochs (more data -> fewer passes).
"""
from __future__ import annotations

import argparse
from pathlib import Path

from ultralytics import YOLO

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_DATA = ROOT / "data" / "yolo_det" / "data.yaml"
PROJECT = ROOT / "runs" / "dice_det"
OPSET = 22


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--name", default="train")
    ap.add_argument("--epochs", type=int, default=120)
    ap.add_argument("--data", default=str(DEFAULT_DATA))
    args = ap.parse_args()

    model = YOLO("yolo26s.pt")  # auto-downloads pretrained weights once

    results = model.train(
        data=args.data,
        epochs=args.epochs,
        imgsz=640,
        batch=16,
        patience=30,
        seed=42,
        close_mosaic=15,
        project=str(PROJECT),
        name=args.name,
        exist_ok=True,
    )

    best = Path(results.save_dir) / "weights" / "best.pt"
    print(f"\nbest weights: {best}")

    # Reload best and export end-to-end ONNX.
    onnx_path = YOLO(str(best)).export(
        format="onnx",
        imgsz=640,
        opset=OPSET,
        simplify=True,
        dynamic=False,
    )
    print(f"exported ONNX: {onnx_path}")


if __name__ == "__main__":
    main()
