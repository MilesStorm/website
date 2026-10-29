FROM lukemathwalker/cargo-chef:latest AS chef
# The cluster's compile cache (.github/actions/rust), when the build is given it: sccache in front
# of rustc, so when a layer has to be rebuilt, this build on either node's BuildKit reuses what an
# earlier one compiled. Entries are keyed by the hash of each compile's inputs, never by file
# times. Without SCCACHE_WEBDAV_ENDPOINT (GitHub's runners), or when the cache can't be reached,
# rustc compiles as usual.
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
    RUSTC_WRAPPER=${SCCACHE_WEBDAV_ENDPOINT:+sccache}

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

# Copy over the source code and build the project
COPY . .
RUN cargo build --release && \
    { [ -z "$RUSTC_WRAPPER" ] || sccache --stop-server || true; }

# Final stage: Copy the built files to an nginx image to serve the website
FROM debian:bookworm-slim
# The commit this image is built from (CI passes github.sha): `service.version` on traces and
# logs. Declared in this stage only, so a new commit doesn't invalidate the build cache.
ARG GIT_SHA=unknown
ENV GIT_SHA=$GIT_SHA

# Install OpenSSL
RUN apt-get update && \
  apt-get install -y --no-install-recommends openssl libssl3 ca-certificates && \
  rm -rf /var/lib/apt/lists/*

RUN useradd -m -d /home/appuser appuser
USER appuser

# Copy the built static files from the frontend-builder stage
COPY --from=builder /app/target/release/auth /usr/local/bin/auth


ENV SERVER_IP=
ENV DATABASE_URL=
ENV CLIENT_ID=
ENV CLIENT_SECRET=
ENV G_CLIENT_ID=
ENV G_CLIENT_SECRET=

# Expose port and run nginx
EXPOSE 7070
CMD ["auth"]
