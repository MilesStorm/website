#!/usr/bin/env python3
"""Out-of-fold label audit using PyTorch runs from tools/train_head_torch.py.

Same idea and CSV format as `dice_detector audit`: every crop is scored by the run
whose held-out split contains it, then rows are sorted by the probability given to
the labelled class (most suspicious first). Render with tools/audit_sheet.py:

  python tools/audit_torch.py runs/head_torch/resnet18_experiment_40 runs/head_torch/resnet18_experiment_41
  python tools/audit_sheet.py --csv art/audit_torch.csv --out art/audit_sheets_torch
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

import torch

sys.path.insert(0, str(Path(__file__).resolve().parent))
import train_head_torch as T  # noqa: E402


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("runs", nargs="+", help="runs/head_torch/<name> dirs (one per fold)")
    ap.add_argument("--out", default=str(T.ROOT / "art" / "audit_torch.csv"))
    ap.add_argument("--tta", action="store_true", help="average predictions over 0/90/180/270 degree rotations")
    args = ap.parse_args()

    keys, x_all, y_all = T.load_crops()
    index = {k: i for i, k in enumerate(keys)}
    names = T.label_names()
    rows, seen = [], set()

    for run in map(Path, args.runs):
        meta = json.loads((run / "metrics.json").read_text())
        model = T.build_model(meta["model"], meta.get("upscale", T.SIZE))
        model.load_state_dict(torch.load(run / "best.pt", map_location="cuda"))
        model = model.cuda().eval()
        val = (T.ROOT / meta["split"] / "val_files.txt").read_text().split("\n")
        idx = [index[k] for k in val if k in index and k not in seen]
        seen.update(keys[i] for i in idx)
        with torch.no_grad():
            for s in range(0, len(idx), 1024):
                b = idx[s:s + 1024]
                xb = x_all[b].cuda().float() / 255
                rots = range(4) if args.tta else range(1)
                probs = sum(torch.softmax(model(torch.rot90(xb, k, (2, 3))).float(), 1) for k in rots).cpu() / len(rots)
                for i, p in zip(b, probs):
                    lab, pred = int(y_all[i]), int(p.argmax())
                    d, stem = keys[i].split("/", 1)
                    path = T.CROPS / d / f"{stem}.png"
                    rows.append((float(p[lab]), keys[i], str(path), names[lab], names[pred],
                                 float(p[pred]), float(p[lab]), str(run)))
        print(f"{run}: audited {len(idx)} held-out images")

    rows.sort(key=lambda r: r[0])
    with open(args.out, "w") as f:
        f.write("key,path,label,pred,pred_prob,label_prob,experiment\n")
        for _, key, path, lab, pred, pp, lp, run in rows:
            f.write(f'"{key}","{path}",{lab},{pred},{pp:.4f},{lp:.4f},{run}\n')
    acc = sum(r[3] == r[4] for r in rows) / len(rows)
    flagged = sum(r[0] < 0.1 for r in rows)
    print(f"audited {len(rows)}/{len(keys)} crops; out-of-fold acc {acc:.1%}; {flagged} with label prob < 0.1 -> {args.out}")


if __name__ == "__main__":
    main()
