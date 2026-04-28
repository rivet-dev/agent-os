#!/bin/bash
set -euo pipefail

# Reference only:
# - https://github.com/duckdb/duckdb-wasm#readme
# - https://github.com/duckdb/duckdb-wasm/blob/main/Makefile
# - https://github.com/duckdb/duckdb-wasm/blob/main/extension_config_wasm.cmake
#
# Unlike duckdb-wasm, we do not use their prebuilt WebAssembly bundles or
# Emscripten runtime shims. This script builds upstream DuckDB directly against
# our patched WASI/POSIX sysroot so file and network operations flow through the
# existing registry host bindings.

: "${DUCKDB_SRC_DIR:?DUCKDB_SRC_DIR is required}"
: "${DUCKDB_BUILD_DIR:?DUCKDB_BUILD_DIR is required}"
: "${DUCKDB_OUTPUT:?DUCKDB_OUTPUT is required}"
: "${WASI_SDK_DIR:?WASI_SDK_DIR is required}"
: "${SYSROOT_DIR:?SYSROOT_DIR is required}"
: "${MODULE_PATH:?MODULE_PATH is required}"
: "${OVERLAY_INCLUDE_DIR:?OVERLAY_INCLUDE_DIR is required}"
: "${DUCKDB_GIT_DESCRIBE:?DUCKDB_GIT_DESCRIBE is required}"

TOOLCHAIN_FILE="$WASI_SDK_DIR/share/cmake/wasi-sdk.cmake"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PATCH_DIR="$SCRIPT_DIR/../patches/duckdb"
COMMON_FLAGS="-I$OVERLAY_INCLUDE_DIR -D_WASI_EMULATED_PTHREAD -D_WASI_EMULATED_MMAN -D_WASI_EMULATED_SIGNAL -D_WASI_EMULATED_PROCESS_CLOCKS"
COMMON_CXX_FLAGS="$COMMON_FLAGS -DDUCKDB_DISABLE_EXTENSION_LOAD -DSQLITE_NOHAVE_SYSTEM -DSQLITE_OMIT_POPEN -fwasm-exceptions -DWEBDB_FAST_EXCEPTIONS=1"
CXX_STDLIB_INCLUDE="$SYSROOT_DIR/include/wasm32-wasi/c++/v1"
RUNTIME_LIB_DIR="$(cd "$(dirname "$SYSROOT_DIR")" && pwd)/build/llvm-runtimes-install/lib"
SHIM_BUILD_DIR="$DUCKDB_BUILD_DIR/agentos-shims"
SHIM_CFLAGS="--target=wasm32-wasip1 --sysroot=$SYSROOT_DIR -isystem $SYSROOT_DIR/include/wasm32-wasi -I$OVERLAY_INCLUDE_DIR -D_GNU_SOURCE"
SHIM_OBJECTS="$SHIM_BUILD_DIR/fcntl.o $SHIM_BUILD_DIR/mlock.o $SHIM_BUILD_DIR/sched.o"

if [ ! -d "$CXX_STDLIB_INCLUDE" ]; then
  echo "missing libc++ headers at $CXX_STDLIB_INCLUDE" >&2
  exit 1
fi
if [ ! -d "$RUNTIME_LIB_DIR" ]; then
  echo "missing rebuilt C++ runtime libraries at $RUNTIME_LIB_DIR" >&2
  exit 1
fi

if [ -d "$PATCH_DIR" ]; then
  while IFS= read -r patch_file; do
    if patch --dry-run -p1 -d "$DUCKDB_SRC_DIR" < "$patch_file" >/dev/null 2>&1; then
      patch --no-backup-if-mismatch -p1 -d "$DUCKDB_SRC_DIR" < "$patch_file" >/dev/null
    elif patch --dry-run -R -p1 -d "$DUCKDB_SRC_DIR" < "$patch_file" >/dev/null 2>&1; then
      :
    else
      echo "failed to apply DuckDB patch: $patch_file" >&2
      exit 1
    fi
  done < <(find "$PATCH_DIR" -name '*.patch' -type f | sort)
fi

mkdir -p "$DUCKDB_BUILD_DIR"
mkdir -p "$SHIM_BUILD_DIR"

"$WASI_SDK_DIR/bin/clang" $SHIM_CFLAGS \
  -c "$SCRIPT_DIR/../../patches/wasi-libc-overrides/fcntl.c" \
  -o "$SHIM_BUILD_DIR/fcntl.o"
"$WASI_SDK_DIR/bin/clang" $SHIM_CFLAGS \
  -c "$SCRIPT_DIR/../../patches/wasi-libc-overrides/mlock.c" \
  -o "$SHIM_BUILD_DIR/mlock.o"
"$WASI_SDK_DIR/bin/clang" $SHIM_CFLAGS \
  -c "$SCRIPT_DIR/../../patches/wasi-libc-overrides/sched.c" \
  -o "$SHIM_BUILD_DIR/sched.o"

cmake \
  -S "$DUCKDB_SRC_DIR" \
  -B "$DUCKDB_BUILD_DIR" \
  -G "Unix Makefiles" \
  -DCMAKE_TOOLCHAIN_FILE="$TOOLCHAIN_FILE" \
  -DWASI_SDK_PREFIX="$WASI_SDK_DIR" \
  -DCMAKE_SYSROOT="$SYSROOT_DIR" \
  -DCMAKE_MODULE_PATH="$MODULE_PATH" \
  -DCMAKE_BUILD_TYPE=Release \
  -DCMAKE_C_FLAGS="$COMMON_FLAGS" \
  -DCMAKE_CXX_FLAGS="$COMMON_CXX_FLAGS -isystem $CXX_STDLIB_INCLUDE" \
  -DCMAKE_EXE_LINKER_FLAGS="$SHIM_OBJECTS -L$RUNTIME_LIB_DIR -fwasm-exceptions -lwasi-emulated-mman -lwasi-emulated-signal -lwasi-emulated-process-clocks" \
  -DCMAKE_SHARED_LINKER_FLAGS="-L$RUNTIME_LIB_DIR -fwasm-exceptions" \
  -DCMAKE_CXX_STANDARD_LIBRARIES="-lc++ -lc++abi -lunwind -lc" \
  -DBUILD_UNITTESTS=0 \
  -DENABLE_UNITTEST_CPP_TESTS=0 \
  -DBUILD_BENCHMARKS=0 \
  -DENABLE_SANITIZER=0 \
  -DENABLE_UBSAN=0 \
  -DDISABLE_THREADS=1 \
  -DSMALLER_BINARY=1 \
  -DDISABLE_EXTENSION_LOAD=1 \
  -DBUILD_EXTENSIONS=core_functions \
  -DSKIP_EXTENSIONS="parquet;jemalloc" \
  -DDUCKDB_EXPLICIT_PLATFORM=wasm32-wasip1-posix \
  -DOVERRIDE_GIT_DESCRIBE="$DUCKDB_GIT_DESCRIBE"

cmake --build "$DUCKDB_BUILD_DIR" --target shell -j"$(nproc 2>/dev/null || echo 4)"
cp "$DUCKDB_BUILD_DIR/duckdb" "$DUCKDB_OUTPUT"
