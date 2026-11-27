# Ubuntu 24.04 like the runtime stage (NVIDIA's CUDA images): the binary links the glibc and
# libstdc++ it runs with.
FROM ubuntu:24.04 AS chef
RUN apt-get update && apt-get install -y --no-install-recommends \
    curl build-essential pkg-config ca-certificates \
  && rm -rf /var/lib/apt/lists/*
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --default-toolchain stable --profile minimal
ENV PATH="/root/.cargo/bin:${PATH}"
RUN cargo install cargo-chef --locked
# The cluster's compile cache (.github/actions/rust), when the build is given it: sccache in front
# of rustc, so when a layer has to be rebuilt, this build on either node's BuildKit reuses what an
# earlier one compiled. Entries are keyed by the hash of each compile's inputs, never by file
# times. Without SCCACHE_WEBDAV_ENDPOINT (GitHub's runners), or when the cache can't be reached,
# rustc compiles as usual. CARGO_BUILD_JOBS: for memory, not CPU. The services build at
# once in one BuildKit (8 GiB, peaked at 7.7 GiB with 4 jobs each), and 3 x 4 already fills a
# node's 12 cores.
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
    RUSTC_WRAPPER=${SCCACHE_WEBDAV_ENDPOINT:+sccache} \
    CARGO_BUILD_JOBS=4

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

# Copy over the source code and build the workspace's binaries (kindred-world; the kindred
# service joins in M3)
COPY . .
RUN cargo build --release --locked && \
    { [ -z "$RUSTC_WRAPPER" ] || sccache --stop-server || true; }

# ONNX Runtime, which runs the text embedding model (crates/world-build/src/embed.rs):
# Microsoft's CUDA 13 build from PyPI, the version services/ai_pipeline uses, pinned by SHA-256.
# The TensorRT provider is left out (unused, and TensorRT isn't in the image).
FROM python:3.12-slim AS onnxruntime
ADD --checksum=sha256:b52998a075de27ff30e46a125001822541fa5b03f5d31398ab0e50789d80c51c \
    https://files.pythonhosted.org/packages/8f/48/6fbfdbce25e5756c7b9ff8f40645d98b166b8022cf00ede58a2bb4571b8e/onnxruntime_gpu-1.31.0-cp312-cp312-manylinux_2_28_x86_64.whl /tmp/onnxruntime.whl
RUN python -m zipfile -e /tmp/onnxruntime.whl /tmp/whl \
 && mkdir -p /onnxruntime/lib \
 && mv /tmp/whl/onnxruntime/capi/libonnxruntime*.so* /onnxruntime/lib/ \
 && rm /onnxruntime/lib/libonnxruntime_providers_tensorrt.so \
 && ln -s libonnxruntime.so.1.31.0 /onnxruntime/lib/libonnxruntime.so

# One final stage per binary; the release workflow picks one with `target` (release.yml,
# SERVICES). kindred-world: the monthly world build (crds/kindred-world). Nothing from the datasets
# is in the image (PLAN.md rule 13): it downloads them onto its scratch volume at run time. The
# embedding model is public and is in the image.
# NVIDIA's runtime image: ONNX Runtime's CUDA provider needs cuDNN, cuBLAS and the other CUDA
# libraries it ships; the driver comes from the host through the NVIDIA container runtime. On a
# node without a GPU the build embeds on the CPU.
FROM nvidia/cuda:13.0.2-cudnn-runtime-ubuntu24.04 AS world
# The base image's entrypoint only prints a licence banner into the logs.
ENTRYPOINT []
RUN apt-get update && \
  apt-get install -y --no-install-recommends ca-certificates && \
  rm -rf /var/lib/apt/lists/*

COPY --from=onnxruntime /onnxruntime/lib /opt/onnxruntime/lib
# `ort` loads the library from here; the CUDA provider is found next to it.
ENV ORT_DYLIB_PATH=/opt/onnxruntime/lib/libonnxruntime.so
# /usr/local/nvidia/* is where Kubernetes GPU setups may mount the driver.
ENV LD_LIBRARY_PATH=/usr/local/cuda/lib64:/usr/local/nvidia/lib:/usr/local/nvidia/lib64
ENV NVIDIA_VISIBLE_DEVICES=all
ENV NVIDIA_DRIVER_CAPABILITIES=compute,utility

# The text embedding model (world.toml [embed] model_dir), at a pinned revision of its
# repository, each file checked by SHA-256. A new revision: new URLs and checksums here, and new
# reference embeddings in fixtures/embedding-reference.json.
ARG BGE=https://huggingface.co/BAAI/bge-small-en-v1.5/resolve/5c38ec7c405ec4b44b94cc5a9bb96e735b38267a
ADD --checksum=sha256:828e1496d7fabb79cfa4dcd84fa38625c0d3d21da474a00f08db0f559940cf35 \
    ${BGE}/onnx/model.onnx /opt/kindred/bge-small-en-v1.5/model.onnx
ADD --checksum=sha256:d241a60d5e8f04cc1b2b3e9ef7a4921b27bf526d9f6050ab90f9267a1f9e5c66 \
    ${BGE}/tokenizer.json /opt/kindred/bge-small-en-v1.5/tokenizer.json
RUN chmod -R a+rX /opt/kindred

# uid 1000: the CronJob's fsGroup makes the scratch volume writable for it. The CUDA image has
# user ubuntu at uid 1000 already: reuse it.
USER 1000

COPY --from=builder /app/target/release/kindred-world /usr/local/bin/kindred-world
# The embedding through ort and the Rust tokenizer must give the model card's reference
# embeddings (03-engine.md §3): checked here, on the CPU, every time the image is built.
RUN kindred-world embed-check

# The commit this image is built from (CI passes github.sha): `service.version` on traces and
# logs, and the second half of a world's version name. Declared last, so a new commit doesn't
# invalidate the build cache.
ARG GIT_SHA=unknown
ENV GIT_SHA=$GIT_SHA
ENTRYPOINT ["kindred-world"]
CMD ["build"]
