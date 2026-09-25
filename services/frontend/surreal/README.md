# SurrealDB: dice training data

Shared and flagged roll pictures are stored in SurrealDB 3.x (in-cluster:
`surrealdb.surreal.svc.cluster.local:8000`), in namespace `milesstorm`, database `arcane`.
The tables are described in `dataset.surql`.

| Login | Role | Used by | Kubernetes secret |
|---|---|---|---|
| root (Infisical `cluster-infra-6c0g` / prod / `/surreal`) | root | these setup steps only | – |
| `dice` | EDITOR on `arcane` only | the website (frontend) | `surreal-dice` (`username`, `password`) in namespace `frontend` |
| `dice_reader` | VIEWER on `arcane` only | `services/ai_pipeline/tools/pull_dataset.py` | none; env vars on the training PC |

EDITOR is the smallest SurrealDB role that can write records. It could also change
the tables in `arcane`, but not anything outside that database. That is accepted
for the website.

## One-time setup
1. Create the tables. Run as root from anywhere that can reach SurrealDB, e.g.
   through `kubectl -n surreal port-forward svc/surrealdb 8000:8000`:
   ```
   surreal sql --endpoint http://localhost:8000 --user <root> --pass <root> < dataset.surql
   ```
   It is safe to run again after edits.
2. Create the two logins, using new random passwords (e.g. `openssl rand -hex 24`),
   and store them in Infisical. They're typed here rather than kept in a file, so
   they never land in git:
   ```
   surreal sql --endpoint http://localhost:8000 --user <root> --pass <root> --ns milesstorm --db arcane
   > DEFINE USER IF NOT EXISTS dice ON DATABASE PASSWORD '<dice password>' ROLES EDITOR;
   > DEFINE USER IF NOT EXISTS dice_reader ON DATABASE PASSWORD '<reader password>' ROLES VIEWER;
   ```
3. Sync the `dice` login into the Kubernetes secret `surreal-dice` (keys `username`,
   `password`) in the `frontend` namespace, the same way the other secrets are synced
   from Infisical. The frontend deployment reads it.

## Notes
- Inside the cluster the website talks to SurrealDB over plain HTTP with Basic auth,
  like the other in-cluster services. It's worth a NetworkPolicy that lets only the
  frontend pods (and your port-forward) reach SurrealDB.
- Roll pictures are held in Redis for 10 minutes so "flag as wrong roll" can save
  them. Only each user's latest roll is held. Nothing reaches SurrealDB unless the
  user opted in on their profile, or flagged that roll.
