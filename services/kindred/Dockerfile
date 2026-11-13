FROM lukemathwalker/cargo-chef:latest AS chef
# The cluster's compile cache (.github/actions/rust), when the build is given it: sccache in front
# of rustc, so when a layer has to be rebuilt, this build on either node's BuildKit reuses what an
# earlier one compiled. Entries are keyed by the hash of each compile's inputs, never by file
# times. Without SCCACHE_WEBDAV_ENDPOINT (GitHub's runners), or when the cache can't be reached,
# rustc compiles as usual. CARGO_BUILD_JOBS: for memory, not CPU. The services build at
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

FROM chef as planner
WORKDIR /app
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# Cook the dependencies using the recipe prepared earlier
FROM chef AS builder
WORKDIR /app
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json && \
    { [ -z "$RUSTC_WRAPPER" ] || sccache --stop-server || true; }

# Copy over the source code and build the workspace's binaries (kindred-world; the kindred
# service joins in M3)
COPY . .
RUN cargo build --release --locked && \
    { [ -z "$RUSTC_WRAPPER" ] || sccache --stop-server || true; }

# One final stage per binary; the release workflow picks one with `target` (release.yml,
# SERVICES). kindred-world: the monthly world build (crds/kindred-world). Nothing from the datasets
# is in the image (PLAN.md rule 13): it downloads them onto its scratch volume at run time.
# The same Debian release as the builder (cargo-chef:latest is trixie): the binary links that
# release's glibc and libstdc++ (GLIBC_2.38, CXXABI_1.3.15), which bookworm lacks.
FROM debian:trixie-slim AS world
# The commit this image is built from (CI passes github.sha): `service.version` on traces and
# logs, and the second half of a world's version name. Declared in this stage only, so a new
# commit doesn't invalidate the build cache.
ARG GIT_SHA=unknown
ENV GIT_SHA=$GIT_SHA

RUN apt-get update && \
  apt-get install -y --no-install-recommends ca-certificates libstdc++6 && \
  rm -rf /var/lib/apt/lists/*

# uid 1000: the CronJob's fsGroup makes the scratch volume writable for it.
RUN useradd -m -u 1000 -d /home/appuser appuser
USER appuser

COPY --from=builder /app/target/release/kindred-world /usr/local/bin/kindred-world
ENTRYPOINT ["kindred-world"]
CMD ["build"]
