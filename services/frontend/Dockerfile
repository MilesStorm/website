# syntax=docker/dockerfile:1.7
# Four stages: `web` (the Astro pages) and `bff` (the server) build in parallel, `verify` checks
# they agree, and the runtime takes both and has no Node. Caching comes from building on eta's
# own Docker, which keeps unchanged layers and the cargo cache mounts between CI runs.

# ---- web: the Astro build. Node is here only. ----
FROM node:22.23.3-trixie-slim@sha256:154ba2f4d6fec323d28e4f4bb86bba4677f1223391a1979cf521304e03a98dfa AS web
ENV ASTRO_TELEMETRY_DISABLED=1
WORKDIR /app/web
COPY web/package.json web/package-lock.json ./
RUN npm ci
COPY web/ ./
# The TypeScript types must be the ones generated from the committed OpenAPI document (`verify`
# checks that document against the server), and the pages must type-check against them.
RUN cp src/api/schema.d.ts /tmp/schema.d.ts && \
    npm run api:types && \
    cmp src/api/schema.d.ts /tmp/schema.d.ts && \
    npm run vendor && \
    npx astro check && \
    npm run build

# ---- bff: the server ----
FROM rust:1-trixie AS bff
WORKDIR /app
# Not web/: a change to the Astro pages rebuilds no Rust.
COPY Cargo.toml Cargo.lock clippy.toml ./
COPY surreal/ surreal/
COPY crates/ crates/
# Cache folders kept on the build machine (eta) between builds: downloaded crates and compiled
# code, so a code change only recompiles what changed. They are not part of the image, so the
# binary is copied out of target/.
RUN --mount=type=cache,id=frontend-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=frontend-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=frontend-target,target=/app/target,sharing=locked \
    cargo build --release --locked -p bff --bin server && \
    cp target/release/server /app/server

# ---- verify: the server and the pages agree ----
FROM debian:trixie-slim AS verify
RUN apt-get update && \
    apt-get install -y --no-install-recommends openssl libssl3 ca-certificates libc6 && \
    rm -rf /var/lib/apt/lists/*
WORKDIR /verify
COPY --from=bff /app/server ./server
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
# The server binary and the Astro pages it serves.
COPY --from=bff /app/server ./server
COPY --from=web /app/web/dist ./web

USER appuser

ENV IP=0.0.0.0
ENV PORT=80
ENV WEB_DIST=/app/web

EXPOSE 80
CMD ["/app/server"]
