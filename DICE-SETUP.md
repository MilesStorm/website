# Arcane dice: getting it running

Five steps take branch `new_work` live. Do them in order. Each step says **where** to
run it, **what** to run (copy-paste), and **what you should see**.

Two machines are involved:

| Name here | What it is |
|---|---|
| **lambda** | this PC: the repo, the trained model, `gh`, `surrealkit` |
| **kube PC** | the machine where you run `kubectl` |

The commands work in fish, bash and zsh.

---

## Step 1: Upload the model weights to GitHub

**Why:** the dice server's image downloads the weights from a GitHub release while it
builds. If the release doesn't exist, the build fails.

**Where:** lambda.

```
gh release create dice-head-v1 /home/miles/Documents/code/website/services/ai_pipeline/runs/head_torch/resnet18_final/dice_head_resnet18.safetensors --repo MilesStorm/website --title "Dice head v1 (ResNet18)" --notes "ResNet18 dice head"
```

**You should see:** a link ending in `/releases/tag/dice-head-v1`. Open it; it lists
`dice_head_resnet18.safetensors`, 44.8 MB.

---

## Step 2: Create the picture store in SurrealDB

**Why:** shared and flagged roll pictures are saved in SurrealDB, in namespace
`milesstorm`, database `arcane`. surrealkit creates the tables and two logins there:

| Login | Used by | Can |
|---|---|---|
| `dice` | the website | read and write the dice tables |
| `dice_reader` | the training download tool (step 5) | only read them |

### 2a. Make two new passwords
**Where:** lambda. Run this twice and keep both results:
```
openssl rand -hex 24
```
The first is the **dice password**, the second the **dice_reader password**. Save both
in Infisical.

### 2b. Connect lambda to SurrealDB
You need two terminals, left open until step 2c is done.

**Terminal 1, on the kube PC:**
```
kubectl -n surreal port-forward svc/surrealdb 8000:8000
```
You should see `Forwarding from 127.0.0.1:8000 -> 8000`.

**Terminal 2, on lambda** (replace `USER@KUBE-PC` with how you ssh into the kube PC):
```
ssh -N -L 8000:localhost:8000 USER@KUBE-PC
```
It prints nothing and keeps running; that's correct. SurrealDB is now at
`http://localhost:8000` on lambda, through ssh (encrypted).

### 2c. Run surrealkit
**Where:** a third terminal, on lambda.

First get the SurrealDB **admin** login: in Infisical, project `cluster-infra-6c0g`,
environment `prod`, folder `/surreal`, copy `username` and `password`.

Then replace the four `...` values below and run:
```
cd /home/miles/Documents/code/website/services/frontend/surreal

env SURREALDB_HOST=http://localhost:8000 SURREALDB_NAMESPACE=milesstorm SURREALDB_NAME=arcane SURREALDB_USER=...admin-username... SURREALDB_PASSWORD=...admin-password... SURREALKIT_VAR_DICE_PASSWORD=...dice-password... SURREALKIT_VAR_DICE_READER_PASSWORD=...dice_reader-password... ~/.cargo/bin/surrealkit test

env SURREALDB_HOST=http://localhost:8000 SURREALDB_NAMESPACE=milesstorm SURREALDB_NAME=arcane SURREALDB_USER=...admin-username... SURREALDB_PASSWORD=...admin-password... SURREALKIT_VAR_DICE_PASSWORD=...dice-password... SURREALKIT_VAR_DICE_READER_PASSWORD=...dice_reader-password... ~/.cargo/bin/surrealkit sync
```

**You should see:**
- after `test`: `cases: 6 total, 6 passed, 0 failed`. It ran in a temporary copy and
  changed nothing real;
- after `sync`: four `applied database/schema/….surql` lines. Running `sync` again
  prints `schema already in sync`.

You can now close terminals 1 and 2.

### 2d. Give the website its password
The website reads the dice password from `website-secrets`, the same secret that
already holds `CLIENT_ID`, `CLIENT_SECRET`, `G_CLIENT_ID` and the rest. Add one key to
it in Infisical, next to those:

| Key | Value |
|---|---|
| `SURREAL_PASS` | the dice password from 2a |

(The username, `dice`, is already set in `crds/frontend/deployment.yaml`.)

**You should see**, once your pull has updated the secret (on the kube PC):
```
kubectl -n frontend get secret website-secrets -o jsonpath='{.data.SURREAL_PASS}'
```
prints a long random-looking text (not nothing).

If the key is missing, the website still runs; only sharing and flagging are switched
off.

---

## Step 3: Deploy (merge to `main`)

**Why:** GitHub Actions builds and deploys only from `main`. Pushing `new_work` alone
deploys nothing.

**Where:** lambda.
```
cd /home/miles/Documents/code/website
git push origin new_work
gh pr create --repo MilesStorm/website --base main --head new_work --title "Arcane dice: live rolls, extension, roll sharing" --body "See DICE-SETUP.md"
```
Review the PR on GitHub and merge it. That starts three builds (dice server, auth,
website); ArgoCD then rolls them out. The dice server build is slow (CUDA image).

**You should see**, once ArgoCD shows all three as synced and healthy:
- on milesstorm.com, the Arcane page shows dice values and a **Last roll:** line. The
  first roll after the dice server starts can take up to 90 seconds (GPU warm-up);
- on your profile page, a section **Help improve dice recognition** with an on/off
  switch. You need the `arcane` permission to see it.

The three services don't need to go live in a particular order. Each one keeps
working, with less functionality, while the others are still updating.

---

## Step 4: Install the Firefox extension

### Quick install (until Firefox restarts)
**Where:** lambda, or any PC with a copy of the repo.
1. In Firefox, open `about:debugging#/runtime/this-firefox`.
2. Click **Load Temporary Add-on…**.
3. Pick `/home/miles/Documents/code/website/extension/arcane-dice/manifest.json`.
4. Click the dice icon in the toolbar. If it asks for permission to reach
   milesstorm.com, click the button and allow it. If it says you're not logged in,
   log in on milesstorm.com.

**You should see:** `Live, as <your username>`, and your latest roll after the next
throw.

### Permanent install
Temporary add-ons disappear when Firefox restarts. A permanent one must be signed by
Mozilla (it stays private; "unlisted" means it's not published):
1. Get API keys at https://addons.mozilla.org/developers/addon/api/key/ (log in, then
   **Generate new credentials**). You get a "JWT issuer" and a "JWT secret".
2. On lambda:
   ```
   cd /home/miles/Documents/code/website/extension/arcane-dice
   npx web-ext sign --channel unlisted --api-key ...JWT-issuer... --api-secret ...JWT-secret...
   ```
3. It creates a `.xpi` file in `web-ext-artifacts/`. Open that file in Firefox
   (drag it into a window) and confirm the install. Send the same file to friends; they
   also need the `arcane` permission on their account.

---

## Step 5: Download shared pictures for training (repeat whenever you want)

**Where:** lambda, with terminals 1 and 2 from step 2b running again.
```
cd /home/miles/Documents/code/website/services/ai_pipeline
env SURREAL_USER=dice_reader SURREAL_PASS=...dice_reader-password... .venv/bin/python tools/pull_dataset.py --url http://localhost:8000
```

**You should see:** a count of downloaded rolls. The pictures land in
`services/ai_pipeline/data_incoming/<date>/`, each as a `.jpg` plus a `.json` (what the
model read, and the user's correction if they flagged it). Nothing is written to
`data/`.

To only list what's new without downloading, add `--dry-run` at the end. If a user
pressed "Delete all pictures I've shared", the next run also deletes their pictures
from `data_incoming/`.

---

## Changing things later

| To change | Do this |
|---|---|
| The SurrealDB tables | Edit the files in `services/frontend/surreal/database/schema/`, then repeat step 2b and 2c. |
| A SurrealDB login's password | Make a new password (2a) and save it in Infisical. In `services/frontend/surreal/database/schema/users.surql`, change the date on the line `-- Passwords last set:` (without that, surrealkit won't notice the change). Repeat 2b and 2c. If it's the `dice` password, also update `SURREAL_PASS` in Infisical (2d); the website picks it up when it restarts. |
| The model | Publish a new release like step 1 under a new tag. In `services/ai_pipeline/Dockerfile`, update the download URL, the `--checksum=sha256:` value and `DICE_MODEL_ID`. |
| How often rolls are sampled | Set `DATASET_SAMPLE_EVERY` on the website (in `crds/frontend/deployment.yaml`). Default `20`: 1 in 20 rolls, plus every roll the model was unsure about. |

All settings are listed in `ENV.md`.

## Still open
- `services/ai_pipeline/weights/model/model.bpk` (old, unused weights) is still in git.
  Remove or keep?
- A page for labelling the downloaded pictures in `data_incoming/`.
- When account deletion is added, it must also delete that user's pictures in
  SurrealDB.
- When copying downloaded pictures into `data/`, keep the `<user>__<roll_id>` file name,
  so they can be found again if that user deletes their pictures.
