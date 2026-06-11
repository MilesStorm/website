#!/usr/bin/env python3
"""Synthesize a single-class dice *detection* dataset from classifier crops.

The only labeled data this project has is the DiceHead classifier set
(data/dice_face/<face>/*.jpg — tight crops, label = folder name, NO bboxes;
see load_dataset_folder in src/datasets/dataset.rs). A detector needs boxes,
so this script composites those crops onto background images at random
scale/position/rotation: the paste rectangle IS the ground-truth box, exact
and free, and the face value rides along into meta.jsonl so make_head_crops.py
can later regenerate head training data with the real detector's framing.

Backgrounds: pass --backgrounds with a folder of real images. The closer to
the serving scene the better — a few hundred frames of the actual desk/table
WITHOUT dice are ideal and cheap to record. Without --backgrounds, procedural
solid/gradient/noise canvases are used (works, but expect more false
positives on real clutter).

Output (Ultralytics layout + sidecar):
    OUT/images/{train,val}/*.jpg
    OUT/labels/{train,val}/*.txt      (class 0 = dice)
    OUT/meta.jsonl                    (per-image pixel boxes + face values)
    OUT/dice.yaml

Usage:
    python make_detection_dataset.py ../data/dice_face --backgrounds /path/to/bg \\
        [--out dice_dataset] [--n-train 4000] [--n-val 500] [--seed 42]
"""

from __future__ import annotations

import argparse
import json
import random
import sys
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, ImageEnhance, ImageFilter

IMG_EXTS = {".jpg", ".jpeg", ".png", ".webp"}
CANVAS = 640  # YOLO_INPUT in inferance.rs

# Valid face-folder names, as accepted by folder_name_to_label in dataset.rs:
# "1".."9", "0" (d10 zero glyph), "10".."20".
FACE_NAMES = [str(i) for i in range(1, 10)] + ["0"] + [str(i) for i in range(10, 21)]


def load_crop_index(crops_root: Path) -> list[tuple[Path, str]]:
    """(crop_path, face_name) for every image in face-named folders."""
    index = []
    for face_dir in sorted(crops_root.iterdir()):
        if not face_dir.is_dir() or face_dir.name not in FACE_NAMES:
            continue
        for p in sorted(face_dir.iterdir()):
            if p.suffix.lower() in IMG_EXTS:
                index.append((p, face_dir.name))
    return index


def load_backgrounds(bg_dir: Path | None) -> list[Path]:
    if bg_dir is None:
        return []
    return [p for p in sorted(bg_dir.rglob("*"))
            if p.suffix.lower() in IMG_EXTS and p.is_file()]


def procedural_background(rng: random.Random) -> Image.Image:
    kind = rng.choice(["solid", "gradient", "noise"])
    if kind == "solid":
        c = tuple(rng.randrange(30, 226) for _ in range(3))
        return Image.new("RGB", (CANVAS, CANVAS), c)
    if kind == "gradient":
        a = np.array([rng.randrange(20, 236) for _ in range(3)], dtype=np.float32)
        b = np.array([rng.randrange(20, 236) for _ in range(3)], dtype=np.float32)
        t = np.linspace(0, 1, CANVAS, dtype=np.float32)[:, None]
        col = a[None, :] * (1 - t) + b[None, :] * t  # [CANVAS, 3]
        arr = np.repeat(col[:, None, :], CANVAS, axis=1).astype(np.uint8)
        img = Image.fromarray(arr)
        return img.rotate(rng.uniform(0, 360), expand=False)
    arr = (np.random.default_rng(rng.randrange(2**32))
           .uniform(40, 215, (CANVAS // 4, CANVAS // 4, 3)).astype(np.uint8))
    return Image.fromarray(arr).resize((CANVAS, CANVAS), Image.BILINEAR) \
        .filter(ImageFilter.GaussianBlur(2))


def background_canvas(bg_paths: list[Path], rng: random.Random) -> Image.Image:
    if not bg_paths:
        return procedural_background(rng)
    img = Image.open(rng.choice(bg_paths)).convert("RGB")
    w, h = img.size
    side = min(w, h)
    crop_side = max(1, int(side * rng.uniform(0.6, 1.0)))
    x0 = rng.randrange(max(1, w - crop_side + 1))
    y0 = rng.randrange(max(1, h - crop_side + 1))
    return img.crop((x0, y0, x0 + crop_side, y0 + crop_side)) \
        .resize((CANVAS, CANVAS), Image.BILINEAR)


def prepare_die(crop_path: Path, rng: random.Random) -> Image.Image:
    """Crop -> jittered, feather-masked, rotated RGBA sprite."""
    die = Image.open(crop_path).convert("RGB")

    die = ImageEnhance.Brightness(die).enhance(rng.uniform(0.75, 1.25))
    die = ImageEnhance.Contrast(die).enhance(rng.uniform(0.8, 1.2))

    target = rng.randint(int(CANVAS * 0.10), int(CANVAS * 0.45))
    # Mild independent w/h scaling: the source crops aren't perfectly square
    # framings, and the detector should tolerate slight aspect variation.
    tw = max(8, int(target * rng.uniform(0.9, 1.1)))
    th = max(8, int(target * rng.uniform(0.9, 1.1)))
    die = die.resize((tw, th), Image.LANCZOS)

    # Feathered rounded-rect alpha hides the square paste seam so the model
    # can't learn "sharp rectangle edge = die".
    mask = Image.new("L", (tw, th), 0)
    radius = int(min(tw, th) * 0.12)
    ImageDraw.Draw(mask).rounded_rectangle((0, 0, tw - 1, th - 1),
                                           radius=radius, fill=255)
    mask = mask.filter(ImageFilter.GaussianBlur(max(1, int(min(tw, th) * 0.03))))
    die.putalpha(mask)

    # +-15 deg matches the rotation range the head was trained/augmented with
    # (augment_crop in src/datasets/mod.rs), keeping head-crop domain aligned.
    return die.rotate(rng.uniform(-15, 15), expand=True, resample=Image.BICUBIC)


def boxes_overlap(a: tuple, b: tuple, thresh: float = 0.05) -> bool:
    ix = max(0, min(a[2], b[2]) - max(a[0], b[0]))
    iy = max(0, min(a[3], b[3]) - max(a[1], b[1]))
    inter = ix * iy
    smaller = min((a[2] - a[0]) * (a[3] - a[1]), (b[2] - b[0]) * (b[3] - b[1]))
    return smaller > 0 and inter / smaller > thresh


def synthesize_one(crops, bg_paths, rng):
    """Returns (canvas, [(xyxy_px, face_name), ...]) — list empty for negatives."""
    canvas = background_canvas(bg_paths, rng)
    if rng.random() < 0.10:  # background-only negative
        return canvas, []

    n_dice = rng.choices([1, 2, 3], weights=[0.6, 0.3, 0.1])[0]
    placed = []
    for _ in range(n_dice):
        crop_path, face = rng.choice(crops)
        sprite = prepare_die(crop_path, rng)
        sw, sh = sprite.size
        if sw >= CANVAS or sh >= CANVAS:
            continue
        for _attempt in range(20):
            x0 = rng.randrange(CANVAS - sw)
            y0 = rng.randrange(CANVAS - sh)
            # Tight box from the sprite's actual alpha extent (rotation pads it).
            ax0, ay0, ax1, ay1 = sprite.getchannel("A").getbbox()
            box = (x0 + ax0, y0 + ay0, x0 + ax1, y0 + ay1)
            if not any(boxes_overlap(box, b) for b, _ in placed):
                canvas.paste(sprite, (x0, y0), sprite)
                placed.append((box, face))
                break

    if rng.random() < 0.3:
        canvas = canvas.filter(ImageFilter.GaussianBlur(rng.uniform(0.3, 1.0)))
    return canvas, placed


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("crops", type=Path,
                    help="classifier crop root, e.g. ../data/dice_face")
    ap.add_argument("--backgrounds", type=Path, default=None,
                    help="folder of background images (real desk/table frames are best)")
    ap.add_argument("--out", type=Path, default=Path("dice_dataset"))
    ap.add_argument("--n-train", type=int, default=4000)
    ap.add_argument("--n-val", type=int, default=500)
    ap.add_argument("--seed", type=int, default=42)
    args = ap.parse_args()

    crops = load_crop_index(args.crops.resolve())
    if not crops:
        sys.exit(f"error: no face-named crop folders found under {args.crops}")
    bg_paths = load_backgrounds(args.backgrounds)
    print(f"{len(crops)} crops across "
          f"{len({f for _, f in crops})} face classes; "
          f"{len(bg_paths) or 'procedural'} backgrounds")

    # Split the CROPS 90/10 first so val composites never contain a die the
    # detector saw in training (otherwise val mAP is optimistically inflated).
    rng = random.Random(args.seed)
    rng.shuffle(crops)
    n_val_crops = max(1, len(crops) // 10)
    crop_split = {"val": crops[:n_val_crops], "train": crops[n_val_crops:]}

    out = args.out.resolve()
    meta_f = None
    counts = {"train": args.n_train, "val": args.n_val}
    n_boxes = 0
    (out).mkdir(parents=True, exist_ok=True)
    with open(out / "meta.jsonl", "w") as meta_f:
        for split, n in counts.items():
            img_dir = out / "images" / split
            lbl_dir = out / "labels" / split
            img_dir.mkdir(parents=True, exist_ok=True)
            lbl_dir.mkdir(parents=True, exist_ok=True)
            for i in range(n):
                canvas, placed = synthesize_one(crop_split[split], bg_paths, rng)
                stem = f"comp_{split}_{i:06d}"
                canvas.save(img_dir / f"{stem}.jpg",
                            quality=rng.randint(70, 95))

                lines = []
                for (x0, y0, x1, y1), _face in placed:
                    cx, cy = (x0 + x1) / 2 / CANVAS, (y0 + y1) / 2 / CANVAS
                    bw, bh = (x1 - x0) / CANVAS, (y1 - y0) / CANVAS
                    lines.append(f"0 {cx:.6f} {cy:.6f} {bw:.6f} {bh:.6f}")
                (lbl_dir / f"{stem}.txt").write_text(
                    "\n".join(lines) + ("\n" if lines else ""))

                meta_f.write(json.dumps({
                    "image": f"images/{split}/{stem}.jpg",
                    "boxes": [{"xyxy": list(b), "face": f} for b, f in placed],
                }) + "\n")
                n_boxes += len(placed)
                if (i + 1) % 500 == 0:
                    print(f"  {split}: {i + 1}/{n}")

    (out / "dice.yaml").write_text(
        f"path: {out}\ntrain: images/train\nval: images/val\nnames:\n  0: dice\n")

    print(f"\nwrote {args.n_train} train / {args.n_val} val composites "
          f"({n_boxes} boxes) -> {out}")
    print(f"next: python train_detector.py --data {out / 'dice.yaml'}")


if __name__ == "__main__":
    main()
