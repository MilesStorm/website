# syntax=docker/dockerfile:1.7
FROM nvidia/cuda:12.9.1-cudnn-devel-ubuntu24.04 AS chef

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
RUN cargo chef cook --release --recipe-path recipe.json

# Full source build.
COPY . .
RUN cargo build --release

# `--strip-debug` keeps the symbol table, so eBPF CPU profiles show function names.
RUN strip --strip-debug /app/target/release/dice_detector


# ONNX Runtime, which runs YOLO: Microsoft's build for CUDA 12, pinned by SHA-256. It
# needs a newer CUDA than 12.6 (libcudart's cudaLibrary* functions; 12.9.1 has them): check a new
# version with `ldd -r libonnxruntime_providers_cuda.so` in the runtime image. The
# TensorRT provider is left out (unused, and TensorRT itself isn't in the image).
FROM chef AS onnxruntime
ADD --checksum=sha256:4e16d2ec66521a24fe917bd848eb8ae3a9107cb8b4aec978fb2b3ab1a640fcb7 \
    https://github.com/microsoft/onnxruntime/releases/download/v1.28.3/onnxruntime-linux-x64-gpu_cuda12-1.28.3.tgz /tmp/onnxruntime.tgz
RUN mkdir -p /onnxruntime \
 && tar -xzf /tmp/onnxruntime.tgz -C /onnxruntime --strip-components=1 --wildcards '*/lib/libonnxruntime*' \
 && rm /onnxruntime/lib/libonnxruntime_providers_tensorrt.so


# The half-precision copy of YOLO, for cards with tensor cores (src/model/inferance.rs).
FROM python:3.12-slim AS fp16
RUN pip install --no-cache-dir onnx==1.23.2 onnxruntime==1.31.0 sympy==1.14.0
COPY tools/yolo_fp16.py src/model/yolo26s.onnx /work/
RUN python /work/yolo_fp16.py /work/yolo26s.onnx /work/yolo26s_fp16.onnx


# NVIDIA's runtime image: ONNX Runtime's CUDA provider needs cuDNN, cuBLAS and the
# other CUDA libraries it ships, and burn's CUDA backend (the dice head) needs its NVRTC.
# The GPU driver itself comes from the host through the NVIDIA container runtime.
FROM nvidia/cuda:12.9.1-cudnn-runtime-ubuntu24.04 AS runtime
# The base image's entrypoint only prints a licence banner into the logs.
ENTRYPOINT []
# The commit this image is built from (CI passes github.sha): `service.version` on traces and
# logs. Declared in this stage only, so a new commit doesn't invalidate the build cache.
ARG GIT_SHA=unknown
ENV GIT_SHA=$GIT_SHA

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
  && rm -rf /var/lib/apt/lists/*

# burn compiles the head's kernels while running, with NVRTC and the CUDA headers; the
# runtime image has no headers.
COPY --from=builder /usr/local/cuda/targets/x86_64-linux/include /usr/local/cuda/targets/x86_64-linux/include
ENV CUDA_PATH=/usr/local/cuda
COPY --from=onnxruntime /onnxruntime/lib /opt/onnxruntime/lib
# `ort` loads the library from here; the CUDA provider is found next to it.
ENV ORT_DYLIB_PATH=/opt/onnxruntime/lib/libonnxruntime.so
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
COPY --chown=appuser:appuser src/model/yolo26s.onnx                        ./models/yolo26s.onnx
COPY --chown=appuser:appuser --from=fp16 /work/yolo26s_fp16.onnx           ./models/yolo26s_fp16.onnx

# Dice-value head (ResNet18 trained by tools/train_head_torch.py --all). Model
# weights never go in git: the file is a GitHub Release asset, pinned by SHA-256
# so the build fails if the download differs. To ship a new model: upload a new
# release asset, then update the URL default and the checksum here.
ARG DICE_HEAD_URL=https://github.com/MilesStorm/website/releases/download/dice-head-v1/dice_head_resnet18.safetensors
ADD --chown=appuser:appuser --chmod=0644 --checksum=sha256:6b6e71693e5b9b667c2d047a3f1dd464017538637eea8c7f6b136c1cf20c2bd9 ${DICE_HEAD_URL} ./models/dice_head_resnet18.safetensors

USER appuser

ENV YOLO_ONNX_PATH=/app/models/yolo26s.onnx
ENV YOLO_FP16_ONNX_PATH=/app/models/yolo26s_fp16.onnx
ENV DICE_HEAD_PATH=/app/models/dice_head_resnet18.safetensors
# Minimum averaged confidence to report a die's value; below it the die is
# reported as unreadable (null) instead of guessed.
ENV DICE_CONF_THRESHOLD=0.7
# Stamped on roll events; keep in step with the release tag in DICE_HEAD_URL.
ENV DICE_MODEL_ID=dice-head-v1

EXPOSE 9000
CMD ["./dice_detector", "yolo", "0.0.0.0:9000"]
