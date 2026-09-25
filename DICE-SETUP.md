# Arcane dice: getting it running

Everything in this repo deploys the normal way: merge to `main`, CI builds the images,
Argo deploys them. That includes the SurrealDB tables, which the website creates itself
when it starts (like auth does with its PostgreSQL tables).

What this repo can't do is infrastructure: a SurrealDB database, its logins, and the
secret with the website's password. Those are step 1, in your infrastructure repo.

---

## Step 1: Infrastructure (in your infrastructure repo)

The website expects the following. The login and database details are also in
`services/frontend/surreal/README.md`.

| # | What | Exact value |
|---|---|---|
| 1 | SurrealDB namespace / database | `milesstorm` / `arcane` |
| 2 | The website's SurrealDB login | database-level user `dice` in `milesstorm`/`arcane`, role `EDITOR` |
| 3 | The training tool's SurrealDB login | database-level user `dice_reader` in `milesstorm`/`arcane`, role `VIEWER` |
| 4 | The website's password | key `SURREAL_PASS` in the Kubernetes secret `website-secrets`, namespace `frontend` (the secret the website already reads `BFF_SERVICE_SECRET` from), holding the password of `dice` |
| 5 | Network | pods in namespace `frontend` can reach `surrealdb.surreal.svc.cluster.local:8000` |

The username (`dice`), address and database are already set in
`crds/frontend/deployment.yaml`. The website creates its own tables (step 2), so the
infrastructure doesn't need to know about them.

---

## Step 2: Merge `new_work` into `main`

On lambda:
```
cd /home/miles/Documents/code/website
git push origin new_work
gh pr create --repo MilesStorm/website --base main --head new_work --title "Arcane dice: live rolls, extension, roll sharing" --body "See DICE-SETUP.md"
```
Merge the PR on GitHub. CI builds `ai-pipeline`, `auth` and `frontend`; Argo Image
Updater picks up the new images and Argo deploys them. The model weights come from the
GitHub release `dice-head-v1`, which already exists.

Order doesn't matter, and step 1 can happen before or after: each part keeps working
(with less) until the others are there. The website retries SurrealDB in the background
and switches sharing on by itself once it can create its tables.

### How to tell it worked
- **Website logs** (`frontend` pods) contain
  `roll-sharing schema applied; sharing and flagging are on`. If they instead repeat
  `applying the roll-sharing schema failed`, the log line says why (for example
  `database signin failed`: check items 2 and 4; `Failed connecting`: check item 5).
- **Arcane page:** shows dice values and a **Last roll:** line. The first roll after the
  dice server starts can take up to 90 seconds (GPU warm-up).
- **Profile page:** a section **Help improve dice recognition** with an on/off switch
  (you need the `arcane` permission).

---

## Step 3: The Firefox extension

A browser extension isn't deployed by CI; each person installs it in their Firefox.

**Quick install (until Firefox restarts):**
1. Open `about:debugging#/runtime/this-firefox`.
2. Click **Load Temporary Add-on…** and pick
   `/home/miles/Documents/code/website/extension/arcane-dice/manifest.json`.
3. Click the dice icon in the toolbar. If it asks for permission to reach
   milesstorm.com, allow it. If it says you're not logged in, log in on milesstorm.com.

You should see `Live, as <your username>`, and your latest roll after the next throw.

**Permanent install:** Firefox only keeps add-ons signed by Mozilla ("unlisted" = signed
but not published).
1. Create API keys at https://addons.mozilla.org/developers/addon/api/key/: a "JWT
   issuer" and a "JWT secret".
2. On lambda:
   ```
   cd /home/miles/Documents/code/website/extension/arcane-dice
   npx web-ext sign --channel unlisted --api-key <JWT issuer> --api-secret <JWT secret>
   ```
3. Open the `.xpi` file it writes into `web-ext-artifacts/` in Firefox and confirm.
   Friends can install the same file; they need the `arcane` permission on their account.

---

## Step 4: Download shared pictures for training (whenever you want new data)

This is part of training, not deployment. It runs on the training PC and needs to reach
SurrealDB, for example through a port-forward on the machine with kubectl plus an ssh
tunnel to it:
```
# on the machine with kubectl
kubectl -n surreal port-forward svc/surrealdb 8000:8000
# on lambda (replace USER@KUBE-PC)
ssh -N -L 8000:localhost:8000 USER@KUBE-PC
# on lambda, in another terminal
cd /home/miles/Documents/code/website/services/ai_pipeline
env SURREAL_USER=dice_reader SURREAL_PASS=<password of dice_reader> .venv/bin/python tools/pull_dataset.py --url http://localhost:8000
```
Pictures land in `services/ai_pipeline/data_incoming/<date>/` (never in `data/`). Add
`--dry-run` to only list what's new.

---

## Changing things later

| To change | Do this |
|---|---|
| SurrealDB tables | Edit `services/frontend/surreal/database/schema/` and merge; the website applies it when it starts. Read "Changing the tables" in `services/frontend/surreal/README.md` first: removing is never automatic, and a field must be made optional before it's removed. |
| A SurrealDB password | Change it in the infrastructure (items 2 to 4 of step 1). The website uses the new `SURREAL_PASS` after its next restart. |
| The model | New release like `dice-head-v1`, then update the URL, `--checksum=sha256:` and `DICE_MODEL_ID` in `services/ai_pipeline/Dockerfile`, and merge. |
| How often rolls are sampled | `DATASET_SAMPLE_EVERY` in `crds/frontend/deployment.yaml` (default 20: 1 roll in 20, plus every unsure roll). |

All settings: `ENV.md`.

## Still open
- `services/ai_pipeline/weights/model/model.bpk` (old, unused weights) is still in git.
  Remove or keep?
- A page for labelling the downloaded pictures in `data_incoming/`.
- When account deletion is added, it must also delete that user's pictures in
  SurrealDB.
