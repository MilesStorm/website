# Tracing

A user interaction is one trace in Tempo, from the click to the database:

```
milesstorm-web  submit Log In                         (browser, root)
└─ POST /bff/login_password                          (browser fetch, CLIENT)
   └─ public-istio.istio-ingress                     (Gateway)
      └─ frontend  POST /bff/login_password
         └─ bff.login_password
            ├─ POST /internal/token/exchange         (CLIENT span)
            │  └─ waypoint.auth                      (SERVER, then CLIENT to auth)
            │     └─ auth  POST /internal/token/exchange
            │        └─ sqlx.fetch_optional …        (postgres, CLIENT)
            └─ redis session.save                    (redis, CLIENT)
```

A page load is the server's trace, and the browser joins it: the server writes its span into
`<meta name="traceparent">`, and the browser's `page load <path>` span (with the WASM download
and hydration-time server calls under it) is a child of that span (OTel's document-load pattern):

```
public-istio.istio-ingress
└─ frontend  GET <Route>
   ├─ ssr.server_future  check_login_status        (SSR only)
   │  └─ bff.check_login_status
   │     └─ POST /internal/token/introspect         (CLIENT span) → waypoint.auth → auth → postgres
   ├─ redis session.load
   └─ milesstorm-web  page load /path               (browser)
      └─ GET /assets/…wasm, POST /bff/…
```

### Reading a page-load trace

The `page load` span starts when the HTML arrives and ends after the WASM has hydrated, so it
outlives the server span it is a child of: the server is done once the HTML is sent. It carries
`trace.relation=follows` to say so; it is the one browser span allowed outside its parent. Two
consequences:

- Grafana computes the critical path from the trace's root, the Gateway span, and stops at the
  server render; the page load is never on it. To see what the page load waited for, open the
  `page load` span's subtree (the WASM download, hydration-time `POST /bff/…` calls).
- The page load's own time (time no child covers) is the browser parsing and hydrating. It is
  labelled by span events: the navigation timing marks after the HTML arrived (`responseEnd`,
  `domInteractive`, `domContentLoadedEventStart`/`End`, `domComplete`, `loadEventStart`/`End`)
  and one `longtask` event per task over 50 ms (`longtask.duration_ms`). Long tasks are
  Chromium-only: in Safari and Firefox the own time stays unlabelled.

A camera session and a roll stream are not one long span: see [Streams](#streams).

The services send spans to Alloy over OTLP (`OTEL_EXPORTER_OTLP_ENDPOINT`). Istio sends its spans to
Tempo directly. The browser sends its spans (and errors, web vitals) through Grafana Faro to
`https://milesstorm.com/faro/collect`, which the homelab routes to Alloy's `faro.receiver`
(`monitor/alloy.yaml`, `monitor/manifests/routes.yaml`).

Sampling: the first traced hop decides and everything after follows the `traceparent` flag. For
browser traffic that is Faro (session sampling, `sessionTracking.samplingRate`, default all); for
everything else the Gateway (`istio/manifests/telemetry.yaml` in the homelab repo, 100%; only for
milesstorm.com, other hosts on the Gateway start no traces: `istio/manifests/gateway-trace-hosts.yaml`).
Lowering one without the other leaves the other's traces at full volume. A client can send its own
`traceparent`; Faro's uploads use that to opt out (below).

## Browser

`packages/web/assets/trace.js` runs after the Faro bundles (`packages/web/assets/vendor/`, copied
from `node_modules` by `npm run vendor`; the Dockerfile does this before `dx bundle`). The App
component loads all three in `<head>`, before the WASM. The script:

- starts a span on a click or form submit and makes every fetch it causes a child of it. The span
  is only created when a server call happens, so clicks that stay in the browser make no trace. It
  ends 300 ms after its last fetch span ended (at most 10 s), and never before a child: OTel's
  fetch span ends when the response body is read and reaches the span processor 300 ms later, so
  trace.js counts child spans in an OTel `SpanProcessor` rather than trusting the fetch promise.
- starts a `navigate <route>` span the same way on a client-side route change
  (`history.pushState`, `popstate`), named by the templated path only, never the query string.
  A route change caused by a click keeps the click as the root, and one during the page load
  stays in the page load. Faro's navigation instrumentation is off: it records query strings.
- gives the arcane camera WebSocket a `camera session` span (above), by wrapping `window.WebSocket`.
- names the span from `data-trace-name`, then `aria-label`, then the button's text (links: their
  path). Give a button `data-trace-name="…"` when its text isn't a good name or holds user data.
- links the page load to the server's trace through `<meta name="traceparent">`
  (`api::trace::traceparent()`). This needs HTML rendered per request: no Dioxus incremental
  rendering and no Cloudflare cache rule for pages, or page loads join someone else's trace.
- ends the page-load span once the page has loaded and no server call has run for 1.5 s (at most
  15 s), so calls made during hydration land under it. A click that calls the server ends it.
- records long tasks (Chromium) as `longtask` events on the open click, navigate or page-load span.
- only starts Faro on milesstorm.com (`deployment.environment.name=production`) and
  staging.milesstorm.com (`staging`), so `dx serve` sends nothing.

Faro's fetch instrumentation sends `traceparent` on same-origin requests only. It ignores `/faro/`,
`/ws/` and `/api/arcane/rolls`. Faro's own uploads send an unsampled `traceparent` (flags `00`):
the Gateway traces everything else, and would start a trace per upload. Only Faro's errors, web
vitals, session, view and tracing instrumentations are on; performance, user-action, navigation and
console are off (volume, and they record URLs whose query strings hold reset and invite codes).

`/faro/collect` is public, so its input is untrusted (homelab repo): the Gateway rejects compressed
or non-JSON bodies and bodies over 64 KiB (`istio/manifests/faro-guard.yaml`); Alloy caps the rate,
never downloads source maps, sets `service.name` to `milesstorm-web`, keeps only INTERNAL and CLIENT
spans, drops attributes that would add service-graph nodes and strips query strings
(`monitor/alloy.yaml`); Tempo leaves browser spans out of span-metrics (`monitor/tempo.yaml`).

### Browser clock

A visitor's clock is often tens of milliseconds off the servers' (seconds on some devices). Server
spans are then drawn outside the browser span that caused them, and Grafana's critical path can't
follow a child it doesn't find inside its parent, so it stops at the browser span. Grafana, Tempo
and Faro don't correct this, so trace.js does it the way NTP does (a small piece of our own code:
there is no standard one for browser traces):

- The public Gateway stamps every response `Server-Timing: gateway;dur=<ms>;desc="<epoch ms>"`:
  when it received the request and how long until the backend answered (homelab repo,
  `istio/manifests/gateway-server-timing.yaml`).
- Only responses no cache can replay are used: the page's own navigation and `/bff/` server calls
  (POSTs). A sample whose round trip comes out negative, or whose offset is over an hour, is
  dropped as not belonging to that request. The frontend sends `Cache-Control: no-store` on
  `/bff/` responses and `private, no-cache` on rendered pages (`no_store` in
  `packages/web/src/main.rs`), so a future Cloudflare cache rule can't replay a stamp either.
  Pages don't get `no-store`: it would turn off the browser's back/forward cache.
- For each such request the browser has its own `requestStart` and `responseStart` (Resource
  Timing). With the Gateway's two times that gives the offset, accurate to within half the network
  round trip; the lowest-round-trip of the last 8 wins.
- A trace's offset is fixed when its root span starts (so all of a trace's spans move together).
  Before export, every browser span is moved by it and carries `browser.clock_offset_ms` (added to
  the browser's own time) and `browser.clock_offset_error_ms` (the most it can be off). Server
  spans then land inside their browser parent to within that error.
- A trace that starts before any sample exists (the page load usually does: the navigation entry
  is read just after) is held back by trace.js's span processor and takes the first sample that
  arrives. If its root ends first, or the page is hidden, it is sent uncorrected, without those
  attributes, and stays uncorrected: holding it longer would lose it when the tab closes, and a
  page with no sample by then usually gets none (its responses lack the stamp).

After the correction, the part of a fetch span outside the Gateway span is time between the
browser and the Gateway: the internet and Cloudflare.

## Rules for new code

| You are adding | Do this |
|---|---|
| An axum service | Layer `OtelAxumLayer::default().filter(\|p\| p != "/metrics")` and register the W3C `TraceContextPropagator` at startup. Copy `auth/src/main.rs`. |
| An HTTP client, connection pool, login token, or anything that loads TLS certificates | Create it at startup and reuse it; keep pool connections open (auth: `min_connections`, no idle timeout or max lifetime); renew tokens in the background before they expire (frontend: `Dataset::keep_signed_in`, since a SurrealDB sign-in checks a slow password hash, ~40 ms). Loading the system CA bundle costs 30–100+ ms of CPU, which otherwise shows up as unexplained own time in whichever request comes first. |
| An outbound HTTP call | frontend: `api::trace::client(Peer, timeout, \|b\| b)`; auth: `trace::client(Peer, timeout, redirect)`. Both give a CLIENT span, `traceparent`, `peer.service` and a timeout. A raw `reqwest::Client` is a clippy error ([Chokepoints](#chokepoints)). |
| A `use_server_future` or `use_loader` | Use `api::trace::use_server_future("name", f)`; add a matching `use_loader` wrapper when first needed. |
| Work a request starts but doesn't wait for | `trace::spawn_in_trace("name", fut)` (frontend, auth): a child span marked `trace.relation=follows`, so the request's span still ends on time. |
| Background work with no request (a unit of a stream, a job) | `trace::spawn("name", link, fut)`: its own trace, linked to what caused it. |
| A background loop (metrics poller, cleanup, token renewal) | `trace::spawn_loop("reason", fut)`: no span of its own, and its ticks stay on untraced clients (auth's raw `PgPool`, `RedisTracing::Off`). Every tick would otherwise start a root trace. |
| CPU work off the async threads | `trace::spawn_blocking("name", f)`: the closure runs in a span that is a child of the caller's. |
| An in-process queue | `trace::channel` / `trace::broadcast` (frontend), `trace::channel` / `trace::watch_channel` (ai_pipeline). Messages are `Carried<T>`: the sender's context travels with them and the receiver continues or links to it. |
| A database or cache client | Its calls need CLIENT spans with `peer.service` (that names the node in the service graph). Postgres in auth: the `Db` pool (sqlx-tracing), and `trace::begin` / `commit` / `rollback` for transactions. Redis in the frontend: `trace::redis_pool` / `redis_client` / `redis_subscriber`, each with an explicit `RedisTracing::{Commands, Off}`. A session store: `trace::TracedStore`. HTTP to a database (SurrealDB): `.with_extension(api::trace::DbCall { .. })`. |
| A WebSocket to another service | `api::trace::connect_ws(url, peer, timeout)`: CLIENT span and `traceparent` on the handshake; the callee extracts it as ai_pipeline's `accept_hdr_async` does. |
| A long-lived connection (WebSocket, server-sent events) | Not one span. Follow [Streams](#streams). |
| A function that does real work on a request path (business logic, token or permission checks, hashing, encoding big payloads, store calls) | `#[tracing::instrument(name = "area.verb", skip_all)]`. Add `fields(..)` only for safe, low-cardinality values (counts, flags, which branch), never secrets, codes, emails or bodies; no `err` unless the error can't hold them (sqlx and Resend errors can). CPU work in `spawn_blocking` gets its span inside the closure: `let span = info_span!("password.hash"); spawn_blocking(move \|\| span.in_scope(\|\| ..))`. INFO level, in the service's own crate (the default filters keep it). Not on getters, trivial conversions, per-item or per-frame code (ai_pipeline's inference runs per frame: at most `debug_span!`), or background loops. |

### Chokepoints

Each service has one module of traced helpers: `services/frontend/packages/api/src/trace.rs`,
`services/auth/src/auth/trace.rs`, `services/ai_pipeline/src/trace.rs`. The raw APIs behind them
(`reqwest::Client`, `tokio::spawn`, `spawn_blocking`, `std::thread::spawn`, the tokio and std
channels, the fred constructors, `connect_async`, `std::process::exit`) are banned in each
service's `clippy.toml`, so code that skips the helper fails CI. A raw use that is right carries
`#[allow(clippy::disallowed_.., reason = "..")]` and an entry in `services/<svc>/allowed_raw_io.toml`;
the `allowed_raw_io` test fails on an allow that isn't listed, a listed one that is gone, or one
without a reason.

### Errors

A span is ERROR when the work failed, and carries `error.type`, never the error's text (sqlx,
Resend and SurrealDB errors can hold addresses and values).

- frontend server functions that have no expected rejection: `#[instrument(err)]`.
- frontend server functions that answer 4xx on purpose (login, register, emails, invites):
  `trace::rejectable(..)`, which leaves a 4xx `auth_error` unmarked.
- store, Redis and database failures: `trace::failed("kind")` (frontend), `trace::fail` /
  `trace::db_failed` (auth).
- a panic in spawned work: ERROR with `error.type=panic` (auth).
- a CLIENT span is ERROR on any 4xx or 5xx, except 401 and 404 from auth's
  `/internal/token/exchange` and `/internal/token/introspect` (`trace::WRONG_TOKEN_OK`): a wrong
  or expired token is their normal answer.

### Streams

A camera session (`/ws/arcane`) and a roll stream (`/api/arcane/rolls`, server-sent events) can
stay open for hours. One span over all of that would be exported only at the end, lost on a pod
restart, and would say nothing about where time went. So:

- **Open and close.** The connection makes a short `session open` span when it is set up and a
  zero-length `session close` span when it ends (duration, counts and close reason as
  attributes). Both carry the same `session.id`, and the close span links to the open span.
  frontend says which stream in `session.kind` (`arcane.ws_session` or `arcane.rolls_stream`);
  ai_pipeline has its own pair for its side of the camera socket. Find a session with
  `{ span.session.id = "…" }`.
- **Units.** Each piece of work on the stream is its own short trace, linked to the open span
  and carrying `session.id`:
  - ai_pipeline `roll.settle`: a roll that settled, with `frame.pending`, `infer.queue`,
    `infer.decode`, `infer.yolo`, `infer.crop`, `infer.head` and `ws.send` under it. Their
    times are measured in the inference thread and written as spans afterwards, so the
    per-frame code creates no spans. `infer.crop` leaves out the crop's upload. A roll's
    `ws.send` is a PRODUCER span, started before the send: the roll message names it.
  - ai_pipeline `frame.infer`: the same breakdown for one frame in `FRAME_TRACE_EVERY`
    (default 10) that settled nothing, and for a failed frame (the first, then at most one
    per 10 s).
  - frontend `roll receive` (CONSUMER, the roll arriving from ai_pipeline), under it
    `roll publish` (PRODUCER, to Redis) and `roll capture` → `arcane.capture` (camera side),
    `roll deliver` (CONSUMER, to one browser's stream), `arcane.rolls_replay` (rolls sent on
    connect) and `permission recheck` (once a minute per stream).
- **A roll is one trace across services.** ai_pipeline adds a top-level
  `"_trace": {"traceparent": "…"}` to the roll message, naming its `ws.send` span. The
  frontend's `roll receive` is that span's child (send, then process: the pair gives the
  service graph its ai-pipeline → frontend edge). The frontend passes the trace through Redis
  (adding `published_ms`) to the other replicas, and cuts `_trace` out before the browser
  gets the message: the rest is ai_pipeline's text byte for byte. A roll delivered more than 10 s after it was published
  (a replay) starts a new trace that links back instead.
- **Drops.** When a slow reader skips messages, the next unit (or the close span) gets a
  `dropped` event with `dropped.count`.

Browsers can't set headers on a WebSocket. The browser's own `camera session` span (the
handshake, ending at the socket's `open`; a child of the click that opened it, else a root)
sends its context as `/ws/arcane?traceparent=…`, and the frontend's `session open` span links
to it: the Gateway's span for the handshake is its parent already.

### Shutdown

On SIGTERM each service stops accepting, waits for open requests and tracked tasks (frontend
10 s, auth 5 s), then flushes the exporter, so the last spans of a rollout aren't lost. The
frontend does this in release builds only: a debug build runs `dioxus::serve` for `dx serve`'s
hot reload and doesn't drain.

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
  so nothing is propagated. At plain `info`, library spans with no request around them (Dioxus
  signals, tower-sessions, axum-login) become thousands of one-span traces. The defaults in the
  frontend's and auth's `main.rs` keep the services' own logs at `info` and those libraries at
  `warn` (ai_pipeline's libraries make no such spans); the deployments don't set `RUST_LOG`. Targets match by prefix: `sqlx=warn` also hides `sqlx_tracing`,
  so auth adds `sqlx_tracing=info`, and `sqlx::pool::acquire=info` for the event behind `db.pool.acquire` (a wait for a pool connection; the event itself is kept out of the logs and off spans unless the wait was 2 s or more). `opentelemetry=warn` keeps the OTel SDK's own logs to its
  warnings (spans dropped, an export failed); they go to stdout only, never through the OTLP log
  bridge, which would feed them back to itself.

## Profiles

CPU profiles of frontend, auth and ai-pipeline are collected all the time by an eBPF profiler
(homelab: Alloy `pyroscope.ebpf` → Pyroscope); the code needs nothing. Use them when a span's own
time is large and no child span explains it: Explore → Pyroscope, `service_name` = the span's
`service.name`, over the trace's time range. Spans have no "Profiles" link: Grafana only links spans
that carry `pyroscope.profile.id`, which eBPF profiles can't provide. Keep the binaries' symbol tables
(`strip --strip-debug`, never a full `strip`), or flame graphs show addresses. A block of
`{unknown}` with no stack under it comes from a process's first minute (or the profiler's):
the profiler hasn't read that binary yet. Narrow the time range past it or filter by `pod`.

## Checks

`.github/workflows/checks.yml` runs clippy (`-D warnings`) and the tests of all three services
on every push and pull request. The frontend and auth tests start Redis and Postgres with
testcontainers, so they need Docker (auth: or `TEST_DATABASE_URL`). The same locally:

```sh
cd services/frontend
npm install && npm run vendor                 # asset! needs the vendored Faro bundles
node_modules/.bin/tailwindcss -i packages/ui/tailwind.css -o packages/ui/assets/styling/tailwind.css
cargo clippy -p api -p web --features web/server --all-targets -- -D warnings
cargo test -p api -p web --features web/server
npm run test:trace                            # browser script
cd ../auth && cargo clippy --all-targets -- -D warnings && cargo test
cd ../ai_pipeline && cargo clippy --all-targets -- -D warnings && cargo test   # no GPU needed
```

## Upgrading OpenTelemetry

The frontend pins all OTel crates in `services/frontend/Cargo.toml` (`[workspace.dependencies]`).
auth and ai_pipeline pin their own copies. Upgrade them together:

1. Bump `opentelemetry`, `opentelemetry_sdk`, `opentelemetry-otlp`, `opentelemetry-appender-tracing`,
   `tracing-opentelemetry` and `axum-tracing-opentelemetry` together. Check each crate's changelog
   for the version that pairs with your `opentelemetry` version.
2. Set reqwest-tracing's feature to `opentelemetry_0_NN` for the same `opentelemetry` version.
   reqwest-tracing 0.7.1 goes up to `opentelemetry_0_32`.
3. Run every service's tests (see Checks). If the versions are mismatched, the build still passes
   but propagation silently stops, and the trace tests are what catch it: each service has one
   that sends `traceparent` across a real hop (frontend `server_continues_the_clients_trace`, auth
   `traceparent_continues_the_trace_across_a_hop`, ai_pipeline
   `handshake_traceparent_continues_the_trace`), and `log_records_carry_the_span_and_skip_sdk_logs`
   checks that OTLP log records carry the span's trace_id/span_id.

## Upgrading Faro

Bump `@grafana/faro-web-sdk` and `@grafana/faro-web-tracing` together (exact versions in
`services/frontend/package.json`). `trace.js` depends on these Faro surfaces:

- `faro.api.getOTEL()` (the standard `@opentelemetry/api` `trace` and `context`), and Faro patching
  `window.fetch` while `initializeFaro` runs, before `trace.js` wraps it;
- `TracingInstrumentation`'s `spanProcessor` option, with `FaroMetaAttributesSpanProcessor` and
  `FaroTraceExporter` from the tracing bundle and the live `GrafanaFaroWebSdk.faro` instance
  (`trace.js` rebuilds Faro's default chain around its own processor. The Faro bundle doesn't
  export OTel's `BatchSpanProcessor`, so `npm run vendor` builds `otel-batch.iife.js` from
  `@opentelemetry/sdk-trace-web`; keep that package on the version Faro depends on);
- `FetchTransport`'s `requestOptions.headers` (the unsampled `traceparent` on uploads);
- the fetch instrumentation's `requestHook`, reading `http.request.method` and `url.full` from the
  span to name it;
- the fetch instrumentation's `ignoreNetworkEvents` (Faro defaults it to `true`; `trace.js` sets
  `false` so fetch spans carry OTel's DNS/connect/TLS/request/response timing events);
- the instrumentation class names on the `GrafanaFaroWebSdk` and `GrafanaFaroWebTracing` globals.

After a bump, run `npm run test:trace`, then check on the site that `/faro/collect` uploads carry a
`traceparent` ending in `-00`, and in Tempo that a click still roots its trace and fetch spans are
named like `POST /bff/login_password`.

## Checking traces in Tempo

- **Clicks root their trace.** `{ resource.service.name = "milesstorm-web" && name =~ "click.*|submit.*" }`:
  the trace should reach `frontend`, and `auth` when the click needed it.
- **Page loads have one root.** `{ resource.service.name = "frontend" && name =~ "bff.*" }`: the root
  service should be `public-istio.istio-ingress` or `milesstorm-web`, never `frontend` or `auth`.
- **Service graph.** In VictoriaMetrics,
  `sum by (client, server) (increase(traces_service_graph_request_total[15m]))` should show
  milesstorm-web → public-istio.istio-ingress → frontend → waypoint.auth → auth, and auth → postgres,
  frontend → redis, frontend → surrealdb. An edge from `user` means a SERVER span had no parent:
  metrics scrapes are filtered out, so look for whatever else is calling.
- **No orphans.** The trace view shows no "root span not yet received". Expected exceptions:
  - Gateway and waypoint spans while Tempo restarts: Istio sends them to Tempo directly, without
    Alloy's retry, so a trace from that minute misses its Envoy spans;
  - browsers that never send their spans (an ad blocker blocks `/faro/collect`, or the tab closed
    within Faro's 1 s batch): the request still carries the browser's `traceparent`, so the
    `milesstorm-web` root is missing.

One-time account and invite links use fragments (`/reset-password#<code>`,
`/verify-email#<code>`, `/delete-account#<code>`, `/invite#<code>`). Dioxus reads the
fragment in the browser after hydration and sends the code in the server-function
body. It never reaches the gateway or frontend as part of the HTTP URL. Faro also
scrubs fragments from browser telemetry. Auth and frontend must be deployed
together for this link format. Previously issued query-string links need to be
reissued (or converted from `?code=<code>` to `#<code>`); already stored traces
are unaffected. OAuth callbacks still use query strings.

## Not covered yet

- **A dropped transaction.** auth's `trace::rollback` has a span; a transaction dropped on an
  early return is rolled back by sqlx with none.
- **surrealkit.** The schema migration at frontend startup uses surrealkit's own client; its two
  calls have plain CLIENT spans without `db.operation.name`.
- **Redis subscriber and schema lock.** Their connections are `RedisTracing::Off`: a subscriber
  has no commands per request, and the lock is taken once at startup.
- **Shared telemetry crate.** Each service has its own telemetry setup. A shared crate becomes
  worthwhile with a fourth service or at the next OTel upgrade. It needs the CI Docker build context
  changed from `services/<svc>` to `services/`.
