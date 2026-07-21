#!/bin/sh
set -eu

BINARYEN_VERSION=128
DESTINATION=${1:?usage: ensure-wasm-opt.sh <destination>}

if [ -x "$DESTINATION" ] \
  && "$DESTINATION" --version 2>/dev/null | grep -Fq "version $BINARYEN_VERSION" \
  && "$DESTINATION" --help 2>/dev/null | grep -Fq -- "--translate-to-exnref"; then
  echo "wasm-opt found: $($DESTINATION --version)"
  exit 0
fi

case "$(uname -s):$(uname -m)" in
  Linux:x86_64)
    PLATFORM=x86_64-linux
    SHA256=4ce79586d1c4762502eebe9a1db071fa5e446ef8897f2f766eb1cce5ec6dee9e
    ;;
  Linux:aarch64 | Linux:arm64)
    PLATFORM=aarch64-linux
    SHA256=bafe0468976d923f09052f8ec6a6a0a9d942ee7f02ac113c85a80afea7ba3679
    ;;
  Darwin:x86_64)
    PLATFORM=x86_64-macos
    SHA256=0b4bbd58c46b73a3de1fd485579a56cd413dd395414306d9f33df407fde58b9b
    ;;
  Darwin:arm64 | Darwin:aarch64)
    PLATFORM=arm64-macos
    SHA256=0ef730ecedf2dac894812185fc78f5940ab980cdde79427e49fa87331d24422f
    ;;
  *)
    echo "unsupported Binaryen host platform: $(uname -s) $(uname -m)" >&2
    exit 1
    ;;
esac

ARCHIVE="binaryen-version_${BINARYEN_VERSION}-${PLATFORM}.tar.gz"
URL="https://github.com/WebAssembly/binaryen/releases/download/version_${BINARYEN_VERSION}/${ARCHIVE}"
TEMP_DIR=$(mktemp -d)
trap 'rm -rf "$TEMP_DIR"' EXIT HUP INT TERM

echo "downloading pinned Binaryen $BINARYEN_VERSION for $PLATFORM" >&2
curl -fL "$URL" -o "$TEMP_DIR/$ARCHIVE"
if command -v sha256sum >/dev/null 2>&1; then
  printf '%s  %s\n' "$SHA256" "$TEMP_DIR/$ARCHIVE" | sha256sum -c -
elif command -v shasum >/dev/null 2>&1; then
  printf '%s  %s\n' "$SHA256" "$TEMP_DIR/$ARCHIVE" | shasum -a 256 -c -
else
  echo "sha256sum or shasum is required to verify Binaryen" >&2
  exit 1
fi

tar -xzf "$TEMP_DIR/$ARCHIVE" -C "$TEMP_DIR"
SOURCE="$TEMP_DIR/binaryen-version_${BINARYEN_VERSION}/bin/wasm-opt"
test -x "$SOURCE"
mkdir -p "$(dirname "$DESTINATION")"
STAGED_DESTINATION="$DESTINATION.tmp.$$"
cp "$SOURCE" "$STAGED_DESTINATION"
chmod 0755 "$STAGED_DESTINATION"
mv "$STAGED_DESTINATION" "$DESTINATION"

"$DESTINATION" --version | grep -F "version $BINARYEN_VERSION"
"$DESTINATION" --help | grep -Fq -- "--translate-to-exnref"
