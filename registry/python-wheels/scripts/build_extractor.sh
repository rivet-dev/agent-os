#!/usr/bin/env bash
# Build the dbt-extractor Pyodide wheel.
#
# Args:
#   $1 = venv dir (with pyodide-build installed)
#   $2 = emsdk dir
#   $3 = output wheels dir
#
# Strategy:
#   1. Vendor tree-sitter-jinja2 (the Pyodide cross-build sandbox can't `git clone`).
#   2. Apply the rayon-emscripten gate patch.
#   3. Run `pyodide build` to produce a cp313 / emscripten wasm32 wheel.
#   4. Copy the wheel into the output dir.

set -euo pipefail

VENV_DIR="${1:?missing venv dir}"
EMSDK_DIR="${2:?missing emsdk dir}"
OUT_DIR="${3:?missing output dir}"

EXTRACTOR_VERSION="0.6.0"
TREE_SITTER_JINJA2_TAG="v0.2.0"

WORK=".build-cache/dbt-extractor"
rm -rf "$WORK"
mkdir -p "$WORK"

echo "=== Downloading dbt-extractor sdist ==="
SDIST_URL="https://files.pythonhosted.org/packages/source/d/dbt-extractor/dbt_extractor-${EXTRACTOR_VERSION}.tar.gz"
curl -fL -o "$WORK/dbt_extractor.tar.gz" "$SDIST_URL"
tar -xzf "$WORK/dbt_extractor.tar.gz" -C "$WORK"
SRC_DIR="$WORK/dbt_extractor-${EXTRACTOR_VERSION}"
[ -d "$SRC_DIR" ] || SRC_DIR="$(echo $WORK/dbt_extractor-*)"

echo "=== Vendoring tree-sitter-jinja2 ==="
mkdir -p "$SRC_DIR/vendor"
git clone --depth 1 --branch "$TREE_SITTER_JINJA2_TAG" \
  https://github.com/dbt-labs/tree-sitter-jinja2 \
  "$SRC_DIR/vendor/tree-sitter-jinja2"
rm -rf "$SRC_DIR/vendor/tree-sitter-jinja2/.git"

echo "=== Applying patches ==="
RECIPE_DIR="recipes/dbt-extractor"
for patch in "$RECIPE_DIR/patches"/*.patch; do
  echo "  applying $patch"
  patch -d "$SRC_DIR" -p1 < "$patch"
done

echo "=== Activating emsdk ==="
# shellcheck disable=SC1091
source "$EMSDK_DIR/emsdk_env.sh"

echo "=== Building wheel ==="
cd "$SRC_DIR"
"$VENV_DIR/bin/pyodide" build --exports=whole_archive

echo "=== Copying wheel to $OUT_DIR ==="
mkdir -p "$OUT_DIR"
cp dist/*.whl "$OUT_DIR/"

echo "=== Done. Built wheels: ==="
ls -lh dist/*.whl
