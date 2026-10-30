# syntax=docker/dockerfile:1.7
# `web` (the Astro pages) and `bff` (the server) build in parallel, `verify` checks they agree,
# and the runtime takes both and has no Node. Caching is by layer only: the server's dependencies
# are one layer (cargo-chef), rebuilt only when Cargo.lock or a manifest changes.

# ---- web: the Astro build. Node is here only. ----
FROM node:26.13.0-trixie-slim@sha256:e4db4c914bf8ad241bd24804fc25d272d89bad97e68cdc8a1414862790a02c39 AS web
ENV ASTRO_TELEMETRY_DISABLED=1
WORKDIR /app/web
COPY web/package.json web/package-lock.json ./
RUN npm ci
COPY web/ ./
# The TypeScript types must be the ones generated from the committed OpenAPI document (`verify`
# checks that document against the server). That the pages type-check against them is CI's
# frontend check (`npm run check`), not the image's.
RUN cp src/api/schema.d.ts /tmp/schema.d.ts && \
    npm run api:types && \
    cmp src/api/schema.d.ts /tmp/schema.d.ts && \
    npm run tracing && \
    npm run build

# ---- bff: the server ----
# cargo-chef: the dependencies compile in a layer of their own, keyed by the recipe (Cargo.lock
# and the manifests), so a code change compiles only this workspace's crates. A layer is reused
# only when its inputs are identical, so a build always compiles its own commit's code, and no
# build waits for another (no shared cache folder).
FROM lukemathwalker/cargo-chef:0.1.78-rust-1-trixie AS chef
WORKDIR /app
# The cluster's compile cache (.github/actions/rust), when the build is given it: sccache in front
# of rustc, so when a layer has to be rebuilt, this build on either node's BuildKit reuses what an
# earlier one compiled. Entries are keyed by the hash of each compile's inputs, never by file
# times. Without SCCACHE_WEBDAV_ENDPOINT (GitHub's runners), or when the cache can't be reached,
# rustc compiles as usual. CARGO_BUILD_JOBS: for memory, not CPU. The three services build at
# once in one BuildKit (8 GiB, peaked at 7.7 GiB with 4 jobs each), and 3 x 4 already fills a
# node's 12 cores.
# Checked against the SHA-256 published with the release.
ARG SCCACHE_VERSION=v0.19.0
RUN url=https://github.com/mozilla/sccache/releases/download/$SCCACHE_VERSION/sccache-$SCCACHE_VERSION-x86_64-unknown-linux-musl.tar.gz && \
    curl -sSfL -o /tmp/sccache.tgz "$url" && \
    echo "$(curl -sSfL "$url.sha256")  /tmp/sccache.tgz" | sha256sum -c --quiet && \
    tar -xzf /tmp/sccache.tgz -C /usr/local/bin --strip-components=1 --wildcards '*/sccache' && rm /tmp/sccache.tgz
# Its own socket in the step's /tmp, not 127.0.0.1:4226: BuildKit runs every build's steps in the
# node's network, so two builds at once would share one server.
ARG SCCACHE_WEBDAV_ENDPOINT=
ENV SCCACHE_WEBDAV_ENDPOINT=$SCCACHE_WEBDAV_ENDPOINT \
    SCCACHE_WEBDAV_DISABLE_CREATE_DIR=true \
    SCCACHE_SKIP_CACHE_CHECK=true \
    SCCACHE_SERVER_UDS=/tmp/sccache.sock \
    RUSTC_WRAPPER=${SCCACHE_WEBDAV_ENDPOINT:+sccache} \
    CARGO_BUILD_JOBS=4

FROM chef AS planner
COPY Cargo.toml Cargo.lock clippy.toml ./
COPY surreal/ surreal/
COPY crates/ crates/
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS bff
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --locked --recipe-path recipe.json -p bff --bin server && \
    { [ -z "$RUSTC_WRAPPER" ] || sccache --stop-server || true; }
# Not web/: a change to the Astro pages rebuilds no Rust.
COPY Cargo.toml Cargo.lock clippy.toml ./
COPY surreal/ surreal/
COPY crates/ crates/
RUN cargo build --release --locked -p bff --bin server && \
    cp target/release/server /app/server && \
    { [ -z "$RUSTC_WRAPPER" ] || sccache --stop-server || true; }

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
RUN apt-get update && \
    apt-get install -y --no-install-recommends openssl libssl3 ca-certificates libc6 && \
    rm -rf /var/lib/apt/lists/*

RUN useradd -m -d /home/appuser appuser

WORKDIR /app
# BuildKit skips a stage nothing copies from: this makes `verify` run.
COPY --from=verify /verified /app/.verified
# The server binary, the Astro pages it serves, and the build's source maps, which it uses to
# map browser errors and never serves (crates/bff/src/telemetry.rs).
COPY --from=bff /app/server ./server
COPY --from=web /app/web/dist ./web
COPY --from=web /app/web/sourcemaps ./sourcemaps

USER appuser

# The commit this image is built from (CI passes github.sha): `service.version` on traces and
# logs. Declared last, so a new commit rebuilds nothing above it.
ARG GIT_SHA=unknown
ENV GIT_SHA=$GIT_SHA
ENV IP=0.0.0.0
ENV PORT=80
ENV WEB_DIST=/app/web
ENV SOURCE_MAPS=/app/sourcemaps

EXPOSE 80
CMD ["/app/server"]
