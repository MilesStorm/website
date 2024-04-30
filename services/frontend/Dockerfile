FROM lukemathwalker/cargo-chef:latest AS chef

# Install build tools
RUN cargo install dioxus-cli
RUN rustup target add wasm32-unknown-unknown

FROM chef as planner
WORKDIR /app
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# Cook the dependencies using the recipe prepared earlier
FROM chef AS cacher
WORKDIR /app
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json --target wasm32-unknown-unknown

# Copy over the source code and build the project
FROM chef as builder
WORKDIR /app
COPY --from=planner . .
RUN dx build --release

# Final stage: Copy the built files to an nginx image to serve the website
FROM nginx:alpine AS runtime
COPY nginx.conf /etc/nginx/nginx.conf

# Copy the built static files from the frontend-builder stage
WORKDIR /usr/share/nginx/html
COPY --from=builder /app/dist ./
COPY --from=builder /app/assets ./

# Expose port and run nginx
EXPOSE 80
CMD ["nginx", "-g", "daemon off;"]
