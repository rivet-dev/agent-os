#!/usr/bin/env bash
# patch-vendor.sh — Apply crate-level patches to vendored dependencies
#
# Iterates std-patches/crates/<crate-name>/*.patch, finds the matching
# vendor/<crate-name> directory, applies each patch, and nulls out
# .cargo-checksum.json file hashes so Cargo accepts the modified source.
#
# Usage:
#   ./scripts/patch-vendor.sh [--check] [--reverse]
#
# Options:
#   --check    Dry-run: verify patches apply cleanly without modifying files
#   --reverse  Reverse (unapply) previously applied patches

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
WASMCORE_DIR="$(dirname "$SCRIPT_DIR")"
PATCHES_DIR="$WASMCORE_DIR/std-patches/crates"
# VENDOR_DIR defaults to the toolchain's own vendor/ (the `make wasm` command
# build) but can be overridden so the reproducible codex clone build
# (clone-and-build-codex-wasi.sh) can apply the same crate patches to the codex
# workspace's vendored sources.
VENDOR_DIR="${VENDOR_DIR:-$WASMCORE_DIR/vendor}"

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

if [ ! -d "$PATCHES_DIR" ]; then
    echo "No std-patches/crates/ directory found — nothing to patch"
    exit 0
fi

if [ ! -d "$VENDOR_DIR" ]; then
    echo "ERROR: vendor/ directory not found. Run 'cargo vendor' first."
    exit 1
fi

# Find all crate patch directories
CRATE_DIRS=$(find "$PATCHES_DIR" -mindepth 1 -maxdepth 1 -type d 2>/dev/null | sort)

if [ -z "$CRATE_DIRS" ]; then
    echo "No crate patch directories found in $PATCHES_DIR — nothing to patch"
    exit 0
fi

TOTAL_PATCHES=0
FAILED=0

for CRATE_DIR in $CRATE_DIRS; do
    CRATE_NAME="$(basename "$CRATE_DIR")"

    # Find the matching vendor directory (crate-name or crate-name-version)
    VENDOR_CRATE=""
    if [ -d "$VENDOR_DIR/$CRATE_NAME" ]; then
        VENDOR_CRATE="$VENDOR_DIR/$CRATE_NAME"
    else
        # Try matching crate-name-* (versioned directory)
        MATCHES=$(find "$VENDOR_DIR" -maxdepth 1 -type d -name "${CRATE_NAME}-*" 2>/dev/null | sort -V | tail -1)
        if [ -n "$MATCHES" ]; then
            VENDOR_CRATE="$MATCHES"
        fi
    fi

    if [ -z "$VENDOR_CRATE" ]; then
        echo "WARNING: No vendor directory found for crate '$CRATE_NAME' — skipping"
        continue
    fi

    # Find patch files for this crate
    if [ "$MODE" = "reverse" ]; then
        PATCH_FILES=$(find "$CRATE_DIR" -name '*.patch' -type f 2>/dev/null | sort -r)
    else
        PATCH_FILES=$(find "$CRATE_DIR" -name '*.patch' -type f 2>/dev/null | sort)
    fi

    if [ -z "$PATCH_FILES" ]; then
        continue
    fi

    echo "=== $CRATE_NAME ($(basename "$VENDOR_CRATE")) ==="

    for PATCH in $PATCH_FILES; do
        PATCH_NAME="$(basename "$PATCH")"
        TOTAL_PATCHES=$((TOTAL_PATCHES + 1))

        case "$MODE" in
            check)
                echo -n "  Checking $PATCH_NAME ... "
                if patch --dry-run -p1 -d "$VENDOR_CRATE" < "$PATCH" > /dev/null 2>&1; then
                    echo "OK (applies cleanly)"
                elif patch --dry-run -R -p1 -d "$VENDOR_CRATE" < "$PATCH" > /dev/null 2>&1; then
                    echo "OK (already applied)"
                else
                    echo "FAIL (does not apply)"
                    FAILED=$((FAILED + 1))
                fi
                ;;
            apply)
                echo -n "  Applying $PATCH_NAME ... "
                if patch --dry-run -p1 -d "$VENDOR_CRATE" < "$PATCH" > /dev/null 2>&1; then
                    patch -p1 -d "$VENDOR_CRATE" < "$PATCH" > /dev/null 2>&1
                    echo "applied"
                elif patch --dry-run -R -p1 -d "$VENDOR_CRATE" < "$PATCH" > /dev/null 2>&1; then
                    echo "already applied"
                else
                    # Mixed state (e.g. an interrupted earlier run left some
                    # hunks applied): apply the remaining hunks, tolerating
                    # already-applied ones; fail only on genuine rejects.
                    set +e
                    OUT=$(patch -p1 -N -r /dev/null -d "$VENDOR_CRATE" < "$PATCH" 2>&1)
                    RC=$?
                    set -e
                    if [ $RC -le 1 ] && ! echo "$OUT" | grep -q "FAILED"; then
                        echo "converged (mixed state)"
                    else
                        echo "FAIL (does not apply)"
                        FAILED=$((FAILED + 1))
                    fi
                fi
                ;;
            reverse)
                echo -n "  Reversing $PATCH_NAME ... "
                if patch -R --dry-run -p1 -d "$VENDOR_CRATE" < "$PATCH" > /dev/null 2>&1; then
                    patch -R -p1 -d "$VENDOR_CRATE" < "$PATCH" > /dev/null 2>&1
                    echo "reversed"
                else
                    echo "not applied (skipping)"
                fi
                ;;
        esac
    done

    # Install companion source files that a patch needs but cannot carry inline
    # (a `diff`/`patch` cannot create a brand-new file in vendored sources whose
    # parent doesn't exist as a tracked /dev/null hunk reliably across patch
    # versions). Convention: `std-patches/crates/<crate>/copy.manifest` with lines
    # "<src-relative-to-crate-patch-dir> <dest-relative-to-vendor-crate>".
    # Example (tokio): `wasi-process-imp.rs src/process/wasi.rs` installs the
    # cooperative wasm32-wasip1 process impl that the .patch's `#[path]` wiring
    # references. Without this the patched tokio fails to compile (missing module).
    MANIFEST="$CRATE_DIR/copy.manifest"
    if [ "$MODE" = "apply" ] && [ -f "$MANIFEST" ]; then
        while read -r SRC DEST; do
            # Skip blank lines and comments.
            case "$SRC" in ""|\#*) continue ;; esac
            if [ ! -f "$CRATE_DIR/$SRC" ]; then
                echo "  ERROR: copy.manifest source missing: $SRC"
                FAILED=$((FAILED + 1))
                continue
            fi
            mkdir -p "$(dirname "$VENDOR_CRATE/$DEST")"
            cp "$CRATE_DIR/$SRC" "$VENDOR_CRATE/$DEST"
            echo "  Installed companion: $SRC -> $DEST"
        done < "$MANIFEST"
    elif [ "$MODE" = "reverse" ] && [ -f "$MANIFEST" ]; then
        while read -r SRC DEST; do
            case "$SRC" in ""|\#*) continue ;; esac
            rm -f "$VENDOR_CRATE/$DEST"
        done < "$MANIFEST"
    fi

    # Null out .cargo-checksum.json hashes so Cargo accepts patched sources
    if [ "$MODE" = "apply" ]; then
        CHECKSUM_FILE="$VENDOR_CRATE/.cargo-checksum.json"
        if [ -f "$CHECKSUM_FILE" ]; then
            # Replace the "files" object with an empty object to skip checksum verification
            python3 -c "
import json, sys
path = sys.argv[1]
with open(path) as f:
    data = json.load(f)
data['files'] = {}
with open(path, 'w') as f:
    json.dump(data, f)
" "$CHECKSUM_FILE" 2>/dev/null || \
            # Fallback: use sed if python3 is not available
            sed -i 's/"files":{[^}]*}/"files":{}/' "$CHECKSUM_FILE"
        fi
    fi

    echo ""
done

echo "=== Summary ==="
echo "Total patches processed: $TOTAL_PATCHES"

if [ "$FAILED" -ne 0 ]; then
    echo "FAILED: $FAILED patch(es) did not apply"
    exit 1
else
    case "$MODE" in
        check)   echo "All vendor patches verified." ;;
        apply)   echo "All vendor patches applied successfully." ;;
        reverse) echo "All vendor patches reversed." ;;
    esac
fi
