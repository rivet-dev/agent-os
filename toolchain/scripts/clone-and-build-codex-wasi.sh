#!/usr/bin/env bash
#
# clone-and-build-codex-wasi.sh — REPRODUCIBLE wasm32-wasip1 build of the codex-exec
# `--session-turn` engine from the pinned codex WASI fork.
#
# Unlike scripts/build-codex-wasi.sh (which builds an EXISTING local checkout and
# relies on that machine's [patch.crates-io] paths + a pre-patched crates.io cache),
# this script is self-contained and reproducible from a clean environment:
#
#   1. Read the pin from toolchain/codex-ref ("<owner>/<repo>@<sha>").
#   2. Shallow-clone the fork at that exact SHA into a gitignored scratch dir.
#   3. Rewrite the clone's existing local development overrides to the
#      reproducible agentOS adapters, while keeping real AWS authentication and
#      the Codex network-policy crate. Drop tokio's machine-path patch so the
#      locked Tokio release comes through the vendored+patched tree.
#   4. Strip the hardcoded `-L .../self-contained` from the clone's .cargo/config.toml
#      (that absolute rustup path is machine-specific; it is recomputed and passed via
#      RUSTFLAGS at build time instead).
#   5. `cargo vendor` the workspace (+ the std library deps needed by -Z build-std) and
#      apply toolchain/std-patches/crates/* to the vendored sources (tokio wasi-process,
#      path-dedot, rustls-native-certs, socket2, ...) via scripts/patch-vendor.sh.
#   6. Build codex-exec for wasm32-wasip1 by reusing the fork's own
#      scripts/build-wasi-codex-exec.sh (sysroot massaging + build-std + wasm-opt).
#   7. Install the optimized artifact to software/codex/wasm/{codex,codex-exec}.
#
# Usage:
#   toolchain/scripts/clone-and-build-codex-wasi.sh
#
# Env (all optional; sensible defaults):
#   CODEX_BUILD_DIR  scratch root for the clone/vendor/target (default: toolchain/.codex-build)
#   WASI_SDK_DIR     wasi-sdk C toolchain    (default: toolchain/c/vendor/wasi-sdk)
#   DEST_DIR         install destination     (default: software/codex/wasm)
#   TOOLCHAIN        rust toolchain          (default: nightly-2026-03-01)
#   CODEX_GIT_BASE   git host base           (default: https://github.com)
#   STOP_AFTER       "vendor" -> stop right after vendor+patch (bounded verification:
#                    proves clone + patch-injection + vendor/patch succeed without the
#                    full ~29MB build). Unset -> full build + install.
#   KEEP_SYSROOT     forwarded to the fork build script (1 = skip sysroot restore).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TOOLCHAIN_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
AGENTOS_ROOT="$(cd "$TOOLCHAIN_DIR/.." && pwd)"

STUBS="$TOOLCHAIN_DIR/stubs"
CODEX_REF_FILE="$TOOLCHAIN_DIR/codex-ref"
CODEX_PATCH_DIR="$TOOLCHAIN_DIR/std-patches/codex"

BUILD_ROOT="${CODEX_BUILD_DIR:-$TOOLCHAIN_DIR/.codex-build}"
CHECKOUT="$BUILD_ROOT/checkout"            # git repo root (rivet-dev/codex)
WORKSPACE="$CHECKOUT/codex-rs"             # cargo workspace (the fork nests it here)
WASI_SDK_DIR="${WASI_SDK_DIR:-$TOOLCHAIN_DIR/c/vendor/wasi-sdk}"
DEST_DIR="${DEST_DIR:-$AGENTOS_ROOT/software/codex/wasm}"
TOOLCHAIN="${TOOLCHAIN:-nightly-2026-03-01}"
CODEX_GIT_BASE="${CODEX_GIT_BASE:-https://github.com}"
STOP_AFTER="${STOP_AFTER:-}"

# --- preflight ---------------------------------------------------------------
[ -f "$CODEX_REF_FILE" ] || { echo "ERROR: pin file not found: $CODEX_REF_FILE" >&2; exit 1; }
[ -x "$WASI_SDK_DIR/bin/clang" ] || {
	echo "ERROR: wasi-sdk clang not found at $WASI_SDK_DIR/bin/clang" >&2
	echo "       Run: make -C toolchain/c wasi-sdk" >&2
	exit 1
}
command -v cargo >/dev/null 2>&1 || { echo "ERROR: cargo not on PATH" >&2; exit 1; }

# --- 1. parse the pin --------------------------------------------------------
REF="$(tr -d '[:space:]' < "$CODEX_REF_FILE")"
REPO="${REF%@*}"      # owner/repo
SHA="${REF#*@}"       # commit sha
[ -n "$REPO" ] && [ -n "$SHA" ] && [ "$REPO" != "$SHA" ] || {
	echo "ERROR: malformed codex-ref '$REF' (expected '<owner>/<repo>@<sha>')" >&2; exit 1; }
URL="$CODEX_GIT_BASE/$REPO.git"
PATCH_DIGEST="$({ find "$CODEX_PATCH_DIR" -maxdepth 1 -type f -name '*.patch' -print0 | sort -z | xargs -0 sha256sum; } | sha256sum | cut -d ' ' -f1)"
EXPECTED_STAMP="$SHA:$PATCH_DIGEST"

echo "== codex-ref: $REPO @ $SHA =="
echo "== clone url: $URL =="
echo "== scratch:   $BUILD_ROOT =="

# --- 2. shallow clone at the exact SHA (idempotent by SHA) -------------------
STAMP="$CHECKOUT/.codex-built-sha"
PATCHES_ALREADY_STAMPED=0
if [ -f "$STAMP" ] && [ "$(cat "$STAMP" 2>/dev/null)" = "$EXPECTED_STAMP" ]; then
	PATCHES_ALREADY_STAMPED=1
	echo "== reusing existing patched clone at $SHA =="
else
	echo "== fetching fork at $SHA =="
	rm -rf "$CHECKOUT"
	mkdir -p "$CHECKOUT"
	git -C "$CHECKOUT" init -q
	git -C "$CHECKOUT" remote add origin "$URL"
	# Prefer a shallow fetch of the exact commit (GitHub allows reachable SHA-1
	# wants). Fall back to an unshallow fetch if the host rejects arbitrary SHAs.
	if ! git -C "$CHECKOUT" fetch --depth 1 origin "$SHA" 2>/dev/null; then
		echo "   (shallow SHA fetch unsupported; fetching history)"
		git -C "$CHECKOUT" fetch origin
	fi
	git -C "$CHECKOUT" checkout -q "$SHA"
fi
[ -f "$WORKSPACE/Cargo.toml" ] || {
	echo "ERROR: expected cargo workspace at $WORKSPACE/Cargo.toml" >&2; exit 1; }

echo "== applying Codex session-turn source patches =="
if [ "$PATCHES_ALREADY_STAMPED" = "1" ]; then
	echo "   source patch digest already applied: $PATCH_DIGEST"
else
	for patch_file in "$CODEX_PATCH_DIR"/*.patch; do
		[ -e "$patch_file" ] || continue
		patch -p1 -d "$WORKSPACE" < "$patch_file"
	done
fi
grep -q 'Config::load_default_with_cli_overrides' \
	"$WORKSPACE/exec/src/session_turn_wasi.rs" || {
	echo "ERROR: Codex session-turn config isolation patch is missing" >&2
	exit 1
}
printf '%s\n' "$EXPECTED_STAMP" > "$STAMP"

# --- 3. rewrite local fork overrides to reproducible agentOS adapters --------
echo "== rewriting local Codex overrides -> reproducible agentOS adapters =="
python3 - "$WORKSPACE/Cargo.toml" "$STUBS" <<'PY'
import re, sys
cargo, stubs = sys.argv[1], sys.argv[2]
lines = open(cargo).read().split('\n')
# Remove the fork developer's machine-local paths. reqwest is the trusted
# sidecar HTTP boundary. portable-pty is a thin adapter over agentOS's real
# POSIX PTY/process host imports. The stale ctrlc path override is removed;
# this build does not depend on ctrlc. Tokio resolves through vendored+patched
# sources.
OWNED = ('portable-pty', 'reqwest', 'ctrlc', 'tokio')
owned_re = re.compile(r'^\s*#?\s*(?:%s)\s*=' % '|'.join(map(re.escape, OWNED)))
inject = [
    '# [patch.crates-io] injected by clone-and-build-codex-wasi.sh',
    'portable-pty = {{ path = "{}/portable-pty-wasi" }}'.format(stubs),
    'reqwest = {{ path = "{}/reqwest-shim" }}'.format(stubs),
    '# tokio: vendored and patched from the pinned lockfile, not a path patch',
]
injected_comment_re = re.compile(
    r'^# (?:\[patch\.crates-io\] injected by clone-and-build-codex-wasi\.sh|tokio: vendored)'
)
in_patch = False
injected = False
out = []
for ln in lines:
    s = ln.strip()
    if s.startswith('[') and s.endswith(']'):
        in_patch = (s == '[patch.crates-io]')
        out.append(ln)
        if in_patch:                    # inject our lines right after the header
            out.extend(inject); injected = True
        continue
    if in_patch and owned_re.match(ln):  # drop any prior line for an owned crate
        continue
    if in_patch and injected_comment_re.match(ln):
        continue
    out.append(ln)
if not injected:                         # no [patch.crates-io] in the fork: add one
    out += ['', '[patch.crates-io]'] + inject
open(cargo, 'w').write('\n'.join(out))
PY
grep -Fq 'codex-network-proxy = { path = "network-proxy" }' "$WORKSPACE/Cargo.toml" || {
	echo "ERROR: Codex must use its pinned network-proxy policy/config crate" >&2
	exit 1
}
grep -Fq "proxy execution and network policy are owned by the trusted agentOS sidecar" \
	"$WORKSPACE/network-proxy/src/proxy.rs" || {
	echo "ERROR: Codex's agentOS proxy boundary must fail explicitly" >&2
	exit 1
}
echo "   --- resulting [patch.crates-io] head ---"
sed -n '/^\[patch.crates-io\]/,/^\[/p' "$WORKSPACE/Cargo.toml" | sed 's/^/   /'

# --- 4. strip the hardcoded -L .../self-contained from .cargo/config.toml ----
CLONE_CARGO_CFG="$WORKSPACE/.cargo/config.toml"
if [ -f "$CLONE_CARGO_CFG" ]; then
	echo "== stripping machine -L from .cargo/config.toml =="
	python3 - "$CLONE_CARGO_CFG" <<'PY'
import re, sys
p = sys.argv[1]
s = open(p).read()
# Drop the "-C", "link-arg=-L<abs>/self-contained" pair anywhere in a rustflags
# array; the self-contained dir is recomputed and passed via RUSTFLAGS at build.
s = re.sub(r'"-C",\s*"link-arg=-L[^"]*self-contained",\s*', '', s)
open(p, 'w').write(s)
PY
fi

# --- 5. vendor the workspace (+ std deps) and apply crate patches ------------
RUST_STD_SRC="$(rustc "+$TOOLCHAIN" --print sysroot)/lib/rustlib/src/rust"
[ -d "$RUST_STD_SRC/library/std" ] || {
	echo "ERROR: rust-src not found at $RUST_STD_SRC (rustup component add rust-src)" >&2; exit 1; }

echo "== cargo vendor (workspace + std library deps for -Z build-std) =="
cd "$WORKSPACE"
mkdir -p .cargo
cargo "+$TOOLCHAIN" vendor \
	--sync "$RUST_STD_SRC/library/std/Cargo.toml" \
	--sync "$RUST_STD_SRC/library/test/Cargo.toml" \
	"$WORKSPACE/vendor" > "$WORKSPACE/.cargo/vendor-sources.toml"

# Append the source-replacement config so cargo builds from the patched vendor/
# tree. Guard against double-append on a reused clone.
if ! grep -q 'source.crates-io' "$CLONE_CARGO_CFG" 2>/dev/null; then
	printf '\n' >> "$CLONE_CARGO_CFG"
	cat "$WORKSPACE/.cargo/vendor-sources.toml" >> "$CLONE_CARGO_CFG"
fi

echo "== applying toolchain/std-patches/crates/* to vendored sources =="
# patch-vendor.sh exits non-zero if ANY crate patch fails to apply. codex's
# dependency graph overlaps the command build's only partially, so some patches
# legitimately do not apply and are BENIGN for codex:
#   - crossterm: codex uses the nornagon crossterm *git fork* (its own wasi
#     support), not the crates.io crossterm the patch targets.
#   - socket2 0.6.4: no longer carries the `not(target_env="p1")` exclusion the
#     patch removes; codex builds against stock socket2 (verified in the fork).
# So don't let its exit code abort us; instead assert the patches codex REQUIRES.
VENDOR_DIR="$WORKSPACE/vendor" "$SCRIPT_DIR/patch-vendor.sh" || \
	echo "   (patch-vendor reported failures; verifying codex-critical patches below)"

# `cargo vendor --sync` can retain a second libc release for build-std. The
# generic patcher selects the unversioned application copy, so apply the same
# agentOS ABI declarations to every versioned libc source Cargo may compile.
for libc_dir in "$WORKSPACE/vendor"/libc-*; do
	[ -d "$libc_dir" ] || continue
	libc_patch="$TOOLCHAIN_DIR/std-patches/crates/libc/0001-wasip1-socket-bindings.patch"
	if patch --dry-run -p1 -d "$libc_dir" < "$libc_patch" >/dev/null 2>&1; then
		patch -p1 -d "$libc_dir" < "$libc_patch" >/dev/null
	elif ! patch --dry-run -R -p1 -d "$libc_dir" < "$libc_patch" >/dev/null 2>&1; then
		echo "ERROR: agentOS libc socket bindings do not apply to $libc_dir" >&2
		exit 1
	fi
	python3 - "$libc_dir/.cargo-checksum.json" <<'PY'
import json, sys
path = sys.argv[1]
with open(path) as source:
    checksum = json.load(source)
checksum["files"] = {}
with open(path, "w") as output:
    json.dump(checksum, output)
PY
done

echo "== verifying codex-critical crate patches applied =="
assert_patched() {  # <file> <needle> <label>
	if grep -Fq -- "$2" "$1" 2>/dev/null; then
		echo "   OK: $3"
	else
		echo "ERROR: required patch missing: $3 ($1)" >&2
		exit 1
	fi
}
if [ -f "$WORKSPACE/vendor/path-dedot/src/lib.rs" ]; then
	assert_patched "$WORKSPACE/vendor/path-dedot/src/lib.rs" \
		'cfg(any(unix, target_family = "wasm"))' 'path-dedot wasi unix-paths'
else
	echo "   SKIP: path-dedot is not in the pinned Codex dependency graph"
fi
assert_patched "$WORKSPACE/vendor/rustls-native-certs/src/lib.rs" \
	'/etc/ssl/certs/ca-certificates.crt' 'rustls-native-certs loads the agentOS VM trust store'
assert_patched "$WORKSPACE/vendor/tokio/src/process/mod.rs" \
	'path = "wasi.rs"' 'tokio wasi-process routing'
[ -f "$WORKSPACE/vendor/tokio/src/process/wasi.rs" ] || {
	echo "ERROR: tokio companion src/process/wasi.rs not installed" >&2; exit 1; }
echo "   OK: tokio wasi.rs companion installed"
assert_patched "$WORKSPACE/vendor/tokio/src/fs/mod.rs" \
	'cfg(not(target_os = "wasi"))' 'tokio wasi filesystem operations run inline'
assert_patched "$WORKSPACE/vendor/tokio/src/runtime/blocking/pool.rs" \
	'let result = func();' 'tokio wasi spawn_blocking runs inline'
assert_patched "$WORKSPACE/vendor/gix-fs/src/capabilities.rs" \
	'executable_bit: true' 'gix-fs probes real executable modes'
assert_patched "$WORKSPACE/vendor/gix-fs/src/symlink.rs" \
	'std::os::wasi::fs::symlink_path' 'gix-fs creates real symlinks'
assert_patched "$WORKSPACE/vendor/mio/src/sys/wasip1/mod.rs" \
	'libc::socket' 'mio uses agentOS POSIX sockets'
assert_patched "$WORKSPACE/vendor/mio/src/net/mod.rs" \
	'cfg(any(unix, target_os = "wasi"))' 'mio exposes agentOS Unix-domain sockets'
assert_patched "$WORKSPACE/vendor/mio/src/sys/wasip1/mod.rs" \
	'pub(crate) mod uds;' 'mio routes Unix-domain sockets to agentOS libc'
assert_patched "$WORKSPACE/vendor/socket2/src/lib.rs" \
	'pub const UNIX: Domain = Domain(sys::AF_UNIX);' 'socket2 exposes AF_UNIX on agentOS'
assert_patched "$WORKSPACE/vendor/tokio/src/macros/cfg.rs" \
	'($($item:item)*) => { $($item)* }' 'tokio exposes evented networking'
assert_patched "$WORKSPACE/vendor/tokio/src/net/unix/mod.rs" \
	'pub mod pipe;' 'tokio exposes agentOS Unix sockets and pipes'
assert_patched "$WORKSPACE/vendor/tokio/src/runtime/io/registration.rs" \
	'pub(crate) fn poll_read_io<R>' 'tokio exposes readiness-driven socket reads'
assert_patched "$WORKSPACE/vendor/tokio/Cargo.toml" \
	'cfg(any(not(target_family = "wasm"), target_os = "wasi"))' 'tokio links socket2 on agentOS'

if [ "$STOP_AFTER" = "vendor" ]; then
	echo ""
	echo "== STOP_AFTER=vendor: verifying codex crates compile past the patched frontier =="
	SELF_CONTAINED="$(rustc "+$TOOLCHAIN" --print sysroot)/lib/rustlib/wasm32-wasip1/lib/self-contained"
	export CC_wasm32_wasip1="$WASI_SDK_DIR/bin/clang"
	export AR_wasm32_wasip1="$WASI_SDK_DIR/bin/llvm-ar"
	export CFLAGS_wasm32_wasip1="--sysroot=$TOOLCHAIN_DIR/c/sysroot -D_WASI_EMULATED_SIGNAL -D_WASI_EMULATED_PTHREAD -D_WASI_EMULATED_MMAN -D_WASI_EMULATED_PROCESS_CLOCKS"
	# Compile-check the crates whose WASI patches this script injects plus their
	# reverse-dependencies, without the full link. path-dedot is no longer in the
	# pinned Codex dependency graph.
	RUSTFLAGS="-C link-arg=-L$SELF_CONTAINED --cfg tokio_unstable" \
		cargo "+$TOOLCHAIN" build --target wasm32-wasip1 -Z build-std \
		-p rustls-native-certs -p tokio -p tonic
	echo "== vendor/patch frontier OK =="
	exit 0
fi

# --- 6. build via the fork's own reproducible build script -------------------
echo "== building codex-exec (fork scripts/build-wasi-codex-exec.sh) =="
AGENTOS_WASI_LIBDIR="$TOOLCHAIN_DIR/c/sysroot/lib/wasm32-wasi"
[ -f "$AGENTOS_WASI_LIBDIR/libc.a" ] || {
	echo "ERROR: patched AgentOS wasi-libc is missing: $AGENTOS_WASI_LIBDIR/libc.a" >&2
	echo "       Run: make -C $TOOLCHAIN_DIR c/sysroot/lib/wasm32-wasi/libc.a" >&2
	exit 1
}
LIBC_DIGEST="$(sha256sum "$AGENTOS_WASI_LIBDIR/libc.a" | cut -d ' ' -f1 | cut -c1-16)"
BUILD_SCRIPT="$WORKSPACE/scripts/build-wasi-codex-exec.sh"
[ -x "$BUILD_SCRIPT" ] || { echo "ERROR: fork build script missing: $BUILD_SCRIPT" >&2; exit 1; }

INSTALL=0 \
TOOLCHAIN="$TOOLCHAIN" \
KEEP_SYSROOT="${KEEP_SYSROOT:-0}" \
WASI_SDK_DIR="$WASI_SDK_DIR" \
AGENTOS_C_SYSROOT="$TOOLCHAIN_DIR/c/sysroot" \
RUSTFLAGS="-C link-self-contained=no -C link-arg=$AGENTOS_WASI_LIBDIR/crt1-command.o -C link-arg=$AGENTOS_WASI_LIBDIR/libc.a -C link-arg=$AGENTOS_WASI_LIBDIR/libwasi-emulated-pthread.a -C link-arg=-L$AGENTOS_WASI_LIBDIR -C metadata=agentos-libc-$LIBC_DIGEST --cfg tokio_unstable" \
	"$BUILD_SCRIPT"

# --- 7. install optimized artifact ------------------------------------------
OUT="$WORKSPACE/target/wasm32-wasip1/release/codex-exec.opt.wasm"
[ -f "$OUT" ] || { echo "ERROR: expected build output missing: $OUT" >&2; exit 1; }
mkdir -p "$DEST_DIR"
cp "$OUT" "$DEST_DIR/codex-exec"
cp "$OUT" "$DEST_DIR/codex"
echo "== installed $(wc -c < "$OUT") bytes to $DEST_DIR/{codex-exec,codex} =="
echo "DONE"
