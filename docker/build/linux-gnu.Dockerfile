# syntax=docker/dockerfile:1.10.0
#
# Build AgentOS Linux GNU native artifacts in one cached container. The sysroot
# setup mirrors scripts/ci/setup-linux-gnu-sysroot.sh, but runs inside Docker so
# Cargo caches can be persisted with BuildKit/GHA like the Darwin build.
FROM ubuntu:24.04

ARG RUST_TOOLCHAIN=1.94.0
ARG TARGET=x86_64-unknown-linux-gnu
ARG BUILD_PROFILE=debug
ARG CACHE_PLATFORM=linux-x64-gnu
ARG NODE_MAJOR=22
ARG LINUX_GNU_LLVM_VERSION=22
ARG LINUX_GNU_SYSROOT_TAG=sysroot-20250207

ENV DEBIAN_FRONTEND=noninteractive \
    CARGO_HOME=/usr/local/cargo \
    COREPACK_ENABLE_DOWNLOAD_PROMPT=0 \
    PNPM_HOME=/usr/local/pnpm \
    RUSTUP_HOME=/usr/local/rustup \
    PATH=/usr/local/cargo/bin:/usr/local/pnpm:/usr/local/rustup/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
    RUSTC_WRAPPER=sccache \
    SCCACHE_DIR=/root/.cache/sccache \
    SCCACHE_IDLE_TIMEOUT=0

WORKDIR /build

COPY scripts/ci/deno-memfd-create-shim.c scripts/ci/agentos-gettid-shim.c scripts/ci/agentos-renameat2-shim.c /tmp/agentos-ci/

RUN set -eux; \
    . /etc/os-release; \
    codename="${VERSION_CODENAME:?missing VERSION_CODENAME}"; \
    apt-get update; \
    apt-get install -y --no-install-recommends ca-certificates curl gnupg xz-utils binutils sccache build-essential pkg-config libssl-dev; \
    curl -fsSL "https://deb.nodesource.com/setup_${NODE_MAJOR}.x" | bash -; \
    apt-get install -y --no-install-recommends nodejs; \
    corepack enable; \
    corepack prepare pnpm@10.13.1 --activate; \
    echo "deb http://apt.llvm.org/${codename}/ llvm-toolchain-${codename}-${LINUX_GNU_LLVM_VERSION} main" \
      > "/etc/apt/sources.list.d/llvm-toolchain-${codename}-${LINUX_GNU_LLVM_VERSION}.list"; \
    curl -fsSL https://apt.llvm.org/llvm-snapshot.gpg.key \
      | gpg --dearmor \
      > /etc/apt/trusted.gpg.d/llvm-snapshot.gpg; \
    apt-get update; \
    apt-get install -y --no-install-recommends \
      "clang-${LINUX_GNU_LLVM_VERSION}" \
      "lld-${LINUX_GNU_LLVM_VERSION}"; \
    "clang-${LINUX_GNU_LLVM_VERSION}" -c -o /tmp/agentos_memfd_create_shim.o \
      /tmp/agentos-ci/deno-memfd-create-shim.c -fPIC; \
    "clang-${LINUX_GNU_LLVM_VERSION}" -c -o /tmp/agentos_gettid_shim.o \
      /tmp/agentos-ci/agentos-gettid-shim.c -fPIC; \
    "clang-${LINUX_GNU_LLVM_VERSION}" -c -o /tmp/agentos_renameat2_shim.o \
      /tmp/agentos-ci/agentos-renameat2-shim.c -fPIC; \
    if [ ! -x /usr/local/cargo/bin/rustup ]; then \
      curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --default-toolchain "${RUST_TOOLCHAIN}" --profile minimal --no-modify-path; \
    fi; \
    rustup default "${RUST_TOOLCHAIN}"; \
    rustup target add "${TARGET}"; \
    sysroot_arch="$(uname -m)"; \
    sysroot_url="https://github.com/denoland/deno_sysroot_build/releases/download/${LINUX_GNU_SYSROOT_TAG}/sysroot-${sysroot_arch}.tar.xz"; \
    curl -fsSL "${sysroot_url}" -o /tmp/agentos-sysroot.tar.xz; \
    rm -rf /sysroot; \
    cd /; \
    xzcat /tmp/agentos-sysroot.tar.xz | tar -x; \
    rm -rf /var/lib/apt/lists/* /tmp/agentos-sysroot.tar.xz

COPY . .

RUN --mount=type=cache,id=cargo-registry-agentos-${CACHE_PLATFORM},target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=cargo-git-agentos-${CACHE_PLATFORM},target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=cargo-target-agentos-${CACHE_PLATFORM},target=/build/target,sharing=locked \
    --mount=type=cache,id=pnpm-store-agentos-${CACHE_PLATFORM},target=/root/.local/share/pnpm/store,sharing=locked \
    --mount=type=cache,id=rusty-v8-agentos-${CACHE_PLATFORM},target=/root/.cargo/.rusty_v8,sharing=locked \
    --mount=type=cache,id=sccache-agentos-${CACHE_PLATFORM},target=/root/.cache/sccache,sharing=locked \
    set -eux; \
    pnpm install --frozen-lockfile --filter '@rivet-dev/agentos-build-tools'; \
    . /sysroot/.env; \
    if ! command -v sccache >/dev/null 2>&1; then \
      unset RUSTC_WRAPPER; \
    elif ! (sccache --start-server 2>/tmp/sccache-start.err && sccache --show-stats >/dev/null 2>&1); then \
      echo "[sccache] unavailable, disabling:"; cat /tmp/sccache-start.err 2>/dev/null || true; \
      sccache --stop-server >/dev/null 2>&1 || true; \
      unset RUSTC_WRAPPER SCCACHE_DIR; \
    else \
      echo "[sccache] enabled via local BuildKit cache"; \
    fi; \
    export CC="/usr/bin/clang-${LINUX_GNU_LLVM_VERSION}"; \
    export CFLAGS="${CFLAGS:-}"; \
    export RUSTFLAGS="-C linker-plugin-lto=true \
      -C linker=clang-${LINUX_GNU_LLVM_VERSION} \
      -C link-arg=-fuse-ld=lld-${LINUX_GNU_LLVM_VERSION} \
      -C link-arg=-ldl \
      -C link-arg=-Wl,--allow-shlib-undefined \
      -C link-arg=-Wl,--thinlto-cache-dir=/build/target/release/lto-cache \
      -C link-arg=-Wl,--thinlto-cache-policy,cache_size_bytes=700m \
      -C link-arg=/tmp/agentos_memfd_create_shim.o \
      -C link-arg=/tmp/agentos_gettid_shim.o \
      -C link-arg=/tmp/agentos_renameat2_shim.o \
      ${RUSTFLAGS:-}"; \
    cargo test -p agentos-execution wasmtime --lib --target "$TARGET"; \
    if [ "$BUILD_PROFILE" = "release" ]; then FLAG="--release"; PROF=release; else FLAG=""; PROF=debug; fi; \
    cargo build $FLAG -p agentos-sidecar -p agentos-native-sidecar --target "$TARGET"; \
    mkdir -p /artifacts; \
    cp "target/$TARGET/$PROF/agentos-sidecar" /artifacts/agentos-sidecar; \
    cp "target/$TARGET/$PROF/agentos-native-sidecar" /artifacts/agentos-native-sidecar; \
    (sccache --show-stats 2>/dev/null || true)

CMD ["ls", "-la", "/artifacts"]
