# syntax=docker/dockerfile:1.7
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
COPY src/model/yolo26s.onnx src/model/yolo26s.onnx
RUN cargo chef cook --release --recipe-path recipe.json

# Full source build.
COPY . .
RUN cargo build --release

# Extract the burn-onnx-generated YOLO burnpack to a stable path so the
# runtime stage can copy it without knowing the cargo hash in OUT_DIR.
RUN find /app/target -name "yolo26s.bpk" -exec cp {} /app/yolo26s.bpk \;


# Gather the only CUDA files the runtime needs. burn's CUDA backend compiles its
# kernels while running, with NVRTC and the CUDA headers; the GPU driver itself
# comes from the host through the NVIDIA container runtime. `cp -a` keeps the
# library symlinks as symlinks. `--strip-debug` keeps the symbol table, so eBPF CPU
# profiles show function names.
RUN mkdir -p /cuda/lib64 \
 && cp -a /usr/local/cuda/lib64/libnvrtc.so* /usr/local/cuda/lib64/libnvrtc-builtins.so* /cuda/lib64/ \
 && cp -a /usr/local/cuda/targets/x86_64-linux/include /cuda/include \
 && strip --strip-debug /app/target/release/dice_detector


# Plain Ubuntu plus the files above, instead of an NVIDIA image: none of the
# CUDA libraries those ship (cudart, cuBLAS, cuDNN, ...) are used.
FROM ubuntu:24.04 AS runtime
# The commit this image is built from (CI passes github.sha): `service.version` on traces and
# logs. Declared in this stage only, so a new commit doesn't invalidate the build cache.
ARG GIT_SHA=unknown
ENV GIT_SHA=$GIT_SHA

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
  && rm -rf /var/lib/apt/lists/*

COPY --from=builder /cuda /usr/local/cuda
ENV CUDA_PATH=/usr/local/cuda
# /usr/local/nvidia/* is where Kubernetes GPU setups may mount the driver.
ENV LD_LIBRARY_PATH=/usr/local/cuda/lib64:/usr/local/nvidia/lib:/usr/local/nvidia/lib64
# Tell the NVIDIA container runtime to pass the GPU and driver into the container.
ENV NVIDIA_VISIBLE_DEVICES=all
ENV NVIDIA_DRIVER_CAPABILITIES=compute,utility

RUN useradd -m -d /home/appuser appuser \
 && mkdir /app && chown appuser:appuser /app

WORKDIR /app

# --chown on each copy; a separate `chown -R` would store every file a second time.
COPY --chown=appuser:appuser --from=builder /app/target/release/dice_detector ./dice_detector
COPY --chown=appuser:appuser --from=builder /app/yolo26s.bpk               ./models/yolo26s.bpk

# Dice-value head (ResNet18 trained by tools/train_head_torch.py --all). Model
# weights never go in git: the file is a GitHub Release asset, pinned by SHA-256
# so the build fails if the download differs. To ship a new model: upload a new
# release asset, then update the URL default and the checksum here.
ARG DICE_HEAD_URL=https://github.com/MilesStorm/website/releases/download/dice-head-v1/dice_head_resnet18.safetensors
ADD --chown=appuser:appuser --chmod=0644 --checksum=sha256:6b6e71693e5b9b667c2d047a3f1dd464017538637eea8c7f6b136c1cf20c2bd9 ${DICE_HEAD_URL} ./models/dice_head_resnet18.safetensors

USER appuser

ENV YOLO_MODEL_PATH=/app/models/yolo26s.bpk
ENV DICE_HEAD_PATH=/app/models/dice_head_resnet18.safetensors
# Minimum averaged confidence to report a die's value; below it the die is
# reported as unreadable (null) instead of guessed.
ENV DICE_CONF_THRESHOLD=0.7
# Stamped on roll events; keep in step with the release tag in DICE_HEAD_URL.
ENV DICE_MODEL_ID=dice-head-v1

EXPOSE 9000
CMD ["./dice_detector", "yolo", "0.0.0.0:9000"]
