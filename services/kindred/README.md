# kindred: books

The world builder, the world's format and the taste model. The kindred service (crate
`kindred`) joins this workspace in M3.

| Crate | What |
|---|---|
| `crates/world` | The world's on-disk format: types, the writer, the reader (memory-mapped columns, the search index). |
| `crates/taste` | The per-user taste model (pure maths, no I/O): fit, scores, fit bands, score breakdowns, questions, picks, taste profile. |
| `crates/world-build` | Binary `kindred-world`: the monthly build, the book vectors, the benchmark, the fixture world, publishing and rollback. |

## The world

The read-only data every user shares: which books exist (Open Library works), how to find them
(a tantivy index over titles, other edition titles, authors and series names) and per-work
numbers (popularity, smoothed rating, year, series and position). No user data. Built monthly
by the CronJob `kindred-world` (namespace `kindred`, `crds/kindred-world`), stored in the Garage
bucket `kindred-world`:

```
current.json                  {"version": "...", "manifest_sha256": "..."}: what is served
worlds/<YYYYMMDD-sha7>/       one version: manifest.json, report.json, columns, search/
```

A version is never changed after upload; `current.json` moves only after the new version
passed every check, and the bucket keeps the new version and the one it replaced.

### The build

`kindred-world build --scratch /scratch` runs, in order: download (Open Library's monthly dumps
again when they changed, the Goodreads files once), the SQL stages in
`crates/world-build/sql/` (ingest, identity, catalogue, series, popularity, interactions,
aspects) against one DuckDB file, the model stages (factors, embed, text-maps, vectors, prior,
bench; their files in the scratch volume's `model/`), search-index and pack, verify, publish.

The model stages make each work's book vector (`[1 | collaborative | aspects | meta]`): matrix factorization of the
Goodreads readers' likes, aspect numbers from shelf tags and Open Library subjects matched by
`aspects.toml`, text embeddings (`BAAI/bge-small-en-v1.5` through ONNX Runtime, on eta's GPU;
cached in `embed-cache/` by text), and ridge maps from the embedding to the other blocks for
books without reading data. Stage bench measures the model on the benchmark's fixed gate users;
verify refuses a world whose NDCG@10 at 20 ratings falls below `bench.min_share` of the accepted
value. Configuration is `world.toml`. A stage whose
inputs, SQL and settings are unchanged since its last success is skipped, so a failed run
resumes where it stopped.

Verify reads the new world back through crate `world` and refuses to publish when:
- less than `identity.min_mapped_share` of Goodreads ratings map to Open Library works,
- Open Library works, editions or searchable works moved by more than `verify.max_change`
  since the version being served,
- any case of `crates/world-build/search-acceptance.toml` fails, or search is slower than
  `search.p95_ms` at the 95th percentile.

The build report (counts and coverage, no titles) is in `report.json`, the manifest and one
log line.

### The benchmark

`kindred-world bench --scratch /scratch` reads the scratch volume of a finished build and prints
the benchmark's report on the test users (tables per metric and method, the cold-book test, the
gates) as Markdown: aggregates only. `--users tuning` runs the hyperparameter search instead and
prints the `[model]` section for `world.toml`. Run it in the cluster (the data may not leave
it), as a one-off Job from the CronJob with these arguments.

### Rollback

The one operational action: point `current.json` at an older version still in the bucket.
`versions` lists them; `publish --version <version>` rolls back. Run either as a one-off pod
with the build's key:

```sh
kubectl run -n kindred kindred-world-cli --rm -it --restart=Never --image=ghcr.io/milesstorm/kindred-world:production \
  --overrides='{"spec":{"containers":[{"name":"kindred-world-cli","image":"ghcr.io/milesstorm/kindred-world:production","args":["publish","--version","<version>"],"envFrom":[{"secretRef":{"name":"kindred-world-secrets"}}]}]}}'
```

The service picks the change up within its poll interval. The next successful build moves
`current.json` forward again.

### Starting a build by hand

```sh
kubectl create job -n kindred --from=cronjob/kindred-world kindred-world-manual-1
```

## Data rules

- The Goodreads dataset may not be redistributed. Nothing derived from it goes into git, an
  image, a PR description (aggregate numbers only) or anywhere outside the cluster: only the
  build's scratch volume and the private bucket.
- Logs, spans and metrics carry no book titles and no search text.

## Fixture world

`fixtures/` holds real Open Library records for the works in `fixtures/works.toml` (open data)
and nothing from Goodreads: `kindred-world fixture` generates synthetic Goodreads files from a
fixed seed, including readers drawn from a planted low-rank taste model that the benchmark must
recover. The text embedding is a stand-in (hashed words), so no model and no GPU are needed.
Every test builds it; nothing is downloaded.

`fixtures/embedding-reference.json` holds the embedding model card's own embeddings of a few
texts; `kindred-world embed-check` compares the Rust embedder with them, and the image runs it
when it is built.

```sh
cargo run -p world-build -- fixture --out /tmp/world
```

After changing `fixtures/works.toml`, download the records again (one request a second):

```sh
cargo run -p world-build -- fixture-refresh
```

## Searching a world by hand

`kindred-world search --world <dir> "<query>"` prints the hits, titles included, to the
terminal. Use it on the fixture world or a local copy only, never in the cluster: there its
output is the pod's log, and logs must not hold titles or search text.

## Checks

```sh
cargo clippy --all-targets -- -D warnings
cargo test
```

Raw HTTP clients, task spawns and the DuckDB and S3 constructors are banned outside
`crates/world-build/src/trace.rs` (`clippy.toml`); exceptions are listed in
`allowed_raw_io.toml` (TRACING.md).
