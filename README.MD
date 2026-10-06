# MilesStorm

My personal website and a home for the projects I use with friends, built in Rust. The main project is **Arcane**, a camera-based dice reader that sends real tabletop rolls to the website, a browser extension, and Roll20.

[Visit milesstorm.com](https://milesstorm.com) · [Install the Firefox extension](https://github.com/MilesStorm/website/releases/download/arcane-dice/arcane-dice.xpi) · [Report an issue](https://github.com/MilesStorm/website/issues)

## What's here

- **Arcane dice reader:** stream a camera from the browser, detect dice with YOLO, read their values with a ResNet18 head, and publish settled rolls across devices. Uncertain readings appear as `?` rather than being guessed.
- **Browser extension:** see the latest roll and send readable rolls to Roll20 chat. Firefox has a signed release with automatic updates; the Chrome loading path is available but has not been tested.
- **Accounts and profiles:** password, GitHub, and Google login; display names and profile pictures; email verification, password reset, and account deletion.
- **Permissions and invitations:** role-based access to features, with an admin panel for users, roles, permissions, and invite links. Arcane access requires the `arcane` permission.
- **Dice feedback:** opt in to sharing roll images, flag incorrect readings with corrections, and delete shared pictures from the profile page.

The repository contains application code, container builds, and Kubernetes workload manifests. Shared cluster infrastructure is maintained separately. The deployed web application is the main target; desktop and mobile crates also exist in the frontend workspace.

## Architecture

```mermaid
flowchart LR
    Browser["Browser: Dioxus UI + camera"] --> Frontend["Frontend / BFF"]
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
| Frontend | Dioxus UI, server rendering, Axum routes, account pages, camera and roll streams | [services/frontend](services/frontend/) |
| BFF API | Server functions that delegate authentication and protected actions to auth | [packages/api](services/frontend/packages/api/) |
| Auth | Credentials, OAuth, account lifecycle, roles, invitations, SQLx migrations | [services/auth](services/auth/) |
| Dice inference | YOLO detection, ResNet18 value classification, and temporal roll settling; Rust/Burn serving with Python training tools | [services/ai_pipeline](services/ai_pipeline/) |
| Extension | Roll display, stream handling, and Roll20 integration | [extension/arcane-dice](extension/arcane-dice/README.md) |

The frontend acts as a backend for frontend (BFF). The browser receives a session cookie, and the BFF keeps the auth service's opaque token in the server-side Redis session and uses the internal auth channel for protected operations. JWTs used between services are not exposed to the browser.

Redis also distributes live rolls between frontend replicas and replays the most recent roll to a newly connected extension. SurrealDB stores profile pictures and shared or flagged roll images; its schema is embedded in the frontend and applied at startup. PostgreSQL migrations run when auth starts.

Auth and the frontend expose Prometheus-format metrics. All three services produce structured logs and OpenTelemetry traces. Browser instrumentation uses Grafana Faro; see [TRACING.md](TRACING.md) for propagation, sampling, and telemetry handling.

## Run the web application locally

For the website and accounts, you need Rust with edition 2024 support, the `wasm32-unknown-unknown` target, a Dioxus CLI matching the workspace's 0.7 series, Node.js 22+, PostgreSQL, and Redis. Linux builds also need a C toolchain, pkg-config, and OpenSSL development libraries for auth's database/client dependencies.

GPU inference and SurrealDB are additional dependencies for their respective features. You can develop the main website and password flows without running dice inference. Without SurrealDB configuration, roll sharing and flagging are disabled, and profile picture storage is unavailable.

### 1. Clone and prepare frontend assets

```sh
git clone https://github.com/MilesStorm/website.git
cd website
rustup target add wasm32-unknown-unknown

cd services/frontend
npm ci
npm run vendor
npx --no-install tailwindcss -i packages/ui/tailwind.css -o packages/ui/assets/styling/tailwind.css --minify
```

Generate the CSS and vendor assets before compiling: Rust's `asset!` macro checks that they exist.

### 2. Start PostgreSQL and Redis

Use existing local instances, or start disposable development containers:

```sh
docker run -d --rm --name milesstorm-dev-postgres -p 127.0.0.1:5432:5432 -e POSTGRES_USER=website -e POSTGRES_PASSWORD=website-dev -e POSTGRES_DB=website postgres:17

docker run -d --rm --name milesstorm-dev-redis -p 127.0.0.1:6379:6379 redis:7
```

These example credentials are for local development. The containers have no persistent volumes, so their data is disposable when the containers are removed.

### 3. Configure the two services

Create `services/auth/.env`:

```dotenv
DATABASE_URL=postgres://website:website-dev@127.0.0.1:5432/website
SERVER_IP=127.0.0.1
SERVER_PORT=7070
BFF_CALLBACK_URL=http://localhost:8080
SITE_URL=http://localhost:8080
BFF_SERVICE_SECRET=replace-with-a-shared-local-secret
JWT_SECRET=replace-with-an-independent-local-secret
CLIENT_ID=unused-local
CLIENT_SECRET=unused-local
G_CLIENT_ID=unused-local
G_CLIENT_SECRET=unused-local
```

Auth requires both OAuth providers' variables at startup. The placeholders let you work on password registration/login; replace them with your own provider credentials to test OAuth. Generate the two local secrets separately, for example with `openssl rand -hex 32`.

Create `services/frontend/packages/web/.env`:

```dotenv
BFF_SERVICE_SECRET=replace-with-a-shared-local-secret
AUTH_SERVICE_URL=http://127.0.0.1:7070
PUBLIC_AUTH_URL=http://localhost:7070
REDIS_HOST=127.0.0.1
REDIS_PORT=6379
ARCANE_ALLOWED_ORIGINS=http://localhost:8080,http://127.0.0.1:8080
```

Set **exactly the same** `BFF_SERVICE_SECRET` in both files. The `.env` files are ignored by Git. Both debug-mode services expect a `.env` file; their local cookies support HTTP. Email delivery is optional: without `RESEND_API_KEY`, debug auth logs the generated emails and links instead of sending them.

The complete configuration reference is [ENV.md](ENV.md), with account email settings in [services/auth/README.md](services/auth/README.md).

### 4. Start auth and the frontend

From the repository root, in separate terminals:

```sh
cd services/auth
cargo run --locked
```

```sh
cd services/frontend/packages/web
dx serve --platform web
```

Open `http://localhost:8080`. Auth listens on port 7070. Register a password account to try the account flows; features governed by roles require the corresponding permissions.

### Add dice inference or picture storage

The inference service needs an NVIDIA GPU, the CUDA runtime/toolchain, and the model weights expected by [its Dockerfile](services/ai_pipeline/Dockerfile). The frontend connects through `AI_PIPELINE_SERVICE_URL`, which defaults to `ws://localhost:9000`. Camera access requires HTTPS or a browser-recognized local origin.

For SurrealDB logins, schema changes, picture storage, and database tests, follow [services/frontend/surreal/README.md](services/frontend/surreal/README.md). [DICE-SETUP.md](DICE-SETUP.md) contains deployment and training-data workflow notes, while the [AI roadmap](services/ai_pipeline/ROADMAP.md) covers model evaluation and future work.

## Checks

The Rust services have separate Cargo roots. After preparing the frontend assets:

```sh
# From services/auth
cargo test --locked

# From services/frontend
cargo test -p api --features server,dioxus/router --lib --locked
cargo test -p web --features server --bin web --locked
cargo check -p web --features web --target wasm32-unknown-unknown --locked
npm run test:trace

# From extension/arcane-dice
node --test "test/*.test.js"
```

These cover account helpers, server-side helpers, roll handling, browser telemetry, and the extension. SurrealDB integration tests and GPU/model evaluation have additional requirements documented alongside those components.

## Delivery and repository layout

GitHub Actions builds the frontend, auth, and AI service images into GHCR when their paths change on `main`. The deployment environment uses Argo CD and Image Updater to reconcile those images. Extension releases follow their own workflow: a manifest version bump triggers tests, Mozilla signing, and publication of the Firefox package and update manifest.

| Path | Contents |
| --- | --- |
| [services/frontend/](services/frontend/) | Web application, shared UI/API crates, desktop/mobile entry points, and SurrealDB schema |
| [services/auth/](services/auth/) | Auth server and PostgreSQL migrations |
| [services/ai_pipeline/](services/ai_pipeline/) | Dice inference, training tools, and model work |
| [extension/arcane-dice/](extension/arcane-dice/) | Browser extension, tests, and release configuration |
| [crds/](crds/) | Kubernetes deployments, services, and routing for the application |
| [.github/workflows/](.github/workflows/) | Container builds and extension release automation |

For a change, start in the relevant component, run its checks, and open a pull request. Include what changed, how it was verified, and any environment or migration requirements.
