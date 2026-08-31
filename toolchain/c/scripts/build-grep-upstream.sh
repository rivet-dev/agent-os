#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: build-grep-upstream.sh \
  --version <grep-version> \
  --url <release-url> \
  --cache-dir <cache-dir> \
  --build-dir <build-dir> \
  --cc <cc> \
  --ar <ar> \
  --ranlib <ranlib> \
  --output <output>
EOF
}

VERSION=""
URL=""
CACHE_DIR=""
BUILD_DIR=""
CC_CMD=""
AR_CMD=""
RANLIB_CMD=""
OUTPUT=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version)
      VERSION="$2"
      shift 2
      ;;
    --url)
      URL="$2"
      shift 2
      ;;
    --cache-dir)
      CACHE_DIR="$2"
      shift 2
      ;;
    --build-dir)
      BUILD_DIR="$2"
      shift 2
      ;;
    --cc)
      CC_CMD="$2"
      shift 2
      ;;
    --ar)
      AR_CMD="$2"
      shift 2
      ;;
    --ranlib)
      RANLIB_CMD="$2"
      shift 2
      ;;
    --output)
      OUTPUT="$2"
      shift 2
      ;;
    *)
      echo "Unknown argument: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
done

if [[ -z "$VERSION" || -z "$URL" || -z "$CACHE_DIR" || -z "$BUILD_DIR" || -z "$CC_CMD" || -z "$AR_CMD" || -z "$RANLIB_CMD" || -z "$OUTPUT" ]]; then
  usage >&2
  exit 1
fi

fetch() {
  local url="$1"
  local out="$2"
  if command -v curl >/dev/null 2>&1; then
    curl --retry 3 --retry-all-errors -fSL "$url" -o "$out"
  elif command -v wget >/dev/null 2>&1; then
    wget -q "$url" -O "$out"
  else
    echo "Neither curl nor wget is available to fetch $url" >&2
    exit 1
  fi
}

mkdir -p "$CACHE_DIR"
rm -rf "$BUILD_DIR"
mkdir -p "$BUILD_DIR"

TARBALL="$CACHE_DIR/grep-${VERSION}.tar.xz"
if [[ ! -f "$TARBALL" ]]; then
  echo "Fetching upstream GNU grep ${VERSION} release tarball..."
  fetch "$URL" "$TARBALL"
fi

echo "Extracting upstream GNU grep ${VERSION}..."
tar -xJf "$TARBALL" -C "$BUILD_DIR"

SRC_DIR="$BUILD_DIR/grep-${VERSION}"
if [[ ! -d "$SRC_DIR" ]]; then
  echo "Expected extracted source at $SRC_DIR" >&2
  exit 1
fi

pushd "$SRC_DIR" >/dev/null

echo "Configuring upstream GNU grep for wasm32-wasip1..."
CC="$CC_CMD" \
AR="$AR_CMD" \
RANLIB="$RANLIB_CMD" \
PKG_CONFIG=false \
PCRE_CFLAGS="" \
PCRE_LIBS="" \
gl_cv_func_select_supports0=yes \
gl_cv_func_select_detects_ebadf=yes \
gl_cv_func_pselect_detects_ebadf=yes \
gl_cv_func_lseek_pipe=no \
ac_cv_func_sigaction=yes \
ac_cv_func_sigprocmask=yes \
ac_cv_func_clock_getres=no \
ac_cv_func_clock_gettime=no \
CFLAGS="-O2 -flto -D_WASI_EMULATED_MMAN -D_WASI_EMULATED_PROCESS_CLOCKS" \
LIBS="-lwasi-emulated-mman -lwasi-emulated-process-clocks" \
./configure \
  --host=wasm32-unknown-wasi \
  --disable-shared \
  --disable-nls \
  --disable-perl-regexp \
  --disable-threads

echo "Building upstream GNU grep support library..."
make -C lib

echo "Building upstream GNU grep..."
make -C src grep

BIN=""
for candidate in "src/grep" "src/.libs/grep" "src/grep.wasm"; do
  if [[ -f "$candidate" ]]; then
    BIN="$candidate"
    break
  fi
done

if [[ -z "$BIN" ]]; then
  echo "Unable to locate built grep binary in src/" >&2
  exit 1
fi

mkdir -p "$(dirname "$OUTPUT")"
if command -v wasm-opt >/dev/null 2>&1; then
  echo "Optimizing GNU grep WASM binary..."
  if wasm-opt --help 2>&1 | grep -q -- '--disable-compact-imports'; then
    # New Binaryen's --all-features enables compact imports, which Node/V8 does not implement.
    wasm-opt -O3 --strip-debug --all-features --disable-compact-imports "$BIN" -o "$OUTPUT"
  else
    # Older Binaryen does not know the compact-imports disable flag. Do not enable every proposal:
    # doing so could emit compact imports with no way to lower them again.
    wasm-opt -O3 --strip-debug "$BIN" -o "$OUTPUT"
  fi
else
  cp "$BIN" "$OUTPUT"
fi

popd >/dev/null

echo "Built upstream GNU grep at $OUTPUT"
