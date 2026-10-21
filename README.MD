# MilesStorm

My personal website and a home for the projects I use with friends, built in Rust. The main project is **Arcane**, a camera-based dice reader that sends real tabletop rolls to the website, a browser extension, and Roll20.

[Visit milesstorm.com](https://milesstorm.com) · [Install the Firefox extension](https://github.com/MilesStorm/website/releases/download/arcane-dice/arcane-dice.xpi) · [Report an issue](https://github.com/MilesStorm/website/issues)

## What's here

- **Arcane dice reader:** stream a camera from the browser, detect dice with YOLO, read their values with a ResNet18 head, and publish settled rolls across devices. Uncertain readings appear as `?` rather than being guessed.
- **Browser extension:** see the latest roll and send readable rolls to Roll20 chat. Firefox has a signed release with automatic updates; the Chrome loading path is available but has not been tested.
- **Accounts and profiles:** password, GitHub, and Google login; display names and profile pictures; email verification, password reset, and account deletion.
- **Permissions and invitations:** role-based access to features, with an admin panel for users, roles, permissions, and invite links. Arcane access requires the `arcane` permission.
- **Dice feedback:** opt in to sharing roll images, flag incorrect readings with corrections, and delete shared pictures from the profile page.

The repository contains application code, container builds, and Kubernetes workload manifests. Shared cluster infrastructure is maintained separately.

## Architecture

```mermaid
flowchart LR
    Browser["Browser: Astro pages + camera"] --> Frontend["Frontend / BFF"]
    Frontend --> Auth["Auth: Axum"]
    Auth --> Postgres["PostgreSQL"]
    Frontend --> Redis["Redis: sessions + live rolls"]
    Frontend --> Surreal["SurrealDB: pictures + shared rolls"]
    Frontend -->|"Camera frames over WebSocket"| AI["Dice inference: Rust / Burn / CUDA"]
    AI -->|"Predictions + settled rolls"| Frontend
    Extension["Browser extension"] -->|"Authenticated roll stream"| Frontend
    Extension --> Roll20["Roll20 chat"]
```

| Component | Responsibilities | Source |
| --- | --- | --- |
| Frontend | Astro pages with React islands (shadcn/ui), built to static HTML | [services/frontend/web](services/frontend/web/) |
| BFF | Rust (Axum) server: serves the pages with the viewer spliced in, the typed JSON API under `/api/*`, OAuth, camera and roll streams; delegates authentication and protected actions to auth | [services/frontend/crates/bff](services/frontend/crates/bff/) |
| Auth | Credentials, OAuth, account lifecycle, roles, invitations, SQLx migrations | [services/auth](services/auth/) |
| Dice inference | YOLO detection, ResNet18 value classification, and temporal roll settling; Rust/Burn serving with Python training tools | [services/ai_pipeline](services/ai_pipeline/) |
| Extension | Roll display, stream handling, and Roll20 integration | [extension/arcane-dice](extension/arcane-dice/README.md) |

The frontend acts as a backend for frontend (BFF). The browser receives a session cookie, and the BFF keeps the auth service's opaque token in the server-side Redis session and uses the internal auth channel for protected operations. JWTs used between services are not exposed to the browser.

Redis also distributes live rolls between frontend replicas and replays the most recent roll to a newly connected extension. SurrealDB stores profile pictures and shared or flagged roll images; its schema is embedded in the frontend and applied at startup. PostgreSQL migrations run when auth starts.

Auth and the frontend expose Prometheus-format metrics. All three services produce structured logs and OpenTelemetry traces. Browser instrumentation uses Grafana Faro; see [TRACING.md](TRACING.md) for propagation, sampling, and telemetry handling.

## Run the web application locally

You need Rust with edition 2024 support, Node.js 22.22 or later, Docker (or your own PostgreSQL and Redis), and on Linux a C toolchain, pkg-config and the OpenSSL development libraries. GPU inference and SurrealDB are only needed for their features: without them you can develop the pages, the accounts and the password flows. Without SurrealDB, roll sharing, flagging and profile pictures are off.

Three processes run side by side: auth, the Rust server (the BFF, which also serves the built pages), and Astro's dev server for the pages you are editing.

### 1. Start PostgreSQL and Redis

```sh
docker run -d --rm --name milesstorm-dev-postgres -p 127.0.0.1:5432:5432 -e POSTGRES_USER=website -e POSTGRES_PASSWORD=website-dev -e POSTGRES_DB=website postgres:17

docker run -d --rm --name milesstorm-dev-redis -p 127.0.0.1:6379:6379 redis:7
```

These example credentials are for local development. The containers have no volumes, so their data goes when they are removed.

### 2. Configure auth and the server

Create `services/auth/.env`:

```dotenv
DATABASE_URL=postgres://website:website-dev@127.0.0.1:5432/website
SERVER_IP=127.0.0.1
SERVER_PORT=7070
BFF_CALLBACK_URL=http://localhost:4321
SITE_URL=http://localhost:4321
BFF_SERVICE_SECRET=replace-with-a-shared-local-secret
JWT_SECRET=replace-with-an-independent-local-secret
CLIENT_ID=unused-local
CLIENT_SECRET=unused-local
G_CLIENT_ID=unused-local
G_CLIENT_SECRET=unused-local
```

Auth requires both OAuth providers' variables at startup. The placeholders are enough for password registration and login; put in your own provider credentials to try OAuth. `SITE_URL` (the links in emails) and `BFF_CALLBACK_URL` (where a provider sends the browser back) point at Astro's dev server; use `http://localhost:8080` while you work on the built pages instead (step 5). Generate the two secrets separately, for example with `openssl rand -hex 32`.

Create `services/frontend/.env` (the server reads `.env` from the directory it runs in):

```dotenv
BFF_SERVICE_SECRET=replace-with-a-shared-local-secret
AUTH_SERVICE_URL=http://127.0.0.1:7070
PUBLIC_AUTH_URL=http://localhost:7070
REDIS_HOST=127.0.0.1
REDIS_PORT=6379
```

Set **exactly the same** `BFF_SERVICE_SECRET` in both files. The `.env` files are ignored by Git, and debug builds of both services require one. Debug builds use cookies without `Secure` and accept plain-http pages from their own host, so no HTTPS is needed locally. Email is optional: without `RESEND_API_KEY`, debug auth logs each email and its link instead of sending it. The full reference is [ENV.md](ENV.md); account email settings are in [services/auth/README.md](services/auth/README.md).

### 3. Build the pages once

The server refuses to start without a complete Astro build in `services/frontend/web/dist`, since it serves exactly the pages in its `PAGES` table:

```sh
cd services/frontend/web
npm ci
npm run vendor   # the browser tracing script (src/vendor/tracing.js), which every page loads
npm run build
```

### 4. Start auth, the server and Astro's dev server

In separate terminals, from the repository root:

```sh
cd services/auth
cargo run --locked
```

```sh
cd services/frontend
cargo run --locked -p bff --bin server   # http://localhost:8080
```

```sh
cd services/frontend/web
npm run dev                              # http://localhost:4321
```

Open `http://localhost:4321`. Astro reloads a page as you edit it, and its dev server passes `/api`, `/oauth` and `/ws` on to the Rust server (`BFF_URL`, default `http://localhost:8080`), keeping the page's host so the server's same-origin check passes as it does in production. Register a password account to try the account flows; features behind roles need the matching permissions.

What the dev server's pages don't have, compared with production:

- **No splice.** Astro serves the page as written, without the viewer and permissions the Rust server puts into each page (`data-session`/`data-perms` on `<html>` and the bootstrap JSON). The page asks `GET /api/session` instead once it has loaded, so logged-in pages work, but each one first paints as logged out and then switches.
- **No page guards.** Every page is served to anyone: a logged-out visitor gets `/profile` instead of a redirect to `/login`, and a page you lack the permission for shows its islands' "You don't have access" errors instead of the 403 page. The API still checks every call.
- **No trace context.** Pages carry no `traceparent` or `service-version` (Faro only starts on milesstorm.com and staging anyway).

### 5. The full-fidelity loop

To see exactly what production serves (the splice, the guards, the 403/503 and 404 pages, the cache headers), build the pages and use the Rust server directly:

```sh
cd services/frontend/web
npm run build
```

Then restart the server (it reads the build once at startup) and open `http://localhost:8080`.

### Add dice inference or picture storage

The inference service needs an NVIDIA GPU, the CUDA runtime/toolchain, and the model weights expected by [its Dockerfile](services/ai_pipeline/Dockerfile). The frontend connects through `AI_PIPELINE_SERVICE_URL`, which defaults to `ws://localhost:9000`. Camera access requires HTTPS or a browser-recognized local origin.

For SurrealDB logins, schema changes, picture storage, and database tests, follow [services/frontend/surreal/README.md](services/frontend/surreal/README.md). [DICE-SETUP.md](DICE-SETUP.md) contains deployment and training-data workflow notes, while the [AI roadmap](services/ai_pipeline/ROADMAP.md) covers model evaluation and future work.

## Checks

The Rust services have separate Cargo roots:

```sh
# From services/auth
cargo test --locked

# From services/frontend (the Redis tests need Docker)
cargo clippy --all-targets -- -D warnings
cargo test --locked

# From services/frontend/web (the Astro pages; Node 22.22 or later)
npm ci && npm run vendor
npm run api        # after an API change: the OpenAPI document and src/api/schema.d.ts
npm run check      # types, against src/api/schema.d.ts
npm test           # component tests (Vitest)
npm run test:trace # the browser tracing script
npm run build      # dist/, which the server serves
# The built image in a browser (Docker): build it as `frontend:e2e`, then
e2e/serve.sh && npx playwright test; e2e/serve.sh stop

# From extension/arcane-dice
node --test "test/*.test.js"
```

These cover account helpers, server-side helpers, roll handling, browser telemetry, and the extension. SurrealDB integration tests and GPU/model evaluation have additional requirements documented alongside those components.

## Delivery and repository layout

GitHub Actions builds the frontend, auth, and AI service images into GHCR when their paths change on `main`. The deployment environment uses Argo CD and Image Updater to reconcile those images. Extension releases follow their own workflow: a manifest version bump triggers tests, Mozilla signing, and publication of the Firefox package and update manifest.

| Path | Contents |
| --- | --- |
| [services/frontend/](services/frontend/) | The website: the Astro pages (`web/`), the Rust server and its API types (`crates/`), and the SurrealDB schema |
| [services/auth/](services/auth/) | Auth server and PostgreSQL migrations |
| [services/ai_pipeline/](services/ai_pipeline/) | Dice inference, training tools, and model work |
| [extension/arcane-dice/](extension/arcane-dice/) | Browser extension, tests, and release configuration |
| [crds/](crds/) | Kubernetes deployments, services, and routing for the application |
| [.github/workflows/](.github/workflows/) | Container builds and extension release automation |

Astro's files under `/_astro/` are named by a hash of their content, and the frontend sends them `public, max-age=31536000, immutable`. Cloudflare caches them through a cache rule made by hand in its dashboard (there is no Cloudflare configuration in code): URI path starts with `/_astro/`, eligible for cache, edge TTL from the origin's header. Pages are per request (`private, no-cache`) and never cached at the edge. So a deploy purges nothing: the pages aren't in Cloudflare's cache, and a changed asset has a new name.

For a change, start in the relevant component, run its checks, and open a pull request. Include what changed, how it was verified, and any environment or migration requirements.
