#!/usr/bin/env python3
"""Self-training: pseudo-label the unboxed data/dice_face images with the baseline
detector, then build a combined dataset for a second training round.

Why: data/dice_face holds ~18k single-die photos on real backgrounds at varied
scale/position, but with no bounding boxes (they were the DiceHead classifier's
data). dice_bbox (638 hand-labeled images) is a *subset* of dice_face by stem, so
the baseline detector trained on those generalizes to the rest. Each face image
contains exactly one die, so the detector's top-1 box above a confidence threshold
is a reliable pseudo-label. This ~30x's the detection training set with real-world
background/material/lighting variety.

Leak guard: any face image whose *normalized* stem (leading face-value digits
stripped) matches a dice_bbox stem is skipped — that removes every duplicate of the
hand-labeled 638 (which include the val set), so val stays honest.

Output (gitignored):
  data/yolo_det_self/images/{train,val}/...   (symlinks/copies of round-1 + pseudo)
  data/yolo_det_self/labels/{train,val}/...
  data/yolo_det_self/data.yaml
The val split is copied verbatim from data/yolo_det (real labels only).

Usage:
  python tools/pseudo_label_face.py runs/dice_det/train/weights/best.pt [--conf 0.6]
"""
from __future__ import annotations

import argparse
import re
import shutil
from pathlib import Path

from PIL import Image
from ultralytics import YOLO

ROOT = Path(__file__).resolve().parent.parent
FACE = ROOT / "data" / "dice_face"
BBOX_IMG = ROOT / "data" / "dice_bbox" / "obj_train_data" / "dice"
BASE = ROOT / "data" / "yolo_det"          # round-1 dataset (real labels)
OUT = ROOT / "data" / "yolo_det_self"      # round-2 combined dataset
IMG_EXTS = {".jpg", ".jpeg", ".png", ".webp"}


def norm_stem(stem: str) -> str:
    """Strip leading face-value digits so '1IMG_4881' and 'IMG_4881' collapse."""
    return re.sub(r"^\d+", "", stem)


def bbox_norm_stems() -> set[str]:
    s = set()
    for p in BBOX_IMG.rglob("*"):
        if p.suffix.lower() in IMG_EXTS:
            s.add(norm_stem(p.stem))
    return s


def copy_round1(out: Path) -> tuple[int, int]:
    """Copy round-1 images+labels into the combined dataset (train and val)."""
    n_tr = n_val = 0
    for split in ("train", "val"):
        (out / "images" / split).mkdir(parents=True, exist_ok=True)
        (out / "labels" / split).mkdir(parents=True, exist_ok=True)
        for img in (BASE / "images" / split).glob("*.jpg"):
            shutil.copy2(img, out / "images" / split / img.name)
            lbl = BASE / "labels" / split / f"{img.stem}.txt"
            if lbl.exists():
                shutil.copy2(lbl, out / "labels" / split / lbl.name)
            if split == "train":
                n_tr += 1
            else:
                n_val += 1
    return n_tr, n_val


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("weights")
    ap.add_argument("--conf", type=float, default=0.6,
                    help="min confidence to accept a pseudo-label")
    ap.add_argument("--imgsz", type=int, default=640)
    args = ap.parse_args()

    if OUT.exists():
        shutil.rmtree(OUT)
    n_tr, n_val = copy_round1(OUT)
    print(f"copied round-1: {n_tr} train, {n_val} val (real labels)")

    skip_stems = bbox_norm_stems()
    print(f"leak guard: {len(skip_stems)} bbox normalized stems will be skipped")

    # Gather candidate face images (excluding bbox duplicates), dedup by normalized
    # stem so the many '<value>IMG_x' / 'IMG_x' copies aren't all trained on.
    candidates: dict[str, Path] = {}
    for face_dir in sorted(FACE.iterdir()):
        if not face_dir.is_dir():
            continue
        for img in face_dir.iterdir():
            if img.suffix.lower() not in IMG_EXTS:
                continue
            ns = norm_stem(img.stem)
            if ns in skip_stems:
                continue
            key = f"{face_dir.name}__{ns}"  # face-value folder + normalized stem
            candidates.setdefault(key, img)
    print(f"candidate face images after dedup/leak-guard: {len(candidates)}")

    model = YOLO(args.weights)
    kept = 0
    dropped = 0
    img_out = OUT / "images" / "train"
    lbl_out = OUT / "labels" / "train"

    keys = list(candidates)
    paths = [candidates[k] for k in keys]
    BATCH = 16  # one forward batch at a time; 256 OOMs a 12GB card
    for i in range(0, len(paths), BATCH):
        batch_paths = paths[i:i + BATCH]
        batch_keys = keys[i:i + BATCH]
        results = model.predict(
            [str(p) for p in batch_paths],
            imgsz=args.imgsz, conf=args.conf, max_det=5, verbose=False,
        )
        for key, src, res in zip(batch_keys, batch_paths, results):
            boxes = res.boxes
            if boxes is None or len(boxes) == 0:
                dropped += 1
                continue
            # exactly one die per image -> take the single highest-confidence box
            confs = boxes.conf.cpu().numpy()
            best = confs.argmax()
            xywhn = boxes.xywhn.cpu().numpy()[best]  # normalized cx,cy,w,h
            cx, cy, w, h = (float(v) for v in xywhn)
            name = f"face__{key}"
            try:
                Image.open(src).convert("RGB").save(
                    img_out / f"{name}.jpg", "JPEG", quality=95)
            except Exception as e:  # noqa: BLE001
                print(f"  SKIP unreadable {src}: {e}")
                dropped += 1
                continue
            (lbl_out / f"{name}.txt").write_text(f"0 {cx:.6f} {cy:.6f} {w:.6f} {h:.6f}\n")
            kept += 1
        print(f"  {i + len(batch_paths)}/{len(paths)} processed "
              f"(kept {kept}, dropped {dropped})", flush=True)

    (OUT / "data.yaml").write_text(
        f"path: {OUT}\ntrain: images/train\nval: images/val\nnc: 1\nnames: ['die']\n"
    )

    print("\n=== self-training dataset ===")
    print(f"  train: {n_tr} real + {kept} pseudo = {n_tr + kept} images")
    print(f"  val:   {n_val} real (unchanged, honest eval)")
    print(f"  dropped (no confident box >= {args.conf}): {dropped}")
    print(f"  data.yaml -> {OUT / 'data.yaml'}")


if __name__ == "__main__":
    main()
