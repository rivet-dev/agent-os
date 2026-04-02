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
  --output <output>
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
OUTPUT=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version)
      VERSION="$2"
      shift 2
      ;;
    --tag)
      TAG="$2"
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
    --overlay-dir)
      OVERLAY_DIR="$2"
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

if [[ -z "$VERSION" || -z "$TAG" || -z "$URL" || -z "$CACHE_DIR" || -z "$BUILD_DIR" || -z "$OVERLAY_DIR" || -z "$CC_CMD" || -z "$AR_CMD" || -z "$RANLIB_CMD" || -z "$OUTPUT" ]]; then
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

echo "Applying agentOS overlay..."
while IFS= read -r -d '' file; do
  rel="${file#$OVERLAY_DIR/}"
  mkdir -p "$SRC_DIR/$(dirname "$rel")"
  cp "$file" "$SRC_DIR/$rel"
done < <(find "$OVERLAY_DIR" -type f -print0)

pushd "$SRC_DIR" >/dev/null

echo "Patching WASI-incompatible signal/setjmp includes..."
python3 - <<'PY'
from pathlib import Path

replacements = {
    "lib/hostip.h": [
        (
            '#include <setjmp.h>\n',
            '#ifndef __wasi__\n#include <setjmp.h>\n#endif\n',
        ),
    ],
    "lib/hostip.c": [
        (
            '#include <setjmp.h>\n#include <signal.h>\n',
            '#ifndef __wasi__\n#include <setjmp.h>\n#include <signal.h>\n#endif\n',
        ),
    ],
    "lib/transfer.c": [
        (
            '#include <signal.h>\n',
            '#ifndef __wasi__\n#include <signal.h>\n#endif\n',
        ),
    ],
    "src/tool_main.c": [
        (
            '#include <signal.h>\n',
            '#ifndef __wasi__\n#include <signal.h>\n#endif\n',
        ),
    ],
}

for rel_path, edits in replacements.items():
    path = Path(rel_path)
    updated = path.read_text()
    for old, new in edits:
        text = updated
        if new in text:
            continue
        if old not in text:
            raise SystemExit(f"Expected to patch {rel_path}, but no replacement matched")
        updated = text.replace(old, new)
    path.write_text(updated)
PY

echo "Configuring upstream curl for wasm32-wasip1..."
CC="$CC_CMD" \
AR="$AR_CMD" \
RANLIB="$RANLIB_CMD" \
CFLAGS="-O2 -flto" \
./configure \
  --host=wasm32-unknown-wasi \
  --disable-shared \
  --disable-threaded-resolver \
  --disable-ldap \
  --without-zlib \
  --without-brotli \
  --without-zstd \
  --without-libpsl \
  --without-ca-bundle \
  --without-ca-path \
  --without-ssl

echo "Enabling the agentOS WASI TLS backend in generated curl config..."
python3 - <<'PY'
from pathlib import Path

config = Path("lib/curl_config.h")
text = config.read_text()

updates = {
    "#define CURL_DISABLE_HSTS 1": "/* #undef CURL_DISABLE_HSTS */",
}
for old, new in updates.items():
    text = text.replace(old, new)

if "#define USE_WASI_TLS 1" not in text:
    text += "\n#define USE_WASI_TLS 1\n"
if "#define CURL_DISABLE_OPENSSL_AUTO_LOAD_CONFIG 1" not in text:
    text += "#define CURL_DISABLE_OPENSSL_AUTO_LOAD_CONFIG 1\n"

config.write_text(text)
PY

echo "Patching generated lib/Makefile to compile wasi_tls.c..."
cat >> lib/Makefile <<'EOF'

am_libcurl_la_OBJECTS += vtls/libcurl_la-wasi_tls.lo
libcurl_la_LIBADD += vtls/libcurl_la-wasi_tls.lo
libcurl.la: vtls/libcurl_la-wasi_tls.lo

vtls/libcurl_la-wasi_tls.lo: vtls/$(am__dirstamp) vtls/$(DEPDIR)/$(am__dirstamp) vtls/wasi_tls.c
	$(AM_V_CC)$(LIBTOOL) $(AM_V_lt) --tag=CC $(AM_LIBTOOLFLAGS) $(LIBTOOLFLAGS) --mode=compile $(CC) $(DEFS) $(DEFAULT_INCLUDES) $(INCLUDES) $(libcurl_la_CPPFLAGS) $(CPPFLAGS) $(libcurl_la_CFLAGS) $(CFLAGS) -MT vtls/libcurl_la-wasi_tls.lo -MD -MP -MF vtls/$(DEPDIR)/libcurl_la-wasi_tls.Tpo -c -o vtls/libcurl_la-wasi_tls.lo `test -f 'vtls/wasi_tls.c' || echo '$(srcdir)/'`vtls/wasi_tls.c
	$(AM_V_at)$(am__mv) vtls/$(DEPDIR)/libcurl_la-wasi_tls.Tpo vtls/$(DEPDIR)/libcurl_la-wasi_tls.Plo
EOF

echo "Building upstream libcurl..."
make -C lib libcurl.la

echo "Building upstream curl tool..."
make -C src curl

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
