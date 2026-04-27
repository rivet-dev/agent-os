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
# ICU's putilimp.h falls back to declaring `extern` references to
# tzset() / timezone / tzname when the platform isn't Win32/iOS/etc.
# The patched WASI sysroot doesn't expose those symbols. Defining
# U_HAVE_* short-circuits the fallback, leaving uprv_tzset() a no-op
# and routing uprv_timezone() through localtime_r/mktime (which we
# do support — VM is permanently UTC, so it returns 0 cleanly).
ICU_TZ_FLAGS="-DU_HAVE_TZSET=1 -DU_HAVE_TIMEZONE=1 -DU_HAVE_TZNAME=1"
COMMON_FLAGS="$COMMON_FLAGS $ICU_TZ_FLAGS"
# Bundled extensions: parquet (read .parquet files), json (read/write JSON
# data), icu (date/time conversions; uses our timezone.c override since the
# VM is permanently UTC), core_functions (additional builtins).
# httpfs is intentionally excluded — its OpenSSL+CURL hard dependency requires
# a WASM-compiled mbedtls/curl that we don't ship in this sysroot. Server-side
# DuckDB (`@duckdb/node-api` in the converter actor) handles S3 reads.
COMMON_CXX_FLAGS="$COMMON_FLAGS -DSQLITE_NOHAVE_SYSTEM -DSQLITE_OMIT_POPEN -fwasm-exceptions -DWEBDB_FAST_EXCEPTIONS=1"
CXX_STDLIB_INCLUDE="$SYSROOT_DIR/include/wasm32-wasi/c++/v1"

if [ ! -d "$CXX_STDLIB_INCLUDE" ]; then
  echo "missing libc++ headers at $CXX_STDLIB_INCLUDE" >&2
  exit 1
fi

# Prefer GNU patch (gpatch on macOS) — see comment in build-llvm-runtimes.sh.
PATCH_TOOL="$(command -v gpatch 2>/dev/null || command -v patch)"

if [ -d "$PATCH_DIR" ]; then
  while IFS= read -r patch_file; do
    # `-N` (forward-only) makes gpatch SKIP already-applied patches instead of
    # silently reversing them.
    if "$PATCH_TOOL" --dry-run -N --batch -p1 -d "$DUCKDB_SRC_DIR" < "$patch_file" >/dev/null 2>&1; then
      "$PATCH_TOOL" -N --batch --no-backup-if-mismatch -p1 -d "$DUCKDB_SRC_DIR" < "$patch_file" >/dev/null
      find "$DUCKDB_SRC_DIR" -name '*.rej' -delete 2>/dev/null || true
    elif "$PATCH_TOOL" --dry-run --batch -R -p1 -d "$DUCKDB_SRC_DIR" < "$patch_file" >/dev/null 2>&1; then
      :
    else
      echo "failed to apply DuckDB patch: $patch_file" >&2
      exit 1
    fi
  done < <(find "$PATCH_DIR" -name '*.patch' -type f | sort)
fi

mkdir -p "$DUCKDB_BUILD_DIR"

# httpfs WASI patches: applied directly to the FetchContent-populated src
# rather than via DuckDB's APPLY_PATCHES mechanism. The latter calls
# scripts/apply_extension_patches.py which expects a .git inside the
# fetched dir; FetchContent's default ExternalProject_Add doesn't keep
# one, so the python script aborts on `git diff`. We instead do a 2-pass
# configure: first pass populates the source (failing later at OpenSSL
# find), then we patch in-place, then the second pass passes through.
HTTPFS_PATCH_SRC="$SCRIPT_DIR/../patches/httpfs"
HTTPFS_FETCHED_SRC="$DUCKDB_BUILD_DIR/_deps/httpfs_extension_fc-src"

run_cmake_configure() {
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
    -DCMAKE_EXE_LINKER_FLAGS="-lwasi-emulated-mman -lwasi-emulated-signal -lwasi-emulated-process-clocks" \
    -DBUILD_UNITTESTS=0 \
    -DENABLE_UNITTEST_CPP_TESTS=0 \
    -DBUILD_BENCHMARKS=0 \
    -DENABLE_SANITIZER=0 \
    -DENABLE_UBSAN=0 \
    -DDISABLE_THREADS=1 \
    -DSMALLER_BINARY=1 \
    -DBUILD_EXTENSIONS="core_functions;parquet;json;icu;httpfs" \
    -DSKIP_EXTENSIONS="jemalloc" \
    -DDUCKDB_EXPLICIT_PLATFORM=wasm32-wasip1-posix \
    -DOVERRIDE_GIT_DESCRIBE="$DUCKDB_GIT_DESCRIBE"
}

# Pass 1: populate FetchContent dirs. Will fail at find_package(OpenSSL)
# in httpfs/CMakeLists.txt — that's fine, our patch fixes that and the
# second pass goes clean.
if [ ! -d "$HTTPFS_FETCHED_SRC" ]; then
  run_cmake_configure || true
fi

# Apply our httpfs patches in-place (gpatch -N skips already-applied).
if [ -d "$HTTPFS_PATCH_SRC" ] && [ -d "$HTTPFS_FETCHED_SRC" ]; then
  for patch_file in "$HTTPFS_PATCH_SRC"/*.patch; do
    [ -f "$patch_file" ] || continue
    "$PATCH_TOOL" -N --batch --no-backup-if-mismatch -p1 -d "$HTTPFS_FETCHED_SRC" < "$patch_file" || true
    find "$HTTPFS_FETCHED_SRC" -name '*.rej' -delete 2>/dev/null || true
  done
fi

# Pass 2: configure with patches applied. This must succeed.
run_cmake_configure

cmake --build "$DUCKDB_BUILD_DIR" --target shell -j"$(nproc 2>/dev/null || echo 4)"
cp "$DUCKDB_BUILD_DIR/duckdb" "$DUCKDB_OUTPUT"
