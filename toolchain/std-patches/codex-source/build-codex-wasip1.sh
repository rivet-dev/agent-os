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
export CFLAGS_wasm32_wasip1="--sysroot=$SECEXEC/toolchain/c/sysroot -D_WASI_EMULATED_SIGNAL -D_WASI_EMULATED_PTHREAD -D_WASI_EMULATED_MMAN -D_WASI_EMULATED_PROCESS_CLOCKS"

echo "== 2. crate-cache patches (become std-patches/crates/* artifacts when vendored) =="
# path-dedot + path-absolutize: route target_family=wasm to the unix-paths impl.
for c in path-dedot-3.1.1 path-absolutize-3.1.1; do
  for f in "$CARGO_CACHE$c/src/"*.rs; do
    [ -f "$f" ] && sed -i 's/all(target_family = "wasm", feature = "use_unix_paths_on_wasm")/target_family = "wasm"/g' "$f"
  done
done
# rustls-native-certs: load agentOS's real VM trust store from its Linux path.
RNC="${CARGO_CACHE}rustls-native-certs-0.8.3/src/lib.rs"
if [ -f "$RNC" ] && ! grep -q 'target_os = "wasi"' "$RNC"; then
  python3 - "$RNC" <<'PY'
import sys
p=sys.argv[1]; s=open(p).read()
a='#[cfg(target_os = "macos")]\nuse macos as platform;\n'
s=s.replace(a, a+'\n#[cfg(target_os = "wasi")]\nmod platform {\n    use super::{CertPaths, CertificateResult};\n    use std::path::PathBuf;\n    pub fn load_native_certs() -> CertificateResult {\n        CertPaths {\n            file: Some(PathBuf::from("/etc/ssl/certs/ca-certificates.crt")),\n            dirs: Vec::new(),\n        }.load()\n    }\n}\n',1)
open(p,"w").write(s)
PY
fi
# TODO(next frontier): fd-lock-4.0.4 — its sys/unsupported module is broken on wasi
# (`pub use unsupported;` name collision; read/write/rw guards `use std::os::unix`).
# Needs a wasi sys arm (advisory locks are no-ops in the single-process VM).

echo "== 3. build =="
# This diagnostic helper assumes clone-and-build-codex-wasi.sh has already
# applied the checked-in source, sysroot, libc, and dependency patches. It does
# not describe or authorize compile-only WASI fallbacks; see README.md.
cd "$CODEX"
RUSTFLAGS="--cfg tokio_unstable" cargo +$TOOLCHAIN build -p codex-core \
	--target wasm32-wasip1 -Z build-std=std,panic_abort "$@"
