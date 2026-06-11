#!/usr/bin/env python3
"""Regenerate DiceHead training crops using the *real* fine-tuned detector.

Runs the exported dice ONNX over the composites from make_detection_dataset.py,
matches each detection to a known pasted box (meta.jsonl) by IoU to inherit its
face label, and saves the crop of the *predicted* box — exact box, no padding,
identical framing to crop_for_head in src/model/inferance.rs. Retraining the
head on these closes the domain gap between the crops it was trained on and
the boxes the detector actually emits at serving time.

Output layout matches the Rust folder-mode loader (src/datasets/dataset.rs):
    OUT/<face>/<stem>_<i>.jpg

The default OUT is a fresh directory so the original data/dice_face is never
touched; review the crops, then either merge them into data/dice_face or point
the folder-mode path in src/main.rs at the new directory.

Usage:
    python make_head_crops.py ../src/model/yolo26n.onnx dice_dataset \\
        [--out ../data/dice_face_detected] [--conf 0.25] [--iou 0.5]

Then retrain the head from the repo root:  cargo run --release -- folder
"""

from __future__ import annotations

import argparse
import json
from collections import Counter
from pathlib import Path

# Face-folder names accepted by folder_name_to_label in dataset.rs.
FACE_NAMES = [str(i) for i in range(1, 10)] + ["0"] + [str(i) for i in range(10, 21)]


def iou(a, b) -> float:
    ix1, iy1 = max(a[0], b[0]), max(a[1], b[1])
    ix2, iy2 = min(a[2], b[2]), min(a[3], b[3])
    inter = max(0.0, ix2 - ix1) * max(0.0, iy2 - iy1)
    area_a = (a[2] - a[0]) * (a[3] - a[1])
    area_b = (b[2] - b[0]) * (b[3] - b[1])
    return inter / (area_a + area_b - inter) if inter > 0 else 0.0


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("onnx_path", type=Path, help="exported single-class dice ONNX")
    ap.add_argument("dataset", type=Path,
                    help="composite dataset dir containing meta.jsonl")
    ap.add_argument("--out", type=Path,
                    default=Path(__file__).resolve().parent.parent
                    / "data" / "dice_face_detected")
    ap.add_argument("--conf", type=float, default=0.25,
                    help="detector confidence cutoff (match what serving will use)")
    ap.add_argument("--iou", type=float, default=0.5,
                    help="min IoU between detection and known box to inherit its label")
    args = ap.parse_args()

    import numpy as np
    import onnxruntime as ort
    from PIL import Image

    meta_path = args.dataset / "meta.jsonl"
    records = [json.loads(line) for line in meta_path.read_text().splitlines()]

    sess = ort.InferenceSession(
        str(args.onnx_path),
        providers=["CUDAExecutionProvider", "CPUExecutionProvider"])
    input_name = sess.get_inputs()[0].name

    n_images = n_dets = n_matched = missed_gt = 0
    per_face: Counter[str] = Counter()

    for rec in records:
        gt = [(tuple(b["xyxy"]), b["face"]) for b in rec["boxes"]]
        if not gt:
            continue
        n_images += 1

        img = Image.open(args.dataset / rec["image"]).convert("RGB")
        w, h = img.size
        # Same preprocessing as infer_frame: plain squash resize, /255, CHW.
        x = (np.asarray(img.resize((640, 640), Image.BILINEAR), dtype=np.float32)
             .transpose(2, 0, 1)[None] / 255.0)
        (out,) = sess.run(None, {input_name: x})

        matched_idx: set[int] = set()
        for i, row in enumerate(out[0]):
            if row[4] < args.conf:
                continue
            n_dets += 1
            # Model coords are pixels in 640-space; map to image pixels.
            det = (row[0] / 640 * w, row[1] / 640 * h,
                   row[2] / 640 * w, row[3] / 640 * h)

            best_iou, best_j = 0.0, -1
            for j, (gbox, _) in enumerate(gt):
                v = iou(det, gbox)
                if v > best_iou:
                    best_iou, best_j = v, j
            if best_iou < args.iou:
                continue
            n_matched += 1
            matched_idx.add(best_j)

            px = (max(0, int(det[0])), max(0, int(det[1])),
                  min(w, int(det[2])), min(h, int(det[3])))
            if px[2] - px[0] < 8 or px[3] - px[1] < 8:
                continue
            face = gt[best_j][1]
            face_dir = args.out / face
            face_dir.mkdir(parents=True, exist_ok=True)
            stem = Path(rec["image"]).stem
            img.crop(px).save(face_dir / f"{stem}_{i}.jpg", quality=95)
            per_face[face] += 1

        missed_gt += len(gt) - len(matched_idx)

    print(f"\nimages processed: {n_images}")
    print(f"detections >= conf {args.conf}: {n_dets}, matched: {n_matched}")
    print(f"known boxes the detector missed: {missed_gt} (recall proxy)")
    print("crops per face:",
          dict(sorted(per_face.items(), key=lambda kv: FACE_NAMES.index(kv[0]))))
    print(f"\ncrops written to {args.out}")
    print("review them, then merge into data/dice_face (or point main.rs at the "
          "new dir) and retrain: cargo run --release -- folder")


if __name__ == "__main__":
    main()
