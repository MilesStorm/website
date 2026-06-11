# AI Pipeline — Handoff: what's left to make it work

> Audience: a capable engineer or stronger model who will finish this service.
> Convention used throughout: **[FACT]** = verified by reading the code/model in this
> repo. **[CONJECTURE]** = my opinion/inference; may be wrong, verify before trusting.

---

## Context

`services/ai_pipeline` is a Rust microservice (Burn + CUDA + tokio-tungstenite). It is a
**WebSocket server** (default `0.0.0.0:9000`). The BFF/browser connects to it, streams
camera frames as **binary JPEG/PNG messages**, and the service replies with **JSON**:

```json
{"detections":[{"x1","y1","x2","y2","yolo_conf","yolo_class","dice_class","dice_conf"}],"frame_ms":N}
```

**Intended design (confirmed with the owner):** a YOLO model bounding-boxes dice with a
single `dice` class; for each box, a custom classifier head (`DiceHead`) reads the
rolled value. 21 head classes map to die faces (1–9, 0/d10, 10–20).

**Observed symptom (from owner):** *no boxes at all*, and occasionally *boxes that are
wrong/jumpy*. Value classification can't even be judged yet because good boxes never
arrive.

**Current state:** end-to-end plumbing works — frames arrive through the BFF proxy,
inference runs, JSON returns (the occasional jumpy box proves this). The blocker is the
detector model itself.

---

## Root cause (the actual blocker) — **[FACT]**

**The loaded YOLO is the stock Ultralytics COCO model, not a dice detector.**

Evidence (verified):
- `src/model/yolo26n.onnx` embeds the literal COCO class list:
  `{0:'person',1:'bicycle',2:'car', … ,79:'toothbrush'}` (80 classes, **no `die`**).
- The generated graph (`target/.../out/model/yolo26n.rs`) does
  `split_with_sizes([4, 80])` → confirms 4 bbox + **80 class** channels.
- Only commit touching it: `8dfccc3 add dice detection head` — i.e. the head was added,
  the detector was never replaced.

This single fact explains both symptoms:
- **No boxes:** a die rarely exceeds `DEFAULT_CONF = 0.25` (`inferance.rs:25`) as any COCO
  class, so the loop in `inferance.rs:98` emits nothing.
- **Jumpy boxes:** when it does fire, it's matching `cell phone`/`remote`/`clock`/etc. —
  wrong object, loose/unstable box.

### Important corollary — the Rust side is mostly already correct — **[FACT]**

The detector output contract is **`[1, 300, 6]` per row `[x1,y1,x2,y2,conf,class]`**,
NMS-free (the ONNX graph does an internal `topk(300)`; see `yolo26n.rs:1601,1619,1646`).
The inference parser `raw.chunks_exact(6)` (`inferance.rs:98`) matches this exactly.

> An earlier analysis guessing `[1,300,84]` was wrong — that's the *intermediate*
> pre-topk shape, not the model's final output. **Do not "fix" the `chunks_exact(6)`
> parsing.**

A re-exported **single-class** dice YOLO (end-to-end) keeps the **same `[1,300,6]`
contract** — `class` is just always `0`. So **swapping the model file requires little to
no Rust change.**

---

## Architecture decision & resourcing (read before P0)

The owner's hardware is a single **RTX 4070 Ti (12 GB)** and was worried full retraining
is "extremely resource intensive." It isn't — that worry conflates *fine-tuning* with
*training from scratch*. Clearing this up:

- **This is transfer learning, not from-scratch training.** The expensive part (general
  visual features from ImageNet/COCO) is already baked into the pretrained weights. You
  start from those and nudge them onto dice. **[FACT]** fine-tuning YOLO-nano/small at 640px
  on a 4070 Ti is an **hours-not-days** job; the `DiceHead` (tiny ~8 MB CNN on 128×128
  crops) trains in **minutes-to-an-hour**. Compute is a non-issue here.
- **[OPINION] The real bottleneck is labeled data, not the GPU.** Budget effort there.

**Keep YOLO, and keep the two-stage design.** **[OPINION, well-grounded]**

- *Why YOLO over alternatives:* for real-time localization in a video stream, YOLO-nano
  single-pass is the pragmatic best. RT-DETR/transformers are heavier and finicky with no
  accuracy win for "find a die"; classical CV is brittle to lighting/background.
- *Why two-stage (detect → crop → classify) over one fat multi-class YOLO:* localization
  and fine-grained value-reading are different difficulties. Finding a die is easy; reading
  which number is up on a **d20** is hard and benefits enormously from a high-res normalized
  128×128 crop. A single YOLO would classify the value at detection resolution, where a
  small die in the frame is heavily downsampled — fine for d6 pips, poor for polyhedral
  numbers. Two-stage also lets you **improve the classifier independently** of the detector,
  and keeps the detector to **one easy class** ("dice"). The current split was the right
  instinct; preserve it.
- *The one alternative worth naming:* collapse to a single YOLO whose classes are the
  values. Simpler, but loses fine-grained reading accuracy and independent iteration. Not
  recommended for d20 support.

**Scope of training work (neither step is heavy):**
1. **Fine-tune the detector** (single `dice` class) — the actual missing piece; this alone
   should get boxes back.
2. **Then refresh the head** on crops produced by the *real* detector, to close the
   framing/padding domain gap (and the best-epoch bug). Small, fast run.

**[OPINION] Data tactics that make this tractable:**
- **Synthetic data is a cheat code for dice.** Render in Blender (random pose/lighting/
  background); you get the **bbox AND the top-face value as labels for free**, unlimited
  quantity — especially valuable for rare d20 faces. Mix in some real frames to close the
  sim-to-real gap.
- Lean on Ultralytics' built-in augmentation so a few thousand images go far.

---

## Architecture as-built (orientation map)

| Concern | Location |
|---|---|
| Entrypoint / CLI modes (`yolo` serve, `folder` train, `eval`) | `src/main.rs` |
| WebSocket server + single shared GPU inference thread (mpsc bound=1, drops excess frames) | `src/serve.rs` |
| Pipeline: YOLO → crop → DiceHead, the `Detection` struct | `src/model/inferance.rs` |
| Classifier head (stem→3 res stages→GAP→FC, 21 classes) | `src/model/head.rs` |
| Training loop / dataset loaders | `src/model/training.rs`, `src/datasets/*` |
| ONNX→Rust codegen at build | `build.rs` (burn_onnx) |
| Detector weights | `src/model/yolo26n.onnx` (build) → `.bpk` (runtime), override `YOLO_MODEL_PATH` |
| Head weights | `weights/model/model.bpk` (exists, ~8.4 MB) or `DICE_HEAD_PATH` / `art/experiment_*` |

Key constants: `YOLO_INPUT=640`, `HEAD_INPUT=128`, `DEFAULT_CONF=0.25`
(`inferance.rs:20–25`).

---

## Work to finish — priority order

### P0 — Train a single-class dice YOLO and wire it in (this is the blocker)

This is the whole reason it doesn't work. Everything else is secondary.

1. **Dataset.** Collect/label dice images with one class `dice`, covering the die types
   the head supports (d6…d20, varied lighting/background/angle). **[CONJECTURE]** a few
   thousand labeled frames is a reasonable starting target; the existing
   `annotations.json` (COCO/YOLO format) loader suggests data tooling already exists —
   reuse it.
2. **Fine-tune** an Ultralytics YOLO (keep the `yolo26n` family to match the codegen
   path) on the single `dice` class.
3. **Export end-to-end** to ONNX **preserving the `[1,300,6]` NMS-free output** (the same
   form the current `.onnx` uses), so the Burn codegen and `chunks_exact(6)` parser keep
   working unchanged. **[CONJECTURE]** matching the export exactly (same opset / end-to-end
   flag) is the riskiest step — if the export shape differs, `burn_onnx` will regenerate a
   different `forward` and the parser assumptions break. Verify the new graph still ends in
   a `topk→[1,300,6]` shape.
4. **Swap it in:** replace `src/model/yolo26n.onnx` and rebuild (so `build.rs` regenerates),
   **or** point `YOLO_MODEL_PATH` at a new `.bpk`.
5. **Runtime shape assertion:** add a one-time check/log of the raw YOLO output length
   (`raw.len() % 6 == 0`, rows ≈ 300) on first frame, so a future model swap fails loud
   instead of silently misparsing.
6. **Re-tune `DEFAULT_CONF`** for the new single-class model; 0.25 may be wrong for it.

> With this done, "no boxes" should resolve. Then — and only then — the value-accuracy of
> the head becomes measurable.

### P1 — Preprocessing fidelity (only matters once a real detector exists)

- **[CONJECTURE] Letterbox, don't squash.** `inferance.rs:79` does a plain bilinear
  resize to 640×640, distorting aspect ratio. Ultralytics trains/evals with letterbox
  (aspect-preserve + pad). Coords are currently *self-consistent* (the squash is a uniform
  full-frame→640 map, so `/640` recovers normalized frame coords), so this is an *accuracy*
  concern, not a coordinate bug. Match whatever the fine-tune used.
- **[CONJECTURE] Crop padding parity.** `crop_for_head` (`inferance.rs:166`) crops the bbox
  exactly, then Lanczos3→128. If the head was trained on tighter/looser crops than YOLO
  emits, expect a domain gap. Make the inference crop match training crop framing (consider
  a small fixed padding ratio).

### P1 — Classifier head quality (becomes testable after P0)

- **[FACT] Best-epoch weights are not saved.** `training.rs:171–179` documents that
  burn-train 0.21 returns the **final-epoch** model, not the best-validation epoch; early
  stopping only halts, it doesn't restore best weights. So the shipped head may be
  under-trained. Wire a file checkpointer + restore-best, or re-train and manually keep the
  best checkpoint.
- **[FACT] YOLO-dataset eval path is `unimplemented!()`** (`training.rs:199`) — only
  folder-mode eval works. Implement if you want to evaluate on annotated frames.

### P2 — Robustness / hardening (do after it works end-to-end)

- **[FACT] Panics in the hot path** — turn into error JSON instead of crashing the
  inference thread:
  - `serve.rs:68` `serde_json::...unwrap()`
  - `inferance.rs:93` raw-tensor `.unwrap()`, `:119` softmax `.unwrap()`
  - `inferance.rs:124` `partial_cmp(...).unwrap()` — **NaN confidence will panic**; use a
    NaN-safe argmax.
- **[FACT] Silent frame drops.** mpsc bound = 1 (`serve.rs:31`) drops frames under load
  with no client signal. Acceptable for "latest frame wins", but **[CONJECTURE]** worth a
  dropped-frame metric/log so you can see it happening.
- **[FACT] No tests exist.** Add at least one end-to-end smoke test: feed a known JPEG of a
  die through `DicePipeline::infer_frame` and assert a box + plausible class.

---

## How to verify end-to-end

1. Build & run: `cargo run --release -- yolo 0.0.0.0:9000` (ensure head weights present via
   `weights/` or `DICE_HEAD_PATH`).
2. Open `test.html` (root of `ai_pipeline`), point the camera at a die.
3. **P0 success:** a stable bounding box tracks the die (no longer empty, no longer jumping
   between random objects). Confirm `yolo_conf` is healthy (e.g. >0.5) and `yolo_class==0`.
4. **Head success:** roll several known values; confirm `dice_class` matches the top face
   for d6 and at least one polyhedral die.
5. Temporarily log the raw YOLO output shape and max conf on the first frame to confirm the
   new model parses correctly.
6. Re-run through the real BFF path (`/ws/arcane`, requires `arcane` permission) to confirm
   the proxy still forwards binary→JSON unchanged.

---

## Open conjectures to double-check (don't take these as fact)

- That a stock-COCO→single-class fine-tune of `yolo26n` will re-export to the identical
  `[1,300,6]` end-to-end shape with no Burn codegen surprises. **Highest-risk assumption.**
- That the existing `DiceHead` weights in `weights/model/model.bpk` are good enough; given
  the best-epoch bug, they may need retraining once you can finally feed them real YOLO
  crops.
- That the head's 128×128 Lanczos crop framing matches its training distribution.
- That backpressure (bound=1) is the desired behavior rather than a small bounded queue.
