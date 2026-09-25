# Arcane dice: getting it running

Everything needed to take branch `new_work` live: dice recognition, the live roll in
the Firefox popup, opt-in roll sharing, and "flag as wrong roll". Do the steps in
order; each one says how to tell it worked.

## What's where

| Part | Folder | Deployed by |
|---|---|---|
| Dice server (GPU, `eta` only) | `services/ai_pipeline` | CI on `main` → ArgoCD (`crds/ai-pipeline`) |
| Login service (adds the sharing choice) | `services/auth` | CI on `main` → ArgoCD (`crds/auth`) |
| Website (profile switch, live rolls, flag endpoint) | `services/frontend` | CI on `main` → ArgoCD (`crds/frontend`) |
| Picture store schema and logins | `services/frontend/surreal` | you, with `surrealkit sync` |
| Browser extension | `extension/arcane-dice` | you, in Firefox |
| Training download tool | `services/ai_pipeline/tools/pull_dataset.py` | you, on the training PC |

## 1. Upload the model weights (before merging)
The dice server's image downloads the weights from a GitHub release and checks their
SHA-256. Without the release, the CI build fails.

```
gh release create dice-head-v1 \
  /home/miles/Documents/code/website/services/ai_pipeline/runs/head_torch/resnet18_final/dice_head_resnet18.safetensors \
  --repo MilesStorm/website --title "Dice head v1 (ResNet18)"
```

**Worked if** https://github.com/MilesStorm/website/releases/tag/dice-head-v1 lists
`dice_head_resnet18.safetensors` (44.8 MB).

## 2. Set up the picture store (SurrealDB)
Full details: `services/frontend/surreal/README.md`. In short:

1. Make two new passwords (letters and digits, e.g. `openssl rand -hex 24`) for the
   logins `dice` (website) and `dice_reader` (training tool). Store them in Infisical.
2. Make SurrealDB reachable: `kubectl -n surreal port-forward svc/surrealdb 8000:8000`.
3. From `services/frontend/surreal/`, with these set:

   | Variable | Value |
   |---|---|
   | `SURREALDB_HOST` | `http://localhost:8000` |
   | `SURREALDB_NAMESPACE` / `SURREALDB_NAME` | `milesstorm` / `arcane` |
   | `SURREALDB_USER` / `SURREALDB_PASSWORD` | admin credentials, Infisical `cluster-infra-6c0g` / prod / `/surreal` |
   | `SURREALKIT_VAR_DICE_PASSWORD` | the new `dice` password |
   | `SURREALKIT_VAR_DICE_READER_PASSWORD` | the new `dice_reader` password |

   run:
   ```
   surrealkit test
   surrealkit sync
   ```
4. Put the `dice` login into the Kubernetes secret `surreal-dice` (keys `username` =
   `dice`, `password`) in namespace `frontend`, the same way your other secrets come
   from Infisical.

**Worked if** `surrealkit test` reports 6 passed and a second `surrealkit sync` says
"schema already in sync". Without the secret the website still runs, with sharing and
flagging switched off.

## 3. Merge to `main`
CI builds only from `main`: open a PR from `new_work` and merge it. That builds all
three images; ArgoCD rolls them out.

The best order is dice server → auth → website, but it isn't required. Each part
copes with the others being older:

| If this is still old… | …then until it updates |
|---|---|
| dice server | live rolls work; nothing can be flagged or sampled (no frame numbers) |
| auth | profile says "Couldn't load your sharing setting"; automatic samples pause; deleting still works |

**Worked if**:
- the Arcane page shows dice values and a "Last roll" line;
- a user with the `arcane` permission sees "Help improve dice recognition" on their profile.

The first roll after the dice server starts takes about 30 to 90 seconds (GPU warm-up).

## 4. Install the extension (Firefox)
1. Open `about:debugging#/runtime/this-firefox`, click **Load Temporary Add-on…**, and
   pick `extension/arcane-dice/manifest.json`.
2. Click its toolbar icon. If it asks for permission to reach milesstorm.com, click
   the button. Log in on the website if it says so.
3. Temporary add-ons vanish when Firefox restarts. For a permanent install, sign it
   once: `npx web-ext sign --channel unlisted` (needs addons.mozilla.org API keys).

**Worked if** the popup says "Live, as <you>" and shows your latest roll. Friends need
the `arcane` permission on their account.

## 5. Pull shared pictures for training (whenever you want new data)
On the training PC, with SurrealDB reachable (a port-forward as in step 2):
```
cd services/ai_pipeline
SURREAL_USER=dice_reader SURREAL_PASS=<dice_reader password> \
  .venv/bin/python tools/pull_dataset.py --url http://localhost:8000
```
Pictures land in `data_incoming/<date>/` (never in `data/`). Add `--dry-run` to only
list what's new. Pictures of users who pressed "delete all" are removed from
`data_incoming/` on the next pull.

## Changing things later

| Change | How |
|---|---|
| Table layout in SurrealDB | edit `services/frontend/surreal/database/schema/`, then `surrealkit test` and `surrealkit sync` |
| A SurrealDB login's password | new value in Infisical, change the "Passwords last set" date in `database/schema/users.surql`, `surrealkit sync`, update `surreal-dice` if it's the website's |
| New model weights | new release tag, then update the URL, checksum and `DICE_MODEL_ID` in `services/ai_pipeline/Dockerfile` |
| How often rolls are sampled | `DATASET_SAMPLE_EVERY` on the frontend (default: 1 roll in 20, plus every unsure roll) |

All environment variables: `ENV.md`.

## Still open
- `services/ai_pipeline/weights/model/model.bpk` (old weights, unused) is still in git.
  Remove it or keep it?
- A page for labelling the pulled pictures in `data_incoming/`.
- Deleting an account must also delete that user's pictures in SurrealDB (accounts
  can't be deleted yet, so nothing is affected today).
- When copying pulled pictures into `data/`, keep the `<user>__<roll_id>` name so they
  can be found again if that user deletes their pictures.
