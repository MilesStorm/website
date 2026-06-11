# Dice detector training kit

The service ships the **stock COCO YOLO26n** (80 classes, no `dice`), which is
why no boxes appear. This kit produces a **single-class dice detector** with
the exact same ONNX contract, so the Rust side needs **no parser changes**.

## The data situation (read first)

The only labeled data in this project is the **DiceHead classifier set**:
`data/dice_face/<face>/*.jpg` — tight crops of already-boxed dice, label
inferred from the folder name (`1`–`9`, `0`, `10`–`20`), **no bounding boxes**
(this is the `load_dataset_folder` format in `src/datasets/dataset.rs`, the
only loader wired into `main.rs`).

A detector needs boxes, so `make_detection_dataset.py` **synthesizes** them:
it composites the crops onto background images at random scale, position and
rotation — the paste rectangle is an exact free ground-truth box. Face values
are carried into `meta.jsonl` so head crops can be regenerated later.

**Backgrounds matter:** pass `--backgrounds` with real images; a few hundred
frames of the actual desk/table *without dice* are ideal and cheap to record.
Procedural backgrounds are the fallback, at the cost of more false positives
on real clutter.

## Why the export is low-risk

The original `src/model/yolo26n.onnx` embeds its export recipe in metadata:

| | |
|---|---|
| producer | pytorch 2.9.1, ultralytics 8.4.7 |
| opset | 22 |
| export args | `batch=1, half=False, dynamic=False, simplify=True, opset=None` |
| contract | `images [1,3,640,640]` → `output0 [1,300,6]`, `end2end=True` |

YOLO26's end-to-end output is `[1, max_det, 6]` **regardless of class count**,
so a 1-class fine-tune exported with the pinned versions in `requirements.txt`
reproduces the identical contract. `verify_export.py` asserts this before
anything is swapped in, and the Rust side validates the output shape on the
first frame (`inferance.rs`), so a mismatch fails loud, not silent.

## Run order (on the GPU machine)

```bash
cd services/ai_pipeline/training
python3 -m venv .venv && source .venv/bin/activate   # fish: . .venv/bin/activate.fish
pip install -r requirements.txt

# 1. Synthesize the detection dataset from the classifier crops.
python make_detection_dataset.py ../data/dice_face --backgrounds /path/to/bg_frames

# 2. Fine-tune (transfer learning from COCO weights; fast on a 12 GB GPU).
python train_detector.py --data dice_dataset/dice.yaml

# 3. Export with the exact original recipe, verify the [1,300,6] contract,
#    and install into src/model/yolo26n.onnx (previous model is backed up).
python export_detector.py runs/dice_detector/weights/best.pt --image /path/to/die_photo.jpg

# 4. Rebuild — build.rs regenerates the Burn graph + .bpk from the new ONNX.
cd .. && cargo build --release

# 5. Serve and point a camera at a die (test.html).
cargo run --release -- yolo 0.0.0.0:9000
#    Tune the confidence cutoff without rebuilding:
YOLO_CONF=0.5 cargo run --release -- yolo 0.0.0.0:9000
```

**Success check:** a stable box tracks the die, `yolo_class == 0`, `yolo_conf`
healthy (>0.5). The service logs the first-frame output shape, row count and
max confidence — confirm rows ≈ 300.

## Then: refresh the DiceHead

The head was trained on hand-framed crops; the detector will frame dice
slightly differently. Regenerate head training data from the detector's own
predicted boxes (matched to the composites' known boxes to inherit labels):

```bash
cd training
python make_head_crops.py ../src/model/yolo26n.onnx dice_dataset
# review data/dice_face_detected/, merge into data/dice_face (or repoint main.rs)
cd .. && cargo run --release -- folder     # train head -> art/experiment_N
cargo run --release -- eval                # confusion matrix
```

`make_head_crops.py` also reports how many known boxes the detector missed —
a quick recall sanity check.

## Notes / open items

- **Sim-to-real gap is the main risk** of the compositing approach. If the
  detector underperforms on the live camera: record real frames, label a few
  hundred with boxes (or auto-label with YOLO-World prompt "dice" and skim),
  and mix them into the dataset. Real backgrounds in step 1 shrink this gap a
  lot on their own.
- **Letterbox (deliberately not done):** the Rust side squash-resizes to 640
  while Ultralytics trains with letterbox. Coordinates stay correct; it's only
  a possible accuracy cost. If boxes look good, skip it; if not, implement
  letterbox+unpad in `inferance.rs` rather than changing training.
- **Version drift:** `export_detector.py` refuses to install on contract
  mismatch, and the runtime first-frame check is the last line of defense.
- The Dockerfile copies `src/model/yolo26n.onnx` and bakes the `.bpk` at image
  build time — nothing to change there after the swap.
