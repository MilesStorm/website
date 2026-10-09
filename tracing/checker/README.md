# Trace contract checker

`trace_check.py` checks traces against the tracing contract (rules I1–I8, TRACING.md and the tracing
plan §1). Python 3.11+, stdlib only.

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
- `--env production|staging`: an environment filter, read from the resource `deployment.environment.name` / `deployment.environment`, a `*-staging` namespace, or a `staging.` host. Unlabelled telemetry counts as production.
- `--vlogs URL`: VictoriaLogs, for I8. It uses the same `--since/--until` window.
- `--leaf-spans FILE`: the leaf registry. Default: `tracing/leaf_spans.toml`; a missing file means an empty registry.
- `--ingress REGEX`: ingress services whose parentless SERVER spans count as "browser part missing" (default `istio-ingress`).
- `--json`: JSON instead of the report.
- `--push URL`: pushes counts to VictoriaMetrics (`/api/v1/import/prometheus` is appended to a bare host). Series:
  - `trace_check_violations_total{rule,service,env}`
  - `trace_check_traces_total{kind,env}`, where kind is `browser`, `server`, `background` or `browser_part_missing`
  - `trace_check_violating_traces_total{rule,kind,env}`, the numerator for percentage alerts
  - `trace_check_last_run_timestamp_seconds{env}`
- `--phase P0..P5`: exits 1 if a gate of that phase or an earlier one fails. Gates follow the plan §1 table. `--tolerance PCT` sets the share of in-scope traces a gate may fail (default 0; P1's done-when is `--tolerance 1`).

## Leaf registry (`tracing/leaf_spans.toml`)

```toml
[[leaf]]
service = "auth"            # fnmatch glob, default "*"
name = "password.verify"    # fnmatch glob
reason = "argon2 hash, CPU only"
```

## What each rule checks

| Rule | Check |
|---|---|
| I1 | Exactly one root. A root is a span whose parent isn't in the trace. |
| I2 | Every parent is in the trace. A parentless SERVER span at the ingress is exempt and counted as `browser_part_missing`. |
| I3 | The child lies inside its parent. Tolerance: 1 ms in-process (same `service.name` and the same pod/host, where both sides have one); `browser.clock_offset_error_ms` between browser and server (1 ms if the attribute is missing); 1 ms between nodes. `trace.relation=follows` children are exempt. |
| I4 | INTERNAL/SERVER parents: own time ≤ min(2 ms + 2 %, 20 ms). `follows` children don't count as cover. Browser click/page-load spans are exempt. A childless INTERNAL span must be in the leaf registry. |
| I5 | Reported, never gating: hop overhead (CLIENT minus its SERVER child) as p50/p99/n per edge, plus CLIENT spans that have neither a SERVER child nor `peer.service`. |
| I6 | `service.version` is a git SHA on every resource. HTTP spans: method, status, and route on non-Envoy SERVER spans. `peer.service` on non-browser CLIENT spans, and never on browser ones. `db.system` on db spans. Error status per semconv: a SERVER 5xx is ERROR and a SERVER 4xx isn't; a CLIENT ≥ 400 is ERROR, except an expected 401/404 from auth. |
| I7 | Skipped: it's a metrics rule (VictoriaMetrics alerts). |
| I8 | VictoriaLogs: app log lines with `span.name` but no `trace_id`. Level-less and `istio-proxy` lines are excluded. A missing `span_id` is reported but doesn't gate. Only the stdout JSON path is checked: OTLP logs aren't in VictoriaLogs. |

## Tests

```sh
python3 -m unittest tracing/checker/test_trace_check.py
```
