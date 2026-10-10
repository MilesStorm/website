"""Synthetic traces covering each rule's pass and fail, and each exemption. Run:
python3 -m unittest tracing/checker/test_trace_check.py"""
import base64
import collections
import os
import re
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import trace_check as tc  # noqa: E402

T0 = 1_791_500_000_000_000_000
SHA = "0123abc"
INGRESS = re.compile("istio-ingress")


def _val(v):
    if isinstance(v, bool):
        return {"boolValue": v}
    if isinstance(v, int):
        return {"intValue": str(v)}
    if isinstance(v, float):
        return {"doubleValue": v}
    return {"stringValue": v}


def _b64(hexid):
    return base64.b64encode(bytes.fromhex(hexid)).decode() if hexid else ""


def doc(*spans):
    """Tempo v1 JSON. Each span: (id, parent, service, kind, name, start_ms, end_ms[, attrs[, resource]])."""
    batches = []
    for i, (sid, parent, svc, kind, name, st, en, *rest) in enumerate(spans):
        attrs = rest[0] if rest else {}
        res = {"service.name": svc, "service.version": SHA, **(rest[1] if len(rest) > 1 else {})}
        status = attrs.pop("_status", None)
        span = {"traceId": _b64("ab" * 16), "spanId": _b64(f"{sid:016x}"),
                "parentSpanId": _b64(f"{parent:016x}") if parent else "",
                "name": name, "kind": f"SPAN_KIND_{kind}",
                "startTimeUnixNano": str(T0 + int(st * tc.MS)), "endTimeUnixNano": str(T0 + int(en * tc.MS)),
                "attributes": [{"key": k, "value": _val(v)} for k, v in attrs.items()],
                "status": {"code": f"STATUS_CODE_{status}"} if status else {}}
        batches.append({"resource": {"attributes": [{"key": k, "value": _val(v)} for k, v in res.items()]},
                        "scopeSpans": [{"spans": [span]}]})
    return {"batches": batches}


def trace(*spans):
    return tc.Trace(tc.parse_trace(doc(*spans)), INGRESS)


def leaf(name, service="*"):
    return {"service": service, "name": name, "reason": "test"}


def rules(t, leaves=tc.LeafRegistry()):
    return [v.rule for v in tc.check_trace(t, tc.RULES, leaves, collections.defaultdict(list))]


HTTP_SRV = {"http.request.method": "POST", "http.route": "/bff/x", "http.response.status_code": 200}
HTTP_CLI = {"http.request.method": "POST", "http.response.status_code": 200, "peer.service": "auth"}


def good_server_trace():
    return trace(
        (1, 0, "frontend", "SERVER", "POST /bff/x", 0, 10, dict(HTTP_SRV)),
        (2, 1, "frontend", "CLIENT", "POST /internal/x", 1, 9, dict(HTTP_CLI)),
        (3, 2, "auth", "SERVER", "POST /internal/x", 2, 8, dict(HTTP_SRV, **{"http.route": "/internal/x"})),
        (4, 3, "auth", "CLIENT", "sqlx.fetch", 2.5, 7.5, {"db.system.name": "postgresql", "peer.service": "postgres"}),
    )


class Parsing(unittest.TestCase):
    def test_ids_and_kinds(self):
        t = good_server_trace()
        self.assertEqual(t.id, "ab" * 16)
        self.assertEqual(t.roots[0].id, f"{1:016x}")
        self.assertEqual([s.kind for s in t.spans], ["SERVER", "CLIENT", "SERVER", "CLIENT"])
        self.assertEqual(t.kind, "server")

    def test_search_hex_ids_are_zero_padded(self):
        self.assertEqual(tc._hex_id("abc"), "0000000000000abc")
        self.assertEqual(tc._hex_id("1" * 31), "0" + "1" * 31)

    def test_v2_document(self):
        d = doc((1, 0, "frontend", "SERVER", "GET /", 0, 1))
        self.assertEqual(len(tc.parse_trace({"trace": {"resourceSpans": d["batches"]}})), 1)

    def test_clean_trace_has_no_violations(self):
        self.assertEqual(rules(good_server_trace()), [])


class I1(unittest.TestCase):
    def test_one_root_passes(self):
        self.assertNotIn("I1", rules(good_server_trace()))

    def test_two_roots_fail(self):
        t = trace((1, 0, "frontend", "SERVER", "GET /", 0, 1, dict(HTTP_SRV)),
                  (2, 0, "frontend", "INTERNAL", "tick", 0, 1))
        self.assertIn("I1", rules(t))


class I2(unittest.TestCase):
    def test_missing_parent_fails(self):
        t = trace((1, 0, "frontend", "SERVER", "GET /", 0, 10, dict(HTTP_SRV)),
                  (2, 99, "frontend", "INTERNAL", "lost", 1, 2))
        self.assertIn("I2", rules(t))

    def test_browser_parent_at_ingress_is_exempt_and_counted(self):
        t = trace((1, 99, "public-istio.istio-ingress", "SERVER", "POST /bff/x", 0, 10,
                   {"http.method": "POST", "http.status_code": "200", "component": "proxy"}))
        self.assertNotIn("I2", rules(t))
        self.assertNotIn("I1", rules(t))
        self.assertEqual(t.kind, "browser_part_missing")

    def test_missing_parent_inside_the_mesh_is_not_exempt(self):
        t = trace((1, 99, "frontend", "SERVER", "POST /bff/x", 0, 10, dict(HTTP_SRV)))
        self.assertIn("I2", rules(t))


class I3(unittest.TestCase):
    def test_in_process_within_1ms_passes(self):
        t = trace((1, 0, "frontend", "INTERNAL", "a", 0, 10), (2, 1, "frontend", "INTERNAL", "b", -0.5, 10.9))
        self.assertEqual(tc.check_i3(t, tc.LeafRegistry()), [])

    def test_in_process_over_1ms_fails(self):
        t = trace((1, 0, "frontend", "INTERNAL", "a", 0, 10), (2, 1, "frontend", "INTERNAL", "b", 0, 11.5))
        [v] = tc.check_i3(t, tc.LeafRegistry())
        self.assertAlmostEqual(v.ms, 1.5)
        self.assertIn("in-process", v.detail)

    def test_cross_node_uses_1ms(self):
        t = trace((1, 0, "frontend", "CLIENT", "a", 0, 10), (2, 1, "auth", "SERVER", "b", 0.5, 11.2))
        self.assertIn("cross-node", tc.check_i3(t, tc.LeafRegistry())[0].detail)

    def test_same_service_other_pod_is_cross_process(self):
        t = trace((1, 0, "frontend", "CLIENT", "a", 0, 10, {}, {"k8s.pod.name": "p1"}),
                  (2, 1, "frontend", "SERVER", "b", 0, 11.5, {}, {"k8s.pod.name": "p2"}))
        self.assertIn("cross-node", tc.check_i3(t, tc.LeafRegistry())[0].detail)

    def test_browser_server_uses_clock_error_bound(self):
        ok = trace((1, 0, "milesstorm-web", "CLIENT", "POST /bff/x", 0, 10, {"browser.clock_offset_error_ms": 5.0}),
                   (2, 1, "public-istio.istio-ingress", "SERVER", "POST /bff/x", 1, 14))
        bad = trace((1, 0, "milesstorm-web", "CLIENT", "POST /bff/x", 0, 10, {"browser.clock_offset_error_ms": 2.0}),
                    (2, 1, "public-istio.istio-ingress", "SERVER", "POST /bff/x", 1, 14))
        self.assertEqual(tc.check_i3(ok, tc.LeafRegistry()), [])
        self.assertIn("browser-server tolerance 2.00", tc.check_i3(bad, tc.LeafRegistry())[0].detail)

    def test_follows_relation_is_exempt(self):
        t = trace((1, 0, "frontend", "SERVER", "GET /", 0, 10),
                  (2, 1, "milesstorm-web", "INTERNAL", "page load /", 20, 400, {"trace.relation": "follows"}))
        self.assertEqual(tc.check_i3(t, tc.LeafRegistry()), [])


    def test_follows_only_where_the_contract_allows_it(self):
        t = trace((1, 0, "auth", "SERVER", "POST /x", 0, 10),
                  (2, 1, "auth", "INTERNAL", "slow", 1, 500, {"trace.relation": "follows"}))
        self.assertEqual(len(tc.check_i3(t, tc.LeafRegistry())), 1)
        self.assertEqual(tc.check_i3(t, tc.LeafRegistry([], [leaf("slow", "auth")])), [])

    def test_a_following_span_still_cannot_start_before_its_parent(self):
        t = trace((1, 0, "frontend", "SERVER", "GET /", 100, 110),
                  (2, 1, "milesstorm-web", "INTERNAL", "page load /", 20, 400,
                   {"trace.relation": "follows", "browser.clock_offset_error_ms": 5.0}))
        self.assertIn("starts before", tc.check_i3(t, tc.LeafRegistry())[0].detail)

    def test_an_absurd_clock_error_bound_does_not_excuse_anything(self):
        for bound in (float("nan"), -1.0, 1e6):
            t = trace((1, 0, "milesstorm-web", "CLIENT", "POST /bff/x", 0, 10, {"browser.clock_offset_error_ms": bound}),
                      (2, 1, "public-istio.istio-ingress", "SERVER", "POST /bff/x", 1, 50_000))
            self.assertEqual(len(tc.check_i3(t, tc.LeafRegistry())), 1, bound)


class I4(unittest.TestCase):
    def test_covered_parent_passes(self):
        self.assertNotIn("I4", rules(good_server_trace()))

    def test_own_time_over_limit_fails(self):
        # 100 ms span, 90 ms covered: own 10 ms > 2 + 2 ms
        t = trace((1, 0, "auth", "INTERNAL", "work", 0, 100), (2, 1, "auth", "INTERNAL", "child", 5, 95))
        [v] = tc.check_i4(t, tc.LeafRegistry([leaf("child")]))
        self.assertAlmostEqual(v.ms, 10)

    def test_own_time_within_limit_passes(self):
        t = trace((1, 0, "auth", "INTERNAL", "work", 0, 100), (2, 1, "auth", "INTERNAL", "child", 1, 98))
        self.assertEqual(tc.check_i4(t, tc.LeafRegistry([leaf("child")])), [])

    def test_20ms_cap(self):
        # 2000 ms span: 2 + 2 % would allow 42 ms, the cap is 20 ms
        t = trace((1, 0, "auth", "INTERNAL", "work", 0, 2000), (2, 1, "auth", "INTERNAL", "child", 25, 2000))
        self.assertEqual(len(tc.check_i4(t, tc.LeafRegistry([leaf("child")]))), 1)

    def test_browser_click_and_page_load_are_exempt(self):
        t = trace((1, 0, "milesstorm-web", "INTERNAL", "click /ark", 0, 500, {"ui.event": "click"}),
                  (2, 1, "milesstorm-web", "CLIENT", "POST /bff/x", 400, 450))
        self.assertEqual(tc.check_i4(t, tc.LeafRegistry()), [])

    def test_follows_children_do_not_cover_parent_time(self):
        t = trace((1, 0, "auth", "INTERNAL", "work", 0, 100),
                  (2, 1, "auth", "INTERNAL", "email.verify_send", 0, 100, {"trace.relation": "follows"}))
        self.assertEqual(len(tc.check_i4(t, tc.LeafRegistry([leaf("email.verify_send")], [leaf("email.verify_send", "auth")]))), 1)

    def test_childless_internal_needs_registry(self):
        t = trace((1, 0, "auth", "SERVER", "POST /login", 0, 10),
                  (2, 1, "auth", "INTERNAL", "password.verify", 0.5, 9.5))
        self.assertEqual(len(tc.check_i4(t, tc.LeafRegistry())), 1)
        self.assertEqual(tc.check_i4(t, tc.LeafRegistry([leaf("password.*", "auth")])), [])

    def test_leaf_registry_file(self):
        with tempfile.NamedTemporaryFile("w", suffix=".toml", delete=False) as f:
            f.write('[[leaf]]\nservice = "auth"\nname = "password.verify"\nreason = "argon2"\n')
        try:
            reg = tc.LeafRegistry.load(f.name)
        finally:
            os.unlink(f.name)
        t = trace((1, 0, "auth", "INTERNAL", "password.verify", 0, 30))
        self.assertTrue(reg.allows(t.spans[0]))
        with self.assertRaises(OSError):
            tc.LeafRegistry.load("/nonexistent.toml")

    def test_registry_entries_need_a_reason_and_a_name(self):
        with self.assertRaises(ValueError):
            tc.LeafRegistry([{"service": "auth", "name": "x"}])
        with self.assertRaises(ValueError):
            tc.LeafRegistry([{"service": "auth", "name": "*", "reason": "everything"}])

    def test_childless_server_span_is_all_own_time(self):
        slow = trace((1, 0, "frontend", "SERVER", "POST /bff/x", 0, 300, dict(HTTP_SRV)))
        self.assertIn("childless SERVER", tc.check_i4(slow, tc.LeafRegistry())[0].detail)
        fast = trace((1, 0, "frontend", "SERVER", "GET /assets/a.css", 0, 1.5, dict(HTTP_SRV)))
        self.assertEqual(tc.check_i4(fast, tc.LeafRegistry()), [])
        envoy = trace((1, 0, "public-istio.istio-ingress", "SERVER", "GET /", 0, 300, {"component": "proxy"}))
        self.assertEqual(tc.check_i4(envoy, tc.LeafRegistry()), [])

    def test_page_or_asset_scope(self):
        t = trace((1, 0, "frontend", "SERVER", "GET /", 0, 10, {"http.request.method": "GET", "url.path": "/"}),
                  (2, 1, "frontend", "INTERNAL", "ssr", 0, 10))
        api = trace((1, 0, "frontend", "SERVER", "POST /bff/x", 0, 10, dict(HTTP_SRV, **{"url.path": "/bff/x"})),
                    (2, 1, "frontend", "INTERNAL", "work", 0, 10))
        self.assertTrue(t.page_or_asset(t.spans[1]))
        self.assertFalse(api.page_or_asset(api.spans[1]))


class I5(unittest.TestCase):
    def test_hop_overhead_per_edge(self):
        hops = collections.defaultdict(list)
        self.assertEqual(tc.check_i5(good_server_trace(), hops), [])
        self.assertEqual(hops["frontend -> auth"], [2.0])

    def test_client_leaf_needs_peer_service(self):
        t = trace((1, 0, "frontend", "INTERNAL", "work", 0, 10), (2, 1, "frontend", "CLIENT", "GET ark", 1, 9))
        self.assertEqual(len(tc.check_i5(t, collections.defaultdict(list))), 1)
        t = trace((1, 0, "frontend", "INTERNAL", "work", 0, 10),
                  (2, 1, "frontend", "CLIENT", "GET ark", 1, 9, {"peer.service": "ark"}))
        self.assertEqual(tc.check_i5(t, collections.defaultdict(list)), [])


class I6(unittest.TestCase):
    def details(self, t):
        return [v.detail for v in tc.check_i6(t)]

    def test_clean_passes(self):
        self.assertEqual(self.details(good_server_trace()), [])

    def test_missing_service_version_and_non_sha(self):
        t = trace((1, 0, "frontend", "SERVER", "GET /", 0, 1, dict(HTTP_SRV), {"service.version": ""}),
                  (2, 1, "auth", "SERVER", "GET /", 0, 1, dict(HTTP_SRV), {"service.version": "latest"}))
        d = " ".join(self.details(t))
        self.assertIn("no service.version", d)
        self.assertIn("not a git SHA", d)

    def test_http_server_needs_route_and_status(self):
        t = trace((1, 0, "frontend", "SERVER", "GET x", 0, 1, {"http.request.method": "GET"}))
        d = " ".join(self.details(t))
        self.assertIn("no http.route", d)
        self.assertIn("no HTTP status code", d)

    def test_envoy_server_needs_no_route(self):
        t = trace((1, 0, "public-istio.istio-ingress", "SERVER", "GET /", 0, 1,
                   {"http.method": "GET", "http.status_code": "200", "component": "proxy"}))
        self.assertEqual(self.details(t), [])

    def test_client_needs_peer_service_but_browser_must_not_have_it(self):
        t = trace((1, 0, "frontend", "CLIENT", "POST x", 0, 1, {"http.request.method": "POST",
                                                                 "http.response.status_code": 200}))
        self.assertIn("no peer.service", " ".join(self.details(t)))
        t = trace((1, 0, "milesstorm-web", "CLIENT", "POST x", 0, 1,
                   {"http.request.method": "POST", "http.response.status_code": 200, "peer.service": "frontend"}))
        self.assertIn("browser span carries peer.service", " ".join(self.details(t)))

    def test_db_system(self):
        t = trace((1, 0, "auth", "CLIENT", "sqlx", 0, 1, {"db.query.text": "select 1", "peer.service": "pg"}))
        self.assertIn("db span without db.system", " ".join(self.details(t)))

    def test_error_status(self):
        t = trace((1, 0, "frontend", "SERVER", "GET /", 0, 1, dict(HTTP_SRV, **{"http.response.status_code": 503})))
        self.assertIn("HTTP 503 without ERROR", " ".join(self.details(t)))
        t = trace((1, 0, "frontend", "SERVER", "GET /", 0, 1,
                   dict(HTTP_SRV, **{"http.response.status_code": 404, "_status": "ERROR"})))
        self.assertIn("SERVER span marked ERROR", " ".join(self.details(t)))
        t = trace((1, 0, "frontend", "CLIENT", "GET /", 0, 1,
                   dict(HTTP_CLI, **{"http.response.status_code": 500, "peer.service": "ark"})))
        self.assertIn("HTTP 500 without ERROR", " ".join(self.details(t)))

    def test_expected_auth_401_may_be_ok(self):
        url = "http://auth-service.auth.svc.cluster.local/internal/token/introspect"
        t = trace((1, 0, "frontend", "CLIENT", "POST /internal/token/introspect", 0, 1,
                   dict(HTTP_CLI, **{"http.response.status_code": 401, "_status": "OK", "url.full": url})))
        self.assertEqual(self.details(t), [])
        # Only the auth service's login and token endpoints: not GitHub's OAuth, not other auth routes.
        for peer, url in (("github", "https://github.com/login/oauth/access_token"),
                          ("auth", "http://auth-service.auth.svc.cluster.local/internal/profile")):
            t = trace((1, 0, "auth", "CLIENT", "POST", 0, 1, dict(
                HTTP_CLI, **{"http.response.status_code": 401, "peer.service": peer, "url.full": url})))
            self.assertIn("HTTP 401 without ERROR", " ".join(self.details(t)), url)
        t = trace((1, 0, "frontend", "CLIENT", "POST /x", 0, 1,
                   dict(HTTP_CLI, **{"http.response.status_code": 401, "peer.service": "ark"})))
        self.assertIn("HTTP 401 without ERROR", " ".join(self.details(t)))


class I8(unittest.TestCase):
    def test_query_and_counts(self):
        queries = []

        def fetch(_, q):
            queries.append(q)
            if "-trace_id:*" in q:
                return [{"namespace": "auth", "container": "auth", "span.name": "x", "n": "3"}]
            return [{"n": "10"}]

        total, missing, no_span = tc.check_i8("http://vl", 0, 60, "staging", fetch)
        self.assertEqual((total, missing, no_span), (10, [("auth", "auth", "x", 3)], 10))
        self.assertTrue(all("span.name:* level:* -container:istio-proxy" in q for q in queries))
        self.assertTrue(all('namespace:~"-staging$"' in q and '-namespace' not in q for q in queries))


class Gates(unittest.TestCase):
    def setUp(self):
        self.browser_bad = trace(
            (1, 0, "milesstorm-web", "CLIENT", "POST /bff/x", 0, 10),
            (2, 1, "public-istio.istio-ingress", "SERVER", "POST /bff/x", 1, 50))
        self.server_ok = good_server_trace()
        self.traces = [self.browser_bad, self.server_ok]
        self.viol = [v for t in self.traces for v in tc.check_trace(t, tc.RULES, tc.LeafRegistry(),
                                                                    collections.defaultdict(list))]

    def test_p0_never_gates(self):
        self.assertEqual(tc.evaluate_gates("P0", self.traces, self.viol, None, 0), [])

    def test_p1_fails_on_browser_i3(self):
        g, i5 = tc.evaluate_gates("P1", self.traces, self.viol, None, 0)
        self.assertEqual((g["rule"], g["failing"], g["in_scope"], g["pass"]), ("I3", 1, 1, False))
        self.assertEqual((i5["rule"], i5["failing"], i5["pass"]), ("I5", 0, True))

    def test_no_data_is_not_a_pass(self):
        gates = tc.evaluate_gates("P1", [self.server_ok], [], None, 0)
        self.assertEqual([(g["in_scope"], g["pass"]) for g in gates], [(0, False), (0, False)])
        gates = tc.evaluate_gates("P1", [self.server_ok], [], None, 0, min_traces=0)
        self.assertTrue(all(g["pass"] for g in gates))

    def test_a_browser_request_with_no_server_side_fails_p1(self):
        # Propagation broke: the fetch span has no SERVER child, so I3 has nothing to compare.
        t = trace((1, 0, "milesstorm-web", "INTERNAL", "click Go", 0, 20, {"ui.event": "click"}),
                  (2, 1, "milesstorm-web", "CLIENT", "POST /bff/x", 1, 10, {"url.full": "https://milesstorm.com/bff/x"}))
        viol = tc.check_trace(t, tc.RULES, tc.LeafRegistry(), collections.defaultdict(list))
        i3, i5 = tc.evaluate_gates("P1", [t], viol, None, 0)
        self.assertTrue(i3["pass"])
        self.assertEqual((i5["failing"], i5["pass"]), (1, False))

    def test_a_call_to_a_traced_service_needs_its_server_span(self):
        t = trace((1, 0, "frontend", "SERVER", "POST /bff/x", 0, 10, dict(HTTP_SRV)),
                  (2, 1, "frontend", "CLIENT", "POST /internal/x", 1, 9, dict(HTTP_CLI)))
        self.assertIn("I5", rules(t))
        ark = trace((1, 0, "auth", "SERVER", "POST /x", 0, 10, dict(HTTP_SRV)),
                    (2, 1, "auth", "CLIENT", "GET", 1, 9, dict(HTTP_CLI, **{"peer.service": "ark"})))
        self.assertNotIn("I5", rules(ark))

    def test_tolerance(self):
        gates = tc.evaluate_gates("P1", self.traces, self.viol, None, 100)
        self.assertTrue(all(g["pass"] for g in gates))

    def test_phases_are_cumulative_and_p5_needs_i8(self):
        gates = tc.evaluate_gates("P5", self.traces, self.viol, None, 0)
        self.assertEqual({g["phase"] for g in gates}, {"P1", "P2", "P3", "P4", "P5"})
        self.assertFalse([g for g in gates if g["rule"] == "I8"][0]["pass"])
        gates = tc.evaluate_gates("P5", self.traces, self.viol, (5, [], 0), 0)
        self.assertTrue([g for g in gates if g["rule"] == "I8"][0]["pass"])

    def test_main_exit_codes(self):
        with tempfile.TemporaryDirectory() as d:
            import json
            with open(os.path.join(d, "t.json"), "w") as f:
                json.dump(doc((1, 0, "milesstorm-web", "CLIENT", "POST /bff/x", 0, 10),
                              (2, 1, "public-istio.istio-ingress", "SERVER", "POST /bff/x", 1, 50)), f)
            with open(os.devnull, "w") as null:
                stdout, sys.stdout = sys.stdout, null
                try:
                    self.assertEqual(tc.main(["--dir", d, "--phase", "P0"]), 0)
                    self.assertEqual(tc.main(["--dir", d, "--phase", "P1"]), 1)
                    # Nothing from staging in there: no data is a failure unless asked otherwise.
                    self.assertEqual(tc.main(["--dir", d, "--phase", "P1", "--env", "staging"]), 1)
                    self.assertEqual(tc.main(["--dir", d, "--phase", "P1", "--env", "staging", "--min-traces", "0"]), 0)
                finally:
                    sys.stdout = stdout


class Output(unittest.TestCase):
    def test_env_detection(self):
        self.assertEqual(good_server_trace().env, "production")
        t = trace((1, 0, "frontend", "SERVER", "GET /", 0, 1, {}, {"k8s.namespace.name": "website-staging"}))
        self.assertEqual(t.env, "staging")
        t = trace((1, 0, "milesstorm-web", "INTERNAL", "click", 0, 1, {}, {"deployment.environment.name": "staging"}))
        self.assertEqual(t.env, "staging")
        # The servers decide, whichever batch comes first.
        browser = (1, 0, "milesstorm-web", "CLIENT", "POST /bff/x", 0, 9, {}, {"deployment.environment.name": "production"})
        server = (2, 1, "frontend", "SERVER", "POST /bff/x", 1, 8, {}, {"deployment.environment.name": "staging"})
        self.assertEqual(trace(browser, server).env, "staging")
        self.assertEqual(trace(server, browser).env, "staging")

    def test_prometheus_text(self):
        t = trace((1, 0, "frontend", "SERVER", "GET /", 0, 1, dict(HTTP_SRV)),
                  (2, 0, "frontend", "INTERNAL", "tick", 0, 1))
        v = tc.check_trace(t, ["I1"], tc.LeafRegistry(), collections.defaultdict(list))
        text = tc.prometheus_text([t], v, (4, [("auth-staging", "auth", "x", 2)], 0), ["I1", "I8"])
        text = tc.prometheus_text([t], v, (4, [("auth-staging", "auth", "x", 2)], 0), ["I1", "I2", "I8"], now=1000)
        self.assertIn('trace_check_violations{rule="I1",service="frontend",env="production"} 1 1000000', text)
        self.assertIn('trace_check_violations{rule="I2",service="frontend",env="production"} 0 1000000', text, "zeros")
        self.assertIn('trace_check_violations{rule="I8",service="auth",env="staging"} 2 1000000', text)
        self.assertIn('trace_check_traces{kind="server",env="production"} 1 1000000', text)
        self.assertIn('trace_check_traces{kind="browser_part_missing",env="production"} 0 1000000', text)
        self.assertIn('trace_check_violating_traces{rule="I1",kind="server",env="production"} 1 1000000', text)

    def test_parse_time(self):
        self.assertEqual(tc.parse_time("24h", now=100000), 100000 - 86400)
        self.assertEqual(tc.parse_time("now", now=5), 5)
        self.assertEqual(tc.parse_time("2026-10-09T00:00:00Z"), 1791504000)


if __name__ == "__main__":
    unittest.main()
