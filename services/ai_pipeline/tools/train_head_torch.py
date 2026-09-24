#!/usr/bin/env python3
"""Side experiment: train the dice-face head in PyTorch instead of burn.

Trains on exactly what the burn trainer sees, so the numbers are comparable:
  * the same crops (data/dice_face_crops) and label mapping (obj.names order),
  * the same crop_overrides.tsv (drop / relabel),
  * the SAME held-out images: the val list is read from a burn experiment's
    val_files.txt (e.g. art/experiment_40), not re-derived,
  * the same augmentation recipe, loss (sqrt-inverse-frequency class weights,
    label smoothing 0.05), Adam + cosine schedule, grad clipping and batch size,
  * best-val-accuracy epoch is kept, like the burn checkpoint strategy.

Two models:
  dicehead  - a 1:1 port of src/model/head.rs trained from scratch
              (isolates the framework: speed, mixed precision, data pipeline).
  resnet18  - ImageNet-pretrained ResNet18 fine-tuned (what pretraining buys).
              The stem max-pool is removed so 64x64 crops keep enough resolution.

Everything (dataset, augmentation) lives on the GPU, and bf16 autocast is used
for speed while weights/optimizer stay f32.

Outputs (runs/ is gitignored):
  runs/head_torch/<name>/{metrics.json, confusion.csv, best.pt, model.onnx}

Usage:
  python tools/train_head_torch.py --model dicehead --split art/experiment_40
  python tools/train_head_torch.py --model resnet18 --split art/experiment_40
"""
from __future__ import annotations

import argparse
import json
import math
import re
import time
from pathlib import Path

import numpy as np
import torch
import torch.nn as nn
import torch.nn.functional as F
from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
CROPS = ROOT / "data" / "dice_face_crops"
OVERRIDES = ROOT / "crop_overrides.tsv"
OUT = ROOT / "runs" / "head_torch"
NUM_CLASSES = 21
SIZE = 64  # HEAD_INPUT in src/model/inferance.rs


# --------------------------------------------------------------------------- data

def folder_to_label(name: str) -> int | None:
    """Mirror of folder_name_to_label in src/datasets/dataset.rs."""
    if name == "0":
        return 9
    try:
        v = int(name)
    except ValueError:
        return None
    if 1 <= v <= 9:
        return v - 1
    if 10 <= v <= 20:
        return v
    return None


def label_names() -> list[str]:
    return [str(i) for i in range(1, 10)] + ["0"] + [str(i) for i in range(10, 21)]


def load_overrides() -> dict[str, str]:
    if not OVERRIDES.exists():
        return {}
    out = {}
    for line in OVERRIDES.read_text().splitlines():
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        key, action = line.split("\t", 1)
        out[key.strip()] = action.split("#")[0].strip()
    return out


def load_crops() -> tuple[list[str], torch.Tensor, torch.Tensor]:
    """All crops as (keys, uint8 [N,3,64,64], int64 labels), overrides applied.

    Resized with Lanczos exactly like load_dataset_folder (crops are already 64px,
    so this is normally a no-op)."""
    overrides = load_overrides()
    keys, imgs, labels = [], [], []
    for d in sorted(CROPS.iterdir()):
        folder_label = folder_to_label(d.name) if d.is_dir() else None
        if folder_label is None:
            continue
        for p in sorted(d.iterdir()):
            if p.suffix not in (".jpg", ".png"):
                continue
            key = f"{d.name}/{p.stem}"
            label = folder_label
            if key in overrides:
                if overrides[key] == "drop":
                    continue
                label = folder_to_label(overrides[key])
            im = Image.open(p).convert("RGB")
            if im.size != (SIZE, SIZE):
                im = im.resize((SIZE, SIZE), Image.LANCZOS)
            keys.append(key)
            imgs.append(np.asarray(im).transpose(2, 0, 1))
            labels.append(label)
    return keys, torch.from_numpy(np.stack(imgs)), torch.tensor(labels)


# ------------------------------------------------------------------------- splits

FRAME_GAP = 3  # frames within this many numbers of each other are one roll


def sequence_key(stem: str) -> tuple[str, int | None]:
    """Port of sequence_key in src/datasets/dataset.rs: (scene, frame) for numbered
    video frames, (key, None) for everything else."""
    s = re.sub(r"^\d+", "", stem)
    if ".rf." in s:
        s = s[:s.index(".rf.")].rsplit("_", 1)[0]
    m = re.match(r"^([0-9a-fA-F]{0,40})-(.*)$", s)
    if m:
        s = m.group(2)
    m = re.match(r"^IMG_(\d{8}_\d{4})", s)
    if m:
        return f"IMG_{m.group(1)}", None
    head = s.rstrip("0123456789")
    if head.startswith("d") and head != s:
        return head, int(s[len(head):])
    return s, None


def groups_for(keys: list[str]) -> list[str]:
    """Capture-sequence group per key; numbered frames split into rolls on gaps."""
    out = [""] * len(keys)
    frames = []
    for i, k in enumerate(keys):
        folder, stem = k.split("/", 1)
        scene, frame = sequence_key(stem)
        if frame is None:
            out[i] = f"{folder}__{scene}"
        else:
            frames.append((folder, scene, frame, i))
    frames.sort()
    prev = None
    for folder, scene, frame, i in frames:
        if prev is None or prev[:2] != (folder, scene) or frame - prev[2] > FRAME_GAP:
            gid = f"{folder}__{scene}@{frame}"
        prev = (folder, scene, frame)
        out[i] = gid
    return out


def assign_folds(keys: list[str], labels: list[int], k: int, seed: int) -> dict[str, int]:
    """Class-stratified fold per group (port of assign_folds in training.rs):
    groups shuffled per class, each to the fold holding fewest images of that class."""
    import random
    groups = groups_for(keys)
    per_label: dict[int, dict[str, int]] = {}
    for g, l in zip(groups, labels):
        per_label.setdefault(l, {}).setdefault(g, 0)
        per_label[l][g] += 1
    rng = random.Random(seed)
    folds: dict[str, int] = {}
    for l in sorted(per_label):
        items = sorted(per_label[l].items())
        rng.shuffle(items)
        fill = [0] * k
        for g, n in items:
            if g in folds:
                continue
            f = min(range(k), key=lambda i: fill[i])
            fill[f] += n
            folds[g] = f
    return {key: folds[g] for key, g in zip(keys, groups)}


# ------------------------------------------------------------------ augmentation

def augment(x: torch.Tensor) -> torch.Tensor:
    """Batched port of augment_crop in src/datasets/mod.rs. x: float [B,3,H,W] in [0,1].

    Per-sample random draws, one kernel launch per stage (vs one per image in burn)."""
    b, dev = x.shape[0], x.device
    u = lambda lo, hi, *s: torch.empty(*s or (b,), device=dev).uniform_(lo, hi)
    on = lambda p: (torch.rand(b, device=dev) < p)

    # Affine (p=0.8): full-circle rotation, shear ±0.12, scale 0.9-1.1.
    # bbox_jitter (p=0.7) folds in as a 0.85-1.0 zoom plus a small off-centre shift.
    theta = u(-math.pi, math.pi)
    shx, shy = u(-0.12, 0.12), u(-0.12, 0.12)
    sx, sy = u(0.9, 1.1), u(0.9, 1.1)
    aff = on(0.8)
    theta, shx, shy = theta * aff, shx * aff, shy * aff
    sx, sy = torch.where(aff, sx, 1.0), torch.where(aff, sy, 1.0)
    c, s = torch.cos(theta), torch.sin(theta)
    m = torch.stack([
        torch.stack([(c - s * shy) * sx, (c * shx - s) * sy], -1),
        torch.stack([(s + c * shy) * sx, (s * shx + c) * sy], -1),
    ], 1)  # [B,2,2]
    jit = on(0.7)
    zoom = torch.where(jit, u(0.85, 1.0), 1.0)
    shift = (1 - zoom)[:, None] * 0.5 * u(-1, 1, b, 2) * jit[:, None]
    m = m * zoom[:, None, None]
    grid = F.affine_grid(torch.cat([m, shift[:, :, None]], 2), list(x.shape), align_corners=False)
    x = F.grid_sample(x, grid, mode="bilinear", padding_mode="border", align_corners=False)

    # Colour jitter (p=0.7): brightness/contrast 0.65-1.35, per-channel tint 0.85-1.15.
    cj = on(0.7)[:, None, None, None]
    bri = u(0.65, 1.35)[:, None, None, None]
    con = u(0.65, 1.35)[:, None, None, None]
    tint = u(0.85, 1.15, b, 3)[:, :, None, None]
    x = torch.where(cj, (((x - 0.5) * con + 0.5) * bri * tint).clamp(0, 1), x)

    # Sensor noise (p=0.4), sigma 0.07.
    nz = on(0.4)[:, None, None, None]
    x = torch.where(nz, (x + torch.randn_like(x) * 0.07).clamp(0, 1), x)
    return x


# ------------------------------------------------------------------------- models

class ResBlock(nn.Module):
    def __init__(self, c: int):
        super().__init__()
        self.conv1, self.bn1 = nn.Conv2d(c, c, 3, padding=1), nn.BatchNorm2d(c)
        self.conv2, self.bn2 = nn.Conv2d(c, c, 3, padding=1), nn.BatchNorm2d(c)

    def forward(self, x):
        y = F.relu(self.bn1(self.conv1(x)))
        return F.relu(self.bn2(self.conv2(y)) + x)


class DiceHead(nn.Module):
    """1:1 port of DiceHead in src/model/head.rs (~0.95M params)."""

    def __init__(self):
        super().__init__()
        down = lambda i, o: nn.Sequential(nn.Conv2d(i, o, 3, stride=2, padding=1), nn.BatchNorm2d(o), nn.ReLU())
        self.net = nn.Sequential(
            nn.Conv2d(3, 32, 3, padding=1), nn.BatchNorm2d(32), nn.ReLU(),
            down(32, 64), ResBlock(64),
            down(64, 128), ResBlock(128),
            down(128, 128), ResBlock(128),
            nn.AdaptiveAvgPool2d(4), nn.Flatten(), nn.Dropout(0.3),
            nn.Linear(128 * 16, NUM_CLASSES),
        )

    def forward(self, x):
        return self.net(x)


class Pretrained(nn.Module):
    """ImageNet-pretrained torchvision backbone with a fresh 21-way head.

    ImageNet normalisation is baked in, so it takes the same [0,1] 64x64 input as
    DiceHead. `upscale` > 64 resizes the crop inside the model first (pretrained
    nets are tuned for larger objects); for ResNets at native 64px the stem
    max-pool is removed instead so the final map stays 4x4.
    """

    def __init__(self, arch: str = "resnet18", upscale: int = SIZE):
        super().__init__()
        import torchvision.models as tvm
        m = tvm.get_model(arch, weights="DEFAULT")
        if arch.startswith("resnet"):
            if upscale <= SIZE:
                m.maxpool = nn.Identity()
            m.fc = nn.Sequential(nn.Dropout(0.3), nn.Linear(m.fc.in_features, NUM_CLASSES))
        else:  # efficientnet / mobilenet: last classifier layer is a Linear
            last = m.classifier[-1]
            m.classifier[-1] = nn.Linear(last.in_features, NUM_CLASSES)
        self.m, self.upscale = m, upscale
        self.register_buffer("mean", torch.tensor([0.485, 0.456, 0.406]).view(1, 3, 1, 1))
        self.register_buffer("std", torch.tensor([0.229, 0.224, 0.225]).view(1, 3, 1, 1))

    def forward(self, x):
        if self.upscale > SIZE:
            x = F.interpolate(x, size=(self.upscale, self.upscale), mode="bilinear", align_corners=False)
        return self.m((x - self.mean) / self.std)


def PretrainedResNet18():  # kept for runs saved before `Pretrained` existed
    return Pretrained("resnet18")


def build_model(name: str, upscale: int = SIZE) -> nn.Module:
    return DiceHead() if name == "dicehead" else Pretrained(name, upscale)


# -------------------------------------------------------------------------- train

def class_weights(labels: torch.Tensor) -> torch.Tensor:
    """Mirror of compute_class_weights in src/model/training.rs."""
    counts = torch.bincount(labels, minlength=NUM_CLASSES).float()
    raw = labels.numel() / (NUM_CLASSES * counts.clamp(min=1))
    return torch.where(counts > 0, raw.sqrt().clamp(0.3, 3.0), torch.ones_like(raw))


@torch.no_grad()
def evaluate(model, x_u8, y, bs=1024):
    model.eval()
    preds, loss = [], 0.0
    for i in range(0, len(y), bs):
        xb = x_u8[i:i + bs].float() / 255
        with torch.autocast("cuda", dtype=torch.bfloat16):
            logits = model(xb)
        loss += F.cross_entropy(logits.float(), y[i:i + bs], reduction="sum").item()
        preds.append(logits.argmax(1))
    p = torch.cat(preds)
    return (p == y).float().mean().item(), loss / len(y), p


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", default="dicehead",
                    help="dicehead, or a torchvision backbone: resnet18, resnet34, efficientnet_b0, mobilenet_v3_large")
    ap.add_argument("--upscale", type=int, default=SIZE, help="resize crops to this size inside the model")
    ap.add_argument("--seed", type=int, default=42)
    ap.add_argument("--split", help="dir whose val_files.txt defines the held-out set (e.g. art/experiment_40)")
    ap.add_argument("--folds", type=int, help="instead of --split: own class-stratified group K-fold split")
    ap.add_argument("--fold", type=int, default=0, help="which of --folds is held out")
    ap.add_argument("--epochs", type=int, default=40)
    ap.add_argument("--batch", type=int, default=128)
    ap.add_argument("--lr", type=float, default=None, help="default 1e-3 (dicehead) / 3e-4 (resnet18)")
    ap.add_argument("--limit", type=int, default=0, help="smoke test: use only N train and N val images")
    ap.add_argument("--name", default=None)
    args = ap.parse_args()

    torch.manual_seed(args.seed)
    dev = torch.device("cuda")
    lr = args.lr or (1e-3 if args.model == "dicehead" else 3e-4)
    assert args.split or args.folds, "give --split <dir> or --folds K [--fold i]"
    name = args.name or f"{args.model}_" + (Path(args.split).name if args.split else f"f{args.fold}of{args.folds}")
    out = OUT / name
    out.mkdir(parents=True, exist_ok=True)

    t0 = time.time()
    keys, x_all, y_all = load_crops()
    if args.folds:
        fold_of = assign_folds(keys, y_all.tolist(), args.folds, 42)
        val_list = [k for k in keys if fold_of[k] == args.fold]
        (out / "val_files.txt").write_text("\n".join(val_list))
        args.split = str(out.relative_to(ROOT))
    val_keys = set((ROOT / args.split / "val_files.txt").read_text().split("\n"))
    is_val = torch.tensor([k in val_keys for k in keys])
    tr_idx, va_idx = (~is_val).nonzero().squeeze(1), is_val.nonzero().squeeze(1)
    if args.limit:
        tr_idx, va_idx = tr_idx[torch.randperm(len(tr_idx))[:args.limit]], va_idx[:args.limit]
    x_tr, y_tr = x_all[tr_idx].to(dev), y_all[tr_idx].to(dev)
    x_va, y_va = x_all[va_idx].to(dev), y_all[va_idx].to(dev)
    print(f"loaded {len(keys)} crops in {time.time() - t0:.1f}s -> {len(y_tr)} train / {len(y_va)} val "
          f"(split from {args.split})")

    model = build_model(args.model, args.upscale).to(dev)
    model = model.to(memory_format=torch.channels_last)
    n_params = sum(p.numel() for p in model.parameters())
    crit = nn.CrossEntropyLoss(weight=class_weights(y_tr).to(dev), label_smoothing=0.05)
    opt = torch.optim.AdamW(model.parameters(), lr=lr, weight_decay=5e-5)
    steps = args.epochs * math.ceil(len(y_tr) / args.batch)
    sched = torch.optim.lr_scheduler.CosineAnnealingLR(opt, steps, eta_min=1e-6)

    history, best = [], (-1.0, 0)
    t_train = time.time()
    for epoch in range(1, args.epochs + 1):
        te = time.time()
        model.train()
        perm = torch.randperm(len(y_tr), device=dev)
        tot, correct, seen = 0.0, 0, 0
        for i in range(0, len(perm), args.batch):
            idx = perm[i:i + args.batch]
            xb = augment(x_tr[idx].float() / 255).contiguous(memory_format=torch.channels_last)
            yb = y_tr[idx]
            with torch.autocast("cuda", dtype=torch.bfloat16):
                logits = model(xb)
            loss = crit(logits.float(), yb)
            opt.zero_grad(set_to_none=True)
            loss.backward()
            nn.utils.clip_grad_norm_(model.parameters(), 1.0)
            opt.step()
            sched.step()
            tot += loss.item() * len(yb)
            correct += (logits.argmax(1) == yb).sum().item()
            seen += len(yb)
        va_acc, va_loss, _ = evaluate(model, x_va, y_va)
        dt = time.time() - te
        history.append(dict(epoch=epoch, train_loss=tot / seen, train_acc=correct / seen,
                            val_loss=va_loss, val_acc=va_acc, seconds=dt))
        flag = ""
        if va_acc > best[0]:
            best = (va_acc, epoch)
            torch.save(model.state_dict(), out / "best.pt")
            flag = " *"
        print(f"epoch {epoch:3}/{args.epochs}  train {tot / seen:.3f} / {correct / seen:.1%}  "
              f"val {va_loss:.3f} / {va_acc:.1%}  {dt:.1f}s{flag}", flush=True)
    train_s = time.time() - t_train

    # Final report from the best epoch.
    model.load_state_dict(torch.load(out / "best.pt"))
    va_acc, _, preds = evaluate(model, x_va, y_va)
    conf = torch.zeros(NUM_CLASSES, NUM_CLASSES, dtype=torch.long)
    for t, p in zip(y_va.tolist(), preds.tolist()):
        conf[t, p] += 1
    names = label_names()
    per_class = {names[i]: (conf[i, i].item() / max(conf[i].sum().item(), 1)) for i in range(NUM_CLASSES)}
    with open(out / "confusion.csv", "w") as f:
        f.write("true_label," + ",".join(f"pred_{i}" for i in range(NUM_CLASSES)) + "\n")
        for i in range(NUM_CLASSES):
            f.write(f"{i}," + ",".join(str(v) for v in conf[i].tolist()) + "\n")

    # ONNX for burn/ort serving: [1,3,64,64] in [0,1] -> [1,21] logits.
    model.float().eval().to("cpu")
    onnx_path = out / "model.onnx"
    torch.onnx.export(model, torch.rand(1, 3, SIZE, SIZE), onnx_path, input_names=["image"],
                      output_names=["logits"], opset_version=17, dynamo=False)

    metrics = dict(model=args.model, upscale=args.upscale, seed=args.seed, params=n_params, split=args.split, train=len(y_tr), val=len(y_va),
                   best_epoch=best[1], best_val_acc=va_acc, per_class_acc=per_class,
                   train_seconds=train_s, mean_epoch_seconds=train_s / args.epochs, history=history)
    (out / "metrics.json").write_text(json.dumps(metrics, indent=2))
    print(f"\nbest epoch {best[1]}: val acc {va_acc:.2%} | {n_params / 1e6:.2f}M params | "
          f"{train_s / 60:.1f} min total, {train_s / args.epochs:.1f}s/epoch -> {out}")


if __name__ == "__main__":
    main()
