#!/usr/bin/env bash
# Build the DuckDB Pyodide wheel from source, with httpfs+TLS.
#
# Replaces the xlwings prebuilt wheel download (fetch_duckdb.sh) with
# a from-source build so we can enable BUILD_HTTPFS_EXTENSION=ON.
#
# Args:
#   $1 = venv dir (with pyodide-build installed)
#   $2 = emsdk dir
#   $3 = output wheels dir
#
# Usage from Makefile:
#   bash scripts/build_duckdb_pyodide.sh \
#     "$(VENV_DIR)" "$(EMSDK_DIR)" "$(WHEELS_DIR)"
#
# Strategy:
#   1. Clone duckdb-python at v$(DUCKDB_VERSION) with submodules.
#   2. Apply the wasm-ld export patch (xlwings's known-working tweak).
#   3. Apply our two new patches (cmake httpfs+mbedtls, emscripten
#      socket adapter for httplib).
#   4. Run `pyodide build --exports=whole_archive` with the same env
#      xlwings uses, plus BUILD_HTTPFS_EXTENSION=ON.
#   5. Copy the built wheel into the output dir.
#
# First-run mode: if RECIPE_PATCHES_OPTIONAL=1 in env, missing patches
# don't block the build. Use during the bootstrap phase when verifying
# the baseline (no-httpfs) pipeline produces a working wheel before
# layering httpfs on top.

set -euo pipefail

VENV_DIR="${1:?missing venv dir}"
EMSDK_DIR="${2:?missing emsdk dir}"
OUT_DIR="${3:?missing output dir}"

# Make paths absolute — the build pushd's into the cloned source dir
# and relative paths to the venv / out-dir would no longer resolve.
to_abs() { python3 -c "import os, sys; print(os.path.abspath(sys.argv[1]))" "$1"; }
VENV_DIR="$(to_abs "$VENV_DIR")"
EMSDK_DIR="$(to_abs "$EMSDK_DIR")"
OUT_DIR="$(to_abs "$OUT_DIR")"

# Pin must match Makefile:DUCKDB_VERSION (without the "v" prefix —
# we add it for the git ref). Keep in sync.
DUCKDB_VERSION="${DUCKDB_VERSION:-1.5.0}"
DUCKDB_REF="v${DUCKDB_VERSION}"

WORK=".build-cache/duckdb"
RECIPE_DIR="recipes/duckdb"
PATCH_DIR="$RECIPE_DIR/patches"

rm -rf "$WORK"
mkdir -p "$WORK"

echo "=== Cloning duckdb-python at $DUCKDB_REF ==="
git clone \
  --recurse-submodules \
  --shallow-submodules \
  https://github.com/duckdb/duckdb-python.git \
  "$WORK/duckdb-python"
git -C "$WORK/duckdb-python" checkout "$DUCKDB_REF"
git -C "$WORK/duckdb-python" submodule update --init --recursive

SRC_DIR="$WORK/duckdb-python"

echo "=== Showing duckdb-python version ==="
git -C "$SRC_DIR" log -1 --format='%H %s'

echo "=== Applying patches ==="
applied=0
skipped=0
if [ -d "$PATCH_DIR" ]; then
  while IFS= read -r patch_file; do
    name="$(basename "$patch_file")"
    if [ ! -s "$patch_file" ]; then
      if [ "${RECIPE_PATCHES_OPTIONAL:-0}" = "1" ]; then
        echo "  skip $name (empty placeholder, RECIPE_PATCHES_OPTIONAL=1)"
        skipped=$((skipped + 1))
        continue
      else
        echo "ERROR: $patch_file is empty. Either populate it or set RECIPE_PATCHES_OPTIONAL=1." >&2
        exit 1
      fi
    fi
    echo "  applying $name"
    patch -d "$SRC_DIR" -p1 < "$patch_file"
    applied=$((applied + 1))
  done < <(find "$PATCH_DIR" -name '*.patch' -type f | sort)
fi
echo "  $applied applied, $skipped skipped"

# Three build modes:
#   1. bootstrap (RECIPE_PATCHES_OPTIONAL=1, HTTPFS_PROBE=0) — no httpfs,
#      no patches required. Verifies the end-to-end pipeline.
#   2. probe    (RECIPE_PATCHES_OPTIONAL=1, HTTPFS_PROBE=1) — enables
#      httpfs cmake target with pre-cloned source but NO source patches;
#      used to surface what specific cmake/compile errors need patching.
#   3. full     (RECIPE_PATCHES_OPTIONAL=0) — all patches applied, full
#      httpfs build with TLS via mbedtls.
#
# httpfs is enabled via DUCKDB_EXTENSION_CONFIGS pointing at the upstream
# httpfs.cmake. NOT -DBUILD_HTTPFS_EXTENSION=ON — that flag doesn't
# actually exist in upstream DuckDB; xlwings's docs were misleading.
#
# Path must be ABSOLUTE: pyodide-build runs cmake from a tmpdir, and
# the include() in extension_build_tools.cmake resolves the path
# relative to CMAKE_CURRENT_SOURCE_DIR (which is the tmpdir at
# config time, not our source dir).
#
# httpfs source patching: upstream's register_external_extension macro
# (extension/extension_build_tools.cmake:325) honors the env var
# DUCKDB_<NAME>_DIRECTORY — when set, it skips FetchContent and uses
# that path as the extension source. We pre-clone duckdb-httpfs at the
# upstream-pinned tag, apply our patches in-place, and export
# DUCKDB_HTTPFS_DIRECTORY so the build picks up our patched copy. This
# avoids the WASI build's 2-pass cmake gymnastics — pyodide-build runs
# cmake in a fresh tmpdir each invocation, so 2-pass patching is awkward.
HTTPFS_CONFIG_PATH="$(to_abs "$SRC_DIR/external/duckdb/.github/config/extensions/httpfs.cmake")"
HTTPFS_TAG="74f954001f3a740c909181b02259de6c7b942632"
HTTPFS_LOCAL_DIR="$(to_abs "$WORK/duckdb-httpfs")"
HTTPFS_PATCH_DIR="$RECIPE_DIR/httpfs-patches"

prepare_httpfs_source() {
  if [ ! -d "$HTTPFS_LOCAL_DIR/.git" ]; then
    echo "=== Cloning duckdb-httpfs at $HTTPFS_TAG ==="
    git clone https://github.com/duckdb/duckdb-httpfs.git "$HTTPFS_LOCAL_DIR"
    git -C "$HTTPFS_LOCAL_DIR" checkout "$HTTPFS_TAG"
  fi

  if [ -d "$HTTPFS_PATCH_DIR" ]; then
    echo "=== Applying httpfs patches ==="
    # Reset any prior patch state so we can re-apply cleanly.
    git -C "$HTTPFS_LOCAL_DIR" reset --hard "$HTTPFS_TAG"
    for patch_file in $(find "$HTTPFS_PATCH_DIR" -name '*.patch' -type f | sort); do
      [ -s "$patch_file" ] || {
        if [ "${RECIPE_PATCHES_OPTIONAL:-0}" = "1" ]; then
          echo "  skip $(basename "$patch_file") (empty placeholder)"
          continue
        fi
        echo "ERROR: $patch_file is empty." >&2; exit 1
      }
      echo "  applying $(basename "$patch_file")"
      patch -d "$HTTPFS_LOCAL_DIR" -p1 < "$patch_file"
    done
  fi
}

if [ "${RECIPE_PATCHES_OPTIONAL:-0}" = "1" ] && [ "${HTTPFS_PROBE:-0}" != "1" ]; then
  echo "=== bootstrap mode: building WITHOUT httpfs ==="
  EXTRA_CMAKE_ARGS=""
else
  if [ "${HTTPFS_PROBE:-0}" = "1" ]; then
    echo "=== probe mode: building WITH httpfs (no source patches) ==="
  else
    echo "=== full mode: building WITH httpfs + patches ==="
  fi
  prepare_httpfs_source
  export DUCKDB_HTTPFS_DIRECTORY="$HTTPFS_LOCAL_DIR"
  EXTRA_CMAKE_ARGS="-DDUCKDB_EXTENSION_CONFIGS=$HTTPFS_CONFIG_PATH"
fi

echo "=== Activating emsdk ==="
# shellcheck disable=SC1091
source "$EMSDK_DIR/emsdk_env.sh"

echo "=== Building wheel ==="
pushd "$SRC_DIR" >/dev/null

# Match xlwings's exact build env. See:
# https://github.com/xlwings/duckdb-pyodide/blob/main/.github/workflows/build.yml
export DUCKDB_CUSTOM_PLATFORM="wasm_eh_pyodide"
export CMAKE_ARGS="-DDUCKDB_EXPLICIT_PLATFORM=wasm_eh_pyodide $EXTRA_CMAKE_ARGS"
export CFLAGS="-fwasm-exceptions"
export LDFLAGS="-fwasm-exceptions"
export OVERRIDE_GIT_DESCRIBE="$DUCKDB_REF"

# pyodide-build resolves the xbuildenv via PYODIDE_VERSION-specific
# tarball downloads. The Makefile's setup-toolchain step already
# pinned this; we just need pyodide-build to find emcc on PATH.
"$VENV_DIR/bin/pyodide" build --exports=whole_archive

popd >/dev/null

echo "=== Copying wheel to $OUT_DIR ==="
mkdir -p "$OUT_DIR"
cp "$SRC_DIR"/dist/*.whl "$OUT_DIR/"

echo "=== Done. Built wheels: ==="
ls -lh "$SRC_DIR"/dist/*.whl
