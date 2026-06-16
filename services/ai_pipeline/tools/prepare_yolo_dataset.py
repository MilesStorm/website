#!/usr/bin/env python3
"""Convert data/dice_bbox into an Ultralytics single-class detection dataset.

Source layout (CVAT/darknet export):
    data/dice_bbox/obj_train_data/dice/{d4,d6,d8,d10,d12,d20}/<stem>.<ext>
    data/dice_bbox/obj_train_data/dice_anno/{type}/<stem>.txt   (YOLO: cls cx cy w h)

The annotation `cls` field currently encodes the die *face value* (0-20). For the
detection stage we only need YOLO to *localize* a die, so every box is collapsed to
class 0 ("die"); the trained DiceHead reads the face value downstream.

Output (Ultralytics format, gitignored):
    data/yolo_det/images/{train,val}/{type}__{stem}.jpg
    data/yolo_det/labels/{train,val}/{type}__{stem}.txt
    data/yolo_det/data.yaml

Basenames repeat across die-type folders, so every output name is prefixed with the
type ("d10__...") to stay unique. Images are re-encoded to RGB .jpg with Pillow to
normalize mixed extensions/cases (.JPG/.webp) and avoid loader surprises in training.
Empty annotation files are preserved as empty labels (background negatives).
"""
from __future__ import annotations

import random
from pathlib import Path

from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
SRC = ROOT / "data" / "dice_bbox" / "obj_train_data"
IMG_SRC = SRC / "dice"
ANN_SRC = SRC / "dice_anno"
OUT = ROOT / "data" / "yolo_det"

TYPES = ["d4", "d6", "d8", "d10", "d12", "d20"]
IMG_EXTS = {".jpg", ".jpeg", ".png", ".webp"}
VAL_FRACTION = 0.15
SEED = 42


def collect() -> dict[str, list[tuple[Path, Path]]]:
    """type -> list of (image_path, annotation_path). Annotation may be missing."""
    by_type: dict[str, list[tuple[Path, Path]]] = {}
    for t in TYPES:
        img_dir = IMG_SRC / t
        ann_dir = ANN_SRC / t
        if not img_dir.is_dir():
            print(f"  WARN: missing image dir {img_dir}")
            continue
        pairs: list[tuple[Path, Path]] = []
        for img in sorted(img_dir.iterdir()):
            if img.suffix.lower() not in IMG_EXTS:
                continue
            ann = ann_dir / f"{img.stem}.txt"
            pairs.append((img, ann))
        by_type[t] = pairs
    return by_type


def convert_label(ann: Path) -> list[str]:
    """Read a YOLO label file and rewrite every line's class field to 0.
    Returns the rewritten lines (possibly empty -> background image)."""
    if not ann.is_file():
        return []
    out: list[str] = []
    for line in ann.read_text().splitlines():
        parts = line.split()
        if len(parts) != 5:
            continue
        _cls, cx, cy, w, h = parts
        out.append(f"0 {cx} {cy} {w} {h}")
    return out


def main() -> None:
    rng = random.Random(SEED)
    by_type = collect()

    for split in ("train", "val"):
        (OUT / "images" / split).mkdir(parents=True, exist_ok=True)
        (OUT / "labels" / split).mkdir(parents=True, exist_ok=True)

    counts = {"train": 0, "val": 0}
    boxes = {"train": 0, "val": 0}
    empties = 0
    val_types: set[str] = set()

    for t, pairs in by_type.items():
        pairs = list(pairs)
        rng.shuffle(pairs)
        n_val = max(1, round(len(pairs) * VAL_FRACTION)) if pairs else 0

        for i, (img, ann) in enumerate(pairs):
            split = "val" if i < n_val else "train"

            name = f"{t}__{img.stem}.jpg"
            try:
                im = Image.open(img).convert("RGB")
            except Exception as e:  # noqa: BLE001
                print(f"  SKIP unreadable {img}: {e}")
                continue
            im.save(OUT / "images" / split / name, "JPEG", quality=95)

            lines = convert_label(ann)
            (OUT / "labels" / split / f"{t}__{img.stem}.txt").write_text(
                "\n".join(lines) + ("\n" if lines else "")
            )

            counts[split] += 1
            boxes[split] += len(lines)
            if not lines:
                empties += 1
            if split == "val":
                val_types.add(t)

    data_yaml = (
        f"path: {OUT}\n"
        "train: images/train\n"
        "val: images/val\n"
        "nc: 1\n"
        "names: ['die']\n"
    )
    (OUT / "data.yaml").write_text(data_yaml)

    print("\n=== dataset summary ===")
    for t, pairs in by_type.items():
        print(f"  {t}: {len(pairs)} images")
    print(f"  train images: {counts['train']} ({boxes['train']} boxes)")
    print(f"  val   images: {counts['val']} ({boxes['val']} boxes)")
    print(f"  empty/background labels: {empties}")
    print(f"  val covers types: {sorted(val_types)}")
    print(f"  data.yaml -> {OUT / 'data.yaml'}")

    assert set(val_types) == set(by_type), "val split missing some die types!"
    assert counts["train"] > 0 and counts["val"] > 0


if __name__ == "__main__":
    main()
