#!/usr/bin/env python3
"""Render the most suspicious crops from `art/audit.csv` as contact sheets.

`cargo r --release -- audit <exp>...` scores every crop with a model that never
trained on it and writes art/audit.csv sorted by the probability the model gave
the *labelled* class. Low values are either hard crops or bad labels (commonly a
crop of a different face than the one the folder names). This tool tiles them so
a human can decide, then record the decision in crop_overrides.tsv:

    19/d20_wood0709<TAB>drop     # crop shows a different face
    9/IMG_1234<TAB>6            # wrong folder, relabel

Each tile is captioned `#<n> <label>-><pred> <label_prob>`; the matching keys are
listed in a .txt next to each sheet (same numbering) for copy/paste.

Usage:
  python tools/audit_sheet.py [--csv art/audit.csv] [--max-prob 0.1] [--label 19]
"""
from __future__ import annotations

import argparse
import csv
from pathlib import Path

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parent.parent
TILE = 112
COLS = 10
ROWS = 8


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--csv", default=str(ROOT / "art" / "audit.csv"))
    ap.add_argument("--max-prob", type=float, default=0.1,
                    help="only show crops whose labelled-class probability is below this")
    ap.add_argument("--label", help="only show this label folder (e.g. 19)")
    ap.add_argument("--out", default=str(ROOT / "art" / "audit_sheets"))
    args = ap.parse_args()

    with open(args.csv, newline="") as f:
        rows = [r for r in csv.DictReader(f)
                if float(r["label_prob"]) < args.max_prob
                and (args.label is None or r["key"].split("/")[0] == args.label)]

    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    per_sheet = COLS * ROWS
    for page, start in enumerate(range(0, len(rows), per_sheet)):
        chunk = rows[start:start + per_sheet]
        sheet = Image.new("RGB", (COLS * TILE, ROWS * (TILE + 14)), "white")
        draw = ImageDraw.Draw(sheet)
        for i, r in enumerate(chunk):
            x, y = (i % COLS) * TILE, (i // COLS) * (TILE + 14)
            path = Path(r["path"])
            if not path.is_absolute():
                path = ROOT / path
            sheet.paste(Image.open(path).convert("RGB").resize((TILE, TILE)), (x, y))
            caption = f"#{start + i} {r['label']}->{r['pred']} {float(r['label_prob']):.2f}"
            draw.text((x + 2, y + TILE), caption, fill="black")
        name = out / f"sheet_{page:03}"
        sheet.save(name.with_suffix(".png"))
        name.with_suffix(".txt").write_text(
            "".join(f"#{start + i}\t{r['key']}\t{r['label']}->{r['pred']}\n"
                    for i, r in enumerate(chunk)))

    print(f"{len(rows)} crops with label prob < {args.max_prob} -> {out}")


if __name__ == "__main__":
    main()
