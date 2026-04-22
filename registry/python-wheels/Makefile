SHELL := /bin/bash

# Output goes into the npm package
WHEEL_PKG_DIR := ../software/python-wheels
WHEELS_DIR    := $(WHEEL_PKG_DIR)/wheels
INDEX_DIR     := $(WHEELS_DIR)/index
LOCKFILE      := $(WHEELS_DIR)/lockfile.json

# Toolchain pins
PYODIDE_BUILD_VERSION := 0.34.1
PYODIDE_VERSION       := 0.29.3
PYTHON_VERSION        := 3.13
PYTHON_TAG            := cp313
PYODIDE_ABI_TAG       := pyodide_2025_0_wasm32

# Toolchain layout
TOOLCHAIN_DIR  := .toolchain
EMSDK_DIR      := $(TOOLCHAIN_DIR)/emsdk
VENV_DIR       := $(TOOLCHAIN_DIR)/venv
XBUILDENV_URL  := https://github.com/pyodide/pyodide/releases/download/$(PYODIDE_VERSION)/xbuildenv-$(PYODIDE_VERSION).tar.bz2

# DuckDB wheel pin (matches PYODIDE_ABI_TAG).
# xlwings publishes per (DuckDB version, Pyodide version) pair; the cp313 /
# pyodide_2025_0 wheel maxes out at DuckDB 1.5.0 today. To bump DuckDB,
# also bump PYODIDE_VERSION/PYTHON_TAG to 314.0.0a1 / cp314 (alpha track).
DUCKDB_VERSION    := 1.5.0
DUCKDB_WHEEL_URL := https://github.com/xlwings/duckdb-pyodide/releases/download/duckdb-v$(DUCKDB_VERSION)-pyodide-$(PYODIDE_VERSION)/duckdb-$(DUCKDB_VERSION)-$(PYTHON_TAG)-$(PYTHON_TAG)-$(PYODIDE_ABI_TAG).whl
DUCKDB_WHEEL_SHA256 := ee14bd19e6d9a390bb2803f119d4eff06a88a8e4b52ebf6c947c9dac67f6ba11

.PHONY: all setup-toolchain verify-toolchain build-extractor build-shim fetch-duckdb \
        build-pure-py-index build-all verify-extractor verify-duckdb verify-pure-index \
        verify-all clean clean-toolchain

all: build-all

# ---------------------------------------------------------------------------
# Toolchain setup
# ---------------------------------------------------------------------------
setup-toolchain: $(VENV_DIR)/.ready $(EMSDK_DIR)/.ready
	@echo "=== Toolchain ready ==="

$(VENV_DIR)/.ready:
	@echo "=== Setting up Python venv with pyodide-build ==="
	@mkdir -p $(TOOLCHAIN_DIR)
	@if [ ! -d "$(VENV_DIR)" ]; then \
		python$(PYTHON_VERSION) -m venv $(VENV_DIR) || python3 -m venv $(VENV_DIR); \
	fi
	@$(VENV_DIR)/bin/pip install --upgrade pip
	@$(VENV_DIR)/bin/pip install \
		"pyodide-build==$(PYODIDE_BUILD_VERSION)" \
		"wheel<0.44" \
		"build" \
		"uv"
	@touch $(VENV_DIR)/.ready

$(EMSDK_DIR)/.ready: $(VENV_DIR)/.ready
	@echo "=== Setting up emsdk ==="
	@if [ ! -d "$(EMSDK_DIR)" ]; then \
		git clone --depth 1 https://github.com/emscripten-core/emsdk $(EMSDK_DIR); \
	fi
	@EMSDK_VER=$$($(VENV_DIR)/bin/pyodide config get emscripten_version); \
		echo "Activating emsdk $$EMSDK_VER"; \
		cd $(EMSDK_DIR) && ./emsdk install $$EMSDK_VER && ./emsdk activate $$EMSDK_VER
	@touch $(EMSDK_DIR)/.ready

verify-toolchain: setup-toolchain
	@source $(EMSDK_DIR)/emsdk_env.sh && \
		$(VENV_DIR)/bin/pyodide config get emscripten_version > /dev/null && \
		emcc --version > /dev/null && \
		echo "WHEEL_PIPELINE_READY"

# ---------------------------------------------------------------------------
# dbt-extractor — Rust + PyO3 wheel
# ---------------------------------------------------------------------------
build-extractor: setup-toolchain
	@bash scripts/build_extractor.sh "$(VENV_DIR)" "$(EMSDK_DIR)" "$(WHEELS_DIR)"

# ---------------------------------------------------------------------------
# dbt-extractor pure-Python shim (fallback if the Rust wheel won't compile)
# ---------------------------------------------------------------------------
build-shim: setup-toolchain
	@bash scripts/build_shim.sh "$(VENV_DIR)" "$(WHEELS_DIR)"

# ---------------------------------------------------------------------------
# DuckDB — fetch prebuilt xlwings wheel
# ---------------------------------------------------------------------------
fetch-duckdb:
	@bash scripts/fetch_duckdb.sh "$(DUCKDB_WHEEL_URL)" "$(DUCKDB_WHEEL_SHA256)" "$(WHEELS_DIR)"

# ---------------------------------------------------------------------------
# Pure-Python wheel index (~22 packages)
# ---------------------------------------------------------------------------
build-pure-py-index: setup-toolchain
	@$(VENV_DIR)/bin/python scripts/build_pure_index.py \
		--out "$(WHEELS_DIR)" \
		--index "$(INDEX_DIR)" \
		--lockfile "$(LOCKFILE)" \
		--python-tag "$(PYTHON_TAG)" \
		--abi-tag "$(PYODIDE_ABI_TAG)"

# ---------------------------------------------------------------------------
# Build everything
# ---------------------------------------------------------------------------
build-all: build-extractor fetch-duckdb build-shim build-pure-py-index
	@echo "=== Wheel build complete ==="
	@$(VENV_DIR)/bin/python scripts/build_pure_index.py --finalize-lockfile \
		--out "$(WHEELS_DIR)" \
		--index "$(INDEX_DIR)" \
		--lockfile "$(LOCKFILE)" \
		--python-tag "$(PYTHON_TAG)" \
		--abi-tag "$(PYODIDE_ABI_TAG)"

# ---------------------------------------------------------------------------
# Verification (run inside Pyodide via Node)
# ---------------------------------------------------------------------------
verify-extractor:
	@node scripts/verify_extractor.mjs "$(WHEELS_DIR)" "$(PYODIDE_ABI_TAG)" "$(PYTHON_TAG)"

verify-duckdb:
	@node scripts/verify_duckdb.mjs "$(WHEELS_DIR)"

verify-pure-index:
	@node scripts/verify_pure_index.mjs "$(WHEELS_DIR)"

verify-all: verify-extractor verify-duckdb verify-pure-index
	@echo "=== All wheels verified ==="

# ---------------------------------------------------------------------------
# Cleanup
# ---------------------------------------------------------------------------
clean:
	@rm -rf $(WHEELS_DIR)/*.whl $(INDEX_DIR) $(LOCKFILE)
	@rm -rf .build-cache
	@echo "Cleaned wheels (toolchain preserved; use clean-toolchain to remove)"

clean-toolchain:
	@rm -rf $(TOOLCHAIN_DIR)
	@echo "Toolchain cleaned"
