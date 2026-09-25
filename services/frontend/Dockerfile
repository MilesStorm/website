# syntax=docker/dockerfile:1.7
# No cargo-chef: `dx bundle` builds with its own profiles and features, so a
# `cargo chef cook` layer was never reused (it compiled every dependency a second
# time). Caching comes from building on eta's own Docker, which keeps unchanged
# layers between CI runs.
FROM rust:1-trixie AS builder
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

COPY . .

# Generate Tailwind CSS before dx bundle — the asset! macro checks file existence at compile time
WORKDIR /app
RUN npm install && \
    node_modules/.bin/tailwindcss -i packages/ui/tailwind.css -o packages/ui/assets/styling/tailwind.css --minify

# Run dx bundle from the package directory so Dioxus.toml is picked up
WORKDIR /app/packages/web
# Cache folders kept on the build machine (eta) between builds: downloaded crates
# and compiled code, so a code change only recompiles what changed. They are not
# part of the image; `dist/` (outside target/) is what the next stage copies.
RUN --mount=type=cache,id=frontend-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=frontend-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=frontend-target,target=/app/target,sharing=locked \
    dx bundle --platform web --release --debug-symbols=false

FROM debian:trixie-slim AS runtime
RUN apt-get update && \
    apt-get install -y --no-install-recommends openssl libssl3 ca-certificates libc6 && \
    rm -rf /var/lib/apt/lists/*

RUN useradd -m -d /home/appuser appuser

WORKDIR /app
# Copy the server binary and the static public assets it serves.
# out_dir = "dist" in Dioxus.toml puts the bundle under packages/web/dist/
COPY --from=builder /app/packages/web/dist/server ./server
COPY --from=builder /app/packages/web/dist/public ./public

USER appuser

ENV RUST_LOG=info
ENV IP=0.0.0.0
ENV PORT=80

EXPOSE 80
CMD ["/app/server"]
