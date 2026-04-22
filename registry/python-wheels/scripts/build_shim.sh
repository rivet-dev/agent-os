#!/usr/bin/env bash
# Build the pure-Python dbt-extractor shim wheel.
#
# Args:
#   $1 = venv dir (with `build` installed)
#   $2 = output wheels dir

set -euo pipefail

VENV_DIR="${1:?missing venv dir}"
OUT_DIR="${2:?missing output dir}"

echo "=== Building dbt-extractor shim ==="
SHIM_DIR="recipes/dbt-extractor-shim"
"$VENV_DIR/bin/python" -m build --wheel --outdir "$OUT_DIR" "$SHIM_DIR"

echo "=== Done. Shim wheel: ==="
ls -lh "$OUT_DIR"/dbt_extractor-*+pyodide.shim*.whl 2>/dev/null || \
  ls -lh "$OUT_DIR"/dbt_extractor-*.whl
