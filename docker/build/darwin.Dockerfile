# syntax=docker/dockerfile:1.10.0
#
# Cross-compile AgentOS darwin binaries via osxcross on a Linux runner. The
# base image carries osxcross, the macOS SDK, Node, and pnpm.
#
#   TARGET = aarch64-apple-darwin | x86_64-apple-darwin
#   CLANG  = aarch64-apple-darwin20.4 | x86_64-apple-darwin20.4
FROM ghcr.io/rivet-dev/rivet/builder-base-osxcross:0e33ceb98

ARG TARGET=aarch64-apple-darwin
ARG CLANG=aarch64-apple-darwin20.4
ARG BUILD_PROFILE=debug
ARG CACHE_PLATFORM=darwin-arm64
ARG RUST_TOOLCHAIN=1.94.0

ENV SDK=/root/osxcross/target/SDK/MacOSX11.3.sdk \
    RUSTC_WRAPPER=sccache \
    SCCACHE_DIR=/root/.cache/sccache \
    SCCACHE_IDLE_TIMEOUT=0

WORKDIR /build
COPY . .

RUN rustup toolchain install "$RUST_TOOLCHAIN" --profile minimal && \
    rustup default "$RUST_TOOLCHAIN" && \
    rustup target add "$TARGET"

RUN --mount=type=cache,id=pnpm-store-agentos-darwin,target=/root/.local/share/pnpm/store,sharing=locked \
    corepack enable && \
    pnpm config set store-dir /root/.local/share/pnpm/store && \
    pnpm install --no-frozen-lockfile --filter='!@rivet-dev/agentos-website'

RUN --mount=type=cache,id=cargo-registry-agentos-darwin,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=cargo-git-agentos-darwin,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=cargo-target-agentos-${CACHE_PLATFORM},target=/build/target,sharing=locked \
    --mount=type=cache,id=sccache-agentos-${CACHE_PLATFORM},target=/root/.cache/sccache,sharing=locked \
    if ! command -v sccache >/dev/null 2>&1; then \
      unset RUSTC_WRAPPER; \
    elif ! (sccache --start-server 2>/tmp/sccache-start.err && sccache --show-stats >/dev/null 2>&1); then \
      echo "[sccache] unavailable, disabling:"; cat /tmp/sccache-start.err 2>/dev/null || true; \
      sccache --stop-server >/dev/null 2>&1 || true; \
      unset RUSTC_WRAPPER SCCACHE_DIR; \
    else \
      echo "[sccache] enabled via local BuildKit cache"; \
    fi && \
    tu=$(echo "$TARGET" | tr 'a-z-' 'A-Z_') && \
    tl=$(echo "$TARGET" | tr - _) && \
    export BINDGEN_EXTRA_CLANG_ARGS_${tl}="--sysroot=$SDK -isystem $SDK/usr/include" && \
    export CFLAGS_${tl}="-B/root/osxcross/target/bin" && \
    export CXXFLAGS_${tl}="-B/root/osxcross/target/bin" && \
    export CC_${tl}=${CLANG}-clang && \
    export CXX_${tl}=${CLANG}-clang++ && \
    export AR_${tl}=${CLANG}-ar && \
    export RANLIB_${tl}=${CLANG}-ranlib && \
    export CARGO_TARGET_${tu}_LINKER=${CLANG}-clang && \
    if [ "$BUILD_PROFILE" = "release" ]; then FLAG="--release"; PROF=release; else FLAG=""; PROF=debug; fi && \
    cargo build $FLAG -p agentos-sidecar -p agentos-native-sidecar --target "$TARGET" && \
    mkdir -p /artifacts && \
    cp "target/$TARGET/$PROF/agentos-sidecar" /artifacts/agentos-sidecar && \
    cp "target/$TARGET/$PROF/agentos-native-sidecar" /artifacts/agentos-native-sidecar && \
    (sccache --show-stats 2>/dev/null || true)

CMD ["ls", "-la", "/artifacts"]
