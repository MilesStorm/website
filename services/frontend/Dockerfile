FROM lukemathwalker/cargo-chef:latest AS chef

RUN rustup target add wasm32-unknown-unknown

FROM chef AS planner
WORKDIR /app
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
WORKDIR /app
COPY --from=planner /app/recipe.json recipe.json
# Cook deps for both the WASM client and the native server targets
RUN cargo chef cook --release --recipe-path recipe.json --target wasm32-unknown-unknown
RUN cargo chef cook --release --recipe-path recipe.json

# Install dioxus-cli
RUN curl -L --proto '=https' --tlsv1.2 -sSf https://raw.githubusercontent.com/cargo-bins/cargo-binstall/main/install-from-binstall-release.sh | bash
RUN cargo binstall dioxus-cli --root /.cargo -y --force
ENV PATH="/.cargo/bin:$PATH"

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
# Copy the server binary and the static public assets it serves
COPY --from=builder /app/target/dx/web/release/web/server ./server
COPY --from=builder /app/target/dx/web/release/web/public ./public

USER appuser

ENV RUST_LOG=info
ENV AUTH_SERVICE_URL=
ENV BFF_SERVICE_SECRET=

EXPOSE 8080
CMD ["/app/server"]
