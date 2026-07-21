# wasmvm/c/Makefile — Build C programs to standalone WASM binaries
#
# Targets:
#   wasi-sdk    Download and cache wasi-sdk toolchain
#   programs    Compile package C commands and C test programs to WASM binaries
#   sysroot     Build patched wasi-libc sysroot (invokes patch-wasi-libc.sh)
#   install     Copy opted-in real commands to COMMANDS_DIR
#   native      Compile all .c files to native binaries (for parity testing)
#   clean       Remove build output
#
# The vanilla (unpatched) wasi-sdk sysroot is used by default.
# After US-025 adds patch-wasi-libc.sh, `make sysroot` builds a patched one.

# wasi-sdk version and platform detection
WASI_SDK_VERSION := 25
WASI_SDK_MAJOR   := 25.0

UNAME_S := $(shell uname -s)
UNAME_M := $(shell uname -m)

ifeq ($(UNAME_S),Linux)
  WASI_SDK_PLATFORM := linux
else ifeq ($(UNAME_S),Darwin)
  WASI_SDK_PLATFORM := macos
else
  WASI_SDK_PLATFORM := linux
endif

ifeq ($(UNAME_M),arm64)
  WASI_SDK_ARCH := arm64
else ifeq ($(UNAME_M),aarch64)
  WASI_SDK_ARCH := arm64
else
  WASI_SDK_ARCH := x86_64
endif

WASI_SDK_TARBALL := wasi-sdk-$(WASI_SDK_MAJOR)-$(WASI_SDK_ARCH)-$(WASI_SDK_PLATFORM).tar.gz
WASI_SDK_URL     := https://github.com/WebAssembly/wasi-sdk/releases/download/wasi-sdk-$(WASI_SDK_VERSION)/$(WASI_SDK_TARBALL)

# Directories
VENDOR_DIR   := vendor
WASI_SDK_DIR := $(VENDOR_DIR)/wasi-sdk
BUILD_DIR    := build
NATIVE_DIR   := build/native
SOFTWARE_DIR := ../../software
TEST_PROGRAMS_DIR := ../test-programs
OVERLAY_HEADERS := $(shell find include -type f -name '*.h' -print 2>/dev/null | LC_ALL=C sort)
C_SOURCE_DIRS := $(TEST_PROGRAMS_DIR) $(wildcard $(SOFTWARE_DIR)/*/native/c)
c_source = $(firstword $(foreach dir,$(C_SOURCE_DIRS),$(wildcard $(dir)/$(1).c)))

# Toolchain
CC           := $(WASI_SDK_DIR)/bin/clang
NATIVE_CC    := $(shell command -v cc 2>/dev/null || command -v gcc 2>/dev/null || command -v clang 2>/dev/null)
BINARYEN_VERSION := 128
WASM_OPT ?= $(abspath ../.cache/binaryen-version_$(BINARYEN_VERSION)/bin/wasm-opt)
export WASM_OPT
export PATH := $(dir $(WASM_OPT)):$(PATH)

# Sysroot: use patched sysroot if built, otherwise use wasi-sdk's vanilla sysroot
PATCHED_SYSROOT := sysroot
THREADED_SYSROOT := sysroot-threads
ifeq ($(wildcard $(PATCHED_SYSROOT)/lib/wasm32-wasi/libc.a),)
  SYSROOT := $(WASI_SDK_DIR)/share/wasi-sysroot
else
  SYSROOT := $(PATCHED_SYSROOT)
endif

# Compile flags
WASM_CFLAGS  := --target=wasm32-wasip1 --sysroot=$(SYSROOT) -O2 -flto -I include/
NATIVE_CFLAGS := -O0 -g -D_LARGEFILE64_SOURCE -I include/

# COMMANDS_DIR for install target (configurable, matches Rust binary output)
COMMANDS_DIR ?= ../target/wasm32-wasip1/release/commands

# Fast real commands installed by the default software gate. Slow/heavy commands
# (duckdb, vim) remain available through explicit parent `make cmd/<name>`.
COMMANDS := zip unzip envsubst sqlite3 curl wget grep git git-remote-http tree ssh ssh-keysign ssh-sk-helper \
	acl_tools xattr_tools xfs_io mknod

# Public command names emitted by multi-call filesystem helper binaries.
COMMAND_ALIASES := chacl:acl_tools getfacl:acl_tools setfacl:acl_tools \
	attr:xattr_tools getfattr:xattr_tools setfattr:xattr_tools \
	mkfifo:mknod

# Programs requiring patched sysroot (Tier 2+ custom host imports)
PATCHED_PROGRAMS := http_get_test isatty_test getpid_test getppid_test getppid_verify userinfo pipe_test dup_test closefrom_test spawn_child spawn_contract spawn_exit_code exec_edge exec_variants pipeline itimer_contract kill_child waitpid_return waitpid_edge syscall_coverage getpwuid_test libc_compat_contract libc_bounds_contract getgrouplist_bounds chown_contract signal_tests sigaction_self sigaction_behavior delayed_tcp_echo delayed_kill pipe_edge select_edge socket_flags socketpair_rights ssh_proxy_helper ssh_sk_helper_contract tcp_accept_spawn tcp_echo tcp_server http_server udp_echo unix_socket signal_handler dns_lookup getaddrinfo_connect getnameinfo_contract ppoll_contract open_flags record_lock mlock_contract sqlite3 sqlite3_mem curl wget grep tree zip unzip fs_probe acl_tools xattr_tools xfs_io mknod credentials_test fifo_test flock_test mmap_test openat_test pwritev_test self_stop_status sync_test waitpid_status xattr_test

# Discover all package command and test-program C source files.
ALL_SOURCES := $(foreach dir,$(C_SOURCE_DIRS),$(wildcard $(dir)/*.c))
# Threaded fixtures require the dedicated wasm32-wasip1-threads sysroot and
# linker contract below. Never let the generic single-thread discovery rule
# compile them against the ordinary wasm32-wasip1 sysroot.
SKIPPED_BULK_PROGRAMS := pthread_benchmark pthread_conformance

# Exclude patched-sysroot programs when only vanilla sysroot is available
ifeq ($(wildcard $(PATCHED_SYSROOT)/lib/wasm32-wasi/libc.a),)
  EXCLUDED := $(foreach program,$(PATCHED_PROGRAMS),$(call c_source,$(program)))
  SOURCES := $(filter-out $(EXCLUDED) $(foreach program,$(SKIPPED_BULK_PROGRAMS),$(call c_source,$(program))),$(ALL_SOURCES))
else
  SOURCES := $(filter-out $(foreach program,$(SKIPPED_BULK_PROGRAMS),$(call c_source,$(program))),$(ALL_SOURCES))
endif

# Program names (without .c extension)
ifeq ($(wildcard $(PATCHED_SYSROOT)/lib/wasm32-wasi/libc.a),)
  CUSTOM_WASM_PROG_NAMES :=
else
  CUSTOM_WASM_PROG_NAMES := curl wget git git-remote-http grep sqlite3 tree zip unzip
endif

WASM_PROG_NAMES := $(sort $(basename $(notdir $(SOURCES))) $(CUSTOM_WASM_PROG_NAMES))
NATIVE_PROG_NAMES := $(basename $(notdir $(SOURCES)))
EXTRA_WASM_OUTPUTS := $(addprefix $(BUILD_DIR)/,ssh ssh-keysign ssh-sk-helper)
PROGRAM_REPORT_NAMES := $(WASM_PROG_NAMES) $(notdir $(EXTRA_WASM_OUTPUTS)) fd_mapping_contract
NATIVE_REPORT_NAMES := $(NATIVE_PROG_NAMES) fd_mapping_contract

# Default WASM output targets
WASM_OUTPUTS := $(addprefix $(BUILD_DIR)/,$(WASM_PROG_NAMES)) $(EXTRA_WASM_OUTPUTS) $(BUILD_DIR)/fd_mapping_contract
# Native output targets
NATIVE_OUTPUTS := $(addprefix $(NATIVE_DIR)/,$(NATIVE_PROG_NAMES)) $(NATIVE_DIR)/fd_mapping_contract

.SECONDEXPANSION:

.PHONY: all wasi-sdk programs sysroot sysroot-threads pthread-benchmark-wasm pthread-conformance-wasm install native conformance-artifacts clean wasm-opt-check os-test os-test-native

HAS_WASM_OPT = $(shell test -x "$(WASM_OPT)" && echo 1 || echo 0)

all: programs

# --- wasi-sdk download and cache ---

wasi-sdk: $(WASI_SDK_DIR)/bin/clang

$(WASI_SDK_DIR)/bin/clang:
	@echo "=== Downloading wasi-sdk $(WASI_SDK_MAJOR) ==="
	@mkdir -p $(VENDOR_DIR)
	@if command -v curl >/dev/null 2>&1; then \
		curl -fSL "$(WASI_SDK_URL)" -o "$(VENDOR_DIR)/$(WASI_SDK_TARBALL)"; \
	elif command -v wget >/dev/null 2>&1; then \
		wget -q "$(WASI_SDK_URL)" -O "$(VENDOR_DIR)/$(WASI_SDK_TARBALL)"; \
	else \
		echo "Error: neither curl nor wget found"; exit 1; \
	fi
	@echo "Extracting..."
	@tar -xzf "$(VENDOR_DIR)/$(WASI_SDK_TARBALL)" -C "$(VENDOR_DIR)"
	@mv "$(VENDOR_DIR)/wasi-sdk-$(WASI_SDK_MAJOR)-$(WASI_SDK_ARCH)-$(WASI_SDK_PLATFORM)" "$(WASI_SDK_DIR)"
	@rm -f "$(VENDOR_DIR)/$(WASI_SDK_TARBALL)"
	@echo "wasi-sdk $(WASI_SDK_MAJOR) installed to $(WASI_SDK_DIR)"

# --- C library dependencies (downloaded at build time) ---
# Unmodified upstream libs are fetched from their official sources.
# Modified libs (curl with WASI patches) are fetched from rivet-dev forks.
# All downloads cached in libs/ — add libs/ to .gitignore.

SQLITE3_URL := https://www.sqlite.org/2024/sqlite-amalgamation-3470200.zip
CJSON_URL   := https://github.com/DaveGamble/cJSON/archive/refs/tags/v1.7.18.zip
CURL_COMMIT := main
CURL_FORK_REPO_PREFIX := secure
CURL_FORK_REPO_SUFFIX := exec-curl
CURL_FORK_REPO := $(CURL_FORK_REPO_PREFIX)-$(CURL_FORK_REPO_SUFFIX)
CURL_URL    := https://github.com/rivet-dev/$(CURL_FORK_REPO)/archive/refs/heads/$(CURL_COMMIT).zip
CURL_TOOL_VERSION := 8_11_1
CURL_TOOL_URL := https://github.com/curl/curl/archive/refs/tags/curl-$(CURL_TOOL_VERSION).tar.gz
CURL_RELEASE_VERSION := 8.11.1
CURL_RELEASE_TAG := 8_11_1
CURL_RELEASE_URL := https://github.com/curl/curl/releases/download/curl-$(CURL_RELEASE_TAG)/curl-$(CURL_RELEASE_VERSION).tar.xz
CURL_UPSTREAM_BUILD_DIR := $(BUILD_DIR)/curl-upstream
# Reusable libcurl artifact (headers + libcurl.a) installed by the curl build so
# git's git-remote-http links the same overlaid, mbedTLS-linked libcurl.
CURL_LIBCURL_PREFIX := $(CURL_UPSTREAM_BUILD_DIR)/install
CURL_LIBCURL_ARTIFACT := $(CURL_LIBCURL_PREFIX)/lib/libcurl.a
CURL_UPSTREAM_OVERLAY_DIR := ../../software/curl/native/c/overlay
CURL_UPSTREAM_OVERLAY_FILES := $(wildcard $(CURL_UPSTREAM_OVERLAY_DIR)/lib/*.c $(CURL_UPSTREAM_OVERLAY_DIR)/lib/*.h $(CURL_UPSTREAM_OVERLAY_DIR)/lib/vtls/*.c $(CURL_UPSTREAM_OVERLAY_DIR)/lib/vtls/*.h)
ZLIB_VERSION := 1.3.1
ZLIB_URL := https://github.com/madler/zlib/releases/download/v$(ZLIB_VERSION)/zlib-$(ZLIB_VERSION).tar.gz
ZLIB_BUILD_DIR := $(BUILD_DIR)/zlib
# mbedTLS 3.6 LTS — in-guest TLS for curl/wget/git (see
# docs-internal/networking-parity-spec.md). Use the prepared release tarball
# (`mbedtls-<ver>.tar.bz2`), which bundles the generated sources and the
# `framework` submodule; the bare GitHub source archive does not. Everything
# ships under library/ in the 3.6 LTS line (crypto is not yet split into a
# separate tf-psa-crypto tree as on 4.x/master), so `make lib` needs no
# submodule fetch. We cross-compile every library/*.c against the wasi-sdk
# sysroot, mirroring the zlib rule, plus a getentropy() entropy shim.
MBEDTLS_VERSION := 3.6.2
MBEDTLS_URL := https://github.com/Mbed-TLS/mbedtls/releases/download/mbedtls-$(MBEDTLS_VERSION)/mbedtls-$(MBEDTLS_VERSION).tar.bz2
# Deferred `=`: LIBS_CACHE is defined further down, so expand it lazily.
MBEDTLS_SRC_DIR = $(LIBS_CACHE)/mbedtls-$(MBEDTLS_VERSION)
MBEDTLS_BUILD_DIR := $(BUILD_DIR)/mbedtls
MBEDTLS_CONFIG_DIR := mbedtls
MBEDTLS_LIBS := $(MBEDTLS_BUILD_DIR)/libmbedtls.a $(MBEDTLS_BUILD_DIR)/libmbedx509.a $(MBEDTLS_BUILD_DIR)/libmbedcrypto.a
WGET_VERSION := 1.24.5
WGET_URL := https://ftp.gnu.org/gnu/wget/wget-$(WGET_VERSION).tar.gz
WGET_UPSTREAM_BUILD_DIR := $(BUILD_DIR)/wget-upstream
WGET_UPSTREAM_OVERLAY_INCLUDE_DIR := overlays/wget/include
# mbedTLS backend overlay (mirrors the curl overlay layout): src/wasi_ssl.c
# is copied into the wget source tree at configure time.
WGET_UPSTREAM_OVERLAY_DIR := ../../software/wget/native/c/overlay
WGET_UPSTREAM_OVERLAY_FILES := $(wildcard $(WGET_UPSTREAM_OVERLAY_INCLUDE_DIR)/*.h) \
                               $(shell find $(WGET_UPSTREAM_OVERLAY_DIR) -type f 2>/dev/null)
GIT_VERSION := 2.55.0
GIT_URL := https://www.kernel.org/pub/software/scm/git/git-$(GIT_VERSION).tar.xz
GIT_UPSTREAM_BUILD_DIR := $(BUILD_DIR)/git-upstream
GIT_PATCH_DIR := patches/git
# OpenSSH portable — real upstream ssh client (batch/key-based), linked with a
# hermetic libcrypto build for the same RSA/ECDSA/DH/AES families as Linux.
OPENSSH_VERSION := 10.4p1
OPENSSH_URL := https://cdn.openbsd.org/pub/OpenBSD/OpenSSH/portable/openssh-$(OPENSSH_VERSION).tar.gz
OPENSSH_SHA256 := ef6026dd2aea8d56059638d5d3262902c892ceba9f88395835e0d06d3fb63238
OPENSSH_UPSTREAM_BUILD_DIR := $(BUILD_DIR)/ssh-upstream
OPENSSH_PATCH_DIR := patches/openssh
OPENSSL_VERSION := 3.5.7
OPENSSL_URL := https://github.com/openssl/openssl/releases/download/openssl-$(OPENSSL_VERSION)/openssl-$(OPENSSL_VERSION).tar.gz
OPENSSL_SHA256 := a8c0d28a529ca480f9f36cf5792e2cd21984552a3c8e4aa11a24aa31aeac98e8
OPENSSL_UPSTREAM_BUILD_DIR := $(BUILD_DIR)/openssl-upstream
OPENSSL_PREFIX := $(BUILD_DIR)/openssl
OPENSSL_LIBCRYPTO := $(OPENSSL_PREFIX)/lib/libcrypto.a
OPENSSL_HEADER := $(OPENSSL_PREFIX)/include/openssl/opensslv.h
GREP_VERSION := 3.12
GREP_URL := https://ftp.gnu.org/gnu/grep/grep-$(GREP_VERSION).tar.xz
GREP_UPSTREAM_BUILD_DIR := $(BUILD_DIR)/grep-upstream
INFOZIP_ZIP_VERSION := 30
INFOZIP_ZIP_URL := https://downloads.sourceforge.net/infozip/zip$(INFOZIP_ZIP_VERSION).tar.gz
INFOZIP_ZIP_BUILD_DIR := $(BUILD_DIR)/infozip-zip-upstream
INFOZIP_UNZIP_VERSION := 60
INFOZIP_UNZIP_URL := https://downloads.sourceforge.net/infozip/unzip$(INFOZIP_UNZIP_VERSION).tar.gz
INFOZIP_UNZIP_BUILD_DIR := $(BUILD_DIR)/infozip-unzip-upstream
TREE_VERSION := 2.3.2
TREE_URL := https://gitlab.com/OldManProgrammer/unix-tree/-/archive/$(TREE_VERSION)/unix-tree-$(TREE_VERSION).tar.gz
# duckdb-wasm currently documents DuckDB v1.5.0 in its README. We build the
# matching upstream DuckDB tag ourselves with our patched WASI/POSIX sysroot.
DUCKDB_VERSION := v1.5.0
DUCKDB_GIT_DESCRIBE := $(DUCKDB_VERSION)-0-g0123456789
DUCKDB_URL := https://github.com/duckdb/duckdb/archive/refs/tags/$(DUCKDB_VERSION).tar.gz

LIBS_DIR    := libs
LIBS_CACHE  := .cache/libs

TREE_SOURCES := color.c file.c filter.c hash.c html.c info.c json.c list.c tree.c unix.c util.c xml.c strverscmp.c
TREE_FILES := $(addprefix $(LIBS_DIR)/tree/,$(TREE_SOURCES) tree.h)
TREE_WASM_FLAGS := -DLARGEFILE_SOURCE -D_FILE_OFFSET_BITS=64

libs/sqlite3/sqlite3.c libs/sqlite3/sqlite3.h libs/sqlite3/shell.c:
	@echo "Fetching sqlite3..."
	@mkdir -p $(LIBS_CACHE) $(LIBS_DIR)/sqlite3
	@curl -fSL "$(SQLITE3_URL)" -o "$(LIBS_CACHE)/sqlite3.zip"
	@cd $(LIBS_CACHE) && unzip -qo sqlite3.zip
	@cp $(LIBS_CACHE)/sqlite-amalgamation-*/sqlite3.c \
	    $(LIBS_CACHE)/sqlite-amalgamation-*/sqlite3.h \
	    $(LIBS_CACHE)/sqlite-amalgamation-*/shell.c \
	    $(LIBS_DIR)/sqlite3/

$(TREE_FILES): $(LIBS_DIR)/tree/.fetched

$(LIBS_DIR)/tree/.fetched:
	@echo "Fetching upstream tree $(TREE_VERSION)..."
	@mkdir -p $(LIBS_CACHE) $(LIBS_DIR)/tree
	@curl -fSL "$(TREE_URL)" -o "$(LIBS_CACHE)/tree-$(TREE_VERSION).tar.gz"
	@rm -rf "$(LIBS_CACHE)/unix-tree-$(TREE_VERSION)"
	@tar -xzf "$(LIBS_CACHE)/tree-$(TREE_VERSION).tar.gz" -C "$(LIBS_CACHE)"
	@cp $(addprefix $(LIBS_CACHE)/unix-tree-$(TREE_VERSION)/,$(TREE_SOURCES) tree.h) $(LIBS_DIR)/tree/
	@touch $@

libs/cjson/cJSON.c:
	@echo "Fetching cJSON..."
	@mkdir -p $(LIBS_CACHE) $(LIBS_DIR)/cjson
	@curl -fSL "$(CJSON_URL)" -o "$(LIBS_CACHE)/cjson.zip"
	@cd $(LIBS_CACHE) && unzip -qo cjson.zip
	@cp $(LIBS_CACHE)/cJSON-*/cJSON.c $(LIBS_CACHE)/cJSON-*/cJSON.h $(LIBS_DIR)/cjson/

libs/curl/lib/easy.c:
	@echo "Fetching curl (rivet-dev/$(CURL_FORK_REPO))..."
	@mkdir -p $(LIBS_CACHE)
	@curl -fSL "$(CURL_URL)" -o "$(LIBS_CACHE)/curl.zip"
	@cd $(LIBS_CACHE) && unzip -qo curl.zip
	@rm -rf $(LIBS_DIR)/curl
	@mv $(LIBS_CACHE)/$(CURL_FORK_REPO)-* $(LIBS_DIR)/curl

libs/duckdb/CMakeLists.txt:
	@echo "Fetching DuckDB ($(DUCKDB_VERSION))..."
	@mkdir -p $(LIBS_CACHE)
	@curl -fSL "$(DUCKDB_URL)" -o "$(LIBS_CACHE)/duckdb.tar.gz"
	@rm -rf $(LIBS_DIR)/duckdb
	@mkdir -p $(LIBS_DIR)/duckdb
	@tar -xzf "$(LIBS_CACHE)/duckdb.tar.gz" --strip-components=1 -C $(LIBS_DIR)/duckdb

.PHONY: fetch-libs fetch-fast-libs clean-libs
fetch-libs: libs/sqlite3/sqlite3.c libs/cjson/cJSON.c libs/curl/lib/easy.c libs/duckdb/CMakeLists.txt
fetch-fast-libs: libs/sqlite3/sqlite3.c libs/cjson/cJSON.c libs/curl/lib/easy.c

clean-libs:
	rm -rf $(LIBS_DIR) $(LIBS_CACHE)

# --- os-test POSIX conformance suite (downloaded at build time) ---
# ISC-licensed test suite from the Sortix project (https://sortix.org/os-test/)

OS_TEST_VERSION := main
OS_TEST_URL     := https://gitlab.com/sortix/os-test/-/archive/$(OS_TEST_VERSION)/os-test-$(OS_TEST_VERSION).tar.gz
OS_TEST_DIR     := os-test

.PHONY: fetch-os-test clean-os-test

fetch-os-test: $(OS_TEST_DIR)/include
$(OS_TEST_DIR)/include:
	@echo "Fetching os-test $(OS_TEST_VERSION)..."
	@mkdir -p $(LIBS_CACHE)
	@curl -fSL "$(OS_TEST_URL)" -o "$(LIBS_CACHE)/os-test.tar.gz"
	@mkdir -p $(OS_TEST_DIR)
	@tar -xzf "$(LIBS_CACHE)/os-test.tar.gz" --strip-components=1 -C $(OS_TEST_DIR)
	@echo "os-test $(OS_TEST_VERSION) extracted to $(OS_TEST_DIR)/"
	@echo "  include/ : $$(find $(OS_TEST_DIR)/include -name '*.h' 2>/dev/null | wc -l) headers"
	@echo "  src/     : $$(find $(OS_TEST_DIR)/src -name '*.c' 2>/dev/null | wc -l) test programs"

clean-os-test:
	rm -rf $(OS_TEST_DIR) $(LIBS_CACHE)/os-test.tar.gz

# --- os-test POSIX conformance build targets ---
# Compile every os-test C file to WASM and native binaries.
# Individual compile failures are expected (missing headers, unsupported features)
# and do not abort the build.

OS_TEST_BUILD        := $(BUILD_DIR)/os-test
OS_TEST_NATIVE_BUILD := $(NATIVE_DIR)/os-test
OS_TEST_WASM_CFLAGS  := --target=wasm32-wasip1 --sysroot=$(SYSROOT) -O0 \
                         -D_GNU_SOURCE -D_BSD_SOURCE -D_ALL_SOURCE -D_DEFAULT_SOURCE
OS_TEST_WASM_LDFLAGS := -lc-printscan-long-double
# Stub main() for namespace/ tests (compile-only header conformance checks)
OS_TEST_NS_MAIN := os-test-overrides/namespace_main.c
OS_TEST_NATIVE_CFLAGS := -O0 -g \
                          -D_GNU_SOURCE -D_BSD_SOURCE -D_ALL_SOURCE -D_DEFAULT_SOURCE
OS_TEST_NATIVE_LDFLAGS := -lm
ifeq ($(UNAME_S),Linux)
  OS_TEST_NATIVE_LDFLAGS += -lpthread -lrt
endif
ifeq ($(UNAME_S),Darwin)
  OS_TEST_NATIVE_LDFLAGS += -lpthread
endif

.PHONY: os-test os-test-native

os-test: wasi-sdk fetch-os-test
	@total=0; pass=0; fail=0; \
	for src in $$(find $(OS_TEST_DIR) -name '*.c' -not -path '*/.expect/*' -not -path '*/misc/*' | sort); do \
		rel=$${src#$(OS_TEST_DIR)/}; \
		name=$${rel%.c}; \
		out=$(OS_TEST_BUILD)/$$name; \
		mkdir -p "$$(dirname "$$out")"; \
		total=$$((total + 1)); \
		extras=""; \
		case "$$rel" in namespace/*) extras="$(OS_TEST_NS_MAIN)";; esac; \
		if $(CC) $(OS_TEST_WASM_CFLAGS) -o "$$out" "$$src" $$extras $(OS_TEST_WASM_LDFLAGS) 2>/dev/null; then \
			pass=$$((pass + 1)); \
		else \
			echo "COMPILE FAILED: $$name"; \
			rm -f "$$out"; \
			fail=$$((fail + 1)); \
		fi; \
	done; \
	echo ""; \
	echo "=== os-test WASM Build Report ==="; \
	echo "Total:    $$total"; \
	echo "Compiled: $$pass"; \
	echo "Failed:   $$fail"; \
	echo "Output:   $(OS_TEST_BUILD)/"; \
	echo "=== Build complete ==="; \
	if [ $$fail -gt 0 ]; then echo "ERROR: $$fail tests failed to compile" >&2; exit 1; fi

os-test-native: fetch-os-test
	@total=0; pass=0; fail=0; \
	for src in $$(find $(OS_TEST_DIR) -name '*.c' -not -path '*/.expect/*' -not -path '*/misc/*' | sort); do \
		rel=$${src#$(OS_TEST_DIR)/}; \
		out=$(OS_TEST_NATIVE_BUILD)/$${rel%.c}; \
		mkdir -p "$$(dirname "$$out")"; \
		total=$$((total + 1)); \
		extras=""; \
		case "$$rel" in namespace/*) extras="$(OS_TEST_NS_MAIN)";; esac; \
		if $(NATIVE_CC) $(OS_TEST_NATIVE_CFLAGS) -o "$$out" "$$src" $$extras $(OS_TEST_NATIVE_LDFLAGS) 2>/dev/null; then \
			pass=$$((pass + 1)); \
		else \
			rm -f "$$out"; \
			fail=$$((fail + 1)); \
		fi; \
	done; \
	echo ""; \
	echo "=== os-test Native Build Report ==="; \
	echo "Total:    $$total"; \
	echo "Compiled: $$pass"; \
	echo "Failed:   $$fail"; \
	echo "Output:   $(OS_TEST_NATIVE_BUILD)/"; \
	echo "=== Build complete ==="

# --- musl libc-test conformance suite (downloaded at build time) ---
# MIT-licensed test suite from musl (Bytecode Alliance mirror)

LIBC_TEST_VERSION := master
LIBC_TEST_URL     := https://github.com/bytecodealliance/libc-test/archive/refs/heads/$(LIBC_TEST_VERSION).tar.gz
LIBC_TEST_DIR     := libc-test

.PHONY: fetch-libc-test clean-libc-test

fetch-libc-test: $(LIBC_TEST_DIR)/src
$(LIBC_TEST_DIR)/src:
	@echo "Fetching musl libc-test $(LIBC_TEST_VERSION)..."
	@mkdir -p $(LIBS_CACHE)
	@curl -fSL "$(LIBC_TEST_URL)" -o "$(LIBS_CACHE)/libc-test.tar.gz"
	@mkdir -p $(LIBC_TEST_DIR)
	@tar -xzf "$(LIBS_CACHE)/libc-test.tar.gz" --strip-components=1 -C $(LIBC_TEST_DIR)
	@echo "libc-test $(LIBC_TEST_VERSION) extracted to $(LIBC_TEST_DIR)/"
	@echo "  src/ : $$(find $(LIBC_TEST_DIR)/src -name '*.c' 2>/dev/null | wc -l) test programs"

clean-libc-test:
	rm -rf $(LIBC_TEST_DIR) $(LIBS_CACHE)/libc-test.tar.gz

# --- musl libc-test build targets ---
# Compile functional/ and regression/ tests to WASM and native binaries.
# Tests requiring fork/exec/signals/pthreads will fail to compile for WASM — that's expected.
# The test.h framework (src/common/) is compiled into a static library first.

LIBC_TEST_BUILD        := $(BUILD_DIR)/libc-test
LIBC_TEST_NATIVE_BUILD := $(NATIVE_DIR)/libc-test
LIBC_TEST_WASM_CFLAGS  := --target=wasm32-wasip1 --sysroot=$(SYSROOT) -O0 \
                           -D_GNU_SOURCE -D_BSD_SOURCE -D_ALL_SOURCE -D_DEFAULT_SOURCE \
                           -I$(LIBC_TEST_DIR)/src/common
LIBC_TEST_WASM_LDFLAGS := -lc-printscan-long-double -lm
LIBC_TEST_NATIVE_CFLAGS := -O0 -g \
                            -D_GNU_SOURCE -D_BSD_SOURCE -D_ALL_SOURCE -D_DEFAULT_SOURCE \
                            -I$(LIBC_TEST_DIR)/src/common
LIBC_TEST_NATIVE_LDFLAGS := -lm
ifeq ($(UNAME_S),Linux)
  LIBC_TEST_NATIVE_LDFLAGS += -lpthread -lrt -lcrypt -lresolv -lutil -ldl
endif
ifeq ($(UNAME_S),Darwin)
  LIBC_TEST_NATIVE_LDFLAGS += -lpthread
endif

# Test framework sources compiled alongside each test
LIBC_TEST_COMMON_SRCS := $(LIBC_TEST_DIR)/src/common/print.c

.PHONY: libc-test libc-test-native

libc-test: wasi-sdk fetch-libc-test
	@total=0; pass=0; fail=0; \
	for src in $$(find $(LIBC_TEST_DIR)/src/functional $(LIBC_TEST_DIR)/src/regression -name '*.c' 2>/dev/null | sort); do \
		rel=$${src#$(LIBC_TEST_DIR)/src/}; \
		name=$${rel%.c}; \
		out=$(LIBC_TEST_BUILD)/$$name; \
		mkdir -p "$$(dirname "$$out")"; \
		total=$$((total + 1)); \
		if $(CC) $(LIBC_TEST_WASM_CFLAGS) -o "$$out" "$$src" $(LIBC_TEST_COMMON_SRCS) $(LIBC_TEST_WASM_LDFLAGS) 2>/dev/null; then \
			pass=$$((pass + 1)); \
		else \
			rm -f "$$out"; \
			fail=$$((fail + 1)); \
		fi; \
	done; \
	echo ""; \
	echo "=== libc-test WASM Build Report ==="; \
	echo "Total:    $$total"; \
	echo "Compiled: $$pass"; \
	echo "Failed:   $$fail"; \
	echo "Output:   $(LIBC_TEST_BUILD)/"; \
	echo "=== Build complete ==="

libc-test-native: fetch-libc-test
	@total=0; pass=0; fail=0; \
	for src in $$(find $(LIBC_TEST_DIR)/src/functional $(LIBC_TEST_DIR)/src/regression -name '*.c' 2>/dev/null | sort); do \
		rel=$${src#$(LIBC_TEST_DIR)/src/}; \
		out=$(LIBC_TEST_NATIVE_BUILD)/$${rel%.c}; \
		mkdir -p "$$(dirname "$$out")"; \
		total=$$((total + 1)); \
		if $(NATIVE_CC) $(LIBC_TEST_NATIVE_CFLAGS) -o "$$out" "$$src" $(LIBC_TEST_COMMON_SRCS) $(LIBC_TEST_NATIVE_LDFLAGS) 2>/dev/null; then \
			pass=$$((pass + 1)); \
		else \
			rm -f "$$out"; \
			fail=$$((fail + 1)); \
		fi; \
	done; \
	echo ""; \
	echo "=== libc-test Native Build Report ==="; \
	echo "Total:    $$total"; \
	echo "Compiled: $$pass"; \
	echo "Failed:   $$fail"; \
	echo "Output:   $(LIBC_TEST_NATIVE_BUILD)/"; \
	echo "=== Build complete ==="

# --- Patched sysroot (delegates to patch-wasi-libc.sh) ---

WASI_LIBC_PATCHES := $(wildcard ../std-patches/wasi-libc/*.patch)
WASI_LIBC_OVERRIDES := $(wildcard ../std-patches/wasi-libc-overrides/*.c)
LLVM_RUNTIME_PATCHES := $(wildcard patches/llvm-project/*.patch)

$(PATCHED_SYSROOT)/lib/wasm32-wasi/libc.a: $(WASI_SDK_DIR)/bin/clang ../scripts/patch-wasi-libc.sh scripts/build-llvm-runtimes.sh $(WASI_LIBC_PATCHES) $(WASI_LIBC_OVERRIDES) $(LLVM_RUNTIME_PATCHES)
	../scripts/patch-wasi-libc.sh

sysroot: $(PATCHED_SYSROOT)/lib/wasm32-wasi/libc.a

$(THREADED_SYSROOT)/lib/wasm32-wasi-threads/libc.a: $(WASI_SDK_DIR)/bin/clang ../scripts/patch-wasi-libc.sh scripts/build-llvm-runtimes.sh $(WASI_LIBC_PATCHES) $(WASI_LIBC_OVERRIDES) $(LLVM_RUNTIME_PATCHES)
	../scripts/patch-wasi-libc.sh --threads

sysroot-threads: $(THREADED_SYSROOT)/lib/wasm32-wasi-threads/libc.a

build/pthread_conformance.wasm: ../test-programs/pthread_conformance.c $(THREADED_SYSROOT)/lib/wasm32-wasi-threads/libc.a
	@mkdir -p $(dir $@)
	$(CC) --target=wasm32-wasip1-threads --sysroot=$(THREADED_SYSROOT) -O2 -pthread \
		-Wl,--import-memory -Wl,--export-memory -Wl,--max-memory=134217728 \
		-Wl,--export=wasi_thread_start -o $@ $<

pthread-conformance-wasm: build/pthread_conformance.wasm

build/pthread_benchmark.wasm: ../test-programs/pthread_benchmark.c $(THREADED_SYSROOT)/lib/wasm32-wasi-threads/libc.a
	@mkdir -p $(dir $@)
	$(CC) --target=wasm32-wasip1-threads --sysroot=$(THREADED_SYSROOT) -O2 -pthread \
		-Wl,--import-memory -Wl,--export-memory -Wl,--max-memory=134217728 \
		-Wl,--export=wasi_thread_start -o $@ $<

pthread-benchmark-wasm: build/pthread_benchmark.wasm

# Patched fixtures must relink whenever the owned libc changes. Without this
# edge, make can leave an older host-import ABI embedded in an otherwise
# up-to-date test binary after a sysroot rebuild.
PATCHED_WASM_OUTPUTS := $(addprefix $(BUILD_DIR)/,$(PATCHED_PROGRAMS))
$(PATCHED_WASM_OUTPUTS): $(PATCHED_SYSROOT)/lib/wasm32-wasi/libc.a
$(PATCHED_WASM_OUTPUTS): WASM_CFLAGS := --target=wasm32-wasip1 --sysroot=$(PATCHED_SYSROOT) -O2 -flto -I include/
$(BUILD_DIR)/select_edge: WASM_CFLAGS += -DFD_SETSIZE=8192

# --- wasm-opt check ---

wasm-opt-check:
	@../scripts/ensure-wasm-opt.sh "$(WASM_OPT)"

# --- Compile all C programs to WASM ---

programs: wasi-sdk fetch-fast-libs wasm-opt-check $(WASM_OUTPUTS)
	@echo ""
	@echo "=== C WASM Build Report ==="
	@echo "Programs: $(words $(PROGRAM_REPORT_NAMES)) compiled"
	@echo "Sysroot:  $(SYSROOT)"
	@echo "Output:   $(BUILD_DIR)/"
	@echo "=== Build complete ==="

# Default rule: single-file programs
$(BUILD_DIR)/%: $$(call c_source,$$*) $(WASI_SDK_DIR)/bin/clang
	@mkdir -p $(BUILD_DIR)
	$(CC) $(WASM_CFLAGS) -o $@.wasm $<
	@if [ "$(HAS_WASM_OPT)" = "1" ]; then \
		wasm-opt -O3 --strip-debug $@.wasm -o $@; \
	else \
		cp $@.wasm $@; \
	fi
	@rm -f $@.wasm

$(BUILD_DIR)/fd_mapping_contract: $(PATCHED_SYSROOT)/lib/wasm32-wasi/libc.a
	$(MAKE) -C .. fd-mapping-contract-wasm

# --- Per-program override rules for multi-file compilation ---
# Add overrides here for programs that link vendored libraries.

# json_parse: links cJSON library
$(BUILD_DIR)/json_parse: $(call c_source,json_parse) libs/cjson/cJSON.c $(WASI_SDK_DIR)/bin/clang
	@mkdir -p $(BUILD_DIR)
	$(CC) $(WASM_CFLAGS) -Ilibs/cjson -o $@.wasm $(call c_source,json_parse) libs/cjson/cJSON.c
	@if [ "$(HAS_WASM_OPT)" = "1" ]; then \
		wasm-opt -O3 --strip-debug $@.wasm -o $@; \
	else \
		cp $@.wasm $@; \
	fi
	@rm -f $@.wasm

$(NATIVE_DIR)/json_parse: $(call c_source,json_parse) libs/cjson/cJSON.c
	@mkdir -p $(NATIVE_DIR)
	$(NATIVE_CC) $(NATIVE_CFLAGS) -Ilibs/cjson -o $@ $(call c_source,json_parse) libs/cjson/cJSON.c

# sqlite3_mem: links SQLite amalgamation
SQLITE_COMMON  := -DSQLITE_OMIT_WAL -DSQLITE_OMIT_LOAD_EXTENSION -DSQLITE_THREADSAFE=0 \
                  -DSQLITE_OMIT_LOCALTIME
SQLITE_WASM    := -DSQLITE_OS_OTHER $(SQLITE_COMMON) -lwasi-emulated-mman
SQLITE_NATIVE  := $(SQLITE_COMMON)
SQLITE_SHELL_COMMON := -DSQLITE_OMIT_WAL -DSQLITE_OMIT_LOAD_EXTENSION -DSQLITE_THREADSAFE=0 \
                       -DSQLITE_OMIT_LOCALTIME -DSQLITE_NOHAVE_SYSTEM -DHAVE_READLINE=0
SQLITE_SHELL_WASM := $(SQLITE_SHELL_COMMON) -D_WASI_EMULATED_PROCESS_CLOCKS \
                     -lwasi-emulated-mman -lwasi-emulated-process-clocks
SQLITE_SHELL_NATIVE := $(SQLITE_SHELL_COMMON)

$(BUILD_DIR)/sqlite3_mem: $(call c_source,sqlite3_mem) libs/sqlite3/sqlite3.c $(WASI_SDK_DIR)/bin/clang
	@mkdir -p $(BUILD_DIR)
	$(CC) --target=wasm32-wasip1 --sysroot=$(PATCHED_SYSROOT) -Os -I include/ \
	    $(SQLITE_WASM) -Ilibs/sqlite3 -Wl,--initial-memory=16777216 -o $@ \
	    $(call c_source,sqlite3_mem) libs/sqlite3/sqlite3.c

$(NATIVE_DIR)/sqlite3_mem: $(call c_source,sqlite3_mem) libs/sqlite3/sqlite3.c
	@mkdir -p $(NATIVE_DIR)
	$(NATIVE_CC) $(NATIVE_CFLAGS) $(SQLITE_NATIVE) -Ilibs/sqlite3 -o $@ \
	    $(call c_source,sqlite3_mem) libs/sqlite3/sqlite3.c -lm -lpthread

# sqlite3: full official CLI, links SQLite amalgamation.
# Uses -Os (not -O2) + no wasm-opt — higher optimization breaks SQLite's
# internal function pointer tables in WASM indirect call table.
# Uses --initial-memory=16777216 (16MB) for SQLite's malloc requirements.
$(BUILD_DIR)/sqlite3: libs/sqlite3/shell.c libs/sqlite3/sqlite3.c $(WASI_SDK_DIR)/bin/clang
	@mkdir -p $(BUILD_DIR)
	$(CC) --target=wasm32-wasip1 --sysroot=$(PATCHED_SYSROOT) -Os -I include/ \
	    $(SQLITE_SHELL_WASM) -Ilibs/sqlite3 -Wl,--initial-memory=16777216 \
	    -o $@ libs/sqlite3/shell.c libs/sqlite3/sqlite3.c

$(BUILD_DIR)/sqlite3_cli: $(BUILD_DIR)/sqlite3
	cp $< $@

$(NATIVE_DIR)/sqlite3: libs/sqlite3/shell.c libs/sqlite3/sqlite3.c
	@mkdir -p $(NATIVE_DIR)
	$(NATIVE_CC) $(NATIVE_CFLAGS) $(SQLITE_SHELL_NATIVE) -Ilibs/sqlite3 -o $@ \
	    libs/sqlite3/shell.c libs/sqlite3/sqlite3.c -lm -lpthread

$(LIBS_CACHE)/zlib-$(ZLIB_VERSION)/zlib.h:
	@echo "Fetching zlib $(ZLIB_VERSION)..."
	@mkdir -p $(LIBS_CACHE)
	@curl -fSL "$(ZLIB_URL)" -o "$(LIBS_CACHE)/zlib-$(ZLIB_VERSION).tar.gz"
	@rm -rf "$(LIBS_CACHE)/zlib-$(ZLIB_VERSION)"
	@tar -xzf "$(LIBS_CACHE)/zlib-$(ZLIB_VERSION).tar.gz" -C "$(LIBS_CACHE)"

$(ZLIB_BUILD_DIR)/libz.a: $(LIBS_CACHE)/zlib-$(ZLIB_VERSION)/zlib.h $(PATCHED_SYSROOT)/lib/wasm32-wasi/libc.a $(WASI_SDK_DIR)/bin/clang
	@mkdir -p $(ZLIB_BUILD_DIR)
	@for src in adler32.c crc32.c deflate.c infback.c inffast.c inflate.c inftrees.c trees.c zutil.c compress.c uncompr.c gzclose.c gzlib.c gzread.c gzwrite.c; do \
		$(CC) --target=wasm32-wasip1 --sysroot=$(PATCHED_SYSROOT) -O2 -DHAVE_UNISTD_H \
			-I$(LIBS_CACHE)/zlib-$(ZLIB_VERSION) \
			-c "$(LIBS_CACHE)/zlib-$(ZLIB_VERSION)/$$src" \
			-o "$(ZLIB_BUILD_DIR)/$${src%.c}.o"; \
	done
	$(WASI_SDK_DIR)/bin/llvm-ar rcs $@ $(ZLIB_BUILD_DIR)/*.o

# --- mbedTLS 3.6 LTS for wasm32-wasip1 ---
# Fetch/extract the prepared release tarball, then cross-compile every
# library/*.c (plus the getentropy entropy shim) against the wasi-sdk sysroot,
# splitting objects into the three canonical archives by filename the same way
# mbedTLS's own Makefile does (crypto / x509 / tls). Single-threaded; entropy
# via getentropy() (MBEDTLS_ENTROPY_HARDWARE_ALT); TLS 1.3 on (default in 3.6).
$(MBEDTLS_SRC_DIR)/library/ssl_tls.c:
	@echo "Fetching mbedTLS $(MBEDTLS_VERSION)..."
	@mkdir -p $(LIBS_CACHE)
	@curl -fSL "$(MBEDTLS_URL)" -o "$(LIBS_CACHE)/mbedtls-$(MBEDTLS_VERSION).tar.bz2"
	@rm -rf "$(MBEDTLS_SRC_DIR)"
	@tar -xjf "$(LIBS_CACHE)/mbedtls-$(MBEDTLS_VERSION).tar.bz2" -C "$(LIBS_CACHE)"

.PHONY: mbedtls
mbedtls: $(MBEDTLS_LIBS)

$(MBEDTLS_LIBS) &: $(MBEDTLS_SRC_DIR)/library/ssl_tls.c \
                $(MBEDTLS_CONFIG_DIR)/wasi_user_config.h \
                $(MBEDTLS_CONFIG_DIR)/mbedtls_wasi_entropy.c \
                $(PATCHED_SYSROOT)/lib/wasm32-wasi/libc.a \
                $(WASI_SDK_DIR)/bin/clang
	@echo "=== Building mbedTLS $(MBEDTLS_VERSION) for wasm32-wasip1 ==="
	@mkdir -p $(MBEDTLS_BUILD_DIR)/crypto $(MBEDTLS_BUILD_DIR)/x509 $(MBEDTLS_BUILD_DIR)/tls
	@for src in $(MBEDTLS_SRC_DIR)/library/*.c $(MBEDTLS_CONFIG_DIR)/mbedtls_wasi_entropy.c; do \
		name=$$(basename "$${src%.c}"); \
		case "$$name" in \
			x509*|pkcs7) grp=x509 ;; \
			ssl_*|ssl|mps_*|debug|net_sockets) grp=tls ;; \
			*) grp=crypto ;; \
		esac; \
		$(CC) --target=wasm32-wasip1 --sysroot=$(PATCHED_SYSROOT) -O2 \
			-I$(MBEDTLS_SRC_DIR)/include -I$(MBEDTLS_CONFIG_DIR) \
			-DMBEDTLS_USER_CONFIG_FILE='"wasi_user_config.h"' \
			-c "$$src" -o "$(MBEDTLS_BUILD_DIR)/$$grp/$$name.o" || exit 1; \
	done
	@rm -f $(MBEDTLS_LIBS)
	$(WASI_SDK_DIR)/bin/llvm-ar rcs $(MBEDTLS_BUILD_DIR)/libmbedcrypto.a $(MBEDTLS_BUILD_DIR)/crypto/*.o
	$(WASI_SDK_DIR)/bin/llvm-ar rcs $(MBEDTLS_BUILD_DIR)/libmbedx509.a $(MBEDTLS_BUILD_DIR)/x509/*.o
	$(WASI_SDK_DIR)/bin/llvm-ar rcs $(MBEDTLS_BUILD_DIR)/libmbedtls.a $(MBEDTLS_BUILD_DIR)/tls/*.o
	@echo "=== mbedTLS libs built ==="
	@ls -l $(MBEDTLS_LIBS)

# --- brotli 1.1.0 (decoder) for wasm32-wasip1 ---
# curl's --compressed brotli decode. Only the dependency-free portable C
# `common` + `dec` sources are needed (no encoder). Mirrors the zlib rule:
# cross-compile each .c against the wasi-sdk sysroot and archive into the two
# canonical libs (libbrotlicommon.a, libbrotlidec.a; dec depends on common).
BROTLI_VERSION := 1.1.0
BROTLI_URL := https://github.com/google/brotli/archive/refs/tags/v$(BROTLI_VERSION).tar.gz
BROTLI_SRC_DIR = $(LIBS_CACHE)/brotli-$(BROTLI_VERSION)
BROTLI_BUILD_DIR := $(BUILD_DIR)/brotli
BROTLI_INCLUDE_DIR = $(BROTLI_SRC_DIR)/c/include
BROTLI_COMMON_SRCS := constants.c context.c dictionary.c platform.c shared_dictionary.c transform.c
BROTLI_DEC_SRCS := bit_reader.c decode.c huffman.c state.c
BROTLI_LIBS := $(BROTLI_BUILD_DIR)/libbrotlidec.a $(BROTLI_BUILD_DIR)/libbrotlicommon.a

$(BROTLI_SRC_DIR)/c/dec/decode.c:
	@echo "Fetching brotli $(BROTLI_VERSION)..."
	@mkdir -p $(LIBS_CACHE)
	@curl -fSL "$(BROTLI_URL)" -o "$(LIBS_CACHE)/brotli-$(BROTLI_VERSION).tar.gz"
	@rm -rf "$(BROTLI_SRC_DIR)"
	@tar -xzf "$(LIBS_CACHE)/brotli-$(BROTLI_VERSION).tar.gz" -C "$(LIBS_CACHE)"

.PHONY: brotli
brotli: $(BROTLI_LIBS)

$(BROTLI_LIBS) &: $(BROTLI_SRC_DIR)/c/dec/decode.c \
                  $(PATCHED_SYSROOT)/lib/wasm32-wasi/libc.a \
                  $(WASI_SDK_DIR)/bin/clang
	@echo "=== Building brotli $(BROTLI_VERSION) (dec) for wasm32-wasip1 ==="
	@mkdir -p $(BROTLI_BUILD_DIR)/common $(BROTLI_BUILD_DIR)/dec
	@for src in $(BROTLI_COMMON_SRCS); do \
		$(CC) --target=wasm32-wasip1 --sysroot=$(PATCHED_SYSROOT) -O2 \
			-I$(BROTLI_INCLUDE_DIR) \
			-c "$(BROTLI_SRC_DIR)/c/common/$$src" \
			-o "$(BROTLI_BUILD_DIR)/common/$${src%.c}.o" || exit 1; \
	done
	@for src in $(BROTLI_DEC_SRCS); do \
		$(CC) --target=wasm32-wasip1 --sysroot=$(PATCHED_SYSROOT) -O2 \
			-I$(BROTLI_INCLUDE_DIR) \
			-c "$(BROTLI_SRC_DIR)/c/dec/$$src" \
			-o "$(BROTLI_BUILD_DIR)/dec/$${src%.c}.o" || exit 1; \
	done
	@rm -f $(BROTLI_LIBS)
	$(WASI_SDK_DIR)/bin/llvm-ar rcs $(BROTLI_BUILD_DIR)/libbrotlicommon.a $(BROTLI_BUILD_DIR)/common/*.o
	$(WASI_SDK_DIR)/bin/llvm-ar rcs $(BROTLI_BUILD_DIR)/libbrotlidec.a $(BROTLI_BUILD_DIR)/dec/*.o
	@echo "=== brotli libs built ==="
	@ls -l $(BROTLI_LIBS)

# --- zstd 1.5.6 (decoder) for wasm32-wasip1 ---
# curl's --compressed zstd decode. Single-threaded (ZSTD_MULTITHREAD off),
# ZSTD_DISABLE_ASM=1 so the portable C huffman path is used instead of the
# amd64 .S. Only lib/common + lib/decompress are needed for ZSTD_*DStream.
ZSTD_VERSION := 1.5.6
ZSTD_URL := https://github.com/facebook/zstd/releases/download/v$(ZSTD_VERSION)/zstd-$(ZSTD_VERSION).tar.gz
ZSTD_SRC_DIR = $(LIBS_CACHE)/zstd-$(ZSTD_VERSION)
ZSTD_BUILD_DIR := $(BUILD_DIR)/zstd
ZSTD_INCLUDE_DIR = $(ZSTD_SRC_DIR)/lib
ZSTD_LIB := $(ZSTD_BUILD_DIR)/libzstd.a
# NB: leave ZSTD_MULTITHREAD *undefined* (not =0). zstd's threading.h keys the
# pthread path off `#if defined(ZSTD_MULTITHREAD)`, so even =0 pulls in
# pthread_create/join, which wasi-libc static-asserts away without threads.
ZSTD_CFLAGS := -O2 -DZSTD_DISABLE_ASM=1 \
               -I$(ZSTD_SRC_DIR)/lib -I$(ZSTD_SRC_DIR)/lib/common

$(ZSTD_SRC_DIR)/lib/zstd.h:
	@echo "Fetching zstd $(ZSTD_VERSION)..."
	@mkdir -p $(LIBS_CACHE)
	@curl -fSL "$(ZSTD_URL)" -o "$(LIBS_CACHE)/zstd-$(ZSTD_VERSION).tar.gz"
	@rm -rf "$(ZSTD_SRC_DIR)"
	@tar -xzf "$(LIBS_CACHE)/zstd-$(ZSTD_VERSION).tar.gz" -C "$(LIBS_CACHE)"

.PHONY: zstd
zstd: $(ZSTD_LIB)

$(ZSTD_LIB): $(ZSTD_SRC_DIR)/lib/zstd.h \
             $(PATCHED_SYSROOT)/lib/wasm32-wasi/libc.a \
             $(WASI_SDK_DIR)/bin/clang
	@echo "=== Building zstd $(ZSTD_VERSION) (decompress) for wasm32-wasip1 ==="
	@mkdir -p $(ZSTD_BUILD_DIR)/obj
	@for src in $(ZSTD_SRC_DIR)/lib/common/*.c $(ZSTD_SRC_DIR)/lib/decompress/*.c; do \
		name=$$(basename "$${src%.c}"); \
		$(CC) --target=wasm32-wasip1 --sysroot=$(PATCHED_SYSROOT) $(ZSTD_CFLAGS) \
			-c "$$src" -o "$(ZSTD_BUILD_DIR)/obj/$$name.o" || exit 1; \
	done
	@rm -f $(ZSTD_LIB)
	$(WASI_SDK_DIR)/bin/llvm-ar rcs $(ZSTD_LIB) $(ZSTD_BUILD_DIR)/obj/*.o
	@echo "=== zstd lib built ==="
	@ls -l $(ZSTD_LIB)

$(NATIVE_DIR)/sqlite3_cli: $(NATIVE_DIR)/sqlite3
	cp $< $@

# tree: upstream Steve Baker tree compiled from pinned C source.
$(BUILD_DIR)/tree: $(TREE_FILES) $(WASI_SDK_DIR)/bin/clang $(PATCHED_SYSROOT)/lib/wasm32-wasi/libc.a
	@mkdir -p $(BUILD_DIR)
	$(CC) --target=wasm32-wasip1 --sysroot=$(PATCHED_SYSROOT) -O2 -I include/ \
	    $(TREE_WASM_FLAGS) -I$(LIBS_DIR)/tree -o $@ \
	    $(addprefix $(LIBS_DIR)/tree/,$(TREE_SOURCES))

$(NATIVE_DIR)/tree: $(TREE_FILES)
	@mkdir -p $(NATIVE_DIR)
	$(NATIVE_CC) $(NATIVE_CFLAGS) $(TREE_WASM_FLAGS) -I$(LIBS_DIR)/tree -o $@ \
	    $(addprefix $(LIBS_DIR)/tree/,$(TREE_SOURCES))

# zip/unzip: upstream Info-ZIP releases built against the patched C sysroot.
$(BUILD_DIR)/zip: scripts/build-infozip-upstream.sh $(PATCHED_SYSROOT)/lib/wasm32-wasi/libc.a $(WASI_SDK_DIR)/bin/clang
	@mkdir -p $(BUILD_DIR)
	bash scripts/build-infozip-upstream.sh \
		--tool zip \
		--version "$(INFOZIP_ZIP_VERSION)" \
		--url "$(INFOZIP_ZIP_URL)" \
		--cache-dir "$(abspath $(LIBS_CACHE))" \
		--build-dir "$(abspath $(INFOZIP_ZIP_BUILD_DIR))" \
		--cc "$(abspath $(CC)) --target=wasm32-wasip1 --sysroot=$(abspath $(PATCHED_SYSROOT))" \
		--output "$(abspath $@)"

$(BUILD_DIR)/unzip: scripts/build-infozip-upstream.sh $(PATCHED_SYSROOT)/lib/wasm32-wasi/libc.a $(WASI_SDK_DIR)/bin/clang
	@mkdir -p $(BUILD_DIR)
	bash scripts/build-infozip-upstream.sh \
		--tool unzip \
		--version "$(INFOZIP_UNZIP_VERSION)" \
		--url "$(INFOZIP_UNZIP_URL)" \
		--cache-dir "$(abspath $(LIBS_CACHE))" \
		--build-dir "$(abspath $(INFOZIP_UNZIP_BUILD_DIR))" \
		--cc "$(abspath $(CC)) --target=wasm32-wasip1 --sysroot=$(abspath $(PATCHED_SYSROOT))" \
		--output "$(abspath $@)"

$(BUILD_DIR)/git: scripts/build-git-upstream.sh $(wildcard $(GIT_PATCH_DIR)/*.patch) $(CURL_LIBCURL_ARTIFACT) $(MBEDTLS_LIBS) $(BROTLI_LIBS) $(ZSTD_LIB) $(ZLIB_BUILD_DIR)/libz.a $(PATCHED_SYSROOT)/lib/wasm32-wasi/libc.a $(WASI_SDK_DIR)/bin/clang
	@mkdir -p $(BUILD_DIR)
	bash scripts/build-git-upstream.sh \
		--version "$(GIT_VERSION)" \
		--url "$(GIT_URL)" \
		--cache-dir "$(abspath $(LIBS_CACHE))" \
		--build-dir "$(abspath $(GIT_UPSTREAM_BUILD_DIR))" \
		--patch-dir "$(abspath $(GIT_PATCH_DIR))" \
		--zlib-dir "$(abspath $(LIBS_CACHE)/zlib-$(ZLIB_VERSION))" \
		--zlib-build-dir "$(abspath $(ZLIB_BUILD_DIR))" \
		--curl-prefix "$(abspath $(CURL_LIBCURL_PREFIX))" \
		--mbedtls-libdir "$(abspath $(MBEDTLS_BUILD_DIR))" \
		--brotli-libdir "$(abspath $(BROTLI_BUILD_DIR))" \
		--zstd-libdir "$(abspath $(ZSTD_BUILD_DIR))" \
		--cc "$(abspath $(CC))" \
		--ar "$(abspath $(WASI_SDK_DIR)/bin/llvm-ar)" \
		--ranlib "$(abspath $(WASI_SDK_DIR)/bin/llvm-ranlib)" \
		--sysroot "$(abspath $(PATCHED_SYSROOT))" \
		--remote-http-output "$(abspath $(BUILD_DIR)/git-remote-http)" \
		--output "$(abspath $@)"

# git-remote-http is produced by the same git build invocation.
$(BUILD_DIR)/git-remote-http: $(BUILD_DIR)/git
	@test -f $@ || { echo "Error: git-remote-http missing at $@ (git build did not produce it)"; exit 1; }

# OpenSSL is private to OpenSSH. curl, wget, and git continue to use mbedTLS.
$(OPENSSL_LIBCRYPTO) $(OPENSSL_HEADER) &: scripts/build-openssl-upstream.sh $(OVERLAY_HEADERS) $(PATCHED_SYSROOT)/lib/wasm32-wasi/libc.a $(WASI_SDK_DIR)/bin/clang
	@mkdir -p $(BUILD_DIR)
	bash scripts/build-openssl-upstream.sh \
		--version "$(OPENSSL_VERSION)" \
		--url "$(OPENSSL_URL)" \
		--sha256 "$(OPENSSL_SHA256)" \
		--cache-dir "$(abspath $(LIBS_CACHE))" \
		--build-dir "$(abspath $(OPENSSL_UPSTREAM_BUILD_DIR))" \
		--prefix "$(abspath $(OPENSSL_PREFIX))" \
		--overlay-include-dir "$(abspath include)" \
		--cc "$(abspath $(CC)) --target=wasm32-wasip1 --sysroot=$(abspath $(PATCHED_SYSROOT))" \
		--ar "$(abspath $(WASI_SDK_DIR)/bin/llvm-ar)" \
		--ranlib "$(abspath $(WASI_SDK_DIR)/bin/llvm-ranlib)"

# ssh and its client-side helpers: upstream OpenSSH linked with libcrypto against the patched
# sysroot (closefrom/socketpair implementations live in the owned libc/kernel
# layers). Also lights up git-over-ssh: git's connect.c execs `ssh` from PATH.
$(BUILD_DIR)/ssh $(BUILD_DIR)/ssh-keysign $(BUILD_DIR)/ssh-sk-helper &: scripts/build-ssh-upstream.sh $(wildcard $(OPENSSH_PATCH_DIR)/*.patch) $(OVERLAY_HEADERS) $(OPENSSL_LIBCRYPTO) $(OPENSSL_HEADER) $(ZLIB_BUILD_DIR)/libz.a $(PATCHED_SYSROOT)/lib/wasm32-wasi/libc.a $(WASI_SDK_DIR)/bin/clang
	@mkdir -p $(BUILD_DIR)
	bash scripts/build-ssh-upstream.sh \
		--version "$(OPENSSH_VERSION)" \
		--url "$(OPENSSH_URL)" \
		--sha256 "$(OPENSSH_SHA256)" \
		--cache-dir "$(abspath $(LIBS_CACHE))" \
		--build-dir "$(abspath $(OPENSSH_UPSTREAM_BUILD_DIR))" \
		--patch-dir "$(abspath $(OPENSSH_PATCH_DIR))" \
		--zlib-include "$(abspath $(LIBS_CACHE)/zlib-$(ZLIB_VERSION))" \
		--zlib-libdir "$(abspath $(ZLIB_BUILD_DIR))" \
		--openssl-prefix "$(abspath $(OPENSSL_PREFIX))" \
		--overlay-include-dir "$(abspath include)" \
		--cc "$(abspath $(CC)) --target=wasm32-wasip1 --sysroot=$(abspath $(PATCHED_SYSROOT))" \
		--ar "$(abspath $(WASI_SDK_DIR)/bin/llvm-ar)" \
		--ranlib "$(abspath $(WASI_SDK_DIR)/bin/llvm-ranlib)" \
		--output "$(abspath $(BUILD_DIR)/ssh)" \
		--keysign-output "$(abspath $(BUILD_DIR)/ssh-keysign)" \
		--sk-helper-output "$(abspath $(BUILD_DIR)/ssh-sk-helper)"

# curl_test: links libcurl (HTTP/HTTPS build for WASM via host_net + host_tls)
CURL_SRCS := $(wildcard libs/curl/lib/*.c) $(wildcard libs/curl/lib/vauth/*.c) \
             $(wildcard libs/curl/lib/vtls/*.c) $(wildcard libs/curl/lib/vquic/*.c) \
             $(wildcard libs/curl/lib/vssh/*.c)
CURL_INCLUDES := -Ilibs/curl/include -Ilibs/curl/lib -include libs/curl/lib/curl_setup.h -include libs/curl/lib/curl_printf.h
CURL_LIB_DEFS := -DHAVE_CONFIG_H -DBUILDING_LIBCURL -DHAVE_BASENAME -DHAVE_LIBGEN_H

$(BUILD_DIR)/curl $(CURL_LIBCURL_ARTIFACT) &: scripts/build-curl-upstream.sh $(CURL_UPSTREAM_OVERLAY_FILES) $(MBEDTLS_LIBS) $(ZLIB_BUILD_DIR)/libz.a $(BROTLI_LIBS) $(ZSTD_LIB) $(PATCHED_SYSROOT)/lib/wasm32-wasi/libc.a $(WASI_SDK_DIR)/bin/clang
	@mkdir -p $(BUILD_DIR)
	bash scripts/build-curl-upstream.sh \
		--version "$(CURL_RELEASE_VERSION)" \
		--tag "$(CURL_RELEASE_TAG)" \
		--url "$(CURL_RELEASE_URL)" \
		--cache-dir "$(abspath $(LIBS_CACHE))" \
		--build-dir "$(abspath $(CURL_UPSTREAM_BUILD_DIR))" \
		--overlay-dir "$(abspath $(CURL_UPSTREAM_OVERLAY_DIR))" \
		--cc "$(abspath $(CC)) --target=wasm32-wasip1 --sysroot=$(abspath $(PATCHED_SYSROOT))" \
		--ar "$(abspath $(WASI_SDK_DIR)/bin/llvm-ar)" \
		--ranlib "$(abspath $(WASI_SDK_DIR)/bin/llvm-ranlib)" \
		--mbedtls-include "$(abspath $(MBEDTLS_SRC_DIR)/include)" \
		--mbedtls-libdir "$(abspath $(MBEDTLS_BUILD_DIR))" \
		--zlib-include "$(abspath $(LIBS_CACHE)/zlib-$(ZLIB_VERSION))" \
		--zlib-libdir "$(abspath $(ZLIB_BUILD_DIR))" \
		--brotli-include "$(abspath $(BROTLI_INCLUDE_DIR))" \
		--brotli-libdir "$(abspath $(BROTLI_BUILD_DIR))" \
		--zstd-include "$(abspath $(ZSTD_INCLUDE_DIR))" \
		--zstd-libdir "$(abspath $(ZSTD_BUILD_DIR))" \
		--ca-bundle "/etc/ssl/certs/ca-certificates.crt" \
		--install-prefix "$(abspath $(CURL_LIBCURL_PREFIX))" \
		--output "$(abspath $(BUILD_DIR)/curl)"

$(BUILD_DIR)/wget: scripts/build-wget-upstream.sh $(WGET_UPSTREAM_OVERLAY_FILES) $(MBEDTLS_LIBS) $(ZLIB_BUILD_DIR)/libz.a $(PATCHED_SYSROOT)/lib/wasm32-wasi/libc.a $(WASI_SDK_DIR)/bin/clang
	@mkdir -p $(BUILD_DIR)
	bash scripts/build-wget-upstream.sh \
		--version "$(WGET_VERSION)" \
		--url "$(WGET_URL)" \
		--cache-dir "$(abspath $(LIBS_CACHE))" \
		--build-dir "$(abspath $(WGET_UPSTREAM_BUILD_DIR))" \
		--overlay-include-dir "$(abspath $(WGET_UPSTREAM_OVERLAY_INCLUDE_DIR))" \
		--overlay-dir "$(abspath $(WGET_UPSTREAM_OVERLAY_DIR))" \
		--mbedtls-include "$(abspath $(MBEDTLS_SRC_DIR)/include)" \
		--mbedtls-libdir "$(abspath $(MBEDTLS_BUILD_DIR))" \
		--zlib-include "$(abspath $(LIBS_CACHE)/zlib-$(ZLIB_VERSION))" \
		--zlib-libdir "$(abspath $(ZLIB_BUILD_DIR))" \
		--cc "$(abspath $(CC)) --target=wasm32-wasip1 --sysroot=$(abspath $(PATCHED_SYSROOT))" \
		--ar "$(abspath $(WASI_SDK_DIR)/bin/llvm-ar)" \
		--ranlib "$(abspath $(WASI_SDK_DIR)/bin/llvm-ranlib)" \
		--output "$(abspath $@)"

$(BUILD_DIR)/grep: scripts/build-grep-upstream.sh $(PATCHED_SYSROOT)/lib/wasm32-wasi/libc.a $(WASI_SDK_DIR)/bin/clang
	@mkdir -p $(BUILD_DIR)
	bash scripts/build-grep-upstream.sh \
		--version "$(GREP_VERSION)" \
		--url "$(GREP_URL)" \
		--cache-dir "$(abspath $(LIBS_CACHE))" \
		--build-dir "$(abspath $(GREP_UPSTREAM_BUILD_DIR))" \
		--cc "$(abspath $(CC)) --target=wasm32-wasip1 --sysroot=$(abspath $(PATCHED_SYSROOT))" \
		--ar "$(abspath $(WASI_SDK_DIR)/bin/llvm-ar)" \
		--ranlib "$(abspath $(WASI_SDK_DIR)/bin/llvm-ranlib)" \
		--output "$(abspath $@)"

# duckdb: upstream DuckDB CLI built from source with our patched WASI/POSIX sysroot
$(BUILD_DIR)/duckdb: libs/duckdb/CMakeLists.txt $(PATCHED_SYSROOT)/lib/wasm32-wasi/libc.a $(WASI_SDK_DIR)/bin/clang $(WASI_SDK_DIR)/bin/clang++ cmake/FindThreads.cmake scripts/build-duckdb.sh include/fcntl.h include/ifaddrs.h include/net/if.h include/sys/ioctl.h wasm-opt-check
	@mkdir -p $(BUILD_DIR)
	DUCKDB_SRC_DIR="$(abspath $(LIBS_DIR)/duckdb)" \
	DUCKDB_BUILD_DIR="$(abspath $(BUILD_DIR)/duckdb-cmake)" \
	DUCKDB_OUTPUT="$(abspath $(BUILD_DIR)/duckdb)" \
	WASI_SDK_DIR="$(abspath $(WASI_SDK_DIR))" \
	SYSROOT_DIR="$(abspath $(PATCHED_SYSROOT))" \
	MODULE_PATH="$(abspath cmake)" \
	OVERLAY_INCLUDE_DIR="$(abspath include)" \
	DUCKDB_GIT_DESCRIBE="$(DUCKDB_GIT_DESCRIBE)" \
	PATCH_DIR="$(abspath ../../software/duckdb/native/c/patches)" \
	bash scripts/build-duckdb.sh

# --- Compile all C programs to native binaries (for parity testing) ---

native: $(NATIVE_OUTPUTS)
	@echo ""
	@echo "=== Native Build Report ==="
	@echo "Programs: $(words $(NATIVE_REPORT_NAMES)) compiled"
	@echo "Output:   $(NATIVE_DIR)/"
	@echo "=== Build complete ==="

# Stage only the runnable C parity corpus. The build directory also contains
# downloaded libraries and intermediate objects that must not enter CI
# artifacts or shipped command packages.
CONFORMANCE_STAGE := $(BUILD_DIR)/conformance
conformance-artifacts: programs native
	rm -rf "$(CONFORMANCE_STAGE)"
	mkdir -p "$(CONFORMANCE_STAGE)/native"
	@set -e; \
	for name in $(PROGRAM_REPORT_NAMES); do \
		test -f "$(BUILD_DIR)/$$name" || { echo "missing WASM conformance fixture: $$name" >&2; exit 1; }; \
		cp -p "$(BUILD_DIR)/$$name" "$(CONFORMANCE_STAGE)/$$name"; \
	done; \
	for name in $(NATIVE_REPORT_NAMES); do \
		test -f "$(NATIVE_DIR)/$$name" || { echo "missing native conformance fixture: $$name" >&2; exit 1; }; \
		cp -p "$(NATIVE_DIR)/$$name" "$(CONFORMANCE_STAGE)/native/$$name"; \
	done

$(NATIVE_DIR)/%: $$(call c_source,$$*)
	@mkdir -p $(NATIVE_DIR)
	$(NATIVE_CC) $(NATIVE_CFLAGS) -o $@ $<

$(NATIVE_DIR)/fd_mapping_contract:
	$(MAKE) -C .. fd-mapping-contract-native

# --- Install opted-in commands to COMMANDS_DIR ---

install:
	@if [ -z "$(COMMANDS)" ]; then \
		echo "No C commands opted in for install (COMMANDS is empty)"; \
		exit 0; \
	fi
	@if [ ! -d "$(BUILD_DIR)" ]; then \
		echo "Error: $(BUILD_DIR) not found. Run 'make programs' first."; \
		exit 1; \
	fi
	@mkdir -p $(COMMANDS_DIR)
	@INSTALLED=0; \
	for cmd in $(COMMANDS); do \
		src="$$cmd"; \
		if [ -f "$(BUILD_DIR)/$$src" ]; then \
			cp "$(BUILD_DIR)/$$src" "$(COMMANDS_DIR)/$$cmd"; \
			INSTALLED=$$((INSTALLED + 1)); \
		else \
			echo "Warning: $(BUILD_DIR)/$$src not found — skipping"; \
		fi; \
	done; \
	echo "Installed $$INSTALLED C command(s) to $(COMMANDS_DIR)"
	@cd $(COMMANDS_DIR) && \
	for pair in $(COMMAND_ALIASES); do \
		link=$${pair%%:*}; target=$${pair##*:}; \
		if [ -f "$$target" ]; then \
			ln -sf "$$target" "$$link" 2>/dev/null || cp "$$target" "$$link"; \
		fi; \
	done
	@# Git helper entry points. upload-pack/receive-pack/upload-archive ARE git
	@# builtins, so they alias the git binary. The HTTPS transport lives in the
	@# real git-remote-http binary (libcurl in-process); git-remote-https is an
	@# alias to it (same binary, ftp/ftps schemes unsupported and unused).
	@if [ -f "$(COMMANDS_DIR)/git" ]; then \
		for alias in git-upload-pack git-receive-pack git-upload-archive; do \
			ln -sf git "$(COMMANDS_DIR)/$$alias"; \
		done; \
	fi
	@if [ -f "$(COMMANDS_DIR)/git-remote-http" ]; then \
		ln -sf git-remote-http "$(COMMANDS_DIR)/git-remote-https"; \
	fi
	@if [ -f "$(COMMANDS_DIR)/mknod" ]; then \
		ln -sf mknod "$(COMMANDS_DIR)/mkfifo"; \
	fi
	@if [ -f "$(COMMANDS_DIR)/acl_tools" ]; then \
		for alias in chacl getfacl setfacl; do \
			ln -sf acl_tools "$(COMMANDS_DIR)/$$alias"; \
		done; \
	fi
	@if [ -f "$(COMMANDS_DIR)/xattr_tools" ]; then \
		for alias in attr getfattr setfattr; do \
			ln -sf xattr_tools "$(COMMANDS_DIR)/$$alias"; \
		done; \
	fi

# --- Clean ---

clean:
	rm -rf $(BUILD_DIR)

# --- vim: real vim 9.2 -> wasm32-wasip1 against the full-OS libc ---
#
# vim compiles as a normal terminal program against the PATCHED sysroot plus a
# small bridge library (vim/): a termcap stub (builtin termcaps), and POSIX
# stubs the full-OS libc still lacks. Termios/process spawning live in sysroot.
# Recipe recovered from the original hand build (see vim/ headers). The result
# lands in $(BUILD_DIR)/vim; `make -C .. cmd/vim` installs it into the shared
# commands dir. NOTE: run as `vim -n <file>` docs-wise is no longer needed —
# swap files work; kept here for archaeology.
VIM_VERSION ?= v9.2.0780
VIM_SRC := libs/vim
VIM_BRIDGE_SRC := ../../software/vim/native/c/vim-bridge
VIM_BRIDGE_BUILD := $(BUILD_DIR)/vim-bridge
VIM_BRIDGE_OBJS := $(VIM_BRIDGE_BUILD)/termcap_stub.o $(VIM_BRIDGE_BUILD)/posix_stubs.o
VIM_WCC := $(VIM_BRIDGE_BUILD)/wcc

.PHONY: fetch-vim
fetch-vim: $(VIM_SRC)/src/vim.h
$(VIM_SRC)/src/vim.h:
	git clone --depth 1 --branch $(VIM_VERSION) https://github.com/vim/vim $(VIM_SRC)

$(VIM_BRIDGE_BUILD)/%.o: $(VIM_BRIDGE_SRC)/%.c $(WASI_SDK_DIR)/bin/clang sysroot
	@mkdir -p $(VIM_BRIDGE_BUILD)
	$(CC) --target=wasm32-wasip1 --sysroot=$(PATCHED_SYSROOT) -O2 -I $(VIM_BRIDGE_SRC) -c -o $@ $<

$(VIM_BRIDGE_BUILD)/libtermcap.a: $(VIM_BRIDGE_OBJS)
	rm -f $@
	$(WASI_SDK_DIR)/bin/llvm-ar rcs $@ $(VIM_BRIDGE_OBJS)

# Wrapper compiler: every vim TU gets the sysroot, the bridge headers, and the
# forced compat include, so vim's own Makefile needs no flag surgery.
$(VIM_WCC): $(WASI_SDK_DIR)/bin/clang
	@mkdir -p $(VIM_BRIDGE_BUILD)
	@printf '#!/bin/sh\nexec %s --target=wasm32-wasip1 --sysroot=%s -O2 -I %s -include %s "$$@"\n' \
		"$(abspath $(WASI_SDK_DIR)/bin/clang)" \
		"$(abspath $(PATCHED_SYSROOT))" \
		"$(abspath $(VIM_BRIDGE_SRC))" \
		"$(abspath $(VIM_BRIDGE_SRC)/compat.h)" > $@
	@chmod +x $@

$(BUILD_DIR)/vim: fetch-vim $(VIM_BRIDGE_BUILD)/libtermcap.a $(VIM_WCC) wasm-opt-check
	rm -f $(VIM_SRC)/src/auto/config.cache
	cd $(VIM_SRC)/src && \
CC="$(abspath $(VIM_WCC))" \
LDFLAGS="-L$(abspath $(VIM_BRIDGE_BUILD))" \
	vim_cv_toupper_broken=no \
	vim_cv_terminfo=no \
	vim_cv_tgetent=zero \
	vim_cv_getcwd_broken=no \
	vim_cv_stat_ignores_slash=no \
	vim_cv_memmove_handles_overlap=yes \
vim_cv_bcopy_handles_overlap=yes \
vim_cv_memcpy_handles_overlap=yes \
vim_cv_tty_group=world \
vim_cv_tty_mode=0620 \
ac_cv_header_dlfcn_h=no \
./configure --host=wasm32-wasip1 --build=$$(cc -dumpmachine 2>/dev/null || echo x86_64-pc-linux-gnu) \
		--with-features=normal --with-tlib=termcap \
		--disable-gui --without-x --disable-nls --disable-channel \
		--without-wayland \
		--disable-netbeans --disable-selinux --disable-gpm --disable-sysmouse \
		--disable-icon-cache-update --disable-desktop-database-update
	$(MAKE) -C $(VIM_SRC)/src vim
	@mkdir -p $(BUILD_DIR)
	@if [ "$(HAS_WASM_OPT)" = "1" ]; then \
		wasm-opt -O3 --strip-debug --all-features $(VIM_SRC)/src/vim -o $(BUILD_DIR)/vim; \
	else \
		cp $(VIM_SRC)/src/vim $(BUILD_DIR)/vim; \
	fi
	@echo "Built $(BUILD_DIR)/vim"
