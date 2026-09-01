#!/usr/bin/env bash
# patch-std.sh — Apply wasmVM patches to the Rust std source tree
#
# Patches modify the WASI platform implementation in std to support
# process spawning, pipes, user/group IDs, and terminal detection
# via custom host_process/host_user WASM imports.
#
# Usage:
#   ./scripts/patch-std.sh [--check] [--reverse]
#
# Options:
#   --check    Dry-run: verify patches apply cleanly without modifying files
#   --reverse  Reverse (unapply) previously applied patches

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
WASMCORE_DIR="$(dirname "$SCRIPT_DIR")"
PATCHES_DIR="${PATCH_STD_PATCHES_DIR:-$WASMCORE_DIR/std-patches}"

# Resolve from the toolchain directory so this script honors its colocated
# rust-toolchain.toml even when invoked from the repository root.
SYSROOT=""
if [ -n "${PATCH_STD_SOURCE_ROOT:-}" ]; then
    STD_SRC="$PATCH_STD_SOURCE_ROOT"
else
    SYSROOT="$(cd "$WASMCORE_DIR" && rustc --print sysroot)"
    STD_SRC="$SYSROOT/lib/rustlib/src/rust"
fi

if [ ! -d "$STD_SRC/library/std" ]; then
    echo "ERROR: Rust source not found at $STD_SRC"
    echo "Ensure rust-src component is installed: rustup component add rust-src"
    exit 1
fi

# Parse arguments
MODE="apply"
PATCH_FLAGS="-p1"
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

# Find std patch files in order (reversed for --reverse mode).
# Only top-level std-patches/*.patch are std-source patches; subdirectories
# (std-patches/crates/*, std-patches/wasi-libc/*) target vendored crates and wasi-libc
# and must NOT be applied to the Rust std source tree, so use -maxdepth 1.
if [ "$MODE" = "reverse" ]; then
    PATCH_FILES=$(find "$PATCHES_DIR" -maxdepth 1 -name '*.patch' -type f 2>/dev/null | sort -r)
else
    PATCH_FILES=$(find "$PATCHES_DIR" -maxdepth 1 -name '*.patch' -type f 2>/dev/null | sort)
fi

if [ -z "$PATCH_FILES" ]; then
    echo "No patch files found in $PATCHES_DIR"
    exit 0
fi

PATCH_COUNT=$(echo "$PATCH_FILES" | wc -l)

# `patch-std` mutates rustup's installed rust-src tree. Detect the two healthy
# states (the complete series is already applied, or the complete series applies
# to a pristine tree) in an isolated copy. Refresh rustup only for a malformed or
# partially applied tree. This retains corruption recovery without downloading
# and reinstalling the pinned toolchain on every single-command build.
if [ "$MODE" = "apply" ] && [ -z "${PATCH_STD_SOURCE_ROOT:-}" ] && command -v rustup >/dev/null 2>&1; then
    TOOLCHAIN="$(basename "$SYSROOT")"
    case "$SYSROOT" in
        */.rustup/toolchains/*)
            STATE_SRC="$(mktemp -d "${TMPDIR:-/tmp}/agentos-std-patch-state.XXXXXX")"
            cp -a "$STD_SRC/." "$STATE_SRC/"
            FULLY_PATCHED=1
            while IFS= read -r PATCH; do
                if patch --batch --dry-run -R $PATCH_FLAGS -d "$STATE_SRC" < "$PATCH" > /dev/null 2>&1; then
                    patch --batch -R $PATCH_FLAGS -d "$STATE_SRC" < "$PATCH" > /dev/null 2>&1
                else
                    FULLY_PATCHED=0
                    break
                fi
            done < <(printf '%s\n' "$PATCH_FILES" | sort -r)

            if [ "$FULLY_PATCHED" -eq 1 ]; then
                echo "Rust std patch series is already fully applied; reusing $TOOLCHAIN."
            else
                rm -rf "$STATE_SRC"
                STATE_SRC="$(mktemp -d "${TMPDIR:-/tmp}/agentos-std-patch-state.XXXXXX")"
                cp -a "$STD_SRC/." "$STATE_SRC/"
                PRISTINE=1
                while IFS= read -r PATCH; do
                    if patch --batch --dry-run $PATCH_FLAGS -d "$STATE_SRC" < "$PATCH" > /dev/null 2>&1; then
                        patch --batch $PATCH_FLAGS -d "$STATE_SRC" < "$PATCH" > /dev/null 2>&1
                    else
                        PRISTINE=0
                        break
                    fi
                done < <(printf '%s\n' "$PATCH_FILES" | sort)

                if [ "$PRISTINE" -eq 1 ]; then
                    echo "Rust std source is pristine; reusing $TOOLCHAIN."
                else
                    echo "Rust std source is partially patched or malformed; refreshing $TOOLCHAIN..."
                    rm -rf "$SYSROOT"
                    rustup toolchain install "$TOOLCHAIN" \
                        --profile minimal \
                        --component rust-src \
                        --target wasm32-wasip1 \
                        --force >/dev/null
                fi
            fi
            rm -rf "$STATE_SRC"
            ;;
    esac
fi

echo "Found $PATCH_COUNT patch(es) in $PATCHES_DIR"
echo "Rust std source: $STD_SRC"
echo ""

FAILED=0
CHECK_SRC=""

cleanup() {
    if [ -n "$CHECK_SRC" ] && [ -d "$CHECK_SRC" ]; then
        rm -rf "$CHECK_SRC"
    fi
}

trap cleanup EXIT

if [ "$MODE" = "check" ]; then
    CHECK_SRC="$(mktemp -d "${TMPDIR:-/tmp}/agentos-std-patch-check.XXXXXX")"
    cp -a "$STD_SRC/." "$CHECK_SRC/"

    # Patches can depend on files or context introduced by earlier patches, so
    # classify the source tree using the complete ordered series. Testing each
    # patch independently against pristine Rust incorrectly rejects those valid
    # dependencies. First try reversing a fully applied tree; if that fails,
    # verify that the full series applies to the original copy. Anything else is
    # a partial or malformed tree and remains a hard failure.
    STATE_SRC="$(mktemp -d "${TMPDIR:-/tmp}/agentos-std-patch-state.XXXXXX")"
    cp -a "$STD_SRC/." "$STATE_SRC/"
    FULLY_PATCHED=1
    while IFS= read -r PATCH; do
        if patch --batch --dry-run -R $PATCH_FLAGS -d "$STATE_SRC" < "$PATCH" > /dev/null 2>&1; then
            patch --batch -R $PATCH_FLAGS -d "$STATE_SRC" < "$PATCH" > /dev/null 2>&1
        else
            FULLY_PATCHED=0
            break
        fi
    done < <(printf '%s\n' "$PATCH_FILES" | sort -r)

    if [ "$FULLY_PATCHED" -eq 1 ]; then
        rm -rf "$CHECK_SRC"
        CHECK_SRC="$STATE_SRC"
    else
        rm -rf "$STATE_SRC"
        STATE_SRC="$(mktemp -d "${TMPDIR:-/tmp}/agentos-std-patch-state.XXXXXX")"
        cp -a "$STD_SRC/." "$STATE_SRC/"
        PRISTINE=1
        FAILED_PATCH_NAME=""
        while IFS= read -r PATCH; do
            if patch --batch --dry-run $PATCH_FLAGS -d "$STATE_SRC" < "$PATCH" > /dev/null 2>&1; then
                patch --batch $PATCH_FLAGS -d "$STATE_SRC" < "$PATCH" > /dev/null 2>&1
            else
                PRISTINE=0
                FAILED_PATCH_NAME="$(basename "$PATCH")"
                break
            fi
        done < <(printf '%s\n' "$PATCH_FILES" | sort)
        rm -rf "$STATE_SRC"
        if [ "$PRISTINE" -ne 1 ]; then
            echo "FAIL: Rust std source is partially patched or the patch series is malformed at $FAILED_PATCH_NAME."
            exit 1
        fi
    fi

    STD_SRC="$CHECK_SRC"
    echo "Checking against reconstructed pristine std source: $STD_SRC"
    echo ""
fi

for PATCH in $PATCH_FILES; do
    PATCH_NAME="$(basename "$PATCH")"

    case "$MODE" in
        check)
            echo -n "Checking $PATCH_NAME ... "
            if patch --batch --dry-run $PATCH_FLAGS -d "$STD_SRC" < "$PATCH" > /dev/null 2>&1; then
                patch --batch $PATCH_FLAGS -d "$STD_SRC" < "$PATCH" > /dev/null 2>&1
                echo "OK"
            else
                echo "FAIL (does not apply in sequence)"
                FAILED=1
            fi
            ;;
        apply)
            echo -n "Applying $PATCH_NAME ... "
            # Use `--forward` (-N) for idempotency: it applies hunks that are not
            # yet present and SKIPS hunks already applied (reversed) instead of
            # applying them a second time. Without this, additive (insert-only)
            # patches stay forward-applicable after they are applied — their
            # anchor context is still present — and a naive forward apply inserts
            # a duplicate copy, producing E0119 conflicting-implementation errors
            # on a re-run. `--forward` makes a second `make wasm` a no-op.
            if patch --batch --forward --dry-run $PATCH_FLAGS -d "$STD_SRC" < "$PATCH" > /dev/null 2>&1; then
                patch --batch --forward $PATCH_FLAGS -d "$STD_SRC" < "$PATCH" > /dev/null 2>&1
                echo "applied"
            else
                echo "already applied (skipping)"
            fi
            ;;
        reverse)
            echo -n "Reversing $PATCH_NAME ... "
            if patch --batch -R $PATCH_FLAGS -d "$STD_SRC" < "$PATCH" > /dev/null 2>&1; then
                echo "reversed"
            else
                echo "not applied (skipping)"
            fi
            ;;
    esac
done

# Install companion source files that a patch declares (e.g. `pub mod process;`)
# but cannot reliably carry inline: a `diff`/`patch` cannot create a brand-new
# file in the std source from a `/dev/null` hunk reliably across patch versions
# (the hunk is silently skipped, leaving the declared module with no source file
# and a `file not found for module` E0583 build error). Convention mirrors the
# vendored-crate mechanism in patch-vendor.sh: `std-patches/copy.manifest` with lines
# "<src-relative-to-PATCHES_DIR> <dest-relative-to-STD_SRC>". Example:
# `std/os/wasi/process.rs library/std/src/os/wasi/process.rs` installs the public
# wasi child-pipe fd traits that 0007-wasi-childpipe-fd.patch's `pub mod process;`
# references. Without this the patched std fails to compile (missing module).
MANIFEST="$PATCHES_DIR/copy.manifest"
if [ -f "$MANIFEST" ]; then
    while read -r SRC DEST; do
        # Skip blank lines and comments.
        case "$SRC" in ""|\#*) continue ;; esac
        case "$MODE" in
            apply)
                if [ ! -f "$PATCHES_DIR/$SRC" ]; then
                    echo "copy.manifest source missing: $SRC"
                    FAILED=1
                    continue
                fi
                mkdir -p "$(dirname "$STD_SRC/$DEST")"
                cp "$PATCHES_DIR/$SRC" "$STD_SRC/$DEST"
                echo "Installed companion: $SRC -> $DEST"
                ;;
            reverse)
                rm -f "$STD_SRC/$DEST"
                echo "Removed companion: $DEST"
                ;;
            check)
                if [ ! -f "$PATCHES_DIR/$SRC" ]; then
                    echo "copy.manifest source missing: $SRC"
                    FAILED=1
                fi
                ;;
        esac
    done < "$MANIFEST"
fi

echo ""
if [ "$FAILED" -ne 0 ]; then
    echo "Some patches failed to apply. Check patch compatibility with current nightly."
    exit 1
else
    case "$MODE" in
        check)   echo "All patches verified." ;;
        apply)   echo "All patches applied successfully." ;;
        reverse) echo "All patches reversed." ;;
    esac
fi
