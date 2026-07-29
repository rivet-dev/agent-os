#!/usr/bin/env bash
set -euo pipefail

llvm_version="${LINUX_GNU_LLVM_VERSION:-22}"
sysroot_tag="${LINUX_GNU_SYSROOT_TAG:-sysroot-20250207}"
github_env="${GITHUB_ENV:?GITHUB_ENV must be set by GitHub Actions}"

export DEBIAN_FRONTEND=noninteractive

. /etc/os-release
codename="${VERSION_CODENAME:?missing VERSION_CODENAME in /etc/os-release}"
apt_list="/etc/apt/sources.list.d/llvm-toolchain-${codename}-${llvm_version}.list"

echo "deb http://apt.llvm.org/${codename}/ llvm-toolchain-${codename}-${llvm_version} main" \
  | sudo dd "of=${apt_list}" > /dev/null
curl -fsSL https://apt.llvm.org/llvm-snapshot.gpg.key \
  | gpg --dearmor \
  | sudo dd "of=/etc/apt/trusted.gpg.d/llvm-snapshot.gpg" > /dev/null

sudo apt-get update
sudo apt-get install -y --no-install-recommends \
  binutils \
  "clang-${llvm_version}" \
  "lld-${llvm_version}" \
  xz-utils

"clang-${llvm_version}" -c -o /tmp/agentos_memfd_create_shim.o \
  scripts/ci/deno-memfd-create-shim.c -fPIC
"clang-${llvm_version}" -c -o /tmp/agentos_gettid_shim.o \
  scripts/ci/agentos-gettid-shim.c -fPIC
"clang-${llvm_version}" -c -o /tmp/agentos_renameat2_shim.o \
  scripts/ci/agentos-renameat2-shim.c -fPIC

sysroot_arch="$(uname -m)"
sysroot_url="https://github.com/denoland/deno_sysroot_build/releases/download/${sysroot_tag}/sysroot-${sysroot_arch}.tar.xz"

curl -fsSL "${sysroot_url}" -o /tmp/agentos-sysroot.tar.xz
sudo rm -rf /sysroot
cd /
xzcat /tmp/agentos-sysroot.tar.xz | sudo tar -x
cd "${GITHUB_WORKSPACE:?GITHUB_WORKSPACE must be set by GitHub Actions}"

. /sysroot/.env

rustflags_extra="-C linker-plugin-lto=true \
-C linker=clang-${llvm_version} \
-C link-arg=-fuse-ld=lld-${llvm_version} \
-C link-arg=-ldl \
-C link-arg=-Wl,--allow-shlib-undefined \
-C link-arg=-Wl,--thinlto-cache-dir=$(pwd)/target/release/lto-cache \
-C link-arg=-Wl,--thinlto-cache-policy,cache_size_bytes=700m \
-C link-arg=/tmp/agentos_memfd_create_shim.o \
-C link-arg=/tmp/agentos_gettid_shim.o \
-C link-arg=/tmp/agentos_renameat2_shim.o \
${RUSTFLAGS:-}"

{
  echo "CC=/usr/bin/clang-${llvm_version}"
  echo "CFLAGS=${CFLAGS:-}"
  echo "RUSTFLAGS<<__AGENTOS_SYSROOT_RUSTFLAGS"
  echo "${rustflags_extra}"
  echo "__AGENTOS_SYSROOT_RUSTFLAGS"
} >> "${github_env}"

echo "Configured ${sysroot_tag} (${sysroot_arch}) with LLVM ${llvm_version}."
