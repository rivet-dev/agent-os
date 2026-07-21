#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: build-wget-upstream.sh \
  --version <wget-version> \
  --url <release-url> \
  --cache-dir <cache-dir> \
  --build-dir <build-dir> \
  --overlay-include-dir <include-dir> \
  --overlay-dir <overlay-dir> \
  --mbedtls-include <dir> \
  --mbedtls-libdir <dir> \
  --zlib-include <dir> \
  --zlib-libdir <dir> \
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
OVERLAY_INCLUDE_DIR=""
OVERLAY_DIR=""
MBEDTLS_INCLUDE=""
MBEDTLS_LIBDIR=""
ZLIB_INCLUDE=""
ZLIB_LIBDIR=""
CC_CMD=""
AR_CMD=""
RANLIB_CMD=""
OUTPUT=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version) VERSION="$2"; shift 2 ;;
    --url) URL="$2"; shift 2 ;;
    --cache-dir) CACHE_DIR="$2"; shift 2 ;;
    --build-dir) BUILD_DIR="$2"; shift 2 ;;
    --overlay-include-dir) OVERLAY_INCLUDE_DIR="$2"; shift 2 ;;
    --overlay-dir) OVERLAY_DIR="$2"; shift 2 ;;
    --mbedtls-include) MBEDTLS_INCLUDE="$2"; shift 2 ;;
    --mbedtls-libdir) MBEDTLS_LIBDIR="$2"; shift 2 ;;
    --zlib-include) ZLIB_INCLUDE="$2"; shift 2 ;;
    --zlib-libdir) ZLIB_LIBDIR="$2"; shift 2 ;;
    --cc) CC_CMD="$2"; shift 2 ;;
    --ar) AR_CMD="$2"; shift 2 ;;
    --ranlib) RANLIB_CMD="$2"; shift 2 ;;
    --output) OUTPUT="$2"; shift 2 ;;
    *)
      echo "Unknown argument: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
done

if [[ -z "$VERSION" || -z "$URL" || -z "$CACHE_DIR" || -z "$BUILD_DIR" || -z "$OVERLAY_INCLUDE_DIR" || -z "$OVERLAY_DIR" || -z "$MBEDTLS_INCLUDE" || -z "$MBEDTLS_LIBDIR" || -z "$ZLIB_INCLUDE" || -z "$ZLIB_LIBDIR" || -z "$CC_CMD" || -z "$AR_CMD" || -z "$RANLIB_CMD" || -z "$OUTPUT" ]]; then
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

TARBALL="$CACHE_DIR/wget-${VERSION}.tar.gz"
if [[ ! -f "$TARBALL" ]]; then
  echo "Fetching upstream GNU Wget ${VERSION} release tarball..."
  fetch "$URL" "$TARBALL"
fi

echo "Extracting upstream GNU Wget ${VERSION}..."
tar -xzf "$TARBALL" -C "$BUILD_DIR"

SRC_DIR="$BUILD_DIR/wget-${VERSION}"
if [[ ! -d "$SRC_DIR" ]]; then
  echo "Expected extracted source at $SRC_DIR" >&2
  exit 1
fi

pushd "$SRC_DIR" >/dev/null

# Wget stores POSIX clock identifiers in `int`, although POSIX deliberately
# defines `clockid_t` as an opaque implementation type. wasi-libc uses a pointer
# representation, so the narrowing is a compile error. Use the API's declared
# type throughout; this is portable on Linux (where clockid_t is an int) too.
python3 - <<'PY'
from pathlib import Path

path = Path("src/ptimer.c")
text = path.read_text()
edits = {
    "static int posix_clock_id;": "static clockid_t posix_clock_id;",
    "    int id;\n    int sysconf_name;": "    clockid_t id;\n    int sysconf_name;",
}
for old, new in edits.items():
    if new in text:
        continue
    if old not in text:
        raise SystemExit(f"Expected clockid_t portability edit not found: {old!r}")
    text = text.replace(old, new, 1)
path.write_text(text)
PY

# GNU Wget has no upstream mbedTLS backend. We keep ./configure --without-ssl so
# it never probes GnuTLS/OpenSSL, then light up its SSL abstraction manually:
# patch src/wget.h's HAVE_SSL derivation to trigger on HAVE_WASI_TLS, compile
# -DHAVE_WASI_TLS everywhere (so http.c/main.c/init.c enable the https scheme,
# --secure-protocol/--ca-certificate/--no-check-certificate and HSTS), overlay
# our mbedTLS backend (src/wasi_ssl.c), and append its object + the mbedTLS
# archives to the generated src/Makefile link. This mirrors the curl build's
# playbook. zlib is enabled for gzip Content-Encoding via ZLIB_CFLAGS/ZLIB_LIBS.
echo "Configuring upstream GNU Wget for wasm32-wasip1 (in-guest mbedTLS + zlib)..."
CC="$CC_CMD" \
AR="$AR_CMD" \
RANLIB="$RANLIB_CMD" \
PKG_CONFIG=false \
NETTLE_CFLAGS="" \
NETTLE_LIBS="" \
PCRE2_CFLAGS="" \
PCRE2_LIBS="" \
PCRE_CFLAGS="" \
PCRE_LIBS="" \
ZLIB_CFLAGS="-I$ZLIB_INCLUDE" \
ZLIB_LIBS="-L$ZLIB_LIBDIR -lz" \
CPPFLAGS="-I$OVERLAY_INCLUDE_DIR -I$MBEDTLS_INCLUDE" \
gl_cv_func_posix_spawn_works=yes \
gl_cv_func_posix_spawn_agentos=yes \
gl_cv_func_posix_spawnp_agentos=yes \
ac_cv_func_posix_spawn_file_actions_addchdir=yes \
gl_cv_func_posix_spawn_file_actions_addclose_works=yes \
gl_cv_func_posix_spawn_file_actions_adddup2_works=yes \
gl_cv_func_posix_spawn_file_actions_addopen_works=yes \
gl_cv_func_select_supports0=yes \
gl_cv_func_select_detects_ebadf=yes \
gl_cv_func_pselect_detects_ebadf=yes \
ac_cv_func_sigaction=yes \
ac_cv_func_sigprocmask=yes \
ac_cv_func_clock_getres=no \
ac_cv_func_clock_gettime=no \
CFLAGS="-O2 -flto -DHAVE_WASI_TLS -D_WASI_EMULATED_MMAN -D_WASI_EMULATED_PROCESS_CLOCKS" \
LIBS="-lwasi-emulated-mman -lwasi-emulated-process-clocks" \
./configure \
  --host=wasm32-unknown-wasi \
  --disable-shared \
  --disable-nls \
  --disable-iri \
  --disable-digest \
  --disable-ntlm \
  --disable-opie \
  --disable-pcre \
  --disable-pcre2 \
  --without-ssl \
  --without-libpsl \
  --without-libuuid \
  --without-metalink

# Patch HAVE_SSL derivation: wget only defines HAVE_SSL when a probed backend
# (OpenSSL/GnuTLS) is present. Add our in-guest backend to the condition.
echo "Patching src/wget.h HAVE_SSL derivation for HAVE_WASI_TLS..."
python3 - <<'PY'
from pathlib import Path

path = Path("src/wget.h")
text = path.read_text()
needle = "#if defined HAVE_LIBSSL || defined HAVE_LIBSSL32 || defined HAVE_LIBGNUTLS"
replacement = needle + " || defined HAVE_WASI_TLS"
if "defined HAVE_WASI_TLS" not in text:
    if needle not in text:
        raise SystemExit("Expected HAVE_SSL derivation not found in src/wget.h")
    text = text.replace(needle, replacement, 1)
    path.write_text(text)
    print("  patched src/wget.h")
else:
    print("  src/wget.h already patched")

# Report the real backend in `wget --version` (otherwise it prints "-ssl",
# since neither HAVE_LIBSSL nor HAVE_LIBGNUTLS is defined).
bi = Path("src/build_info.c")
bitext = bi.read_text()
ssl_needle = ('#if defined HAVE_LIBSSL || defined HAVE_LIBSSL32\n'
              '  "+ssl/openssl",\n'
              '#elif defined HAVE_LIBGNUTLS\n'
              '  "+ssl/gnutls",\n'
              '#else\n'
              '  "-ssl",\n'
              '#endif')
ssl_repl = ('#if defined HAVE_LIBSSL || defined HAVE_LIBSSL32\n'
            '  "+ssl/openssl",\n'
            '#elif defined HAVE_LIBGNUTLS\n'
            '  "+ssl/gnutls",\n'
            '#elif defined HAVE_WASI_TLS\n'
            '  "+ssl/mbedtls",\n'
            '#else\n'
            '  "-ssl",\n'
            '#endif')
if '"+ssl/mbedtls"' not in bitext:
    if ssl_needle not in bitext:
        raise SystemExit("Expected ssl feature block not found in src/build_info.c")
    bi.write_text(bitext.replace(ssl_needle, ssl_repl, 1))
    print("  patched src/build_info.c")
else:
    print("  src/build_info.c already patched")
PY

# Overlay the mbedTLS backend (src/wasi_ssl.c) and any other overlay files.
echo "Applying wget overlay from $OVERLAY_DIR ..."
while IFS= read -r -d '' file; do
  rel="${file#"$OVERLAY_DIR"/}"
  mkdir -p "$(dirname "$rel")"
  cp "$file" "$rel"
  echo "  overlaid $rel"
done < <(find "$OVERLAY_DIR" -type f -print0)

# Append the wasi_ssl.o object + mbedTLS archives to the generated src/Makefile.
# wget_OBJECTS/wget_LDADD are recursively-expanded, so += appends cleanly and
# the object is picked up by automake's built-in .c.o rule (which already
# carries our CPPFLAGS mbedTLS include and -DHAVE_WASI_TLS from CFLAGS). The
# archives are ordered tls -> x509 -> crypto for the static link.
echo "Appending wasi_ssl.o + mbedTLS link rules to src/Makefile..."
cat >> src/Makefile <<EOF

# --- agentos: in-guest mbedTLS TLS backend ---
wget_OBJECTS += wasi_ssl.\$(OBJEXT)
wget_LDADD += -L$MBEDTLS_LIBDIR -lmbedtls -lmbedx509 -lmbedcrypto
# The wget link rule's prerequisite list was already expanded above (before this
# += ran), so name wasi_ssl.o as an explicit prerequisite to force its build,
# and give it an explicit compile rule (\$(COMPILE) carries the mbedTLS include
# from CPPFLAGS and -DHAVE_WASI_TLS from CFLAGS).
wget\$(EXEEXT): wasi_ssl.\$(OBJEXT)
wasi_ssl.\$(OBJEXT): wasi_ssl.c
	\$(COMPILE) -c -o wasi_ssl.\$(OBJEXT) wasi_ssl.c
EOF

echo "Building upstream GNU Wget support library..."
make -C lib

echo "Building upstream GNU Wget..."
make -C src wget

BIN=""
for candidate in "src/wget" "src/.libs/wget" "src/wget.wasm"; do
  if [[ -f "$candidate" ]]; then
    BIN="$candidate"
    break
  fi
done

if [[ -z "$BIN" ]]; then
  echo "Unable to locate built wget binary in src/" >&2
  exit 1
fi

mkdir -p "$(dirname "$OUTPUT")"
if command -v wasm-opt >/dev/null 2>&1; then
  echo "Optimizing Wget WASM binary..."
  wasm-opt -O3 --strip-debug --all-features "$BIN" -o "$OUTPUT"
else
  cp "$BIN" "$OUTPUT"
fi

popd >/dev/null

echo "Built upstream GNU Wget at $OUTPUT"
