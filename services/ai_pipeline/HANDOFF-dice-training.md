# Handoff: dice detector training — continue on the GPU machine

> Continues `goal-this-repository-is-atomic-chipmunk.md` (the original P0:
> the shipped YOLO is stock COCO, hence no boxes). Prepared 2026-06-11 on a
> GPU-less machine; everything below is staged to run where the **data and
> GPU live**. Same convention: **[FACT]** = verified, **[DECISION]** = choice
> made with the owner.

## What the next session needs to know first

1. **[FACT] The only labeled data is the classifier set** —
   `data/dice_face/<face>/*.jpg`: tight crops of already-boxed dice, label =
   folder name (`1`–`9`, `0` = d10 zero glyph, `10`–`20`), **no bounding
   boxes exist anywhere**. This is the `load_dataset_folder` format, the only
   loader wired into `main.rs`. The `load_dataset` / `obj_train_data` YOLO
   loader in `dataset.rs` is dead code — **do not** assume a bbox dataset
   exists (that mistake was already made and corrected in this session).
2. **[FACT] The export contract is fully recovered** from the stock
   `src/model/yolo26n.onnx` metadata: ultralytics **8.4.7**, pytorch
   **2.9.1**, opset **22**, args `batch=1, half=False, dynamic=False,
   simplify=True, opset=None`, `end2end=True`, `images [1,3,640,640]` →
   `output0 [1,300,6]`. YOLO26's end-to-end output shape is independent of
   class count, so a 1-class fine-tune re-exported with the pinned versions
   (`training/requirements.txt`) keeps the contract — the original handoff's
   "highest-risk assumption" is resolved. No Rust parser changes needed.
3. **[DECISION] Detection data is synthesized from the crops** (compositing
   onto backgrounds; paste rect = exact GT box), because the crops are tight
   (zero-shot auto-boxing them would yield degenerate whole-image boxes).
   Face values ride along in `meta.jsonl` for the head refresh.

## State of the working tree (uncommitted)

- `training/` (new) — the kit, all scripts dry-run-validated on CPU with
  synthetic stand-in data; see `training/README.md` for the full run order:
  - `requirements.txt` — pinned to reproduce the export contract
  - `make_detection_dataset.py` — crops + backgrounds → Ultralytics
    single-class dataset + `meta.jsonl`. Crops are split 90/10 **before**
    compositing so no die appears in both train and val composites.
  - `train_detector.py` — fine-tunes `yolo26n.pt` (downloads on first run)
  - `export_detector.py` — exports with the exact recipe, runs the verify
    gate, backs up the old ONNX, installs into `src/model/yolo26n.onnx`
  - `verify_export.py` — contract assertions + optional smoke inference;
    confirmed it passes the stock model as 80-class and rejects it as 1-class
  - `make_head_crops.py` — new detector + composites → detector-framed head
    crops in `data/dice_face_detected/<face>/` (exact predicted box, no
    padding, matching `crop_for_head`)
- `src/model/inferance.rs` (modified, `cargo check` clean):
  - first-frame log of YOLO output rows / max conf / threshold; loud error +
    empty result (not a silent misparse) if output length isn't divisible by 6
  - `YOLO_CONF` env var overrides `DEFAULT_CONF=0.25` without rebuilding

## Run order on the GPU machine

Exactly `training/README.md`. Inputs the owner must supply:

- location of the real `data/dice_face` (not in git)
- a backgrounds folder for step 1 — **best: a few hundred frames of the real
  desk/table without dice**; procedural fallback exists but expect more false
  positives on real clutter

Sequence: synthesize → train → export+verify+install → `cargo build --release`
→ serve + `test.html` → tune `YOLO_CONF` → `make_head_crops.py` → review →
merge crops → `cargo run --release -- folder` → `eval`.

## Judgment calls already made (don't relitigate without cause)

- ±15° sprite rotation cap, matching `augment_crop`'s rotation range so
  detector-emitted crops stay inside the head's training distribution.
- Feathered rounded-rect alpha on pasted sprites so the detector can't learn
  "sharp square seam = die"; GT box taken from the rotated sprite's actual
  alpha extent.
- ~10% background-only negatives; 1–3 dice per composite (60/30/10).
- Default 4000 train / 500 val composites — cheap to regenerate; scale up
  before adding epochs if underfitting.

## Open items / risks (in priority order)

1. **Sim-to-real gap** — the main risk. Mitigation already staged: real
   backgrounds. If live-camera performance disappoints: record real frames,
   auto-label with YOLO-World (prompt "dice") or hand-label a few hundred,
   mix into the dataset, retrain.
2. **Re-tune confidence** — 0.25 is a COCO-era default; sweep `YOLO_CONF`
   live against the camera.
3. **Letterbox parity (P1, deferred)** — Rust squash-resizes to 640;
   Ultralytics trains letterboxed. Accuracy-only concern; fix in
   `inferance.rs` (letterbox + unpad) only if boxes look weak.
4. **Best-epoch checkpoint bug (P1)** — `training.rs:171–179`: burn-train
   returns the final epoch, not best-valid. Affects the head retrain step;
   acceptable short-term, fix = file checkpointer + restore-best.
5. **P2 hardening** (after it works end-to-end): NaN-unsafe
   `partial_cmp().unwrap()` argmax at `inferance.rs`, `serde_json` unwrap in
   `serve.rs`, dropped-frame metric, a smoke test feeding a known JPEG
   through `DicePipeline::infer_frame`.

## Verification of success (from the original handoff)

Stable box tracking the die via `test.html`, `yolo_class == 0`, `yolo_conf`
healthy; first-frame log shows ~300 rows; then judge `dice_class` accuracy on
known rolls; finally re-check through the BFF `/ws/arcane` path.
