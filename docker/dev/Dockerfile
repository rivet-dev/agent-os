# syntax=docker/dockerfile:1.10.0
#
# Interactive dev container for agentOS. Unlike docker/build/*.Dockerfile, this
# image contains no repo source: the working tree is bind-mounted at /build and
# every build output lands in named volumes, so host edits are visible
# immediately and rebuilds stay incremental.
#
# The deno sysroot / LTO dance from the release build is deliberately omitted —
# dev binaries only need to run in this container, not on old glibc.
FROM ubuntu:24.04

ARG RUST_TOOLCHAIN=1.91.1
ARG NODE_MAJOR=22
ARG PNPM_VERSION=10.13.1
ARG JUST_VERSION=1.36.0

ENV DEBIAN_FRONTEND=noninteractive \
    CARGO_HOME=/usr/local/cargo \
    RUSTUP_HOME=/usr/local/rustup \
    PNPM_HOME=/usr/local/pnpm \
    COREPACK_ENABLE_DOWNLOAD_PROMPT=0 \
    PATH=/usr/local/cargo/bin:/usr/local/pnpm:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

RUN set -eux; \
    apt-get update; \
    apt-get install -y --no-install-recommends \
      ca-certificates curl git make cmake ninja-build build-essential \
      pkg-config libssl-dev xz-utils unzip jq python3 python3-venv \
      clang lld binaryen file procps less vim-tiny; \
    rm -rf /var/lib/apt/lists/*

# Node + pnpm
RUN set -eux; \
    curl -fsSL "https://deb.nodesource.com/setup_${NODE_MAJOR}.x" | bash -; \
    apt-get install -y --no-install-recommends nodejs; \
    corepack enable; \
    corepack prepare "pnpm@${PNPM_VERSION}" --activate; \
    rm -rf /var/lib/apt/lists/*

# Rust: host target for the sidecar, wasm32-wasip1 + rust-src for the WASM
# command toolchain (scripts/patch-std.sh rebuilds std from source).
RUN set -eux; \
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
      | sh -s -- -y --default-toolchain "${RUST_TOOLCHAIN}" --profile minimal --no-modify-path; \
    rustup target add wasm32-wasip1; \
    rustup component add rust-src clippy rustfmt

RUN set -eux; \
    arch="$(uname -m)"; \
    case "$arch" in \
      x86_64)  just_arch=x86_64-unknown-linux-musl ;; \
      aarch64) just_arch=aarch64-unknown-linux-musl ;; \
      *) echo "unsupported arch: $arch" >&2; exit 1 ;; \
    esac; \
    curl -fsSL "https://github.com/casey/just/releases/download/${JUST_VERSION}/just-${JUST_VERSION}-${just_arch}.tar.gz" \
      | tar -xz -C /usr/local/bin just

WORKDIR /build

CMD ["sleep", "infinity"]
