# SurrealDB: dice training data

Storage is split across three stores:

| Store | Holds | Defined in |
|---|---|---|
| **PostgreSQL** (auth) | The opt-in choice (`dataset_consent`, deleted with the account) | `services/auth/migrations`, applied by auth at startup |
| **Redis** | The picture of each user's latest roll, held 10 minutes for "flag as wrong roll"; rate counters | `packages/web/src/capture.rs` |
| **SurrealDB** | Saved rolls, their pictures, and the deletion log | `database/schema/`, applied by the website at startup |

## How the tables get there
The website applies `database/schema/` itself every time it starts, the same way auth
applies its PostgreSQL migrations (`packages/web/src/dataset.rs`, using the
[surrealkit](https://crates.io/crates/surrealkit) library). So a table change ships like
any other website change: merge to `main`, CI builds, Argo deploys.

- The files are compiled into the website. Only files whose text changed are applied
  again (a whole file at a time).
- Replicas take turns: a lock in Redis (`arcane:schema_lock`) lets one apply the schema
  while the others wait. Two surrealkit runs at once break its bookkeeping. The lock
  is renewed while held and expires 30 s after its holder stops.
- Until the schema is applied, the website runs normally with sharing and flagging off;
  "Delete all pictures I've shared" keeps working. If SurrealDB is unreachable or the
  login is wrong, it retries in the background (up to once a minute) and logs
  `applying the roll-sharing schema failed` with the reason.
- surrealkit prints its own progress ("applied …", "schema already in sync", "detected
  N stale managed entities") as plain text on the website's stdout, next to the JSON
  log lines.

## Changing the tables
- **Adding** a table, field or index: add it to a file here and merge.
- **Changing** a field: edit it and merge. Existing rows must still fit the new
  definition, or writes touching them fail.
- **Removing** is never done by a deploy: surrealkit runs with pruning off, so nothing
  is dropped along with the users' pictures. A definition deleted from these files
  stays in the database ("stale") and **is still enforced**. So, to stop using a field:
  1. make it optional (`TYPE option<…>`) and merge;
  2. once nothing writes it, delete the line and merge. It stays in the database,
     unused.

  Actually dropping stale tables or fields is a separate, deliberate job. While stale
  definitions exist, surrealkit refuses to run if a surrealkit rollout is active
  (planned, running or failed) in this database, and the website's schema step then
  fails until it isn't.
- Never run the surrealkit CLI's `sync` against the production database. It prunes by
  default (dropping stale tables and fields with their data), it doesn't take the
  website's lock, and it tracks the files under different names than the website does.

## What the infrastructure provides
The website only uses SurrealDB; everything below comes from the infrastructure repo.

| What | Value the website expects |
|---|---|
| Namespace / database | `milesstorm` / `arcane` |
| The website's login | database-level user `dice` in `milesstorm`/`arcane`, role `EDITOR` (the website creates its own tables) |
| The training tool's login | database-level user `dice_reader` in `milesstorm`/`arcane`, role `VIEWER` (`services/ai_pipeline/tools/pull_dataset.py`) |
| The website's password | key `SURREAL_PASS` in the Kubernetes secret `website-secrets`, namespace `frontend` (username, address and database are set in `crds/frontend/deployment.yaml`) |
| Network | pods in namespace `frontend` reach `surrealdb.surreal.svc.cluster.local:8000` |

EDITOR is the smallest SurrealDB role that can create tables. It can't create logins
or touch anything outside `arcane`.

## Tests (development)
`database/tests/` checks what each kind of login may do with these tables: the website's
can save and delete but not create logins, the training tool's can read but not write,
and bad values are refused. surrealkit runs them in a throwaway namespace and removes it
afterwards; the suite makes its own stand-in logins. Needs the surrealkit CLI
(`cargo install surrealkit --version 0.7.0 --locked`) and any SurrealDB 3 you can reach
as root, for example a local one:
```
docker run -d --rm --name surreal-dev -p 127.0.0.1:8000:8000 surrealdb/surrealdb:v3.2.4 start --user root --pass root memory
cd services/frontend/surreal
env SURREALDB_HOST=http://127.0.0.1:8000 SURREALDB_USER=root SURREALDB_PASSWORD=root surrealkit test --no-seed
```
Expected: `cases: 6 total, 6 passed, 0 failed`.

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
  like the other in-cluster services.
- Roll pictures are held in Redis for 10 minutes so "flag as wrong roll" can save
  them. Only each user's latest roll is held. Nothing reaches SurrealDB unless the
  user opted in on their profile, or flagged that roll.
