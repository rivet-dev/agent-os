# wasmvm/c/Makefile — Build C programs to standalone WASM binaries
#
# Targets:
#   wasi-sdk    Download and cache wasi-sdk toolchain
#   programs    Compile all .c files in programs/ to WASM binaries
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

# Toolchain
CC           := $(WASI_SDK_DIR)/bin/clang
NATIVE_CC    := $(shell command -v cc 2>/dev/null || command -v gcc 2>/dev/null || command -v clang 2>/dev/null)

# Sysroot: use patched sysroot if built, otherwise use wasi-sdk's vanilla sysroot
PATCHED_SYSROOT := sysroot
ifeq ($(wildcard $(PATCHED_SYSROOT)/lib/wasm32-wasi/libc.a),)
  SYSROOT := $(WASI_SDK_DIR)/share/wasi-sysroot
else
  SYSROOT := $(PATCHED_SYSROOT)
endif

# Compile flags
WASM_CFLAGS  := --target=wasm32-wasip1 --sysroot=$(SYSROOT) -O2 -flto -I include/
NATIVE_CFLAGS := -O0 -g -I include/

# COMMANDS_DIR for install target (configurable, matches Rust binary output)
COMMANDS_DIR ?= ../target/wasm32-wasip1/release/commands

# Real commands to install (add command names as they are opted in)
COMMANDS := zip unzip envsubst sqlite3 curl wget duckdb

# Programs requiring patched sysroot (Tier 2+ custom host imports)
PATCHED_PROGRAMS := isatty_test getpid_test getppid_test getppid_verify userinfo pipe_test dup_test spawn_child spawn_exit_code pipeline kill_child waitpid_return waitpid_edge syscall_coverage getpwuid_test signal_tests sigaction_behavior delayed_tcp_echo delayed_kill pipe_edge tcp_echo tcp_server udp_echo unix_socket signal_handler http_get dns_lookup sqlite3_cli curl wget

# Discover all .c source files in programs/
ALL_SOURCES := $(wildcard programs/*.c)

# Exclude patched-sysroot programs when only vanilla sysroot is available
ifeq ($(wildcard $(PATCHED_SYSROOT)/lib/wasm32-wasi/libc.a),)
  EXCLUDED := $(addsuffix .c,$(addprefix programs/,$(PATCHED_PROGRAMS)))
  SOURCES := $(filter-out $(EXCLUDED),$(ALL_SOURCES))
else
  SOURCES := $(ALL_SOURCES)
endif

# Program names (without .c extension)
ifeq ($(wildcard $(PATCHED_SYSROOT)/lib/wasm32-wasi/libc.a),)
  CUSTOM_WASM_PROG_NAMES :=
else
  CUSTOM_WASM_PROG_NAMES := curl
endif

WASM_PROG_NAMES := $(sort $(basename $(notdir $(SOURCES))) $(CUSTOM_WASM_PROG_NAMES))
NATIVE_PROG_NAMES := $(basename $(notdir $(SOURCES)))
PROGRAM_REPORT_NAMES := $(WASM_PROG_NAMES) duckdb

# Default WASM output targets
EXTRA_WASM_OUTPUTS := $(BUILD_DIR)/duckdb
WASM_OUTPUTS := $(addprefix $(BUILD_DIR)/,$(WASM_PROG_NAMES)) $(EXTRA_WASM_OUTPUTS)
# Native output targets
NATIVE_OUTPUTS := $(addprefix $(NATIVE_DIR)/,$(NATIVE_PROG_NAMES))

.PHONY: all wasi-sdk programs sysroot install native clean wasm-opt-check os-test os-test-native

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
ZLIB_URL    := https://github.com/madler/zlib/archive/refs/tags/v1.3.1.zip
CJSON_URL   := https://github.com/DaveGamble/cJSON/archive/refs/tags/v1.7.18.zip
CURL_COMMIT := main
CURL_URL    := https://github.com/rivet-dev/secure-exec-curl/archive/refs/heads/$(CURL_COMMIT).zip
CURL_TOOL_VERSION := 8_11_1
CURL_TOOL_URL := https://github.com/curl/curl/archive/refs/tags/curl-$(CURL_TOOL_VERSION).tar.gz
CURL_RELEASE_VERSION := 8.11.1
CURL_RELEASE_TAG := 8_11_1
CURL_RELEASE_URL := https://github.com/curl/curl/releases/download/curl-$(CURL_RELEASE_TAG)/curl-$(CURL_RELEASE_VERSION).tar.xz
CURL_UPSTREAM_BUILD_DIR := $(BUILD_DIR)/curl-upstream
CURL_UPSTREAM_OVERLAY_DIR := curl-upstream-overlay
CURL_UPSTREAM_OVERLAY_FILES := $(wildcard $(CURL_UPSTREAM_OVERLAY_DIR)/lib/*.c $(CURL_UPSTREAM_OVERLAY_DIR)/lib/*.h $(CURL_UPSTREAM_OVERLAY_DIR)/lib/vtls/*.c $(CURL_UPSTREAM_OVERLAY_DIR)/lib/vtls/*.h)
MINIZIP_URL := https://github.com/madler/zlib/archive/refs/tags/v1.3.1.zip
# duckdb-wasm currently documents DuckDB v1.5.0 in its README. We build the
# matching upstream DuckDB tag ourselves with our patched WASI/POSIX sysroot.
DUCKDB_VERSION := v1.5.0
DUCKDB_GIT_DESCRIBE := $(DUCKDB_VERSION)-0-g0123456789
DUCKDB_URL := https://github.com/duckdb/duckdb/archive/refs/tags/$(DUCKDB_VERSION).tar.gz

LIBS_DIR    := libs
LIBS_CACHE  := .cache/libs

libs/sqlite3/sqlite3.c:
	@echo "Fetching sqlite3..."
	@mkdir -p $(LIBS_CACHE) $(LIBS_DIR)/sqlite3
	@curl -fSL "$(SQLITE3_URL)" -o "$(LIBS_CACHE)/sqlite3.zip"
	@cd $(LIBS_CACHE) && unzip -qo sqlite3.zip
	@cp $(LIBS_CACHE)/sqlite-amalgamation-*/sqlite3.c $(LIBS_CACHE)/sqlite-amalgamation-*/sqlite3.h $(LIBS_DIR)/sqlite3/

libs/zlib/zutil.c:
	@echo "Fetching zlib..."
	@mkdir -p $(LIBS_CACHE) $(LIBS_DIR)/zlib
	@curl -fSL "$(ZLIB_URL)" -o "$(LIBS_CACHE)/zlib.zip"
	@cd $(LIBS_CACHE) && unzip -qo zlib.zip
	@cp $(LIBS_CACHE)/zlib-*/*.c $(LIBS_CACHE)/zlib-*/*.h $(LIBS_DIR)/zlib/

libs/minizip/ioapi.c:
	@echo "Fetching minizip (from zlib contrib)..."
	@mkdir -p $(LIBS_CACHE) $(LIBS_DIR)/minizip
	@if [ ! -f "$(LIBS_CACHE)/zlib.zip" ]; then \
		curl -fSL "$(MINIZIP_URL)" -o "$(LIBS_CACHE)/zlib.zip"; \
		cd $(LIBS_CACHE) && unzip -qo zlib.zip; \
	fi
	@cp $(LIBS_CACHE)/zlib-*/contrib/minizip/ioapi.c $(LIBS_CACHE)/zlib-*/contrib/minizip/ioapi.h \
	    $(LIBS_CACHE)/zlib-*/contrib/minizip/zip.c $(LIBS_CACHE)/zlib-*/contrib/minizip/zip.h \
	    $(LIBS_CACHE)/zlib-*/contrib/minizip/unzip.c $(LIBS_CACHE)/zlib-*/contrib/minizip/unzip.h \
	    $(LIBS_CACHE)/zlib-*/contrib/minizip/crypt.h \
	    $(LIBS_DIR)/minizip/

libs/cjson/cJSON.c:
	@echo "Fetching cJSON..."
	@mkdir -p $(LIBS_CACHE) $(LIBS_DIR)/cjson
	@curl -fSL "$(CJSON_URL)" -o "$(LIBS_CACHE)/cjson.zip"
	@cd $(LIBS_CACHE) && unzip -qo cjson.zip
	@cp $(LIBS_CACHE)/cJSON-*/cJSON.c $(LIBS_CACHE)/cJSON-*/cJSON.h $(LIBS_DIR)/cjson/

libs/curl/lib/easy.c:
	@echo "Fetching curl (rivet-dev/secure-exec-curl)..."
	@mkdir -p $(LIBS_CACHE)
	@curl -fSL "$(CURL_URL)" -o "$(LIBS_CACHE)/curl.zip"
	@cd $(LIBS_CACHE) && unzip -qo curl.zip
	@rm -rf $(LIBS_DIR)/curl
	@mv $(LIBS_CACHE)/secure-exec-curl-* $(LIBS_DIR)/curl

libs/duckdb/CMakeLists.txt:
	@echo "Fetching DuckDB ($(DUCKDB_VERSION))..."
	@mkdir -p $(LIBS_CACHE)
	@curl -fSL "$(DUCKDB_URL)" -o "$(LIBS_CACHE)/duckdb.tar.gz"
	@rm -rf $(LIBS_DIR)/duckdb
	@mkdir -p $(LIBS_DIR)/duckdb
	@tar -xzf "$(LIBS_CACHE)/duckdb.tar.gz" --strip-components=1 -C $(LIBS_DIR)/duckdb

.PHONY: fetch-libs clean-libs
fetch-libs: libs/sqlite3/sqlite3.c libs/zlib/zutil.c libs/minizip/ioapi.c libs/cjson/cJSON.c libs/curl/lib/easy.c libs/duckdb/CMakeLists.txt

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

WASI_LIBC_PATCHES := $(wildcard ../patches/wasi-libc/*.patch)
WASI_LIBC_OVERRIDES := $(wildcard ../patches/wasi-libc-overrides/*.c)
LLVM_RUNTIME_PATCHES := $(wildcard patches/llvm-project/*.patch)

$(PATCHED_SYSROOT)/lib/wasm32-wasi/libc.a: wasi-sdk ../scripts/patch-wasi-libc.sh scripts/build-llvm-runtimes.sh $(WASI_LIBC_PATCHES) $(WASI_LIBC_OVERRIDES) $(LLVM_RUNTIME_PATCHES)
	../scripts/patch-wasi-libc.sh

sysroot: $(PATCHED_SYSROOT)/lib/wasm32-wasi/libc.a

# --- wasm-opt check ---

wasm-opt-check:
	@if ! command -v wasm-opt >/dev/null 2>&1; then \
		echo "Error: wasm-opt (binaryen) is not installed."; \
		echo "Install it via your package manager:"; \
		echo "  apt: sudo apt install binaryen"; \
		echo "  brew: brew install binaryen"; \
		echo "  cargo: cargo install wasm-opt"; \
		exit 1; \
	fi

# --- Compile all C programs to WASM ---

programs: wasi-sdk fetch-libs wasm-opt-check $(WASM_OUTPUTS)
	@echo ""
	@echo "=== C WASM Build Report ==="
	@echo "Programs: $(words $(PROGRAM_REPORT_NAMES)) compiled"
	@echo "Sysroot:  $(SYSROOT)"
	@echo "Output:   $(BUILD_DIR)/"
	@echo "=== Build complete ==="

# Default rule: single-file programs
$(BUILD_DIR)/%: programs/%.c $(WASI_SDK_DIR)/bin/clang
	@mkdir -p $(BUILD_DIR)
	$(CC) $(WASM_CFLAGS) -o $@.wasm $<
	wasm-opt -O3 --strip-debug $@.wasm -o $@
	@rm -f $@.wasm

# --- Per-program override rules for multi-file compilation ---
# Add overrides here for programs that link vendored libraries.

# json_parse: links cJSON library
$(BUILD_DIR)/json_parse: programs/json_parse.c libs/cjson/cJSON.c $(WASI_SDK_DIR)/bin/clang
	@mkdir -p $(BUILD_DIR)
	$(CC) $(WASM_CFLAGS) -Ilibs/cjson -o $@.wasm programs/json_parse.c libs/cjson/cJSON.c
	wasm-opt -O3 --strip-debug $@.wasm -o $@
	@rm -f $@.wasm

$(NATIVE_DIR)/json_parse: programs/json_parse.c libs/cjson/cJSON.c
	@mkdir -p $(NATIVE_DIR)
	$(NATIVE_CC) $(NATIVE_CFLAGS) -Ilibs/cjson -o $@ programs/json_parse.c libs/cjson/cJSON.c

# sqlite3_mem: links SQLite amalgamation
SQLITE_COMMON  := -DSQLITE_OMIT_WAL -DSQLITE_OMIT_LOAD_EXTENSION -DSQLITE_THREADSAFE=0 \
                  -DSQLITE_OMIT_LOCALTIME
SQLITE_WASM    := -DSQLITE_OS_OTHER $(SQLITE_COMMON) -lwasi-emulated-signal -lwasi-emulated-mman
SQLITE_NATIVE  := $(SQLITE_COMMON)

$(BUILD_DIR)/sqlite3_mem: programs/sqlite3_mem.c libs/sqlite3/sqlite3.c $(WASI_SDK_DIR)/bin/clang
	@mkdir -p $(BUILD_DIR)
	$(CC) $(WASM_CFLAGS) $(SQLITE_WASM) -Ilibs/sqlite3 -o $@.wasm \
	    programs/sqlite3_mem.c libs/sqlite3/sqlite3.c
	wasm-opt -O3 --strip-debug $@.wasm -o $@
	@rm -f $@.wasm

$(NATIVE_DIR)/sqlite3_mem: programs/sqlite3_mem.c libs/sqlite3/sqlite3.c
	@mkdir -p $(NATIVE_DIR)
	$(NATIVE_CC) $(NATIVE_CFLAGS) $(SQLITE_NATIVE) -Ilibs/sqlite3 -o $@ \
	    programs/sqlite3_mem.c libs/sqlite3/sqlite3.c -lm -lpthread

# sqlite3_cli: full CLI, links SQLite amalgamation (installed as sqlite3)
# Uses -Os (not -O2) + no wasm-opt — higher optimization breaks SQLite's
# internal function pointer tables in WASM indirect call table.
# Uses --initial-memory=16777216 (16MB) for SQLite's malloc requirements.
$(BUILD_DIR)/sqlite3_cli: programs/sqlite3_cli.c libs/sqlite3/sqlite3.c $(WASI_SDK_DIR)/bin/clang
	@mkdir -p $(BUILD_DIR)
	$(CC) --target=wasm32-wasip1 --sysroot=$(SYSROOT) -Os -I include/ \
	    $(SQLITE_WASM) -Ilibs/sqlite3 -Wl,--initial-memory=16777216 \
	    -o $@ programs/sqlite3_cli.c libs/sqlite3/sqlite3.c

$(NATIVE_DIR)/sqlite3_cli: programs/sqlite3_cli.c libs/sqlite3/sqlite3.c
	@mkdir -p $(NATIVE_DIR)
	$(NATIVE_CC) $(NATIVE_CFLAGS) $(SQLITE_NATIVE) -Ilibs/sqlite3 -o $@ \
	    programs/sqlite3_cli.c libs/sqlite3/sqlite3.c -lm -lpthread

# zip: links zlib + minizip
ZLIB_SRCS   := libs/zlib/adler32.c libs/zlib/compress.c libs/zlib/crc32.c libs/zlib/deflate.c \
               libs/zlib/infback.c libs/zlib/inffast.c libs/zlib/inflate.c libs/zlib/inftrees.c \
               libs/zlib/trees.c libs/zlib/uncompr.c libs/zlib/zutil.c
MINIZIP_SRCS := libs/minizip/ioapi.c libs/minizip/zip.c
ZIP_INCLUDES := -Ilibs/zlib -Ilibs/minizip

$(BUILD_DIR)/zip: programs/zip.c $(ZLIB_SRCS) $(MINIZIP_SRCS) $(WASI_SDK_DIR)/bin/clang
	@mkdir -p $(BUILD_DIR)
	$(CC) $(WASM_CFLAGS) $(ZIP_INCLUDES) -o $@.wasm programs/zip.c $(ZLIB_SRCS) $(MINIZIP_SRCS)
	wasm-opt -O3 --strip-debug $@.wasm -o $@
	@rm -f $@.wasm

$(NATIVE_DIR)/zip: programs/zip.c $(ZLIB_SRCS) $(MINIZIP_SRCS)
	@mkdir -p $(NATIVE_DIR)
	$(NATIVE_CC) $(NATIVE_CFLAGS) $(ZIP_INCLUDES) -o $@ programs/zip.c $(ZLIB_SRCS) $(MINIZIP_SRCS) -lz

# unzip: links zlib + minizip (unzip side)
MINIZIP_UNZIP_SRCS := libs/minizip/ioapi.c libs/minizip/unzip.c

$(BUILD_DIR)/unzip: programs/unzip.c $(ZLIB_SRCS) $(MINIZIP_UNZIP_SRCS) $(WASI_SDK_DIR)/bin/clang
	@mkdir -p $(BUILD_DIR)
	$(CC) $(WASM_CFLAGS) $(ZIP_INCLUDES) -o $@.wasm programs/unzip.c $(ZLIB_SRCS) $(MINIZIP_UNZIP_SRCS)
	wasm-opt -O3 --strip-debug $@.wasm -o $@
	@rm -f $@.wasm

$(NATIVE_DIR)/unzip: programs/unzip.c $(ZLIB_SRCS) $(MINIZIP_UNZIP_SRCS)
	@mkdir -p $(NATIVE_DIR)
	$(NATIVE_CC) $(NATIVE_CFLAGS) $(ZIP_INCLUDES) -o $@ programs/unzip.c $(ZLIB_SRCS) $(MINIZIP_UNZIP_SRCS) -lz

# curl_test: links libcurl (HTTP/HTTPS build for WASM via host_net + host_tls)
CURL_SRCS := $(wildcard libs/curl/lib/*.c) $(wildcard libs/curl/lib/vauth/*.c) \
             $(wildcard libs/curl/lib/vtls/*.c) $(wildcard libs/curl/lib/vquic/*.c) \
             $(wildcard libs/curl/lib/vssh/*.c)
CURL_INCLUDES := -Ilibs/curl/include -Ilibs/curl/lib -include libs/curl/lib/curl_setup.h -include libs/curl/lib/curl_printf.h
CURL_LIB_DEFS := -DHAVE_CONFIG_H -DBUILDING_LIBCURL -D_WASI_EMULATED_SIGNAL -DHAVE_BASENAME -DHAVE_LIBGEN_H

$(BUILD_DIR)/curl: scripts/build-curl-upstream.sh $(CURL_UPSTREAM_OVERLAY_FILES) $(WASI_SDK_DIR)/bin/clang
	@mkdir -p $(BUILD_DIR)
	bash scripts/build-curl-upstream.sh \
		--version "$(CURL_RELEASE_VERSION)" \
		--tag "$(CURL_RELEASE_TAG)" \
		--url "$(CURL_RELEASE_URL)" \
		--cache-dir "$(abspath $(LIBS_CACHE))" \
		--build-dir "$(abspath $(CURL_UPSTREAM_BUILD_DIR))" \
		--overlay-dir "$(abspath $(CURL_UPSTREAM_OVERLAY_DIR))" \
		--cc "$(abspath $(CC)) --target=wasm32-wasip1 --sysroot=$(abspath $(SYSROOT))" \
		--ar "$(abspath $(WASI_SDK_DIR)/bin/llvm-ar)" \
		--ranlib "$(abspath $(WASI_SDK_DIR)/bin/llvm-ranlib)" \
		--output "$(abspath $@)"

# wget: minimal wget built on libcurl
$(BUILD_DIR)/wget: programs/wget.c $(CURL_SRCS) $(WASI_SDK_DIR)/bin/clang
	@mkdir -p $(BUILD_DIR)
	$(CC) $(WASM_CFLAGS) $(CURL_LIB_DEFS) $(CURL_INCLUDES) \
	    -lwasi-emulated-signal \
	    -o $@.wasm programs/wget.c $(CURL_SRCS)
	wasm-opt -O3 --strip-debug --all-features $@.wasm -o $@
	@rm -f $@.wasm

$(NATIVE_DIR)/wget: programs/wget.c
	@mkdir -p $(NATIVE_DIR)
	$(NATIVE_CC) $(NATIVE_CFLAGS) -o $@ programs/wget.c -lcurl

# duckdb: upstream DuckDB CLI built from source with our patched WASI/POSIX sysroot
$(BUILD_DIR)/duckdb: libs/duckdb/CMakeLists.txt $(PATCHED_SYSROOT)/lib/wasm32-wasi/libc.a $(WASI_SDK_DIR)/bin/clang $(WASI_SDK_DIR)/bin/clang++ cmake/FindThreads.cmake scripts/build-duckdb.sh include/fcntl.h include/ifaddrs.h include/net/if.h include/sys/ioctl.h
	@mkdir -p $(BUILD_DIR)
	DUCKDB_SRC_DIR="$(abspath $(LIBS_DIR)/duckdb)" \
	DUCKDB_BUILD_DIR="$(abspath $(BUILD_DIR)/duckdb-cmake)" \
	DUCKDB_OUTPUT="$(abspath $(BUILD_DIR)/duckdb)" \
	WASI_SDK_DIR="$(abspath $(WASI_SDK_DIR))" \
	SYSROOT_DIR="$(abspath $(PATCHED_SYSROOT))" \
	MODULE_PATH="$(abspath cmake)" \
	OVERLAY_INCLUDE_DIR="$(abspath include)" \
	DUCKDB_GIT_DESCRIBE="$(DUCKDB_GIT_DESCRIBE)" \
	bash scripts/build-duckdb.sh

# --- Compile all C programs to native binaries (for parity testing) ---

native: $(NATIVE_OUTPUTS)
	@echo ""
	@echo "=== Native Build Report ==="
	@echo "Programs: $(words $(NATIVE_PROG_NAMES)) compiled"
	@echo "Output:   $(NATIVE_DIR)/"
	@echo "=== Build complete ==="

$(NATIVE_DIR)/%: programs/%.c
	@mkdir -p $(NATIVE_DIR)
	$(NATIVE_CC) $(NATIVE_CFLAGS) -o $@ $<

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
		case "$$cmd" in \
			sqlite3) src="sqlite3_cli" ;; \
		esac; \
		if [ -f "$(BUILD_DIR)/$$src" ]; then \
			cp "$(BUILD_DIR)/$$src" "$(COMMANDS_DIR)/$$cmd"; \
			INSTALLED=$$((INSTALLED + 1)); \
		else \
			echo "Warning: $(BUILD_DIR)/$$src not found — skipping"; \
		fi; \
	done; \
	echo "Installed $$INSTALLED C command(s) to $(COMMANDS_DIR)"
	@# Create symlinks for git remote helpers
	@if [ -f "$(COMMANDS_DIR)/git" ]; then \
		ln -sf git "$(COMMANDS_DIR)/git-remote-http"; \
		ln -sf git "$(COMMANDS_DIR)/git-remote-https"; \
	fi

# --- Clean ---

clean:
	rm -rf $(BUILD_DIR)
