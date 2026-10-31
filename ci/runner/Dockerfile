# The cluster's runner image (homelab repo, argo/apps/ci-runners.yaml): GitHub's runner image with
# what the Rust jobs need, so a job doesn't install them on every run. Built and pushed by
# .github/workflows/ci-runner.yml, weekly too, which brings the newest stable Rust: every job
# of a push then compiles with the same rustc, the one in the image its runner pod started from.
FROM ghcr.io/actions/actions-runner:2.338.2

USER root

# The C compiler for build scripts and linking, OpenSSL for auth, venv for ai-pipeline's tools,
# zstd for the jobs' target/ snapshots.
RUN apt-get update && \
    apt-get install -y --no-install-recommends gcc libc6-dev pkg-config libssl-dev python3-venv zstd && \
    rm -rf /var/lib/apt/lists/*

# Node 26 (LTS) for actions/setup-node, which otherwise downloads it in every job: the newest 26.x
# from the same manifest setup-node reads, laid out as its tool cache. setup-node uses a cached
# version that matches (check-latest is off). The runner reads RUNNER_TOOL_CACHE (and the older
# AGENT_TOOLSDIRECTORY); the runner user owns it, so setup-node can still add a version a workflow
# asks for.
ENV RUNNER_TOOL_CACHE=/opt/hostedtoolcache \
    AGENT_TOOLSDIRECTORY=/opt/hostedtoolcache
RUN url=$(curl -sSfL https://raw.githubusercontent.com/actions/node-versions/main/versions-manifest.json | \
      python3 -c 'import json,sys; v=next(v for v in json.load(sys.stdin) if v["stable"] and v["version"].startswith("26.")); f=next(f for f in v["files"] if f["platform"]=="linux" and f["arch"]=="x64"); print(v["version"], f["download_url"])') && \
    set -- $url && dir=/opt/hostedtoolcache/node/$1/x64 && mkdir -p "$dir" && \
    curl -sSfL "$2" | tar -xz -C "$dir" && touch "$dir.complete" && \
    chown -R runner:runner /opt/hostedtoolcache && "$dir/bin/node" --version

# Read-only for the jobs: they keep their crate downloads in their own CARGO_HOME
# (.github/actions/rust).
ENV RUSTUP_HOME=/opt/rustup \
    PATH=/opt/cargo/bin:$PATH
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | \
      CARGO_HOME=/opt/cargo sh -s -- -y --no-modify-path --profile minimal --default-toolchain stable --component clippy && \
    rustc --version && cargo clippy --version

# Checked against the SHA-256 published with the release.
ARG SCCACHE_VERSION=v0.19.0
RUN url=https://github.com/mozilla/sccache/releases/download/$SCCACHE_VERSION/sccache-$SCCACHE_VERSION-x86_64-unknown-linux-musl.tar.gz && \
    curl -sSfL -o /tmp/sccache.tgz "$url" && \
    echo "$(curl -sSfL "$url.sha256")  /tmp/sccache.tgz" | sha256sum -c --quiet && \
    tar -xzf /tmp/sccache.tgz -C /usr/local/bin --strip-components=1 --wildcards '*/sccache' && \
    rm /tmp/sccache.tgz && sccache --version

USER runner
