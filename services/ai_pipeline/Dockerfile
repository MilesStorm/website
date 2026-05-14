FROM nvidia/cuda:12.6.3-cudnn-devel-ubuntu24.04 AS chef

RUN apt-get update && apt-get install -y --no-install-recommends \
    curl \
    build-essential \
    pkg-config \
    libssl-dev \
    ca-certificates \
  && rm -rf /var/lib/apt/lists/*

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --default-toolchain stable --profile minimal
ENV PATH="/root/.cargo/bin:${PATH}"
RUN cargo install cargo-chef --locked


FROM chef AS planner
WORKDIR /app
COPY . .
RUN cargo chef prepare --recipe-path recipe.json


FROM chef AS builder
WORKDIR /app

# Cook dependencies first (cached layer when only source changes).
COPY --from=planner /app/recipe.json recipe.json
# build.rs needs the ONNX file present during dependency cooking.
COPY src/model/yolo26n.onnx src/model/yolo26n.onnx
RUN cargo chef cook --release --recipe-path recipe.json

# Full source build.
COPY . .
RUN cargo build --release

# Extract the burn-onnx-generated YOLO burnpack to a stable path so the
# runtime stage can copy it without knowing the cargo hash in OUT_DIR.
RUN find /app/target -name "yolo26n.bpk" -exec cp {} /app/yolo26n.bpk \;


FROM nvidia/cuda:12.6.3-cudnn-runtime-ubuntu24.04 AS runtime

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
  && rm -rf /var/lib/apt/lists/*

RUN useradd -m -d /home/appuser appuser

WORKDIR /app

COPY --from=builder /app/target/release/dice_detector ./dice_detector
COPY --from=builder /app/yolo26n.bpk               ./models/yolo26n.bpk
COPY weights/                                       ./weights/

RUN chown -R appuser:appuser /app
USER appuser

ENV RUST_LOG=info
ENV YOLO_MODEL_PATH=/app/models/yolo26n.bpk

EXPOSE 9000
CMD ["./dice_detector", "yolo", "0.0.0.0:9000"]
