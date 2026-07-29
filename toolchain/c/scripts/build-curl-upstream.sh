#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: build-curl-upstream.sh \
  --version <curl-version> \
  --tag <curl-tag> \
  --url <release-url> \
  --cache-dir <cache-dir> \
  --build-dir <build-dir> \
  --overlay-dir <overlay-dir> \
  --cc <cc> \
  --ar <ar> \
  --ranlib <ranlib> \
  --mbedtls-include <dir> \
  --mbedtls-libdir <dir> \
  --zlib-include <dir> \
  --zlib-libdir <dir> \
  --brotli-include <dir> \
  --brotli-libdir <dir> \
  --zstd-include <dir> \
  --zstd-libdir <dir> \
  --ca-bundle <path> \
  --output <output> \
  [--install-prefix <dir>]
EOF
}

VERSION=""
TAG=""
URL=""
CACHE_DIR=""
BUILD_DIR=""
OVERLAY_DIR=""
CC_CMD=""
AR_CMD=""
RANLIB_CMD=""
MBEDTLS_INCLUDE=""
MBEDTLS_LIBDIR=""
ZLIB_INCLUDE=""
ZLIB_LIBDIR=""
BROTLI_INCLUDE=""
BROTLI_LIBDIR=""
ZSTD_INCLUDE=""
ZSTD_LIBDIR=""
CA_BUNDLE=""
OUTPUT=""
INSTALL_PREFIX=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --install-prefix) INSTALL_PREFIX="$2"; shift 2 ;;
    --version) VERSION="$2"; shift 2 ;;
    --tag) TAG="$2"; shift 2 ;;
    --url) URL="$2"; shift 2 ;;
    --cache-dir) CACHE_DIR="$2"; shift 2 ;;
    --build-dir) BUILD_DIR="$2"; shift 2 ;;
    --overlay-dir) OVERLAY_DIR="$2"; shift 2 ;;
    --cc) CC_CMD="$2"; shift 2 ;;
    --ar) AR_CMD="$2"; shift 2 ;;
    --ranlib) RANLIB_CMD="$2"; shift 2 ;;
    --mbedtls-include) MBEDTLS_INCLUDE="$2"; shift 2 ;;
    --mbedtls-libdir) MBEDTLS_LIBDIR="$2"; shift 2 ;;
    --zlib-include) ZLIB_INCLUDE="$2"; shift 2 ;;
    --zlib-libdir) ZLIB_LIBDIR="$2"; shift 2 ;;
    --brotli-include) BROTLI_INCLUDE="$2"; shift 2 ;;
    --brotli-libdir) BROTLI_LIBDIR="$2"; shift 2 ;;
    --zstd-include) ZSTD_INCLUDE="$2"; shift 2 ;;
    --zstd-libdir) ZSTD_LIBDIR="$2"; shift 2 ;;
    --ca-bundle) CA_BUNDLE="$2"; shift 2 ;;
    --output) OUTPUT="$2"; shift 2 ;;
    *)
      echo "Unknown argument: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
done

if [[ -z "$VERSION" || -z "$TAG" || -z "$URL" || -z "$CACHE_DIR" || -z "$BUILD_DIR" || -z "$OVERLAY_DIR" || -z "$CC_CMD" || -z "$AR_CMD" || -z "$RANLIB_CMD" || -z "$MBEDTLS_INCLUDE" || -z "$MBEDTLS_LIBDIR" || -z "$ZLIB_INCLUDE" || -z "$ZLIB_LIBDIR" || -z "$BROTLI_INCLUDE" || -z "$BROTLI_LIBDIR" || -z "$ZSTD_INCLUDE" || -z "$ZSTD_LIBDIR" || -z "$CA_BUNDLE" || -z "$OUTPUT" ]]; then
  usage >&2
  exit 1
fi

fetch() {
  local url="$1"
  local out="$2"
  if command -v curl >/dev/null 2>&1; then
    curl -fSL "$url" -o "$out"
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

TARBALL="$CACHE_DIR/curl-${VERSION}.tar.xz"
if [[ ! -f "$TARBALL" ]]; then
  echo "Fetching upstream curl ${VERSION} release tarball..."
  fetch "$URL" "$TARBALL"
fi

echo "Extracting upstream curl ${VERSION}..."
tar -xf "$TARBALL" -C "$BUILD_DIR"

SRC_DIR="$BUILD_DIR/curl-${VERSION}"
if [[ ! -d "$SRC_DIR" ]]; then
  echo "Expected extracted source at $SRC_DIR" >&2
  exit 1
fi

echo "Applying agentos overlay..."
while IFS= read -r -d '' file; do
  rel="${file#$OVERLAY_DIR/}"
  mkdir -p "$SRC_DIR/$(dirname "$rel")"
  cp "$file" "$SRC_DIR/$rel"
done < <(find "$OVERLAY_DIR" -type f -print0)

# Stage a single dependency prefix (include/ + lib/) so curl's configure can
# find mbedTLS/zlib/brotli/zstd with plain --with-<lib>=<prefix>. PKG_CONFIG is
# disabled (no .pc files for our cross-built statics), so configure derives
# -I<prefix>/include -L<prefix>/lib and the correct -l flags from the prefix.
DEPS="$BUILD_DIR/deps"
rm -rf "$DEPS"
mkdir -p "$DEPS/include" "$DEPS/lib"

# Headers: mbedtls/ + psa/, zlib.h/zconf.h, brotli/, zstd.h — merged into one tree.
cp -a "$MBEDTLS_INCLUDE/." "$DEPS/include/"
cp -a "$ZLIB_INCLUDE/zlib.h" "$ZLIB_INCLUDE/zconf.h" "$DEPS/include/"
cp -a "$BROTLI_INCLUDE/." "$DEPS/include/"
cp -a "$ZSTD_INCLUDE/zstd.h" "$DEPS/include/"

# Static archives.
cp -a "$MBEDTLS_LIBDIR/libmbedtls.a" "$MBEDTLS_LIBDIR/libmbedx509.a" \
      "$MBEDTLS_LIBDIR/libmbedcrypto.a" "$DEPS/lib/"
cp -a "$ZLIB_LIBDIR/libz.a" "$DEPS/lib/"
cp -a "$BROTLI_LIBDIR/libbrotlidec.a" "$BROTLI_LIBDIR/libbrotlicommon.a" "$DEPS/lib/"
cp -a "$ZSTD_LIBDIR/libzstd.a" "$DEPS/lib/"

pushd "$SRC_DIR" >/dev/null

echo "Configuring upstream curl for wasm32-wasip1 (in-guest mbedTLS + zlib/brotli/zstd)..."
# mbedTLS 3.x removed the legacy havege RNG, so curl's configure runs its
# "mbedtls_ssl_init in -lmbedtls" link probe — which needs the three archives
# in the correct static order plus brotlicommon (brotlidec depends on it).
# LIBS carries brotlicommon (configure only appends -lbrotlidec on its own).
CC="$CC_CMD" \
AR="$AR_CMD" \
RANLIB="$RANLIB_CMD" \
PKG_CONFIG="false" \
CFLAGS="-O2 -flto" \
CPPFLAGS="-I$DEPS/include" \
LDFLAGS="-L$DEPS/lib" \
LIBS="-lbrotlicommon" \
./configure \
  --host=wasm32-unknown-wasi \
  --disable-shared \
  --disable-threaded-resolver \
  --disable-ldap \
  --without-libpsl \
  --with-mbedtls="$DEPS" \
  --with-zlib="$DEPS" \
  --with-brotli="$DEPS" \
  --with-zstd="$DEPS" \
  --with-ca-bundle="$CA_BUNDLE"

echo "Building upstream libcurl..."
make -C lib libcurl.la

echo "Building upstream curl tool..."
make -C src curl

# Reusable libcurl artifact: install the overlaid, mbedTLS-linked libcurl into a
# prefix (headers + libcurl.a) so git can link it in-process. Same overlay and
# configure as above, so git's git-remote-http gets the identical TLS backend.
if [[ -n "$INSTALL_PREFIX" ]]; then
  echo "Installing libcurl to $INSTALL_PREFIX ..."
  rm -rf "$INSTALL_PREFIX"
  mkdir -p "$INSTALL_PREFIX/include/curl" "$INSTALL_PREFIX/lib/pkgconfig"
  make -C lib install prefix="$INSTALL_PREFIX" >/dev/null 2>&1 || true
  make -C include install prefix="$INSTALL_PREFIX" >/dev/null 2>&1 || true
  # Guarantee headers + the static archive land regardless of libtool install
  # quirks under a cross toolchain.
  cp -a include/curl/*.h "$INSTALL_PREFIX/include/curl/" 2>/dev/null || true
  if [[ -f lib/.libs/libcurl.a && ! -f "$INSTALL_PREFIX/lib/libcurl.a" ]]; then
    cp -a lib/.libs/libcurl.a "$INSTALL_PREFIX/lib/libcurl.a"
  fi
  cp -a libcurl.pc "$INSTALL_PREFIX/lib/pkgconfig/" 2>/dev/null || true
  if [[ ! -f "$INSTALL_PREFIX/lib/libcurl.a" ]]; then
    echo "libcurl.a missing after install into $INSTALL_PREFIX" >&2
    exit 1
  fi
  if [[ ! -f "$INSTALL_PREFIX/include/curl/curl.h" ]]; then
    echo "curl/curl.h missing after install into $INSTALL_PREFIX" >&2
    exit 1
  fi
  echo "Installed libcurl artifact ($(wc -c < "$INSTALL_PREFIX/lib/libcurl.a") bytes) at $INSTALL_PREFIX"
fi

BIN=""
for candidate in "src/.libs/curl" "src/curl" "src/curl.wasm"; do
  if [[ -f "$candidate" ]]; then
    BIN="$candidate"
    break
  fi
done

if [[ -z "$BIN" ]]; then
  echo "Unable to locate built curl binary in src/" >&2
  exit 1
fi

mkdir -p "$(dirname "$OUTPUT")"
if command -v wasm-opt >/dev/null 2>&1; then
  echo "Optimizing curl WASM binary..."
  wasm-opt -O3 --strip-debug --all-features "$BIN" -o "$OUTPUT"
else
  cp "$BIN" "$OUTPUT"
fi

popd >/dev/null

echo "Built upstream curl at $OUTPUT"
