#!/bin/bash
# patch-wasi-libc.sh — Vendor, patch, and build wasi-libc as a custom sysroot
#
# Clones wasi-libc at the commit pinned by wasi-sdk-25, applies patches from
# patches/wasi-libc/ that route POSIX functions through our host_process and
# host_user WASM imports, and builds the patched sysroot.
#
# Usage:
#   ./scripts/patch-wasi-libc.sh [--check] [--reverse]
#
# Options:
#   --check    Dry-run: verify patches apply cleanly without building
#   --reverse  Reverse (unapply) previously applied patches

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
WASMCORE_DIR="$(dirname "$SCRIPT_DIR")"
PATCHES_DIR="$WASMCORE_DIR/patches/wasi-libc"

# wasi-libc commit pinned by wasi-sdk-25's git submodule
WASI_LIBC_COMMIT="574b88da481569b65a237cb80daf9a2d5aeaf82d"
WASI_LIBC_REPO="https://github.com/WebAssembly/wasi-libc.git"
LLVM_PROJECT_TAG="llvmorg-19.1.5"
LLVM_PROJECT_URL="https://github.com/llvm/llvm-project/archive/refs/tags/${LLVM_PROJECT_TAG}.tar.gz"

# Directories
VENDOR_DIR="$WASMCORE_DIR/c/vendor"
WASI_LIBC_DIR="$VENDOR_DIR/wasi-libc"
LLVM_PROJECT_DIR="$VENDOR_DIR/llvm-project"
WASI_SDK_DIR="$VENDOR_DIR/wasi-sdk"
SYSROOT_DIR="$WASMCORE_DIR/c/sysroot"
WASI_LIBC_SRC_DIR="$WASI_LIBC_DIR"
WORKTREE_DIR=""

# Parse arguments
MODE="apply"
for arg in "$@"; do
    case "$arg" in
        --check)
            MODE="check"
            ;;
        --reverse)
            MODE="reverse"
            ;;
        *)
            echo "Unknown argument: $arg"
            echo "Usage: $0 [--check] [--reverse]"
            exit 1
            ;;
    esac
done

# Ensure wasi-sdk is available (needed for building the sysroot)
if [ "$MODE" = "apply" ] && [ ! -d "$WASI_SDK_DIR" ]; then
    echo "ERROR: wasi-sdk not found at $WASI_SDK_DIR"
    echo "Run 'make -C $WASMCORE_DIR/c wasi-sdk' first."
    exit 1
fi

# Clone or verify wasi-libc at pinned commit
if [ ! -d "$WASI_LIBC_DIR" ]; then
    if [ "$MODE" = "check" ]; then
        echo "ERROR: wasi-libc not vendored at $WASI_LIBC_DIR"
        echo "Run '$0' (without --check) to clone and build."
        exit 1
    fi

    echo "=== Cloning wasi-libc at $WASI_LIBC_COMMIT ==="
    mkdir -p "$VENDOR_DIR"
    git clone "$WASI_LIBC_REPO" "$WASI_LIBC_DIR"
    git -C "$WASI_LIBC_DIR" checkout "$WASI_LIBC_COMMIT"
    echo ""
else
    # Verify we're at the expected commit
    CURRENT_COMMIT="$(git -C "$WASI_LIBC_DIR" rev-parse HEAD 2>/dev/null || echo "unknown")"
    if [ "$CURRENT_COMMIT" != "$WASI_LIBC_COMMIT" ]; then
        echo "WARNING: wasi-libc is at $CURRENT_COMMIT, expected $WASI_LIBC_COMMIT"
        if [ "$MODE" != "check" ]; then
            echo "Resetting to pinned commit..."
            git -C "$WASI_LIBC_DIR" checkout "$WASI_LIBC_COMMIT"
        fi
    fi
fi

# Fetch llvm-project sources used to rebuild the exception-capable C++ runtime.
if [ ! -d "$LLVM_PROJECT_DIR/runtimes" ]; then
    if [ "$MODE" = "check" ]; then
        echo "ERROR: llvm-project not vendored at $LLVM_PROJECT_DIR"
        echo "Run '$0' (without --check) to fetch the runtime sources."
        exit 1
    fi

    echo "=== Fetching llvm-project at $LLVM_PROJECT_TAG ==="
    mkdir -p "$VENDOR_DIR"
    LLVM_TARBALL="$VENDOR_DIR/${LLVM_PROJECT_TAG}.tar.gz"
    if command -v curl >/dev/null 2>&1; then
        curl -fSL "$LLVM_PROJECT_URL" -o "$LLVM_TARBALL"
    elif command -v wget >/dev/null 2>&1; then
        wget -q "$LLVM_PROJECT_URL" -O "$LLVM_TARBALL"
    else
        echo "ERROR: neither curl nor wget found"
        exit 1
    fi
    rm -rf "$LLVM_PROJECT_DIR"
    mkdir -p "$LLVM_PROJECT_DIR"
    tar -xzf "$LLVM_TARBALL" --strip-components=1 -C "$LLVM_PROJECT_DIR"
    echo ""
fi

cleanup() {
    if [ -n "$WORKTREE_DIR" ] && [ -d "$WORKTREE_DIR" ]; then
        git -C "$WASI_LIBC_DIR" worktree remove --force "$WORKTREE_DIR" >/dev/null 2>&1 || true
    fi
}

trap cleanup EXIT

if [ "$MODE" = "apply" ] || [ "$MODE" = "check" ]; then
    WORKTREE_DIR="$(mktemp -d "${TMPDIR:-/tmp}/wasi-libc-worktree.XXXXXX")"
    rm -rf "$WORKTREE_DIR"
    git -C "$WASI_LIBC_DIR" worktree add --detach "$WORKTREE_DIR" "$WASI_LIBC_COMMIT" >/dev/null 2>&1
    WASI_LIBC_SRC_DIR="$WORKTREE_DIR"
fi

# Find patch files
if [ "$MODE" = "reverse" ]; then
    mapfile -t PATCH_FILES < <(find "$PATCHES_DIR" -name '*.patch' -type f 2>/dev/null | sort -r)
else
    mapfile -t PATCH_FILES < <(find "$PATCHES_DIR" -name '*.patch' -type f 2>/dev/null | sort)
fi

if [ "${#PATCH_FILES[@]}" -eq 0 ]; then
    echo "No patch files found in $PATCHES_DIR"
    if [ "$MODE" = "apply" ]; then
        echo "Building vanilla (unpatched) sysroot..."
    else
        exit 0
    fi
else
    PATCH_COUNT="${#PATCH_FILES[@]}"
    echo "Found $PATCH_COUNT patch(es) in $PATCHES_DIR"
    echo "wasi-libc source: $WASI_LIBC_SRC_DIR"
    echo ""

    apply_patch_series_entry() {
        local patch="$1"
        local success_message="$2"

        if git -C "$WASI_LIBC_SRC_DIR" apply --check "$patch" >/dev/null 2>&1; then
            git -C "$WASI_LIBC_SRC_DIR" apply "$patch"
            echo "$success_message"
            return 0
        fi

        if git -C "$WASI_LIBC_SRC_DIR" apply --reverse --check "$patch" >/dev/null 2>&1; then
            echo "OK (already applied)"
            return 0
        fi

        echo "FAIL (does not apply)"
        return 1
    }

    reverse_patch_series_entry() {
        local patch="$1"

        if git -C "$WASI_LIBC_SRC_DIR" apply --reverse --check "$patch" >/dev/null 2>&1; then
            git -C "$WASI_LIBC_SRC_DIR" apply --reverse "$patch"
            echo "reversed"
            return 0
        fi

        if git -C "$WASI_LIBC_SRC_DIR" apply --check "$patch" >/dev/null 2>&1; then
            echo "not applied (skipping)"
            return 0
        fi

        echo "FAIL (cannot reverse cleanly)"
        return 1
    }

    for PATCH in "${PATCH_FILES[@]}"; do
        PATCH_NAME="$(basename "$PATCH")"

        case "$MODE" in
            check)
                echo -n "Checking $PATCH_NAME ... "
                apply_patch_series_entry "$PATCH" "OK (applies cleanly)" || exit 1
                ;;
            apply)
                echo -n "Applying $PATCH_NAME ... "
                apply_patch_series_entry "$PATCH" "applied" || exit 1
                ;;
            reverse)
                echo -n "Reversing $PATCH_NAME ... "
                reverse_patch_series_entry "$PATCH" || exit 1
                ;;
        esac
    done

    echo ""
    case "$MODE" in
        check)   echo "All patches verified."; exit 0 ;;
        reverse) echo "All patches reversed."; exit 0 ;;
    esac
fi

# Build the sysroot (only in apply mode)
echo ""
echo "=== Building patched wasi-libc sysroot ==="

# wasi-sdk tools
WASI_CC="$WASI_SDK_DIR/bin/clang"
WASI_AR="$WASI_SDK_DIR/bin/llvm-ar"
WASI_NM="$WASI_SDK_DIR/bin/llvm-nm"

if [ ! -x "$WASI_CC" ]; then
    echo "ERROR: wasi-sdk clang not found at $WASI_CC"
    exit 1
fi

# Clean previous build artifacts and sysroot for a reproducible build
make -C "$WASI_LIBC_SRC_DIR" clean 2>/dev/null || true
rm -rf "$SYSROOT_DIR"

# Build wasi-libc with wasi-sdk's tools, output to our sysroot directory.
# Build the `libc` target (headers + static libraries) but NOT `finish`, which
# runs check-symbols and fails because our patches add custom undefined symbols
# (__host_*) not in the upstream expected-symbols list.
make -C "$WASI_LIBC_SRC_DIR" \
    CC="$WASI_CC" \
    AR="$WASI_AR" \
    NM="$WASI_NM" \
    SYSROOT="$SYSROOT_DIR" \
    libc \
    -j"$(nproc 2>/dev/null || echo 4)"

# Install CRT startup files (crt1.o etc.) from the vanilla wasi-sdk sysroot.
# CRT objects are standard startup routines that don't need our patches.
SYSROOT_LIB="$SYSROOT_DIR/lib/wasm32-wasi"
VANILLA_LIB="$WASI_SDK_DIR/share/wasi-sysroot/lib/wasm32-wasi"
for crt in "$VANILLA_LIB"/crt*.o; do
    [ -f "$crt" ] && cp "$crt" "$SYSROOT_LIB/"
done

# Install the wasi-sdk libc++ runtime into the patched sysroot so upstream C++
# projects can target the same sysroot we use for libc. We overlay the
# thread-capable headers/libs from wasm32-wasi-threads because libc++'s mutex
# support expects those definitions even when we satisfy pthread calls through
# wasi-emulated-pthread.
VANILLA_INCLUDE="$WASI_SDK_DIR/share/wasi-sysroot/include/wasm32-wasi"
THREADS_INCLUDE="$WASI_SDK_DIR/share/wasi-sysroot/include/wasm32-wasi-threads"
SYSROOT_INCLUDE="$SYSROOT_DIR/include/wasm32-wasi"
mkdir -p "$SYSROOT_INCLUDE/c++/v1"
if [ -d "$VANILLA_INCLUDE/c++/v1" ]; then
    cp -R "$VANILLA_INCLUDE/c++/v1/." "$SYSROOT_INCLUDE/c++/v1/"
fi
if [ -d "$THREADS_INCLUDE/c++/v1" ]; then
    cp -R "$THREADS_INCLUDE/c++/v1/." "$SYSROOT_INCLUDE/c++/v1/"
fi

for runtime in libc++.a libc++.modules.json libc++.so libc++abi.a libc++abi.so libc++experimental.a; do
    [ -f "$VANILLA_LIB/$runtime" ] && cp "$VANILLA_LIB/$runtime" "$SYSROOT_LIB/"
done
THREADS_LIB="$WASI_SDK_DIR/share/wasi-sysroot/lib/wasm32-wasi-threads"
for runtime in libc++.a libc++abi.a libc++experimental.a; do
    [ -f "$THREADS_LIB/$runtime" ] && cp "$THREADS_LIB/$runtime" "$SYSROOT_LIB/"
done

# Rebuild the C++ runtime with Wasm EH enabled so upstream C++ projects can use
# exceptions against the same patched WASI/POSIX sysroot. We also replace the
# libc++ headers with the rebuilt install so the header ABI namespace matches
# the custom libc++/libc++abi archives we overlay into the sysroot.
LLVM_RUNTIME_BUILD_SCRIPT="$WASMCORE_DIR/c/scripts/build-llvm-runtimes.sh"
LLVM_RUNTIME_BUILD_DIR="$WASMCORE_DIR/c/build/llvm-runtimes"
LLVM_RUNTIME_INSTALL_DIR="$WASMCORE_DIR/c/build/llvm-runtimes-install"
echo "Rebuilding libc++/libc++abi/libunwind with -fwasm-exceptions..."
LLVM_PROJECT_SRC_DIR="$LLVM_PROJECT_DIR" \
LLVM_RUNTIME_BUILD_DIR="$LLVM_RUNTIME_BUILD_DIR" \
LLVM_RUNTIME_INSTALL_DIR="$LLVM_RUNTIME_INSTALL_DIR" \
WASI_SDK_DIR="$WASI_SDK_DIR" \
SYSROOT_DIR="$SYSROOT_DIR" \
bash "$LLVM_RUNTIME_BUILD_SCRIPT"

# Create empty dummy libraries (libm, librt, libpthread, etc.)
for lib in m rt pthread crypt util xnet resolv; do
    "$WASI_AR" crs "$SYSROOT_LIB/lib${lib}.a" 2>/dev/null || true
done

echo ""
echo "=== Sysroot build complete ==="

# Verify the build output
if [ -f "$SYSROOT_DIR/lib/wasm32-wasi/libc.a" ]; then
    echo "OK: $SYSROOT_DIR/lib/wasm32-wasi/libc.a exists"
else
    echo "ERROR: libc.a not found in sysroot — build may have failed"
    exit 1
fi

# Remove libc objects that conflict with host_socket.o.
# Our socket patch replaces these entry points with host_net-backed versions.
"$WASI_AR" d "$SYSROOT_LIB/libc.a" accept-wasip1.o send.o recv.o select.o poll.o 2>/dev/null || true
echo "Removed conflicting accept-wasip1.o/send.o/recv.o/select.o/poll.o from libc.a"

# Remove musl's original signal entry points so host_sigaction.o is the only
# resolver for sigaction()/signal() in the patched sysroot.
"$WASI_AR" d "$SYSROOT_LIB/libc.a" sigaction.o signal.o 2>/dev/null || true
echo "Removed conflicting sigaction.o/signal.o from libc.a"

# wasi-libc builds under wasm32-wasi, but clang --target=wasm32-wasip1 expects
# wasm32-wasip1 subdirectories. Create symlinks so both targets work.
for subdir in include lib; do
    if [ -d "$SYSROOT_DIR/$subdir/wasm32-wasi" ] && [ ! -e "$SYSROOT_DIR/$subdir/wasm32-wasip1" ]; then
        ln -s wasm32-wasi "$SYSROOT_DIR/$subdir/wasm32-wasip1"
        echo "Symlink: $subdir/wasm32-wasip1 -> wasm32-wasi"
    fi
done

# === Install sysroot overrides ===
# Override files in patches/wasi-libc-overrides/ fix broken libc behavior
# (fcntl, sched_getcpu, strfmon, open_wmemstream, swprintf, inet_ntop,
# pthread_attr, pthread_mutex, pthread_key, fmtmsg).
# The patched sysroot also provides host_sigaction.o, which must replace musl's
# original sigaction.o / signal.o so cooperative signal registration flows
# through the host_process import instead of the upstream rt_sigaction stub.
# realloc is handled by 0009-realloc-glibc-semantics.patch directly.
# Overrides are compiled and added to libc.a so ALL WASM programs get the fixes.
OVERRIDES_DIR="$WASMCORE_DIR/patches/wasi-libc-overrides"
OVERRIDE_INCLUDE_DIR="$WASMCORE_DIR/c/include"
OVERRIDE_CFLAGS="--target=wasm32-wasip1 --sysroot=$SYSROOT_DIR -O2 -D_GNU_SOURCE -I$OVERRIDE_INCLUDE_DIR"

# Extra flags for overrides that need musl internal headers (struct __pthread, etc.)
MUSL_INTERNAL_DIR="$WASI_LIBC_SRC_DIR/libc-top-half/musl/src/internal"
MUSL_ARCH_DIR="$WASI_LIBC_SRC_DIR/libc-top-half/musl/arch/wasm32"
OVERRIDE_INTERNAL_CFLAGS="-I$MUSL_INTERNAL_DIR -I$MUSL_ARCH_DIR"

if [ -d "$OVERRIDES_DIR" ] && ls "$OVERRIDES_DIR"/*.c >/dev/null 2>&1; then
    echo ""
    echo "=== Installing sysroot overrides ==="

    # Helper: extract .o member name from llvm-nm --print-file-name output.
    # Format: "/path/to/libc.a:member.o: 00000000 T symbol"
    extract_obj() {
        sed 's/.*:\([^:]*\.o\):.*/\1/'
    }

    # Remove original .o files for symbols we're replacing outright.
    # These functions live in their own .o files (one function per file in musl).
    # Note: strfmon.o contains both strfmon and strfmon_l — only need to remove once.
    # pthread_mutex: all 5 functions (lock, trylock, timedlock, unlock, consistent)
    # are in a single mutex.o — remove it so our override replaces them all.
    # pthread_key: create, delete, and tsd_run_dtors are in a single .o — remove
    # via __pthread_key_create to replace the whole TSD compilation unit.
    for sym in fcntl strfmon open_wmemstream swprintf inet_ntop __pthread_mutex_lock pthread_attr_setguardsize pthread_mutexattr_setrobust __pthread_key_create fmtmsg; do
        OBJ_LINE=$("$WASI_NM" --print-file-name "$SYSROOT_LIB/libc.a" 2>/dev/null | { grep " [TW] ${sym}\$" || true; } | head -1)
        if [ -n "$OBJ_LINE" ]; then
            OBJ=$(echo "$OBJ_LINE" | extract_obj)
            if [ -n "$OBJ" ]; then
                echo "  Removing original $OBJ (provides $sym)"
                "$WASI_AR" d "$SYSROOT_LIB/libc.a" "$OBJ" 2>/dev/null || true
            fi
        fi
    done

    # Compile each override and add to libc.a
    for src in "$OVERRIDES_DIR"/*.c; do
        name="$(basename "${src%.c}")"
        EXTRA_FLAGS=""
        # pthread_key needs musl internal headers for struct __pthread
        case "$name" in
            pthread_key) EXTRA_FLAGS="$OVERRIDE_INTERNAL_CFLAGS" ;;
        esac
        echo "  Compiling override: $name"
        "$WASI_CC" $OVERRIDE_CFLAGS $EXTRA_FLAGS -c "$src" -o "$SYSROOT_LIB/override_${name}.o"
        "$WASI_AR" r "$SYSROOT_LIB/libc.a" "$SYSROOT_LIB/override_${name}.o"
        rm -f "$SYSROOT_LIB/override_${name}.o"
    done

    echo "Sysroot overrides installed"
fi

echo "Patched sysroot installed to: $SYSROOT_DIR"
