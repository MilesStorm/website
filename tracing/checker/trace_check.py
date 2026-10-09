#!/usr/bin/env python3
"""Trace contract checker: rules I1-I8 of the tracing plan (TRACING.md, plan section 1).

Reads traces from Tempo, a list of trace IDs, or a directory of saved Tempo trace JSONs,
checks every rule, prints a report (or JSON), optionally pushes counts to VictoriaMetrics,
and exits non-zero when a gating rule of --phase fails. Python 3.11+, stdlib only.
"""
import argparse
import base64
import collections
import concurrent.futures
import datetime
import fnmatch
import json
import os
import re
import sys
import time
import tomllib
import urllib.parse
import urllib.request

BROWSER = "milesstorm-web"
MS = 1_000_000  # ns per ms
RULES = ("I1", "I2", "I3", "I4", "I5", "I6", "I7", "I8")
TRACE_KINDS = ("browser", "server", "background", "browser_part_missing")
KIND_NAMES = {0: "UNSPECIFIED", 1: "INTERNAL", 2: "SERVER", 3: "CLIENT", 4: "PRODUCER", 5: "CONSUMER"}
API_PREFIXES = ("/bff/", "/api/", "/rpc", "/internal/", "/ws/", "/sse/")
BROWSER_UI_PREFIXES = ("page load", "click", "submit", "navigate")
AUTH_EXPECTED = {401, 404}
SHA_RE = re.compile(r"^[0-9a-f]{7,40}$")


# ---------------------------------------------------------------- parsing

def _hex_id(v):
    """Tempo's v1/v2 JSON carries IDs base64-encoded (always '='-padded at these lengths); search
    results use hex with leading zeros dropped."""
    if not v:
        return ""
    if re.fullmatch(r"[0-9a-fA-F]{1,32}", v):
        return v.lower().zfill(16 if len(v) <= 16 else 32)
    return base64.b64decode(v).hex()


def _value(v):
    if "stringValue" in v:
        return v["stringValue"]
    if "intValue" in v:
        return int(v["intValue"])
    if "doubleValue" in v:
        return float(v["doubleValue"])
    if "boolValue" in v:
        return bool(v["boolValue"])
    if "arrayValue" in v:
        return [_value(x) for x in v["arrayValue"].get("values", [])]
    return None


def _attrs(lst):
    return {a["key"]: _value(a.get("value", {})) for a in lst or []}


def _kind(k):
    if isinstance(k, int):
        return KIND_NAMES.get(k, "UNSPECIFIED")
    return str(k).removeprefix("SPAN_KIND_")


def _status(s):
    code = (s.get("status") or {}).get("code", "")
    if code in (2, "STATUS_CODE_ERROR"):
        return "ERROR"
    if code in (1, "STATUS_CODE_OK"):
        return "OK"
    return "UNSET"


class Span:
    __slots__ = ("id", "parent", "trace_id", "svc", "ident", "kind", "name", "st", "en", "attrs", "res", "status")

    @property
    def dur(self):
        return self.en - self.st

    def key(self):
        n = re.sub(r"\d{6,}", "", self.name)
        n = re.sub(r"/[0-9a-f-]{16,}", "/{id}", n)
        return f"{self.svc}: {n} [{self.kind}]"


def parse_trace(doc):
    """Spans from a Tempo trace document: v1 {"batches"}, v2 {"trace": {"resourceSpans"}} or OTLP."""
    if "trace" in doc:
        doc = doc["trace"]
    spans = []
    for rs in doc.get("batches") or doc.get("resourceSpans") or []:
        res = _attrs((rs.get("resource") or {}).get("attributes"))
        svc = res.get("service.name", "?")
        res_ident = next((res[k] for k in ("k8s.pod.name", "k8s.pod.uid", "host.name", "service.instance.id")
                          if res.get(k)), None)
        for ss in rs.get("scopeSpans") or rs.get("instrumentationLibrarySpans") or []:
            for s in ss.get("spans", []):
                sp = Span()
                sp.id = _hex_id(s["spanId"])
                sp.parent = _hex_id(s.get("parentSpanId", ""))
                sp.trace_id = _hex_id(s.get("traceId", ""))
                sp.svc = svc
                sp.kind = _kind(s.get("kind", 0))
                sp.name = s.get("name", "")
                sp.st = int(s["startTimeUnixNano"])
                sp.en = int(s["endTimeUnixNano"])
                sp.attrs = _attrs(s.get("attributes"))
                sp.res = res
                sp.status = _status(s)
                # Envoy puts its pod identity on the span (node_id), not on the resource.
                sp.ident = res_ident or sp.attrs.get("node_id")
                spans.append(sp)
    return spans


# ---------------------------------------------------------------- trace model

def in_process(p, c):
    """Same service.name and the same pod/host. A side without an identity doesn't conflict."""
    return p.svc == c.svc and (p.ident is None or c.ident is None or p.ident == c.ident)


def is_envoy(s):
    return s.attrs.get("component") == "proxy" or any(k.startswith("istio.") for k in s.attrs)


def is_browser_ui(s):
    return s.svc == BROWSER and s.kind == "INTERNAL" and (
        "ui.event" in s.attrs or s.name.startswith(BROWSER_UI_PREFIXES))


def follows(s):
    return s.attrs.get("trace.relation") == "follows"


def http_status(s):
    v = s.attrs.get("http.response.status_code", s.attrs.get("http.status_code"))
    try:
        return int(v)
    except (TypeError, ValueError):
        return None


def http_path(s):
    for k in ("url.path", "http.target", "http.route"):
        if s.attrs.get(k):
            return str(s.attrs[k])
    url = s.attrs.get("http.url") or s.attrs.get("url.full")
    return urllib.parse.urlsplit(str(url)).path if url else ""


def _env_of(spans):
    for s in spans:
        for k in ("deployment.environment.name", "deployment.environment"):
            if s.res.get(k):
                return str(s.res[k])
    for s in spans:
        if str(s.res.get("k8s.namespace.name", "")).endswith("-staging"):
            return "staging"
        for k in ("server.address", "http.url", "url.full"):
            if "staging." in str(s.attrs.get(k, "")):
                return "staging"
    return "production"  # staging labels itself (plan phase S); unlabelled telemetry is production


class Trace:
    def __init__(self, spans, ingress_re):
        self.spans = spans
        self.id = next((s.trace_id for s in spans if s.trace_id), "?")
        self.byid = {s.id: s for s in spans}
        self.kids = collections.defaultdict(list)
        for s in spans:
            self.kids[s.parent].append(s)
        self.roots = sorted((s for s in spans if s.parent not in self.byid), key=lambda s: s.st)
        self.orphans = [s for s in spans if s.parent and s.parent not in self.byid]
        # I2 exemption: a SERVER span at the ingress whose parent came from a browser's traceparent.
        self.browser_orphans = [s for s in self.orphans if s.kind == "SERVER" and ingress_re.search(s.svc)]
        self.env = _env_of(spans)
        self.stream = any(s.kind in ("PRODUCER", "CONSUMER") or any(k.startswith("messaging.") for k in s.attrs)
                          for s in spans)
        if any(s.svc == BROWSER for s in spans):
            self.kind = "browser"
        elif self.browser_orphans:
            self.kind = "browser_part_missing"
        elif self.roots and self.roots[0].kind == "SERVER":
            self.kind = "server"
        else:
            self.kind = "background"

    def parent(self, s):
        return self.byid.get(s.parent)

    def page_or_asset(self, s):
        """True when the nearest SERVER span at or above s is a GET/HEAD of a page or asset."""
        cur = s
        while cur is not None:
            if cur.kind == "SERVER" and cur.svc != BROWSER:
                method = cur.attrs.get("http.request.method") or cur.attrs.get("http.method")
                return method in ("GET", "HEAD") and not http_path(cur).startswith(API_PREFIXES)
            cur = self.parent(cur)
        return False


# ---------------------------------------------------------------- rules

class Violation:
    __slots__ = ("rule", "trace", "service", "detail", "ms", "key", "span")

    def __init__(self, rule, trace, span, detail, ms=None, key=None):
        self.rule, self.trace, self.span = rule, trace, span
        self.service = span.svc if span is not None else "?"
        self.detail, self.ms = detail, ms
        self.key = key or (span.key() if span is not None else detail)

    def as_dict(self):
        return {"rule": self.rule, "trace_id": self.trace.id, "trace_kind": self.trace.kind,
                "env": self.trace.env, "service": self.service, "detail": self.detail,
                "ms": None if self.ms is None else round(self.ms, 3)}


def _pair(p, c):
    return f"{p.key()} > {c.key()}"


def check_i1(t):
    if len(t.roots) != 1:
        names = ", ".join(r.key() for r in t.roots[:3])
        span = t.roots[0] if t.roots else None
        return [Violation("I1", t, span, f"{len(t.roots)} roots: {names}", key=f"{len(t.roots)} roots")]
    return []


def check_i2(t):
    exempt = set(map(id, t.browser_orphans))
    return [Violation("I2", t, s, f"{s.key()} parent {s.parent} not in trace")
            for s in t.orphans if id(s) not in exempt]


def _browser_clock_bound_ns(p, c):
    b = p if p.svc == BROWSER else c
    v = b.attrs.get("browser.clock_offset_error_ms", b.res.get("browser.clock_offset_error_ms"))
    try:
        return float(v) * MS, True
    except (TypeError, ValueError):
        return 1 * MS, False


def check_i3(t):
    out = []
    for c in t.spans:
        p = t.parent(c)
        if p is None or follows(c):
            continue
        excess = max(p.st - c.st, c.en - p.en)
        if excess <= 0:
            continue
        where = "in-process"
        tol, bounded = 1 * MS, True
        if not in_process(p, c):
            if BROWSER in (p.svc, c.svc):
                where = "browser-server"
                tol, bounded = _browser_clock_bound_ns(p, c)
            else:
                where = "cross-node"
        if excess > tol:
            side = "starts before" if p.st - c.st >= c.en - p.en else "ends after"
            note = "" if bounded else " (no browser.clock_offset_error_ms)"
            out.append(Violation("I3", t, c, f"{_pair(p, c)}: child {side} parent by {excess / MS:.2f} ms, "
                                 f"{where} tolerance {tol / MS:.2f} ms{note}", excess / MS, key=_pair(p, c)))
    return out


def _union(intervals):
    total, cur = 0, None
    for a, b in sorted(intervals):
        if cur is None or a > cur[1]:
            if cur:
                total += cur[1] - cur[0]
            cur = [a, b]
        else:
            cur[1] = max(cur[1], b)
    if cur:
        total += cur[1] - cur[0]
    return total


def own_time_limit_ns(dur):
    return min(2 * MS + 0.02 * dur, 20 * MS)


def check_i4(t, leaves):
    out = []
    for s in t.spans:
        if s.kind not in ("INTERNAL", "SERVER") or is_browser_ui(s):
            continue
        kids = [k for k in t.kids.get(s.id, ()) if not follows(k)]
        if not t.kids.get(s.id):
            if s.kind == "INTERNAL" and not leaves.allows(s):
                out.append(Violation("I4", t, s, f"{s.key()}: childless INTERNAL span not in leaf_spans.toml "
                                     f"({s.dur / MS:.2f} ms)", s.dur / MS, key="leaf: " + s.key()))
            continue
        covered = _union([(max(k.st, s.st), min(k.en, s.en)) for k in kids if min(k.en, s.en) > max(k.st, s.st)])
        own = s.dur - covered
        limit = own_time_limit_ns(s.dur)
        if own > limit:
            out.append(Violation("I4", t, s, f"{s.key()}: own time {own / MS:.2f} ms of {s.dur / MS:.2f} ms "
                                 f"(limit {limit / MS:.2f} ms)", own / MS, key="own: " + s.key()))
    return out


def check_i5(t, hops):
    """Hop overhead per edge into `hops`; structural violations (reported, never gating) returned."""
    out = []
    for c in t.spans:
        if c.kind != "CLIENT":
            continue
        servers = [k for k in t.kids.get(c.id, ()) if k.kind == "SERVER" and not in_process(c, k)]
        for k in servers:
            hops[f"{c.svc} -> {k.svc}"].append((c.dur - k.dur) / MS)
        if not servers and c.svc != BROWSER and not c.attrs.get("peer.service"):
            out.append(Violation("I5", t, c, f"{c.key()}: CLIENT with no SERVER child and no peer.service"))
    return out


def _is_auth_target(s):
    fields = (s.attrs.get("peer.service"), s.attrs.get("upstream_cluster"), s.attrs.get("http.url"),
              s.attrs.get("server.address"), s.svc)
    return any("auth" in str(f) for f in fields if f)


def check_i6(t):
    out = []
    seen_res = set()
    for s in t.spans:
        def bad(what):
            out.append(Violation("I6", t, s, f"{s.key()}: {what}", key=f"{s.key()}: {what}"))

        if s.svc not in seen_res:
            seen_res.add(s.svc)
            ver = s.res.get("service.version")
            what = ("resource has no service.version" if not ver else
                    None if SHA_RE.match(str(ver)) else f"service.version {ver!r} is not a git SHA")
            if what:
                out.append(Violation("I6", t, s, f"{s.svc}: {what}", key=f"{s.svc}: {what}"))

        method = s.attrs.get("http.request.method") or s.attrs.get("http.method")
        code = http_status(s)
        failed = s.status == "ERROR" or "error.type" in s.attrs or s.attrs.get("error") in (True, "true")
        if method and s.kind in ("SERVER", "CLIENT"):
            if s.kind == "SERVER" and not is_envoy(s) and not s.attrs.get("http.route"):
                bad("no http.route")
            if not code and not failed:
                bad("no HTTP status code")
        elif s.kind == "SERVER" and ("url.path" in s.attrs or "http.route" in s.attrs):
            bad("no HTTP method")

        if s.kind == "CLIENT":
            if s.svc == BROWSER and s.attrs.get("peer.service"):
                bad("browser span carries peer.service")
            elif s.svc != BROWSER and not s.attrs.get("peer.service"):
                bad("no peer.service")

        if any(k.startswith("db.") for k in s.attrs) and not (s.attrs.get("db.system") or s.attrs.get("db.system.name")):
            bad("db span without db.system")

        if code:
            if s.kind == "SERVER":
                if code >= 500 and s.status != "ERROR":
                    bad(f"HTTP {code} without ERROR status")
                elif 400 <= code < 500 and s.status == "ERROR":
                    bad(f"HTTP {code} on a SERVER span marked ERROR")
            elif s.kind == "CLIENT" and code >= 400 and s.status != "ERROR":
                if not (code in AUTH_EXPECTED and _is_auth_target(s)):
                    bad(f"HTTP {code} without ERROR status")
        elif failed and s.status != "ERROR":
            bad("error recorded but status is not ERROR")
    return out


class LeafRegistry:
    """tracing/leaf_spans.toml: [[leaf]] tables with service, name (fnmatch globs) and reason."""

    def __init__(self, entries=()):
        self.entries = [(e.get("service", "*"), e["name"]) for e in entries]

    @classmethod
    def load(cls, path):
        if not path or not os.path.exists(path):
            return cls()
        with open(path, "rb") as f:
            return cls(tomllib.load(f).get("leaf", []))

    def allows(self, s):
        return any(fnmatch.fnmatchcase(s.svc, svc) and fnmatch.fnmatchcase(s.name, name)
                   for svc, name in self.entries)


def check_trace(t, rules, leaves, hops):
    out = []
    if "I1" in rules:
        out += check_i1(t)
    if "I2" in rules:
        out += check_i2(t)
    if "I3" in rules:
        out += check_i3(t)
    if "I4" in rules:
        out += check_i4(t, leaves)
    if "I5" in rules:
        out += check_i5(t, hops)
    if "I6" in rules:
        out += check_i6(t)
    return out


# ---------------------------------------------------------------- I8 over VictoriaLogs

def _vlogs_stats(base, query):
    url = base.rstrip("/") + "/select/logsql/query"
    req = urllib.request.Request(url, data=urllib.parse.urlencode({"query": query}).encode())
    with urllib.request.urlopen(req, timeout=60) as r:
        return [json.loads(line) for line in r.read().decode().splitlines() if line.strip()]


def check_i8(vlogs, since, until, env, fetch=_vlogs_stats):
    """App log lines inside a span (span.name set) without trace_id. Level-less and istio-proxy lines
    aren't app logs. Returns (lines with a span name, [(namespace, container, span.name, n)], no span_id n)."""
    window = f"_time:[{_iso(since)}, {_iso(until)})"
    base = f"{window} span.name:* level:* -container:istio-proxy"
    if env == "staging":
        base += ' namespace:~"-staging$"'
    elif env == "production":
        base += ' -namespace:~"-staging$"'
    total = sum(int(r["n"]) for r in fetch(vlogs, base + " | stats count() n"))
    missing = [(r.get("namespace", ""), r.get("container", ""), r.get("span.name", ""), int(r["n"]))
               for r in fetch(vlogs, base + " -trace_id:* | stats by (namespace, container, span.name) count() n")]
    no_span_id = sum(int(r["n"]) for r in fetch(vlogs, base + " -span_id:* | stats count() n"))
    return total, sorted(missing, key=lambda m: -m[3]), no_span_id


# ---------------------------------------------------------------- inputs

def _iso(ts):
    return datetime.datetime.fromtimestamp(ts, datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def parse_time(v, now=None):
    """'now', a duration ago ('24h', '90m', '7d', '30s'), epoch seconds, or ISO 8601."""
    now = time.time() if now is None else now
    if v == "now":
        return now
    m = re.fullmatch(r"(\d+(?:\.\d+)?)([smhd])", v)
    if m:
        return now - float(m.group(1)) * {"s": 1, "m": 60, "h": 3600, "d": 86400}[m.group(2)]
    if re.fullmatch(r"\d{9,}(\.\d+)?", v):
        return float(v)
    return datetime.datetime.fromisoformat(v.replace("Z", "+00:00")).timestamp()


def _get_json(url):
    with urllib.request.urlopen(url, timeout=60) as r:
        return json.loads(r.read())


def tempo_search(tempo, query, start, end, limit=500):
    """Trace IDs matching `query` in [start, end); windows that hit the limit are split."""
    ids, stack = [], [(int(start), int(end))]
    while stack:
        a, b = stack.pop()
        if b <= a:
            continue
        qs = urllib.parse.urlencode({"q": query, "start": a, "end": b, "limit": limit, "spss": 1})
        found = [t["traceID"] for t in _get_json(f"{tempo.rstrip('/')}/api/search?{qs}").get("traces", [])]
        if len(found) >= limit and b - a > 60:
            mid = (a + b) // 2
            stack += [(a, mid), (mid, b)]
        else:
            ids += found
    return list(dict.fromkeys(_hex_id(i) for i in ids))


def tempo_fetch(tempo, ids, workers=8):
    def one(tid):
        try:
            return parse_trace(_get_json(f"{tempo.rstrip('/')}/api/traces/{tid}"))
        except Exception as e:  # a trace that expired between search and fetch
            print(f"warning: trace {tid}: {e}", file=sys.stderr)
            return []

    with concurrent.futures.ThreadPoolExecutor(workers) as ex:
        yield from ex.map(one, ids)


def dir_traces(path):
    for name in sorted(os.listdir(path)):
        if name.endswith(".json"):
            with open(os.path.join(path, name)) as f:
                yield parse_trace(json.load(f))


# ---------------------------------------------------------------- gates

def _all(t, v=None):
    return True


# (rule, scope description, trace in scope?, violation counts?) per phase; phases are cumulative.
GATES = {
    "P1": [("I3", "browser traces", lambda t: t.kind == "browser", _all)],
    "P2": [("I1", "all traces", _all, _all),
           ("I2", "all traces", _all, _all),
           ("I6", "all traces", _all, _all),
           ("I4", "server spans outside page/asset requests", _all,
            lambda t, v: v.service != BROWSER and not t.page_or_asset(v.span))],
    "P3": [("I3", "stream traces", lambda t: t.stream, _all)],
    "P4": [("I4", "page and asset requests", _all, lambda t, v: t.page_or_asset(v.span))],
    "P5": [(r, "all traces", _all, _all) for r in ("I1", "I2", "I3", "I4", "I6")] + [("I8", "app log lines", None, None)],
}


def evaluate_gates(phase, traces, violations, i8, tolerance):
    if not phase or phase == "P0":
        return []
    upto = [p for p in GATES if p <= phase]
    by_trace = collections.defaultdict(list)
    for v in violations:
        by_trace[id(v.trace)].append(v)
    results = []
    for p in upto:
        for rule, scope, in_scope, counts in GATES[p]:
            if rule == "I8":
                if i8 is None:
                    results.append({"phase": p, "rule": rule, "scope": scope, "pass": False,
                                    "detail": "not checked (no --vlogs)"})
                    continue
                total, missing, _ = i8
                bad = sum(m[3] for m in missing)
                pct = 100.0 * bad / total if total else 0.0
                results.append({"phase": p, "rule": rule, "scope": scope, "failing": bad, "in_scope": total,
                                "pct": round(pct, 3), "pass": pct <= tolerance})
                continue
            scoped = [t for t in traces if in_scope(t)]
            failing = sum(1 for t in scoped if any(v.rule == rule and counts(t, v) for v in by_trace[id(t)]))
            pct = 100.0 * failing / len(scoped) if scoped else 0.0
            results.append({"phase": p, "rule": rule, "scope": scope, "failing": failing,
                            "in_scope": len(scoped), "pct": round(pct, 3), "pass": pct <= tolerance})
    return results


# ---------------------------------------------------------------- output

def _pct(a, b):
    return 100.0 * a / b if b else 0.0


def _quantile(xs, q):
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(q * len(xs)))]


def summarize(traces, violations, hops, i8, gates, rules):
    kinds = collections.Counter(t.kind for t in traces)
    per_rule = {}
    for rule in rules:
        vs = [v for v in violations if v.rule == rule]
        examples, seen = [], set()
        for v in sorted(vs, key=lambda v: -(v.ms or 0)):
            if v.trace.id not in seen:
                seen.add(v.trace.id)
                examples.append(v.as_dict())
            if len(examples) == 5:
                break
        traces_hit = {id(v.trace) for v in vs}
        per_rule[rule] = {
            "violations": len(vs),
            "traces": len(traces_hit),
            "pct_traces": round(_pct(len(traces_hit), len(traces)), 2),
            "by_kind": dict(collections.Counter(v.trace.kind for v in {id(v.trace): v for v in vs}.values())),
            "top": collections.Counter(v.key for v in vs).most_common(8),
            "examples": examples,
        }
    edges = {e: {"n": len(xs), "p50_ms": round(_quantile(xs, .5), 2), "p99_ms": round(_quantile(xs, .99), 2)}
             for e, xs in sorted(hops.items(), key=lambda kv: -len(kv[1]))}
    out = {"traces": len(traces), "trace_kinds": dict(kinds), "rules": per_rule, "hops": edges, "gates": gates,
           "notes": {"I7": "pipeline loss is a metrics rule (VictoriaMetrics alerts), not checked here",
                     "I5": "hop overhead reported only; budgets gate after P4"}}
    if "I8" in rules:
        if i8 is None:
            out["i8"] = None
        else:
            total, missing, no_span_id = i8
            bad = sum(m[3] for m in missing)
            out["i8"] = {"lines_in_span": total, "missing_trace_id": bad, "pct": round(_pct(bad, total), 2),
                         "missing_span_id": no_span_id,
                         "top": [{"namespace": a, "container": b, "span": c, "n": n} for a, b, c, n in missing[:8]]}
    return out


def render(s):
    lines = [f"traces checked: {s['traces']}  "
             + "  ".join(f"{k}={s['trace_kinds'].get(k, 0)}" for k in TRACE_KINDS)]
    for rule, r in s["rules"].items():
        if rule in ("I7", "I8"):
            continue
        gating = " (reported, not gating)" if rule == "I5" else ""
        lines.append(f"\n## {rule}{gating}: {r['violations']} violations in {r['traces']} traces "
                     f"({r['pct_traces']:.1f}%)  by kind {r['by_kind']}")
        for key, n in r["top"]:
            lines.append(f"  {n:6d}  {key}")
        for e in r["examples"]:
            lines.append(f"  e.g. {e['trace_id']}  {e['detail']}")
        if rule == "I2":
            lines.append(f"  browser part missing (exempt, counted separately): "
                         f"{s['trace_kinds'].get('browser_part_missing', 0)} traces")
    if "I5" in s["rules"]:
        lines.append("\n## I5 hop overhead (CLIENT minus SERVER child), per edge")
        for e, h in s["hops"].items():
            lines.append(f"  {e:60s} n={h['n']:5d}  p50={h['p50_ms']:7.2f} ms  p99={h['p99_ms']:7.2f} ms")
    if "I7" in s["rules"]:
        lines.append(f"\n## I7: skipped; {s['notes']['I7']}")
    if "I8" in s["rules"]:
        i8 = s.get("i8")
        if i8 is None:
            lines.append("\n## I8: not checked (no --vlogs)")
        else:
            lines.append(f"\n## I8: {i8['missing_trace_id']} of {i8['lines_in_span']} app log lines in a span lack "
                         f"trace_id ({i8['pct']:.1f}%); {i8['missing_span_id']} lack span_id (reported only)")
            for m in i8["top"]:
                lines.append(f"  {m['n']:6d}  {m['namespace']}/{m['container']}  span {m['span']}")
    if s["gates"]:
        lines.append("\n## gates")
        for g in s["gates"]:
            mark = "PASS" if g["pass"] else "FAIL"
            if "in_scope" in g:
                lines.append(f"  {mark} {g['phase']} {g['rule']} on {g['scope']}: "
                             f"{g['failing']}/{g['in_scope']} failing ({g['pct']:.2f}%)")
            else:
                lines.append(f"  {mark} {g['phase']} {g['rule']}: {g['detail']}")
    return "\n".join(lines)


def _label(v):
    return str(v).replace("\\", "\\\\").replace('"', '\\"').replace("\n", "\\n")


def prometheus_text(traces, violations, i8, rules, env_override=None):
    env_of = (lambda t: env_override) if env_override else (lambda t: t.env)
    lines = []
    viol = collections.Counter((v.rule, v.service, env_of(v.trace)) for v in violations)
    if i8 is not None:
        for ns, container, _, n in i8[1]:
            viol[("I8", container, env_override or ("staging" if ns.endswith("-staging") else "production"))] += n
    for (rule, svc, env), n in sorted(viol.items()):
        lines.append(f'trace_check_violations_total{{rule="{rule}",service="{_label(svc)}",env="{_label(env)}"}} {n}')
    envs = sorted({env_of(t) for t in traces} | ({env_override} if env_override else set()))
    kinds = collections.Counter((t.kind, env_of(t)) for t in traces)
    hit = collections.Counter()
    for rule, kind, env, _ in {(v.rule, v.trace.kind, env_of(v.trace), id(v.trace)) for v in violations}:
        hit[(rule, kind, env)] += 1
    for env in envs:
        for kind in TRACE_KINDS:
            lines.append(f'trace_check_traces_total{{kind="{kind}",env="{_label(env)}"}} {kinds[(kind, env)]}')
            for rule in rules:
                if rule not in ("I7", "I8"):
                    lines.append(f'trace_check_violating_traces_total{{rule="{rule}",kind="{kind}",'
                                 f'env="{_label(env)}"}} {hit[(rule, kind, env)]}')
        lines.append(f'trace_check_last_run_timestamp_seconds{{env="{_label(env)}"}} {int(time.time())}')
    return "\n".join(lines) + "\n"


def push(url, text):
    if not urllib.parse.urlsplit(url).path.strip("/"):
        url = url.rstrip("/") + "/api/v1/import/prometheus"
    req = urllib.request.Request(url, data=text.encode(), headers={"Content-Type": "text/plain"})
    with urllib.request.urlopen(req, timeout=30) as r:
        r.read()


# ---------------------------------------------------------------- main

def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    src = ap.add_mutually_exclusive_group()
    src.add_argument("--dir", help="directory of saved Tempo trace JSONs")
    src.add_argument("--trace-ids", help="file of trace IDs (one per line) to fetch from --tempo")
    ap.add_argument("--tempo", help="Tempo base URL, e.g. http://tempo.monitoring:3200")
    ap.add_argument("--query", default="{}", help="TraceQL query for the Tempo search (default: {})")
    ap.add_argument("--since", default="1h", help="window start: 24h, 30m, epoch or ISO (default 1h)")
    ap.add_argument("--until", default="2m", help="window end (default 2m ago, so traces are complete)")
    ap.add_argument("--rules", default=",".join(RULES), help="comma-separated rules (default all)")
    ap.add_argument("--env", choices=("production", "staging"), help="only traces of this environment")
    ap.add_argument("--phase", choices=("P0", "P1", "P2", "P3", "P4", "P5"), help="exit 1 if a gate fails")
    ap.add_argument("--tolerance", type=float, default=0.0,
                    help="percent of in-scope traces a gate may fail (default 0; P1 done-when is 1)")
    ap.add_argument("--leaf-spans", help="leaf registry (default: tracing/leaf_spans.toml next to this tool)")
    ap.add_argument("--ingress", default="istio-ingress",
                    help="regex for ingress service names whose orphan SERVER spans have a browser parent")
    ap.add_argument("--vlogs", help="VictoriaLogs base URL for I8")
    ap.add_argument("--push", help="VictoriaMetrics URL to push counts to (/api/v1/import/prometheus)")
    ap.add_argument("--json", action="store_true", help="print JSON instead of the report")
    a = ap.parse_args(argv)

    rules = [r.strip() for r in a.rules.split(",") if r.strip()]
    if unknown := set(rules) - set(RULES):
        ap.error(f"unknown rules: {sorted(unknown)}")
    since, until = parse_time(a.since), parse_time(a.until)
    leaf_path = a.leaf_spans or os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "leaf_spans.toml")
    leaves = LeafRegistry.load(leaf_path)
    ingress_re = re.compile(a.ingress)

    if a.dir:
        raw = dir_traces(a.dir)
    elif a.tempo:
        if a.trace_ids:
            with open(a.trace_ids) as f:
                ids = [_hex_id(x.split("#")[0].strip()) for x in f if x.split("#")[0].strip()]
        else:
            ids = tempo_search(a.tempo, a.query, since, until)
        raw = tempo_fetch(a.tempo, ids)
    elif "I8" not in rules or not a.vlogs:
        ap.error("give --dir, or --tempo (with --trace-ids or a time window), or --vlogs with --rules I8")
    else:
        raw = []

    traces, violations, hops = [], [], collections.defaultdict(list)
    for spans in raw:
        if not spans:
            continue
        t = Trace(spans, ingress_re)
        if a.env and t.env != a.env:
            continue
        traces.append(t)
        violations += check_trace(t, rules, leaves, hops)

    i8 = check_i8(a.vlogs, since, until, a.env) if "I8" in rules and a.vlogs else None
    gates = evaluate_gates(a.phase, traces, violations, i8, a.tolerance)
    summary = summarize(traces, violations, hops, i8, gates, rules)
    print(json.dumps(summary, indent=2) if a.json else render(summary))
    if a.push:
        push(a.push, prometheus_text(traces, violations, i8, rules, a.env))
    return 0 if all(g["pass"] for g in gates) else 1


if __name__ == "__main__":
    sys.exit(main())
