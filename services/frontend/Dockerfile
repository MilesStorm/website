FROM lukemathwalker/cargo-chef:latest AS chef

FROM chef AS planner
WORKDIR /app
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
# clang is required by ring (via jsonwebtoken) when compiling for wasm32
RUN apt-get update && apt-get install -y --no-install-recommends clang && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=planner /app/recipe.json recipe.json
# Cook native server deps; dx bundle manages the WASM dep compilation internally
RUN cargo chef cook --release --recipe-path recipe.json

# Install dioxus-cli
RUN curl -L --proto '=https' --tlsv1.2 -sSf https://raw.githubusercontent.com/cargo-bins/cargo-binstall/main/install-from-binstall-release.sh | bash
RUN cargo binstall dioxus-cli --root /.cargo -y --force
ENV PATH="/.cargo/bin:$PATH"

# wasm32 target is needed by dx bundle for the WASM client binary
RUN rustup target add wasm32-unknown-unknown

COPY . .
# Run dx bundle from the package directory so Dioxus.toml is picked up
WORKDIR /app/packages/web
RUN dx bundle --platform web --release --debug-symbols=false

FROM debian:bookworm-slim AS runtime
RUN apt-get update && \
    apt-get install -y --no-install-recommends openssl libssl3 ca-certificates && \
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
ENV AUTH_SERVICE_URL=
ENV BFF_SERVICE_SECRET=

EXPOSE 80
CMD ["/app/server"]
