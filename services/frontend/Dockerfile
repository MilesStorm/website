# syntax=docker/dockerfile:1.7
# Four stages: `web` (the Astro pages) and `bff` (the server) build in parallel, `verify` checks
# they agree, and the runtime takes both and has no Node. Until the Dioxus app is deleted, `bff`
# is `dx bundle`, which builds the server together with the Dioxus pages.
#
# No cargo-chef: `dx bundle` builds with its own profiles and features, so a `cargo chef cook`
# layer was never reused (it compiled every dependency a second time). Caching comes from
# building on eta's own Docker, which keeps unchanged layers between CI runs.

# ---- web: the Astro build. Node is here only. ----
FROM node:22.23.3-trixie-slim@sha256:154ba2f4d6fec323d28e4f4bb86bba4677f1223391a1979cf521304e03a98dfa AS web
ENV ASTRO_TELEMETRY_DISABLED=1
WORKDIR /app/web
COPY web/package.json web/package-lock.json ./
RUN npm ci
COPY web/ ./
# The tracing script is trace.js plus the Faro bundles (scripts/vendor.mjs); trace.js lives with
# the Dioxus app until that is deleted.
COPY packages/web/assets/trace.js /app/packages/web/assets/trace.js
# The TypeScript types must be the ones generated from the committed OpenAPI document (`verify`
# checks that document against the server), and the pages must type-check against them.
RUN cp src/api/schema.d.ts /tmp/schema.d.ts && \
    npm run api:types && \
    cmp src/api/schema.d.ts /tmp/schema.d.ts && \
    npm run vendor && \
    npx astro check && \
    npm run build

# ---- bff: the server (and, for now, the Dioxus pages) ----
FROM rust:1-trixie AS bff
# clang: required by ring (via jsonwebtoken) for wasm32
# nodejs: required to run tailwindcss + daisyui to generate CSS before dx bundle
RUN apt-get update && \
    apt-get install -y --no-install-recommends clang curl ca-certificates build-essential && \
    curl -fsSL https://deb.nodesource.com/setup_22.x | bash - && \
    apt-get install -y --no-install-recommends nodejs && \
    rm -rf /var/lib/apt/lists/*
WORKDIR /app

# Install dioxus-cli
RUN curl -L --proto '=https' --tlsv1.2 -sSf https://raw.githubusercontent.com/cargo-bins/cargo-binstall/main/install-from-binstall-release.sh | bash
RUN cargo binstall dioxus-cli --root /.cargo -y --force
ENV PATH="/.cargo/bin:$PATH"

# wasm32 target is needed by dx bundle for the WASM client binary
RUN rustup target add wasm32-unknown-unknown

# Not web/: a change to the Astro pages rebuilds no Rust.
COPY Cargo.toml Cargo.lock package.json package-lock.json clippy.toml ./
COPY scripts/ scripts/
COPY surreal/ surreal/
COPY crates/ crates/
COPY packages/ packages/

# Generate Tailwind CSS and vendor the Faro bundles before dx bundle — the asset! macro checks
# file existence at compile time
RUN npm ci && \
    npm run vendor && \
    node_modules/.bin/tailwindcss -i packages/ui/tailwind.css -o packages/ui/assets/styling/tailwind.css --minify

# Run dx bundle from the package directory so Dioxus.toml is picked up
WORKDIR /app/packages/web
# Cache folders kept on the build machine (eta) between builds: downloaded crates
# and compiled code, so a code change only recompiles what changed. They are not
# part of the image; `dist/` (outside target/) is what the next stages copy.
RUN --mount=type=cache,id=frontend-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=frontend-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=frontend-target,target=/app/target,sharing=locked \
    dx bundle --platform web --release --debug-symbols=false

# ---- verify: the server and the pages agree ----
FROM debian:trixie-slim AS verify
RUN apt-get update && \
    apt-get install -y --no-install-recommends openssl libssl3 ca-certificates libc6 && \
    rm -rf /var/lib/apt/lists/*
WORKDIR /verify
COPY --from=bff /app/packages/web/dist/server ./server
COPY --from=web /app/web/dist ./dist
COPY web/src/api/openapi.json ./openapi.json
# The committed OpenAPI document (the TypeScript types' source, checked in `web`) is what the
# server says, and the build has exactly the pages the server serves.
RUN ./server openapi | diff - openapi.json && \
    ./server check-pages dist && \
    touch /verified

FROM debian:trixie-slim AS runtime
# The commit this image is built from (CI passes github.sha): `service.version` on traces and
# logs. Declared in this stage only, so a new commit doesn't invalidate the build cache.
ARG GIT_SHA=unknown
ENV GIT_SHA=$GIT_SHA
RUN apt-get update && \
    apt-get install -y --no-install-recommends openssl libssl3 ca-certificates libc6 && \
    rm -rf /var/lib/apt/lists/*

RUN useradd -m -d /home/appuser appuser

WORKDIR /app
# BuildKit skips a stage nothing copies from: this makes `verify` run.
COPY --from=verify /verified /app/.verified
# The server binary and the Dioxus bundle it serves (out_dir = "dist" in Dioxus.toml puts it
# under packages/web/dist/), and the Astro pages, served with FRONTEND_UI=astro.
COPY --from=bff /app/packages/web/dist/server ./server
COPY --from=bff /app/packages/web/dist/public ./public
COPY --from=web /app/web/dist ./web

USER appuser

ENV IP=0.0.0.0
ENV PORT=80
ENV WEB_DIST=/app/web

EXPOSE 80
CMD ["/app/server"]
