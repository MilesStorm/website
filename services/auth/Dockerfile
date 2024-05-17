FROM lukemathwalker/cargo-chef:latest AS chef

FROM chef as planner
WORKDIR /app
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# Cook the dependencies using the recipe prepared earlier
FROM chef AS builder
WORKDIR /app
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json

# Copy over the source code and build the project
COPY . .
RUN cargo build --release

# Final stage: Copy the built files to an nginx image to serve the website
FROM debian:bookworm-slim

# Install OpenSSL
RUN apt-get update && \
  apt-get install -y --no-install-recommends openssl libssl3 ca-certificates && \
  rm -rf /var/lib/apt/lists/*

RUN useradd -m -d /home/appuser appuser
USER appuser

# Copy the built static files from the frontend-builder stage
COPY --from=builder /app/target/release/server /usr/local/bin/server


ENV SERVER_IP=
ENV RUST_LOG=
ENV DATABASE_URL=
ENV CLIENT_ID=
ENV CLIENT_SECRET=

# Expose port and run nginx
EXPOSE 7070
CMD ["server"]
