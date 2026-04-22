#!/usr/bin/env bash
# Download the prebuilt DuckDB Pyodide wheel from xlwings/duckdb-pyodide.
#
# Args:
#   $1 = wheel URL
#   $2 = expected sha256 (or PLACEHOLDER_REPLACE_AT_BUILD_TIME)
#   $3 = output wheels dir
#
# If sha256 is PLACEHOLDER_*, computes and prints the actual sha256 instead
# of verifying — useful on first setup.

set -euo pipefail

URL="${1:?missing url}"
EXPECTED_SHA="${2:?missing sha256}"
OUT_DIR="${3:?missing output dir}"

mkdir -p "$OUT_DIR"
FILENAME="$(basename "$URL")"
DEST="$OUT_DIR/$FILENAME"

echo "=== Fetching $FILENAME ==="
curl -fL -o "$DEST" "$URL"

ACTUAL_SHA="$(shasum -a 256 "$DEST" | cut -d' ' -f1)"

if [ "$EXPECTED_SHA" = "PLACEHOLDER_REPLACE_AT_BUILD_TIME" ]; then
  echo "WARN: no expected sha256 pinned. Computed: $ACTUAL_SHA"
  echo "      Update DUCKDB_WHEEL_SHA256 in the Makefile to lock this."
elif [ "$ACTUAL_SHA" != "$EXPECTED_SHA" ]; then
  echo "ERROR: sha256 mismatch"
  echo "  expected: $EXPECTED_SHA"
  echo "  actual:   $ACTUAL_SHA"
  rm -f "$DEST"
  exit 1
else
  echo "sha256 OK: $ACTUAL_SHA"
fi

echo "=== Done. DuckDB wheel at $DEST ==="
ls -lh "$DEST"
