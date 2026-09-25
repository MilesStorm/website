# SurrealDB: dice training data

Storage is split across three stores:

| Store | Holds | Where it's defined |
|---|---|---|
| **PostgreSQL** (auth) | The opt-in choice (`dataset_consent`, deleted with the account) | `services/auth/migrations` |
| **Redis** | The picture of each user's latest roll, held 10 minutes for "flag as wrong roll"; rate counters | `packages/web/src/capture.rs` |
| **SurrealDB** | Saved rolls, their pictures, and the deletion log | `database/schema/` (surrealkit) |

Shared and flagged roll pictures are stored in SurrealDB 3.x (in-cluster:
`surrealdb.surreal.svc.cluster.local:8000`), in namespace `milesstorm`, database `arcane`.
The schema and the two logins are managed with [surrealkit](https://crates.io/crates/surrealkit)
(`cargo install surrealkit`, tested with 0.7.0): `database/schema/` is the desired state,
`database/tests/` checks what each login may do.

| Login | Role | Used by | Kubernetes secret |
|---|---|---|---|
| root (Infisical `cluster-infra-6c0g` / prod / `/surreal`) | root | these setup steps only | – |
| `dice` | EDITOR on `arcane` only | the website (frontend) | `surreal-dice` (`username`, `password`) in namespace `frontend` |
| `dice_reader` | VIEWER on `arcane` only | `services/ai_pipeline/tools/pull_dataset.py` | none; env vars on the training PC |

EDITOR is the smallest SurrealDB role that can write records. It could also change
or drop the tables in `arcane` (confirmed on 3.2.4), but not create logins or touch
anything outside that database. That is accepted for the website.

## Setup (and after every schema change)
Run from this folder (surrealkit reads `./database` and `surrealkit.toml` from the
current directory), with SurrealDB reachable, e.g. through
`kubectl -n surreal port-forward svc/surrealdb 8000:8000`.

| Variable | Value |
|---|---|
| `SURREALDB_HOST` | `http://localhost:8000` (the port-forward) |
| `SURREALDB_NAMESPACE` / `SURREALDB_NAME` | `milesstorm` / `arcane` |
| `SURREALDB_USER` / `SURREALDB_PASSWORD` | root, from Infisical `cluster-infra-6c0g` / prod / `/surreal` (`username`, `password`) |
| `SURREALKIT_VAR_DICE_PASSWORD` | the `dice` login's password (new random value the first time, e.g. `openssl rand -hex 24`; keep it in Infisical) |
| `SURREALKIT_VAR_DICE_READER_PASSWORD` | the `dice_reader` login's password (same) |

```
surrealkit test    # runs database/tests in a throwaway namespace, then removes it
surrealkit sync    # applies database/schema to milesstorm/arcane; safe to re-run
```

`sync` only applies files that changed (it prints "schema already in sync" otherwise)
and removes objects it created that are no longer in `database/schema/`. The passwords
are filled in at run time; surrealkit keeps only a hash of each statement, so they are
never in git or in its tracking table. Use letters and digits only (a `'` would break
the statement).

**Changing a password:** surrealkit re-applies a file only when its text changes, not
when a password does. Update the password in Infisical, change the "Passwords last set"
date in `database/schema/users.surql`, and run `sync`.

Then sync the `dice` login into the Kubernetes secret `surreal-dice` (keys `username`,
`password`) in the `frontend` namespace, the same way the other secrets are synced from
Infisical. The frontend deployment reads it.

## Deploy order
1. **ai_pipeline.** It numbers frames (`frame_seq`). Without it the website can't
   match a roll to its picture, and nothing is held or saved.
2. **auth.** It adds `dataset_consent` (its migration runs at startup). With an older
   auth, the profile shows "Couldn't load your sharing setting" and automatic samples
   pause. Deleting still works.
3. **SurrealDB:** `surrealkit sync` (above), then the `surreal-dice` secret.
4. **frontend.** Without the secret it runs with sharing and flagging switched off.

## Known limits / follow-ups
- **Account deletion.** The consent row goes with the account, but saved rolls are
  keyed by username. When accounts can be deleted, that path must also delete the
  user's SurrealDB rows and write a `dataset_deletion` entry. Otherwise a reused
  username would inherit them.
- **Pictures already moved into training data.** `pull_dataset.py` removes deleted
  users' pictures from `data_incoming/` only. If a picture was reviewed and copied into
  `data/`, keep its origin (`<user>__<roll_id>` in the name) so it can be removed
  there too.
- **Only the latest roll per user is held.** With two cameras, flagging the older
  roll gets "can't be flagged any more".
- **Size limit.** SurrealDB's HTTP `/rpc` body limit is 4 MiB by default (measured on
  3.2.4). The largest save is a 2 MiB frame in base64, about 2.8 MB. Don't lower
  `SURREAL_HTTP_MAX_RPC_BODY_SIZE` below that.
- **Camera connections.** Each one buffers up to 16 recent frames (2 MiB cap each).
  The number of camera connections per user isn't limited.

## Notes
- Inside the cluster the website talks to SurrealDB over plain HTTP with Basic auth,
  like the other in-cluster services. It's worth a NetworkPolicy that lets only the
  frontend pods (and your port-forward) reach SurrealDB.
- Roll pictures are held in Redis for 10 minutes so "flag as wrong roll" can save
  them. Only each user's latest roll is held. Nothing reaches SurrealDB unless the
  user opted in on their profile, or flagged that roll.
