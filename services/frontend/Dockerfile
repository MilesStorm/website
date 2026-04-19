FROM lukemathwalker/cargo-chef:latest AS chef

# Install build tools
RUN rustup target add wasm32-unknown-unknown

FROM chef AS planner
WORKDIR /app
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# Cook the dependencies using the recipe prepared earlier
FROM chef AS builder
WORKDIR /app
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json --target wasm32-unknown-unknown

# install dx
RUN curl -L --proto '=https' --tlsv1.2 -sSf https://raw.githubusercontent.com/cargo-bins/cargo-binstall/main/install-from-binstall-release.sh | bash
RUN cargo binstall dioxus-cli --root /.cargo -y --force
ENV PATH="/.cargo/bin:$PATH"

# Copy over the source code and build the project
COPY . .
RUN dx bundle --platform web --debug-symbols=false --release

# Final stage: Copy the built files to an nginx image to serve the website
FROM nginx:alpine AS runtime
COPY nginx.conf /etc/nginx/nginx.conf

# Copy the built static files from the frontend-builder stage
WORKDIR /usr/share/nginx/html
COPY --from=builder /app/target/dx/frontend/release/web/public ./
COPY --from=builder /app/target/dx/frontend/release/web/public/assets ./

# Expose port and run nginx
EXPOSE 80
CMD ["nginx", "-g", "daemon off;"]
