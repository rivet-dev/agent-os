#!/bin/bash
# build-codex-wasip1.sh — reproduce the codex-core wasm32-wasip1 build frontier.
#
# Captures every fix discovered while driving `cargo build -p codex-core
# --target wasm32-wasip1` to its current frontier. Run from the codex-rs checkout.
# This is the DIAGNOSTIC harness (builds codex IN its own workspace to find the
# frontier). The SHIPPING build vendors codex into agentos and swaps the same
# crates via [patch.crates-io]; the fixes are identical.
#
# Usage: CODEX=/path/to/codex-rs SECEXEC=/path/to/agentos ./build-codex-wasip1.sh
set -uo pipefail
CODEX="${CODEX:-/home/nathan/agent-e2e/codex-rs/codex-rs}"
SECEXEC="${SECEXEC:-/home/nathan/agent-e2e/agentos}"
STUBS="$SECEXEC/toolchain/stubs"
WSDK="$SECEXEC/toolchain/c/vendor/wasi-sdk"
TOOLCHAIN="nightly-2026-03-01"
CARGO_CACHE="$(ls -d $HOME/.cargo/registry/src/index.crates.io-*/ | head -1)"

echo "== 1. C toolchain (wasi-sdk clang) for libz-sys/ring/etc. =="
export CC_wasm32_wasip1="$WSDK/bin/clang"
export AR_wasm32_wasip1="$WSDK/bin/llvm-ar"
export CFLAGS_wasm32_wasip1="--sysroot=$WSDK/share/wasi-sysroot -D_WASI_EMULATED_SIGNAL -D_WASI_EMULATED_PTHREAD -D_WASI_EMULATED_MMAN -D_WASI_EMULATED_PROCESS_CLOCKS"

echo "== 2. crate-cache patches (become std-patches/crates/* artifacts when vendored) =="
# path-dedot + path-absolutize: route target_family=wasm to the unix-paths impl.
for c in path-dedot-3.1.1 path-absolutize-3.1.1; do
  for f in "$CARGO_CACHE$c/src/"*.rs; do
    [ -f "$f" ] && sed -i 's/all(target_family = "wasm", feature = "use_unix_paths_on_wasm")/target_family = "wasm"/g' "$f"
  done
done
# rustls-native-certs: add a wasi arm returning empty certs (TLS is host-brokered).
RNC="${CARGO_CACHE}rustls-native-certs-0.8.3/src/lib.rs"
if [ -f "$RNC" ] && ! grep -q 'target_os = "wasi"' "$RNC"; then
  python3 - "$RNC" <<'PY'
import sys
p=sys.argv[1]; s=open(p).read()
a='#[cfg(target_os = "macos")]\nuse macos as platform;\n'
s=s.replace(a, a+'\n#[cfg(target_os = "wasi")]\nmod platform {\n    use super::CertificateResult;\n    pub fn load_native_certs() -> CertificateResult { CertificateResult::default() }\n}\n',1)
open(p,"w").write(s)
PY
fi
# TODO(next frontier): fd-lock-4.0.4 — its sys/unsupported module is broken on wasi
# (`pub use unsupported;` name collision; read/write/rw guards `use std::os::unix`).
# Needs a wasi sys arm (advisory locks are no-ops in the single-process VM).

echo "== 3. codex workspace patches (Cargo.toml) — build config, not agent logic =="
# Applied by hand in this session (see git diff of codex-rs/Cargo.toml):
#  - [patch.crates-io]: reqwest = stubs/reqwest-shim, tokio = /tmp/tokio-dev (1.52.3),
#                       ctrlc = stubs/ctrlc
#  - codex-network-proxy / codex-otel workspace deps repointed to stubs/*
#  - removed "network-proxy","otel" from [workspace].members (collision)
#  - zip = { default-features=false, features=["deflate","time"] }  (drops xz/lzma)
#  - cargo update -p tokio --precise 1.52.3  (unify onto the patched tokio)

echo "== 4. codex SOURCE patch (unavoidable compile_error) =="
#  - utils/git/src/platform.rs: add #[cfg(target_os="wasi")] create_symlink stub +
#    widen the fallback compile_error cfg. (see std-patches/codex-source/README.md)

echo "== 5. build =="
cd "$CODEX"
RUSTFLAGS="--cfg tokio_unstable" cargo +$TOOLCHAIN build -p codex-core \
  --target wasm32-wasip1 -Z build-std=std,panic_abort "$@"
fd-lock-4.0.4: sys/mod.rs unsupported branch (pub use unsupported::* + AsOpenFile=std::os::fd::AsFd); unsupported guards std::os::unix::io -> std::os::fd. Advisory locks are no-ops in single-process VM.

# == portable-pty subtree (codex-utils-pty → portable-pty → filedescriptor) ==
# PTY is NOT on the headless session-turn path (compile-only). Applied:
#  - portable-pty-0.9.0: add src/wasi.rs (WasiPtySystem/Master/Slave/Child stubs
#    impl'ing PtySystem/MasterPty/SlavePty/Child/ChildKiller); lib.rs gate
#    `pub mod serial` to not(wasi) + add `#[cfg(target_os="wasi")] pub mod wasi` +
#    `#[cfg(target_os="wasi")] pub type NativePtySystem = wasi::WasiPtySystem`;
#    Cargo.toml: make serial2 a `cfg(not(target_os="wasi"))` target dep.
#  - reqwest shim expanded: Client::{put,patch,delete,head,execute}, Request type +
#    TryFrom<http::Request<Vec<u8>>>, RequestBuilder::{bearer_auth,basic_auth,query,
#    form}, IntoUrl for &String/url::Url, all reqwest 0.12 features as no-op flags,
#    deps url/base64/serde_urlencoded. reqwest unified to 0.12.28 (kills the
#    wasm-bindgen-futures browser-backend leak from fragmented 0.12.27).
#  - tokio /tmp/tokio-dev stream.rs: add #[cfg(target_os="wasi")] TcpStream::connect
#    stub (compile-only; tungstenite references it, runtime uses HTTP fallback).
# TODO(final dep wall): filedescriptor-0.8.3 has no wasi arm (RawFileDescriptor/
#   SocketDescriptor/HandleType undefined on wasi). Either add a filedescriptor wasi
#   module (RawFileDescriptor=std::os::fd::RawFd + minimal FileDescriptor/OwnedHandle)
#   OR replace portable-pty wholesale with a [patch.crates-io] stub (the src/wasi.rs
#   impls already written are the basis). After that, codex-core's OWN code compiles.
