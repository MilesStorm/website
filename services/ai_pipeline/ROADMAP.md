# Dice recognition roadmap

Goal: a camera over a dice tray → one reliable `{die type, value}` event per roll for the webserver.
Constraint: all training on this machine (RTX 4070 Ti, 12 GB). No cloud.

Started 2026-09-24. This file is the durable record: decisions, results, tools and plan.

## Decisions (owner's calls, 2026-09-24)
- **Train in Python, serve in burn.** Same DiceHead, same split: 0.5 min in PyTorch vs ~36 min in burn, and
  79.9% vs 51.5% held-out. The burn trainer's LR-schedule bug was real (fixed in `src/model/training.rs`) but did not
  close that gap (experiment_42). **Found 2026-09-24:** the burn loaders and the serving crop packed interleaved RGB into an
  [N,3,H,W] tensor, scrambling every image; almost certainly the cause. Fixed with `pack_chw` (commit 32d78070); not re-measured in burn. Burn is the production inference runtime only.
  The burn training code is kept but unused.
- **Labels are reviewed by a human, never by a model's judgement of images.** Tools: `tools/review_server.py` (mark
  wrong crops) → `tools/relabel_server.py` (give them their real number). Results live in `crop_overrides.tsv`.
- **All training data shows dice that will actually be used** (the owner's and friends' dice). More dice arrive
  later through an opt-in "share data to improve the model" feature in production.
- **Production gate: ≥ 96% accuracy.** The system must **abstain** (give no answer) when not confident, never guess.
  Voting over several frames per roll is wanted.
- **No synthetic data** (owner expects it to be too "perfect" to transfer). The d4/top-face and capture-tool ideas remain open.
- **Data folders are never modified.** Back up first if that ever becomes necessary; use `crop_overrides.tsv` instead.

## Where we are (2026-09-24, evening)

| Piece | State | Evidence |
|---|---|---|
| YOLO26s number detector (Python-trained, ONNX → burn) | **Good** | mAP50 0.98, P 0.96, R 0.97 (`runs/dice_det/train_self`) |
| Head labels | **Cleaned by the owner** | `crop_overrides.tsv`: 682 relabelled, 45 dropped (unreadable) of 733 wrong-face crops |
| Best head: ImageNet ResNet18, PyTorch | **~94% always-answer; ~97–98% when abstaining** | see results below |
| Shipped weights (updated 2026-09-25) | **ResNet18, from a GitHub release** | `dice_head_resnet18.safetensors` (release `dice-head-v1`), SHA-256 pinned in the Dockerfile. The old `weights/model/model.bpk` is unused |
| Rust serving of the PyTorch head (updated 2026-09-25) | **Done** | `src/model/resnet.rs`; parity test: max prob diff 4.4e-5, 256/256 same answer |
| Roll detection + abstain (updated 2026-09-25) | **Done** | `src/roll.rs`: waits for dice to settle, votes over frames, reports unsure dice as unreadable (threshold 0.7) |
| End to end: camera page → website → Chrome extension (updated 2026-09-25) | **Built, tested locally** | website `/api/arcane/rolls` (live stream) and `extension/chrome/`; see `.omc/handoffs/e2e-dice-rolls.md` |

### How the head is measured
Out-of-fold over all 17,597 crops: every crop is scored by a model trained without it, held out by
capture sequence (video frames / bursts never straddle the split). Same pictures for every comparison.
This test is **harsher than production**: some die faces were filmed in only one sequence, so when that
sequence is held out the model has never seen that face on that die. Run-to-run noise is about ±0.6 points, so
treat differences under ~1 point as noise.

### Results log (out-of-fold accuracy, always answering)
| Model / change | Accuracy |
|---|---|
| burn DiceHead, leak-free split (exp 40/41, and 42 with LR fix) | 48–52% |
| PyTorch DiceHead port (same net) | 79.8% |
| PyTorch ResNet18 pretrained | 87.0% |
| + owner dropped 733 wrong-face crops (scored on the same cleaned set) | 90.8% → 93.4% |
| + those crops relabelled instead of dropped (same set) | 94.9% |
| ResNet18 seeds 42 / 7 (`runs/head_torch/sweep/`) | 93.7% / 94.3% |
| ResNet18 80 epochs; input upscaled to 128px | 93.3%; 93.7% (no gain) |
| ResNet34 | 94.3%; 94.9% with 0/90/180/270° rotation averaging |
| Rotation averaging on ResNet18 | no gain (93.3–94.3%) |
| EfficientNet-B0 (input 128px) | 91.3% (91.6% rotation-avg) |
| MobileNetV3-Large (input 128px) | 90.1% (91.0% rotation-avg) |

| ResNet18, own split: 50% of data for training (2 folds) → 80% (5 folds) | 93.6% → **95.5%** |

**More data helps** (+1.9 points, well above noise). With 80% training data and abstaining: ≥0.7 confidence →
93% answered, 97.4% correct; ≥0.8 → 90% answered, 97.8% correct. The final model trains on 100%, and opt-in
shared data will keep adding more.

Sweep verdict: the ResNets are best. ResNet18 vs ResNet34 is within noise; the smaller mobile nets are worse.

**Abstaining** (ResNet18, per single picture): answer only at ≥70% confidence → 90% answered, 97.0–97.2% correct;
≥80% → 84% answered, 98.1% correct. (Label smoothing 0.05 caps confidence near 0.95, so thresholds above 0.9 mean little.)

**Where the errors are:** whole capture groups of a face/die the model never saw in training (e.g. all 136 "15"s of
one d20 set read as 10; d20 12.5% and d4 13% error vs d6/d8 ~1%). Only 61 of 1,112 errors name a value impossible
for the die type, so die-type masking is worth little.

### Tools
| Tool | Purpose |
|---|---|
| `tools/train_head_torch.py` | Train the head (`--model dicehead|resnet18|resnet34|efficientnet_b0|mobilenet_v3_large`, `--upscale`, `--seed`, `--epochs`; split via `--split <dir with val_files.txt>` or `--folds K --fold i`). Writes `runs/head_torch/<name>/{best.pt, model.onnx, metrics.json, confusion.csv}` |
| `tools/audit_torch.py` | Out-of-fold scoring of one run per fold → CSV sorted by label probability (`--tta` for rotation averaging) |
| `tools/review_server.py`, `tools/relabel_server.py` | Browser pages (ports 8765 / 8766) for the owner's label review; write `crop_overrides.tsv` |
| `tools/audit_sheet.py` | Static contact sheets (superseded by the review pages) |
| `dice_detector folder / eval / audit / export-crops` | burn-side trainer and tools (training no longer used) |

## Recommendations, in order

### 1. Build a real tray test set first (≈1 evening)
Every later decision needs a number that reflects the real use case, and none of the current metrics do.
- Small capture tool: webcam over the tray → throw → dice settle → press the value(s) on the keyboard →
  save the last ~2 s of frames with that label. One roll yields ~30–60 labelled frames at the cost of a
  single key press.
- A few hundred rolls, all die types, a couple of lighting setups. Keep them **only** as the test set:
  split by roll, never train on it.
- Measure **per roll, end to end** (was the reported value right?), not per crop.

### 2. Decide the architecture with that test set, not with opinions
Two candidates are worth comparing. Both keep Rust/burn for **serving**.

**A. Single-stage YOLO that predicts the value directly** (my first pick)
- Train ultralytics YOLO26s with classes = face value (21) or value × die type, and export ONNX → burn exactly
  as today. This deletes the head, the crop exporter, and the whole crop-label mismatch.
- The hand labels already carry the value: `data/dice_bbox` annotations store the face value in the class
  field, and `tools/prepare_yolo_dataset.py` currently throws it away (collapses to class 0).
  But there are only 613 boxes, and values 9–20 have just 6–23 each. Option A therefore needs value-labelled
  pseudo boxes from `data/dice_face` (folder value + the top-face rule from step 3) and/or tray captures
  (step 4) before it can compete.
- Required augmentation changes: `fliplr=0` (mirror flips mislabel digits; today it's the default 0.5),
  `degrees=180` (dice land at any angle).
- The top-face numbers are ~88 px at 640 input (median box 1.9% of the image area), which YOLO handles comfortably.
- Cost: roughly 1–1.5 h per training run locally.

**B. Keep two stages, but train the classifier in PyTorch from a pretrained backbone**
- Fine-tune a small pretrained net (MobileNetV3 / ResNet18, or ultralytics YOLO-cls) on the crops, export
  ONNX, run it in burn next to the detector. A pretrained model on ~17k crops should beat a from-scratch
  1M-param CNN by a wide margin, with mixed precision and fast data loading for free.
- Choose this if A struggles with small or ambiguous glyphs.

Either way: **train in Python, serve in Rust.** Burn earns its place as the inference runtime (single binary,
no Python in production). As a *training* framework it has cost most of the debugging time here, for no
accuracy benefit. The ONNX → burn path already works for YOLO. If the burn-onnx codegen workarounds
(`build.rs` int64 patch, CUDA runtime headers) keep biting, the `ort` crate (ONNX Runtime + CUDA) is the
mainstream alternative that still keeps serving in Rust.

### 3. Fix labels at the source, not afterwards
Whichever architecture wins, stop minting wrong-face labels:
- For a top-down tray camera the top face is the one facing the camera, so choose the number box **closest to
  the die's centre**, not the most confident one. Skip images where two candidate boxes are similarly likely.
- Keep `crop_overrides.tsv` for the leftovers, and review audit flags by eye: many flags are hard but
  *correct* crops (e.g. upside-down 5s of one white d10), which must stay in.

### 4. Generate training data that looks like the deployment
*(Synthetic renders were declined by the owner on 2026-09-24; tray captures remain the plan.)*
Two cheap sources, both local:
- **The capture tool from step 1**, used for training data on *different* rolls from the test set. This is the
  highest-value data there is: your dice, your tray, your camera, exact labels.
- **Synthetic renders (Blender, runs fine on this GPU).** Model d4–d20 with a few fonts and materials,
  drop them with physics into a tray, render top-down with random lighting and camera noise. The top face,
  value, box and die type all come out of the scene exactly, with zero label noise and unlimited
  quantity. Use it to pre-train, then fine-tune on real data. Worth it if step 1 data is slow to collect.
- De-emphasise the public side-on sets. Keep them only if the tray test set shows they help.

### 5. Use what's physically known
- **Die type masks impossible values.** A d6 can't show 7. Predict the die type (a YOLO class, or from
  the die box) and zero out impossible values. This removes whole error classes for free. d4s are
  read differently (the value is at the top vertex), so handle them as their own case.
- **Temporal smoothing when serving.** Dice come to rest. Track each die's box, wait until it's stationary
  for N frames, then vote over those frames and emit **one event per die per roll**, or `uncertain` below
  a confidence threshold. The webserver gets clean results instead of a per-frame stream.
- **Rotation TTA.** Average predictions over 0/90/180/270° crops (valid once training uses full rotation).
  Cheap for a small classifier.

### 6. Long term: learn from use (the data flywheel)
Deploy, let users flag wrong predictions, and grow the data pool from real use. This becomes the main
data source over time. What it needs to produce useful data:
- **Collect corrections, not just flags.** The "this was wrong" lever asks for the right value with a
  one-tap picker limited to that die type. A bare "wrong" can't be trained on.
- **Store what any future model can use:** the full frame (or the tray region) + all boxes + predictions +
  confidences + model version + roll id + timestamp. Not 64px crops: the architecture may change (option A).
- **Unflagged ≠ correct.** Users miss errors, so sample "good" predictions carefully:
  - confident predictions from rolls where the user corrected a *different* die (evidence they were looking);
  - a small uniform random sample, to keep the pool representative;
  - low-confidence predictions pushed into the review queue first (they teach the most).
- **Frozen test set.** A fixed ~10% of rolls (chosen by hashing the roll id) never enter training. This set is
  the tray benchmark from step 1, and it keeps growing for free.
- **Gated retraining.** A local scheduled job (e.g. weekly) retrains and swaps weights only if the new model
  beats the current one on the frozen test set. Keep previous weights for rollback.
- **Bad feedback.** A review queue and/or per-user trust (agreement with other users and with the model)
  before corrections become labels.
- **Privacy.** Frames come from users' webcams. Say that they're stored, crop to the tray, and set a
  retention period.
- **Protocol sketch:** serve emits one event per settled roll with a `roll_id`. The client sends
  `{roll_id, die_index, correct_value}` back over the same WebSocket, and serve appends the event and frame to the pool.
- **Prerequisite:** a model good enough to ship (sequence step 1).

## What to stop doing
- Reporting accuracy on image-level random splits. Near-duplicate video frames inflate it.
  Split by capture sequence (implemented) and judge on the tray test set.
- Treating "flagged by the audit" as "mislabelled". It needs a human look.
- Hand-tuning the from-scratch head architecture. The data and labels are the bottleneck, not the layer count.

## Suggested sequence (updated 2026-09-24 evening)
1. **Capture tool + real tray test set** (step 1): new rolls of the actual dice, labelled by the owner, never
   trained on. This is the only trustworthy measure for the **96% production gate**.
2. **Serve path in Rust**: load the PyTorch head (ONNX → burn, as for YOLO), confirm burn output matches PyTorch
   on the same crops, then per-die tracking + multi-frame voting + abstain below a confidence threshold.
3. **Final head**: train the chosen model on all data (no hold-out) once the test set exists to pick thresholds.
   Candidates: ResNet18 (simplest) or ResNet34. Check burn can import it before choosing a fancier one.
4. Replace the broken `weights/model/model.bpk`, and ship behind the gate.
5. Data-sharing opt-in and gated retraining (step 6).

## Open questions
- ~~Which dice?~~ The owner's and friends' dice (all are in the data); more come later via opt-in sharing.
- Camera: fixed mount over the tray, or handheld? This decides how voting and the top-face rule work.
- What does the webserver need: values only, or die type and positions too?
- d4: which reading convention do these d4s use (top vertex vs bottom edge)? The crops are glyphs; the roll value needs a rule.
