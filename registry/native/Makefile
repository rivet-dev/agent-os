WASM_TARGET := wasm32-wasip1
RELEASE_DIR := target/$(WASM_TARGET)/release

# Standalone binary output directory (configurable)
COMMANDS_DIR ?= $(RELEASE_DIR)/commands

# Detect patched std: if patches/ has .patch files and patch-std.sh exists, use patched sysroot
PATCHES := $(wildcard patches/*.patch)
PATCH_SCRIPT := scripts/patch-std.sh

# Resolve WASI sysroot for CRT/libc needed by -Z build-std linking
# Uses rustc without +toolchain override so rust-toolchain.toml is respected
WASI_SYSROOT_SELF_CONTAINED := $(shell rustc --print sysroot)/lib/rustlib/$(WASM_TARGET)/lib/self-contained

# Discover command binary names from crates/commands/
COMMAND_NAMES := $(notdir $(wildcard crates/commands/*))

# Alias symlinks: link_name:target_binary
ALIAS_SYMLINKS := \
	egrep:grep fgrep:grep \
	gunzip:gzip zcat:gzip \
	bash:sh \
	dir:ls vdir:ls \
	more:cat \
	[:test

# Stub commands: each gets a symlink to _stubs
STUB_COMMANDS := \
	chcon runcon \
	chgrp chown chroot \
	df \
	groups id \
	hostname hostid \
	install \
	kill \
	mkfifo mknod \
	pinky who users uptime \
	stty sync tty

.PHONY: all wasm wasm-opt-check host test clean patch-std patch-check vendor patch-vendor vendor-patch-check size-report install c-wasm

all: wasm

# Apply std patches if they exist
patch-std:
ifneq ($(PATCHES),)
	@if [ -x "$(PATCH_SCRIPT)" ]; then \
		echo "Applying std patches..."; \
		./$(PATCH_SCRIPT); \
	else \
		echo "Warning: patches found but $(PATCH_SCRIPT) not executable or missing"; \
	fi
else
	@echo "No patches found, using stock std"
endif

# Resolve Rust std source for vendoring build-std deps
RUST_STD_SRC := $(shell rustc --print sysroot)/lib/rustlib/src/rust

# Vendor dependencies from crates.io into vendor/ (includes std deps for -Z build-std)
vendor:
	@if [ ! -d vendor ]; then \
		echo "Vendoring dependencies (including std library deps)..."; \
		cargo vendor \
			--sync "$(RUST_STD_SRC)/library/std/Cargo.toml" \
			--sync "$(RUST_STD_SRC)/library/test/Cargo.toml"; \
	else \
		echo "vendor/ exists — skipping (run 'make clean-vendor' to re-vendor)"; \
	fi

# Apply crate-level patches to vendored sources
patch-vendor: vendor
	@if [ -x scripts/patch-vendor.sh ]; then \
		./scripts/patch-vendor.sh; \
	else \
		echo "Warning: scripts/patch-vendor.sh not found or not executable"; \
	fi

# Dry-run verification of crate patches
vendor-patch-check: vendor
	@if [ -x scripts/patch-vendor.sh ]; then \
		./scripts/patch-vendor.sh --check; \
	else \
		echo "Warning: scripts/patch-vendor.sh not found or not executable"; \
	fi

# Ensure wasm-opt is installed (needed for post-build optimization)
wasm-opt-check:
	@if ! command -v wasm-opt >/dev/null 2>&1; then \
		echo "wasm-opt not found — installing via cargo..."; \
		cargo install wasm-opt; \
	else \
		echo "wasm-opt found: $$(wasm-opt --version)"; \
	fi

# Build all standalone command binaries, optimize, strip .wasm extension, create symlinks
wasm: vendor patch-vendor patch-std wasm-opt-check
	RUSTFLAGS="-C link-arg=-L$(WASI_SYSROOT_SELF_CONTAINED)" \
	cargo build --target $(WASM_TARGET) \
		-Z build-std=std,panic_abort \
		--release
	@mkdir -p $(COMMANDS_DIR)
	@TOTAL=0; SKIPPED=0; \
	HAS_OPT=$$(command -v wasm-opt >/dev/null 2>&1 && echo 1 || echo 0); \
	for cmd in $(COMMAND_NAMES); do \
		wasm="$(RELEASE_DIR)/$$cmd.wasm"; \
		if [ -f "$$wasm" ]; then \
			TOTAL=$$((TOTAL + 1)); \
			if [ "$$HAS_OPT" = "1" ]; then \
				wasm-opt -O3 --strip-debug --all-features "$$wasm" -o "$(COMMANDS_DIR)/$$cmd"; \
			else \
				cp "$$wasm" "$(COMMANDS_DIR)/$$cmd"; \
			fi; \
		else \
			SKIPPED=$$((SKIPPED + 1)); \
			echo "Warning: $$wasm not found — skipping $$cmd"; \
		fi; \
	done; \
	echo ""; \
	echo "=== Standalone Binary Build Report ==="; \
	echo "Binaries: $$TOTAL built"; \
	if [ $$SKIPPED -gt 0 ]; then \
		echo "Skipped:  $$SKIPPED (binary not found)"; \
	fi; \
	if [ "$$HAS_OPT" = "1" ]; then \
		echo "Optimized: wasm-opt -O3 --strip-debug"; \
	else \
		echo "Optimized: none (wasm-opt not found)"; \
	fi
	@# Create alias symlinks (fall back to copy on platforms without symlink support)
	@cd $(COMMANDS_DIR) && \
	for pair in $(ALIAS_SYMLINKS); do \
		link=$${pair%%:*}; target=$${pair##*:}; \
		if [ -f "$$target" ]; then \
			ln -sf "$$target" "$$link" 2>/dev/null || cp "$$target" "$$link"; \
		fi; \
	done
	@# Create stub command symlinks
	@cd $(COMMANDS_DIR) && \
	for stub in $(STUB_COMMANDS); do \
		if [ -f _stubs ]; then \
			ln -sf _stubs "$$stub" 2>/dev/null || cp _stubs "$$stub"; \
		fi; \
	done
	@TOTAL_FILES=$$(ls -1 $(COMMANDS_DIR) 2>/dev/null | wc -l); \
	echo "Total:    $$TOTAL_FILES entries in $(COMMANDS_DIR)"; \
	echo "=== Build complete ==="

# Show per-command binary sizes in COMMANDS_DIR
size-report:
	@if [ ! -d "$(COMMANDS_DIR)" ]; then \
		echo "Error: $(COMMANDS_DIR) not found. Run 'make wasm' first."; \
		exit 1; \
	fi
	@echo "=== Standalone Binary Size Report ==="
	@echo ""
	@printf "%-10s  %s\n" "SIZE" "COMMAND"
	@printf "%-10s  %s\n" "----" "-------"
	@ls -lhS $(COMMANDS_DIR)/* 2>/dev/null | \
		grep -v "^total" | \
		while read line; do \
			size=$$(echo "$$line" | awk '{print $$5}'); \
			name=$$(echo "$$line" | awk '{print $$NF}'); \
			base=$$(basename "$$name"); \
			link=""; \
			if [ -L "$$name" ]; then \
				target=$$(readlink "$$name"); \
				link=" -> $$target"; \
			fi; \
			printf "%-10s  %s%s\n" "$$size" "$$base" "$$link"; \
		done
	@echo ""
	@TOTAL_SIZE=$$(du -sh $(COMMANDS_DIR) 2>/dev/null | awk '{print $$1}'); \
	TOTAL_FILES=$$(ls -1 $(COMMANDS_DIR) 2>/dev/null | wc -l); \
	REAL_FILES=$$(find $(COMMANDS_DIR) -maxdepth 1 -type f 2>/dev/null | wc -l); \
	SYMLINKS=$$(find $(COMMANDS_DIR) -maxdepth 1 -type l 2>/dev/null | wc -l); \
	echo "Total:     $$TOTAL_SIZE ($$TOTAL_FILES entries: $$REAL_FILES binaries, $$SYMLINKS symlinks)"
	@echo ""
	@echo "=== Per-Crate Size Contribution (rlib, top 30) ==="
	@echo "Note: rlib sizes are pre-LTO; actual contribution after LTO/DCE may differ."
	@echo ""
	@printf "%-10s  %s\n" "SIZE" "CRATE"
	@printf "%-10s  %s\n" "----" "-----"
	@ls -lhS $(RELEASE_DIR)/deps/*.rlib 2>/dev/null | \
		awk '{split($$NF, a, "/"); name=a[length(a)]; gsub(/^lib/, "", name); gsub(/-[0-9a-f]+\.rlib$$/, "", name); printf "%-10s  %s\n", $$5, name}' | \
		awk '!seen[$$2]++' | \
		head -30
	@echo ""
	@echo "=== Dependency Count ==="
	@TOTAL=$$(ls $(RELEASE_DIR)/deps/*.rlib 2>/dev/null | \
		sed 's/.*\/lib//' | sed 's/-[0-9a-f]*\.rlib//' | sort -u | wc -l); \
	echo "Unique crates: $$TOTAL"

# Install standalone binaries to a custom directory
install:
	@if [ -z "$(DESTDIR)" ]; then \
		echo "Usage: make install DESTDIR=/path/to/commands"; \
		exit 1; \
	fi
	@if [ ! -d "$(COMMANDS_DIR)" ]; then \
		echo "Error: $(COMMANDS_DIR) not found. Run 'make wasm' first."; \
		exit 1; \
	fi
	@mkdir -p $(DESTDIR)
	@cp -a $(COMMANDS_DIR)/* $(DESTDIR)/
	@echo "Installed to $(DESTDIR)"

# Build C programs to WASM (downloads wasi-sdk if needed)
c-wasm:
	$(MAKE) -C c programs

host:
	@if [ -f host/package.json ]; then \
		echo "Building JS host runtime..."; \
		cd host && npm run build; \
	else \
		echo "Skipping host build: host/package.json not found"; \
	fi

test:
	cargo test --workspace
	@if [ -f host/package.json ]; then \
		cd host && npm test; \
	fi

clean:
	cargo clean
	rm -rf host/dist

clean-vendor:
	rm -rf vendor

patch-check:
ifneq ($(PATCHES),)
	@if [ -x "$(PATCH_SCRIPT)" ]; then \
		./$(PATCH_SCRIPT) --check; \
	else \
		echo "Warning: $(PATCH_SCRIPT) not found or not executable"; \
	fi
else
	@echo "No patches found"
endif
