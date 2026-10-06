# Tracing

Every browser request is one trace in Tempo:

```
public-istio.istio-ingress  (Gateway, starts the trace)
└─ frontend  GET /page  or  POST /bff/<server fn>
   ├─ ssr.server_future  check_login_status        (SSR only)
   │  └─ bff.check_login_status
   │     └─ POST /internal/token/introspect         (CLIENT span)
   │        └─ waypoint.auth
   │           └─ auth  POST /internal/token/introspect
   └─ get_record / save                            (session store)
```

A camera session adds `arcane.ws_session`, with ai_pipeline's `arcane.ws_connection` and one
`arcane.capture` per roll under it.

Spans go from the services to Alloy over OTLP (`OTEL_EXPORTER_OTLP_ENDPOINT`). Istio sends its
spans to Tempo directly. The Gateway makes the sampling decision (`istio/manifests/telemetry.yaml`
in the homelab repo), and the services follow it.

## Rules for new code

| You are adding | Do this |
|---|---|
| An axum service | Layer `OtelAxumLayer::default().filter(\|p\| p != "/metrics")` and register the W3C `TraceContextPropagator` at startup. Copy `auth/src/main.rs`. |
| An outbound HTTP call (frontend) | Build the client with `api::trace::client(reqwest::Client)`. A plain `reqwest::Client` sends no `traceparent`, so the callee starts its own trace. |
| A `use_server_future` or `use_loader` | Use `api::trace::use_server_future("name", f)`; add a matching `use_loader` wrapper when first needed. Clippy warns on the plain hooks (`services/frontend/clippy.toml`). |
| Work that outlives the request: `tokio::spawn`, a queue, a WebSocket | Run it in `api::detached_span!("name")`. Neither `.in_current_span()` nor a plain child span works: they keep the request's span open until the work ends. |
| A non-HTTP hop (WebSocket, queue message) | Send the context with `api::trace::inject(&span, headers)` and extract it on the other side, as ai_pipeline's `accept_hdr_async` does. |

### Why these rules exist

- **SSR.** Dioxus 0.7 runs server-function requests inside the request span, but runs the SSR render
  outside it (dioxus-server `ssr.rs`, `rt.spawn_pinned(create_render_future)`). Without the wrapper,
  every page load splits into three traces. When upgrading Dioxus, check whether that call is now
  instrumented. If it is, delete the wrapper and the clippy entries.
- **Spans that never close.** A span is exported only when it closes, and a tracing parent stays open
  while any child holds it. A child that lives on in a background task therefore keeps its parent
  unexported, and everything under the parent shows in Tempo with a missing root.
- **hyper-util.** Up to 0.1.20, hyper-util's `TokioExecutor` caused exactly that problem: every pooled
  connection held the span it was opened in. 0.1.21 fixed this. Keep hyper-util at 0.1.21 or later,
  and never enable its `rt-tracing-exec-force` feature. `client_propagates_and_releases_the_callers_span`
  in `api/src/trace.rs` fails if either happens.
- **Log level.** `RUST_LOG` filters spans as well as logs. At `warn`, no request or client spans exist,
  so nothing is propagated.

## Checks

CI only builds images. It runs neither clippy nor the tests, so run these before merging changes
to dependencies or tracing code:

```sh
cd services/frontend
cargo clippy -p web --features server -- -D clippy::disallowed-methods
cargo test -p api -p web --features web/server
```

## Upgrading OpenTelemetry

The frontend pins all OTel crates in `services/frontend/Cargo.toml` (`[workspace.dependencies]`).
auth and ai_pipeline pin their own copies. Upgrade them together:

1. Bump `opentelemetry`, `opentelemetry_sdk`, `opentelemetry-otlp`, `opentelemetry-appender-tracing`,
   `tracing-opentelemetry` and `axum-tracing-opentelemetry` together. Check each crate's changelog
   for the version that pairs with your `opentelemetry` version.
2. Set reqwest-tracing's feature to `opentelemetry_0_NN` for the same `opentelemetry` version.
   reqwest-tracing 0.7.1 goes up to `opentelemetry_0_32`.
3. Run `cargo test -p api -p web --features web/server`. If the versions are mismatched, the build
   still passes but propagation silently stops, and the trace tests are what catch it.

## Checking traces in Tempo

- **Page loads have one root.** `{ resource.service.name = "frontend" && name =~ "bff.*" }`: the root
  service should be `public-istio.istio-ingress`, never `frontend`.
- **No orphans.** The trace view shows no "root span not yet received". Camera sessions are the
  exception while they run. `arcane.ws_session` and ai_pipeline's `arcane.ws_connection` are exported
  only when the session ends, and if a pod restarts mid-session they are lost.

## Not covered yet

- **auth's own outbound calls.** These are OAuth, Resend and ark. They carry no client spans, because
  auth uses reqwest 0.12 and reqwest-tracing 0.7 needs reqwest 0.13. The ark calls do forward
  `traceparent`.
- **Shared telemetry crate.** Each service has its own telemetry setup. A shared crate becomes
  worthwhile with a fourth service or at the next OTel upgrade. It needs the CI Docker build context
  changed from `services/<svc>` to `services/`.
