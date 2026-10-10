# Trace contract checker

`trace_check.py` checks traces against the tracing contract (rules I1–I8, described below). Python 3.11+, stdlib only.

## Inputs

```sh
# live Tempo, a time window (optionally narrowed by a TraceQL query)
python3 trace_check.py --tempo http://localhost:3200 --since 24h [--until 2m] [--query '{...}']
# exactly these trace IDs (staging journeys)
python3 trace_check.py --tempo http://localhost:3200 --trace-ids ids.txt --env staging --phase P2
# saved Tempo trace JSONs (v1 `batches` or v2 `trace.resourceSpans`)
python3 trace_check.py --dir corpus/
```

Other flags:
- `--rules I1,I3`: checks to run (default all).
- `--env production|staging`: an environment filter. A trace is staging if any server-side resource says so (`deployment.environment.name`), or it has a `*-staging` namespace or a `staging.` host; the browser's own label doesn't decide. Unlabelled telemetry counts as production.
- `--vlogs URL`: VictoriaLogs, for I8. It uses the same `--since/--until` window.
- `--leaf-spans FILE`: the leaf registry. Default: `tracing/leaf_spans.toml`. Without the file, I4 doesn't run unless `--allow-empty-registry` is given.
- `--ingress REGEX`: ingress services whose parentless SERVER spans count as "browser part missing" (default `istio-ingress`).
- `--json`: JSON instead of the report.
- `--push URL`: pushes counts to VictoriaMetrics (`/api/v1/import/prometheus` is appended to a bare host). Series:
  - `trace_check_violations{rule,service,env}`
  - `trace_check_traces{kind,env}`, where kind is `browser`, `server`, `background` or `browser_part_missing`
  - `trace_check_violating_traces{rule,kind,env}`, the numerator for percentage alerts
  - `trace_check_last_run_timestamp_seconds{env}`

  They are gauges: each run's counts, stamped with the run's time, zeros included. Query them with `last_over_time(...[2h])`.
- `--phase P0..P5`: exits 1 if a gate of that phase or an earlier one fails. `--tolerance PCT` sets the share of in-scope traces a gate may fail (default 0).
- `--min-traces N`: a gate with fewer in-scope traces fails (default 1), so no data is never a pass. Use 0 for unattended monitoring, where an hour without a camera session is normal.
- With `--trace-ids`, a listed trace that isn't in Tempo, or isn't in `--env`, fails the run.

## Leaf registry (`tracing/leaf_spans.toml`)

```toml
[[leaf]]                    # a span allowed to have no children
service = "auth"            # fnmatch glob
name = "password.verify"    # fnmatch glob, not just "*"
reason = "argon2 hash, CPU only"

[[follows]]                 # detached work allowed to outlast its parent
service = "auth"
name = "email.verify_send"
reason = "sent after the response"

[[cpu]]                     # a parent whose own time is CPU work by design
service = "frontend"
name = "ssr.render"
reason = "the page render"
```

Every entry needs a reason.

## What each rule checks

| Rule | Check |
|---|---|
| I1 | Exactly one root. A root is a span whose parent isn't in the trace. |
| I2 | Every parent is in the trace. A parentless SERVER span at the ingress is exempt and counted as `browser_part_missing`. |
| I3 | The child lies inside its parent. Tolerance: 1 ms in-process (same `service.name` and the same pod/host, where both sides have one); `browser.clock_offset_error_ms` between browser and server (1 ms if the attribute is missing); 1 ms between nodes. A `trace.relation=follows` child may end after its parent, but only the browser's `page load` under a server span and the `[[follows]]` entries; it still can't start before the parent. The same holds for a messaging hop: a CONSUMER span, or a PRODUCER passing on another service's message, may run after its parent ended. A clock-error bound that is negative, not a number or over 1 s counts as missing. |
| I4 | INTERNAL/SERVER parents: own time ≤ min(2 ms + 2 %, 20 ms). `follows` children don't count as cover. Browser click/page-load spans are exempt. Browser click, submit, navigate and page-load spans are exempt (`ui.event`). A childless INTERNAL span must be in the leaf registry. A childless SERVER span of our own services is all own time: over the limit it fails, unless registered. A parent listed under `[[cpu]]` is exempt from the own-time limit (its own time is CPU work by design; use the profiler for it). |
| I5 | A CLIENT span to something that is traced itself (the site's `/bff/` and `/api/` from the browser; frontend, auth, ai-pipeline, the waypoints and the Gateway from the servers) must have a SERVER child: without one, the trace broke there. Any other CLIENT span needs `peer.service`. Hop overhead (CLIENT minus its SERVER child) is reported as p50/p99/n per edge and doesn't gate yet. |
| I6 | `service.version` is a git SHA on every resource. HTTP spans: method, status, and route on non-Envoy SERVER spans. `peer.service` on non-browser CLIENT spans, and never on browser ones. `db.system` on db spans and on calls to Postgres, Redis and SurrealDB. No route is required on a 404 (none matched). Error status per semconv: a SERVER 5xx is ERROR and a SERVER 4xx isn't; a CLIENT ≥ 400 is ERROR, except a 401/404 from the auth service's login and token endpoints, where that is an answer. |
| I7 | Skipped: it's a metrics rule (VictoriaMetrics alerts). |
| I8 | VictoriaLogs: app log lines with `span.name` but no `trace_id`, at the top level or on a span in `spans` (where the stdout JSON keeps the request span's). Level-less and `istio-proxy` lines are excluded. A missing `span_id` is reported but doesn't gate. Only the stdout JSON path is checked: OTLP logs aren't in VictoriaLogs. |

## Tests

```sh
python3 -m unittest tracing/checker/test_trace_check.py
```

## Gates by phase

| Phase | Must pass |
|---|---|
| P1 | I3 on browser traces; I5 on the browser's requests |
| P2 | I1, I2, I5, I6 on all traces; I4 on server spans outside page and asset requests |
| P3 | I3 on stream traces |
| P4 | I4 on page and asset requests |
| P5 | I1–I6 on all traces; I8 |

Phases are cumulative.

## Hourly run

`tracing/kustomization.yaml` and `cronjob.yaml` run the checker every hour in the cluster (Argo app
`trace-checker` in the homelab repo), once for production and once for staging, and push the
counts. Violations don't fail the job; they show on the "Trace health" dashboard.
