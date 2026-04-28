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
  # BOTH knobs are required:
  #   DUCKDB_EXTENSION_CONFIGS — tells DuckDB to register/build the
  #     out-of-tree httpfs extension (loads sources via FetchContent
  #     OR via DUCKDB_HTTPFS_DIRECTORY env var)
  #   BUILD_EXTENSIONS — tells duckdb-python's loader cmake to LINK
  #     the resulting httpfs_extension.a into _duckdb.so. Without this
  #     the template instantiation references HttpfsExtension's vtable
  #     but the lib is never linked → load fails with
  #     "bad export type for _ZTVN6duckdb15HttpfsExtensionE: undefined"
  HTTPFS_FETCH_LIB="$HTTPFS_LOCAL_DIR/src/httpfs_fetch_lib.js"
  EXTRA_CMAKE_ARGS="-DDUCKDB_EXTENSION_CONFIGS=$HTTPFS_CONFIG_PATH -DBUILD_EXTENSIONS=core_functions;json;parquet;icu;httpfs -DDUCKDB_HTTPFS_FETCH_LIB=$HTTPFS_FETCH_LIB"
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
# httpfs's emscripten HTTP client uses emscripten_fetch (synchronous mode
# via ASYNCIFY). FETCH=1 enables the fetch API; ASYNCIFY=1 lets sync
# wrappers around async JS fetch work on the main thread (Pyodide-on-Node
# runs in main thread of the worker). Adding these only when building
# WITH httpfs — bootstrap (no httpfs) doesn't need them and avoids the
# ~2x binary growth.
if [ "${HTTPFS_PROBE:-0}" = "1" ] || [ "${RECIPE_PATCHES_OPTIONAL:-0}" = "0" ]; then
  # JSPI (JavaScript Promise Integration) — wasm calls Promise-returning
  # JS imports and suspends until they resolve, via WebAssembly.Suspending.
  # No Asyncify runtime needed (which Pyodide doesn't expose).
  export LDFLAGS="-fwasm-exceptions -sJSPI=1"
else
  export LDFLAGS="-fwasm-exceptions"
fi
export OVERRIDE_GIT_DESCRIBE="$DUCKDB_REF"
# Pin pyodide-build to use the xbuildenv matching the Pyodide runtime
# version the wheel will run under (else it defaults to a newer xbuildenv
# whose ABI tag — `pyemscripten_2025_0_wasm32` — micropip rejects against
# Pyodide 0.29.3 because the underlying Emscripten differs).
PYODIDE_VERSION="${PYODIDE_VERSION:-0.29.3}"
export DEFAULT_CROSS_BUILD_ENV_URL="https://github.com/pyodide/pyodide/releases/download/${PYODIDE_VERSION}/xbuildenv-${PYODIDE_VERSION}.tar.bz2"

# pyodide-build resolves the xbuildenv via PYODIDE_VERSION-specific
# tarball downloads. The Makefile's setup-toolchain step already
# pinned this; we just need pyodide-build to find emcc on PATH.
"$VENV_DIR/bin/pyodide" build --exports=whole_archive

popd >/dev/null

echo "=== Retag wheel to pyodide_2025_0_wasm32 ABI ==="
# pyodide-build 0.34.1 emits the post-rename `pyemscripten_*` ABI tag
# even when xbuildenv is the older Pyodide 0.29.3. The compiled bytes
# are bit-identical to a `pyodide_*`-tagged build (same emscripten,
# same scikit-build-core); only the wheel METADATA records the new
# tag, and Pyodide 0.29.3's micropip rejects it. Rewrite the Tag line
# and rename the file so micropip accepts it.
SRC_TAG="cp313-cp313-pyemscripten_2025_0_wasm32"
DST_TAG="cp313-cp313-pyodide_2025_0_wasm32"
PYODIDE_BUILD_OUT_TAG="${PYODIDE_BUILD_OUT_TAG:-$SRC_TAG}"
if [ "$PYODIDE_BUILD_OUT_TAG" != "$DST_TAG" ]; then
  for in_whl in "$SRC_DIR"/dist/*-${PYODIDE_BUILD_OUT_TAG}.whl; do
    [ -f "$in_whl" ] || continue
    base="$(basename "$in_whl" -${PYODIDE_BUILD_OUT_TAG}.whl)"
    out_whl="$SRC_DIR/dist/${base}-${DST_TAG}.whl"
    work="$(mktemp -d)"
    unzip -q "$in_whl" -d "$work"
    sed -i.bak "s|Tag: ${PYODIDE_BUILD_OUT_TAG}|Tag: ${DST_TAG}|" "$work"/*.dist-info/WHEEL
    rm -f "$work"/*.dist-info/WHEEL.bak
    # Re-zip preserving structure. Use python's zipfile to keep
    # deterministic ordering and avoid macOS zip's metadata files.
    python3 -c "
import os, zipfile, sys
src, dst = sys.argv[1], sys.argv[2]
with zipfile.ZipFile(dst, 'w', compression=zipfile.ZIP_DEFLATED) as zf:
    for root, dirs, files in os.walk(src):
        for f in files:
            full = os.path.join(root, f)
            rel = os.path.relpath(full, src)
            zf.write(full, rel)
" "$work" "$out_whl"
    rm -rf "$work" "$in_whl"
    echo "  retagged: $(basename "$out_whl")"
  done
fi

echo "=== Copying wheel to $OUT_DIR ==="
mkdir -p "$OUT_DIR"
# Remove any older pyemscripten/pyodide variants of duckdb so the npm
# package doesn't ship two competing wheels.
rm -f "$OUT_DIR"/duckdb-*-cp313-cp313-py*emscripten*_wasm32.whl
rm -f "$OUT_DIR"/duckdb-*-cp313-cp313-pyodide_*_wasm32.whl
cp "$SRC_DIR"/dist/*.whl "$OUT_DIR/"

echo "=== Done. Built wheels: ==="
ls -lh "$SRC_DIR"/dist/*.whl
