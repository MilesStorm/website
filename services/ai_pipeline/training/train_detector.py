#!/usr/bin/env python3
"""Fine-tune YOLO26n on the single-class dice dataset.

This is transfer learning from the COCO-pretrained checkpoint, not training
from scratch — on a 12 GB RTX 4070 Ti at 640px this is an hours-not-days job.
Ultralytics automatically replaces the 80-class detection head with a 1-class
head and transfers every other weight.

Usage:
    python train_detector.py --data dice_dataset/dice.yaml
    python train_detector.py --data dice_dataset/dice.yaml --epochs 150 --batch 32
"""

from __future__ import annotations

import argparse


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--data", required=True, help="path to dice.yaml from prepare_dataset.py")
    ap.add_argument("--model", default="yolo26n.pt",
                    help="pretrained checkpoint (yolo26n matches the codegen path)")
    ap.add_argument("--epochs", type=int, default=100)
    ap.add_argument("--imgsz", type=int, default=640, help="must stay 640 (YOLO_INPUT in inferance.rs)")
    ap.add_argument("--batch", type=int, default=-1, help="-1 = auto-fit to GPU memory")
    ap.add_argument("--device", default="0")
    ap.add_argument("--name", default="dice_detector")
    args = ap.parse_args()

    from ultralytics import YOLO

    model = YOLO(args.model)
    results = model.train(
        data=args.data,
        epochs=args.epochs,
        imgsz=args.imgsz,
        batch=args.batch,
        device=args.device,
        project="runs",
        name=args.name,
        patience=30,
    )

    best = results.save_dir / "weights" / "best.pt"
    print(f"\nbest checkpoint: {best}")
    print(f"next: python export_detector.py {best}")


if __name__ == "__main__":
    main()
