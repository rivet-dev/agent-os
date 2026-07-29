WASM_TARGET := wasm32-wasip1
RELEASE_DIR := target/$(WASM_TARGET)/release
WASI_C_SYSROOT := $(CURDIR)/c/sysroot
WASI_C_CC := $(CURDIR)/c/vendor/wasi-sdk/bin/clang
WASI_C_AR := $(CURDIR)/c/vendor/wasi-sdk/bin/llvm-ar
WASI_C_LIBDIR := $(WASI_C_SYSROOT)/lib/wasm32-wasi

# Standalone binary output directory (configurable)
COMMANDS_DIR ?= $(RELEASE_DIR)/commands
WASM_OPT_JOBS ?= 4

# Detect patched std: if std-patches/ has .patch files and patch-std.sh exists, use patched sysroot
PATCHES := $(wildcard std-patches/*.patch)
PATCH_SCRIPT := scripts/patch-std.sh

# Disable rustc's stock self-contained WASI libraries and explicitly link the
# command CRT and libc from AgentOS's owned sysroot. Merely putting this libdir
# first in -L is insufficient: rustc selects its bundled libc internally.
# Include the libc digest in rustc's metadata so Cargo relinks Rust commands
# whenever the external sysroot is rebuilt; Cargo does not otherwise track
# archives passed as linker arguments.
WASI_C_LIBC_DIGEST = $(shell if command -v sha256sum >/dev/null 2>&1; then sha256sum "$(WASI_C_LIBDIR)/libc.a"; elif command -v shasum >/dev/null 2>&1; then shasum -a 256 "$(WASI_C_LIBDIR)/libc.a"; else cksum "$(WASI_C_LIBDIR)/libc.a"; fi 2>/dev/null | cut -d ' ' -f1 | cut -c1-16)
WASI_RUSTFLAGS = -C link-self-contained=no -C link-arg=$(WASI_C_LIBDIR)/crt1-command.o -C link-arg=$(WASI_C_LIBDIR)/libc.a -C link-arg=$(WASI_C_LIBDIR)/libwasi-emulated-pthread.a -C link-arg=-L$(WASI_C_LIBDIR) -C metadata=agentos-libc-$(WASI_C_LIBC_DIGEST) --cfg tokio_unstable

# Discover command binary names from colocated package crates plus toolchain
# helper crates. Keep known slow/heavy commands out of the default software
# gate; they can still be built explicitly
# with `make cmd/<name>`.
# Cargo package names are normally `cmd-<command>`. `_stubs` is the only
# workspace package whose historical name differs from the installed binary.
# The `cmd-fd` and `cmd-rg` packages pull their bin-only upstream crates into
# vendor/; their real binaries are built by manifest path below because a
# crates.io dependency is not selectable with `cargo -p`.
cargo_package_for_command = $(if $(filter _stubs,$(1)),cmd-stubs,cmd-$(1))

SOFTWARE_COMMAND_CRATES := $(wildcard ../software/*/native/crates/cmd-*)
TOOLCHAIN_COMMAND_CRATES := $(wildcard crates/commands/*)
SKIPPED_BULK_COMMANDS := git codex codex-exec
COMMAND_NAMES := $(filter-out $(SKIPPED_BULK_COMMANDS),$(patsubst cmd-%,%,$(notdir $(SOFTWARE_COMMAND_CRATES))) $(notdir $(TOOLCHAIN_COMMAND_CRATES)))
COMMAND_PACKAGES := $(foreach command,$(COMMAND_NAMES),-p $(call cargo_package_for_command,$(command)))
COREUTILS_COMMAND_CRATES := $(wildcard ../software/coreutils/native/crates/cmd-*)
PR_COMMAND_NAMES := $(patsubst cmd-%,%,$(notdir $(COREUTILS_COMMAND_CRATES))) _stubs
PR_COMMAND_PACKAGES := $(foreach command,$(PR_COMMAND_NAMES),-p $(call cargo_package_for_command,$(command)))

# Alias symlinks: link_name:target_binary
ALIAS_SYMLINKS := \
	gunzip:gzip zcat:gzip \
	bash:sh \
	dir:ls vdir:ls \
	more:cat \
	[:test

# Stub commands: each gets a symlink to _stubs
STUB_COMMANDS := \
	chcon runcon \
	chroot \
	groups \
	hostid \
	install \
	pinky who users uptime \
	stty sync tty

.PHONY: all commands pr-commands wasm wasm-opt-check host test clean patch-std patch-check vendor patch-vendor vendor-patch-check size-report install c-wasm codex fd-mapping-contract-wasm fd-mapping-contract-native

all: wasm

# Build the fast command set that lands in COMMANDS_DIR: Rust commands except
# known heavy/external ones, then the opted-in fast C commands. Heavy commands
# (git, duckdb, vim) and the external codex build are intentionally excluded
# from the default software-build gate; `_stubs` is a required Rust input for
# the generated stub aliases and is built with the default command set.
commands: wasm
	$(MAKE) -C c programs install

# PR validation only needs the default coreutils package. Keep the complete
# registry command build in `commands` for nightly and publish workflows.
pr-commands:
	@test "$(abspath $(COMMANDS_DIR))" = "$(abspath $(RELEASE_DIR)/commands)" || \
		{ echo "refusing to clean unexpected PR command directory: $(COMMANDS_DIR)"; exit 1; }
	rm -rf "$(COMMANDS_DIR)"
	$(MAKE) wasm \
		COMMAND_NAMES="$(PR_COMMAND_NAMES)" \
		COMMAND_PACKAGES="$(PR_COMMAND_PACKAGES)"
	$(MAKE) -C c build/mknod install COMMANDS="mknod"

# Build the real wasm32-wasip1 `codex-exec` agent engine and install it where
# @agentos-software/codex-cli stages commands from.
#
# Two modes:
#   - default (CODEX_REPO unset): REPRODUCIBLE build. clone-and-build-codex-wasi.sh
#     shallow-clones the fork pinned in toolchain/codex-ref, rewrites its
#     [patch.crates-io] to the toolchain stubs + vendored/patched tokio, and builds.
#     No local checkout needed; deterministic from the pin.
#   - CODEX_REPO=/path/to/codex-rs/codex-rs: DEV override. Builds that existing
#     checkout in place via scripts/build-codex-wasi.sh (fast local iteration).
AGENTOS_ROOT := $(abspath $(CURDIR)/..)
CODEX_REF := $(shell cat $(CURDIR)/codex-ref 2>/dev/null)
CODEX_REPO ?=
CODEX_WASI_SDK_DIR := $(abspath $(CURDIR)/c/vendor/wasi-sdk)
codex: c/vendor/wasi-sdk/bin/clang c/sysroot/lib/wasm32-wasi/libc.a
ifeq ($(strip $(CODEX_REPO)),)
	@echo "codex: reproducible build from pin $(CODEX_REF)"
	@WASI_SDK_DIR="$(CODEX_WASI_SDK_DIR)" \
		"$(CURDIR)/scripts/clone-and-build-codex-wasi.sh"
else
	@if [ -x "$(CODEX_REPO)/scripts/build-wasi-codex-exec.sh" ]; then \
		echo "codex: CODEX_REPO override -> building $(CODEX_REPO) in place"; \
		CODEX_REPO="$(CODEX_REPO)" \
		WASI_SDK_DIR="$(CODEX_WASI_SDK_DIR)" \
		"$(CURDIR)/scripts/build-codex-wasi.sh"; \
	else \
		echo "codex: CODEX_REPO=$(CODEX_REPO) has no scripts/build-wasi-codex-exec.sh"; \
		echo "       Unset CODEX_REPO to build reproducibly from the pin instead."; \
		exit 1; \
	fi
endif

c/vendor/wasi-sdk/bin/clang:
	$(MAKE) -C c wasi-sdk

c/sysroot/lib/wasm32-wasi/libc.a:
	$(MAKE) -C c sysroot

# Strict variant for environments that require the codex artifact. With CODEX_REPO
# unset it builds reproducibly from the pin; with CODEX_REPO set it fails hard if
# that checkout lacks the fork build script.
.PHONY: codex-required
codex-required:
ifeq ($(strip $(CODEX_REPO)),)
	@$(MAKE) codex
else
	@test -x "$(CODEX_REPO)/scripts/build-wasi-codex-exec.sh" || { \
		echo "ERROR: codex fork build script not found at $(CODEX_REPO); unset CODEX_REPO to build from the pin"; exit 1; }
	@$(MAKE) codex
endif

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
		mkdir -p .cargo; \
		cargo vendor \
			--sync "$(RUST_STD_SRC)/library/std/Cargo.toml" \
			--sync "$(RUST_STD_SRC)/library/test/Cargo.toml" \
			vendor > .cargo/config.toml; \
	else \
		echo "vendor/ exists — skipping (run 'make clean-vendor' to re-vendor)"; \
	fi
	@# Source-replacement config so cargo builds from the patched vendor/ tree
	@# instead of the crates.io registry cache. This file is gitignored, so a
	@# fresh checkout (CI) has none; without it every std-patches/crates/*.patch
	@# wasi fix is silently ignored and crates like uu_tac fail to compile for
	@# wasm32-wasip1. `cargo vendor` prints the exact config (including the
	@# synced std-dep sources) on stdout, so capture it above.
	@test -s .cargo/config.toml || { \
		echo "ERROR: .cargo/config.toml missing or empty after vendor"; \
		echo "Run 'make clean-vendor && make vendor' to regenerate."; \
		exit 1; \
	}

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
wasm: c/sysroot/lib/wasm32-wasi/libc.a vendor patch-vendor patch-std wasm-opt-check
	# --cfg tokio_unstable lifts tokio's wasm `compile_error!` so the patched
	# tokio process/net modules (std-patches/crates/tokio) compile on wasm32-wasip1.
	# Needed by codex (tokio::process/net). Harmless for commands not using tokio.
	CC_wasm32_wasip1="$(WASI_C_CC)" \
	AR_wasm32_wasip1="$(WASI_C_AR)" \
	CFLAGS_wasm32_wasip1="--sysroot=$(WASI_C_SYSROOT)" \
	RUSTFLAGS="$(WASI_RUSTFLAGS)" \
	cargo build --target $(WASM_TARGET) \
		-Z build-std=std,panic_abort \
		--release \
		$(COMMAND_PACKAGES)
	@set -e; \
	for spec in "fd:vendor/fd-find/Cargo.toml" "rg:vendor/ripgrep/Cargo.toml"; do \
		bin=$${spec%%:*}; manifest=$${spec#*:}; \
		case " $(COMMAND_NAMES) " in *" $$bin "*) ;; *) continue ;; esac; \
		rm -f "$${manifest%/*}/Cargo.lock"; \
		CC_wasm32_wasip1="$(WASI_C_CC)" \
		AR_wasm32_wasip1="$(WASI_C_AR)" \
		CFLAGS_wasm32_wasip1="--sysroot=$(WASI_C_SYSROOT)" \
		RUSTFLAGS="$(WASI_RUSTFLAGS)" \
		cargo build --manifest-path "$$manifest" \
			--target-dir target \
			--target $(WASM_TARGET) \
			-Z build-std=std,panic_abort \
			--release \
			--no-default-features \
			--bin "$$bin"; \
	done
	@mkdir -p $(COMMANDS_DIR)
	@set -e; \
	HAS_OPT=$$(command -v wasm-opt >/dev/null 2>&1 && echo 1 || echo 0); \
	if [ "$$HAS_OPT" = "1" ]; then \
		printf '%s\n' $(COMMAND_NAMES) | \
			xargs -P $(WASM_OPT_JOBS) -n 1 sh -c ' \
				wasm="$(RELEASE_DIR)/$$1.wasm"; \
				if [ -f "$$wasm" ]; then \
					wasm-opt -O3 --strip-debug --all-features "$$wasm" -o "$(COMMANDS_DIR)/$$1"; \
				else \
					echo "Warning: $$wasm not found — skipping $$1"; \
				fi \
			' _; \
	else \
		for cmd in $(COMMAND_NAMES); do \
			wasm="$(RELEASE_DIR)/$$cmd.wasm"; \
			if [ -f "$$wasm" ]; then \
				cp "$$wasm" "$(COMMANDS_DIR)/$$cmd"; \
			else \
				echo "Warning: $$wasm not found — skipping $$cmd"; \
			fi; \
		done; \
	fi; \
	TOTAL=0; SKIPPED=0; \
	for cmd in $(COMMAND_NAMES); do \
		wasm="$(RELEASE_DIR)/$$cmd.wasm"; \
		if [ -f "$$wasm" ]; then \
			TOTAL=$$((TOTAL + 1)); \
		else \
			SKIPPED=$$((SKIPPED + 1)); \
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

# ---------------------------------------------------------------------------
# Uniform per-binary entry point: `make cmd/<name>` builds ONE command binary
# into $(COMMANDS_DIR), dispatching to the toolchain that owns it:
#   Rust      software/*/native/crates/cmd-<name> or crates/commands/<name>
#             (cargo package cmd-<name>, via wasm-cmd)
#   C         the C opt-in set below  (toolchain/c, patched sysroot)
#   codex     the codex fork build    (codex, codex-exec)
# `just toolchain-cmd <name>` calls this. `make wasm` builds the fast
# Rust set; `make -C c programs install` builds/installs the fast C set.
C_COMMANDS := zip unzip envsubst sqlite3 curl wget grep git duckdb vim ssh ssh-keysign ssh-sk-helper

.PHONY: cmd/%
cmd/%:
	@name="$*"; \
	crate_dir=$$(find ../software -path "*/native/crates/cmd-$$name" -type d -print -quit); \
	if [ -n "$$crate_dir" ] || [ -d "crates/commands/$$name" ]; then \
		$(MAKE) wasm-cmd CMD=$$name; \
	elif echo " $(C_COMMANDS) " | grep -q " $$name "; then \
		src=$$name; install_commands=$$name; \
		[ "$$name" = "sqlite3" ] && src=sqlite3_cli; \
		[ "$$name" = "git" ] && install_commands="git git-remote-http"; \
		[ "$$name" = "ssh" ] && install_commands="ssh ssh-keysign ssh-sk-helper"; \
		$(MAKE) -C c sysroot "build/$$src" && \
			$(MAKE) -C c install COMMANDS="$$install_commands"; \
	elif [ "$$name" = "codex" ] || [ "$$name" = "codex-exec" ]; then \
		$(MAKE) codex-required; \
	else \
		echo "ERROR: unknown command '$$name' (no software/*/native/crates/cmd-$$name or crates/commands/$$name; not in C_COMMANDS or codex)"; \
		exit 1; \
	fi

# Build ONE command binary (cargo package cmd-$(CMD) -> $(COMMANDS_DIR)/$(CMD)).
# Usage: make wasm-cmd CMD=sh
.PHONY: wasm-cmd
wasm-cmd: c/sysroot/lib/wasm32-wasi/libc.a vendor patch-vendor patch-std wasm-opt-check
	@test -n "$(CMD)" || { echo "ERROR: pass CMD=<name> (a dir under crates/commands/)"; exit 1; }
	@crate_dir=$$(find ../software -path "*/native/crates/cmd-$(CMD)" -type d -print -quit); \
	test -n "$$crate_dir" || test -d "crates/commands/$(CMD)" || { \
		echo "ERROR: no software/*/native/crates/cmd-$(CMD) or crates/commands/$(CMD)"; exit 1; }
	@cargo_pkg="$(call cargo_package_for_command,$(CMD))"; cargo_bin="$(CMD)"; \
	manifest=""; \
	[ "$(CMD)" = "fd" ] && manifest="vendor/fd-find/Cargo.toml"; \
	[ "$(CMD)" = "rg" ] && manifest="vendor/ripgrep/Cargo.toml"; \
	if [ -n "$$manifest" ]; then \
		rm -f "$${manifest%/*}/Cargo.lock"; \
		CC_wasm32_wasip1="$(WASI_C_CC)" \
		AR_wasm32_wasip1="$(WASI_C_AR)" \
		CFLAGS_wasm32_wasip1="--sysroot=$(WASI_C_SYSROOT)" \
		RUSTFLAGS="$(WASI_RUSTFLAGS)" \
		cargo build --manifest-path "$$manifest" \
			--target-dir target \
			--target $(WASM_TARGET) \
			-Z build-std=std,panic_abort \
			--release \
			--no-default-features \
			--bin $$cargo_bin; \
	else \
		CC_wasm32_wasip1="$(WASI_C_CC)" \
		AR_wasm32_wasip1="$(WASI_C_AR)" \
		CFLAGS_wasm32_wasip1="--sysroot=$(WASI_C_SYSROOT)" \
		RUSTFLAGS="$(WASI_RUSTFLAGS)" \
		cargo build --target $(WASM_TARGET) \
			-Z build-std=std,panic_abort \
			--release \
			-p $$cargo_pkg \
			--bin $$cargo_bin; \
	fi
	@mkdir -p $(COMMANDS_DIR)
	@if command -v wasm-opt >/dev/null 2>&1; then \
		wasm-opt -O3 --strip-debug --all-features "$(RELEASE_DIR)/$(CMD).wasm" -o "$(COMMANDS_DIR)/$(CMD)"; \
	else \
		cp "$(RELEASE_DIR)/$(CMD).wasm" "$(COMMANDS_DIR)/$(CMD)"; \
	fi
	@echo "Built $(COMMANDS_DIR)/$(CMD)"

.PHONY: command-dispatch-check
command-dispatch-check:
	@test "$(call cargo_package_for_command,_stubs)" = "cmd-stubs"
	@test "$(call cargo_package_for_command,fd)" = "cmd-fd"
	@test "$(call cargo_package_for_command,rg)" = "cmd-rg"
	@test "$(call cargo_package_for_command,sh)" = "cmd-sh"
	@test -n "$(filter _stubs,$(COMMAND_NAMES))"
	@test -n "$(filter cmd-stubs,$(COMMAND_PACKAGES))"
	@test -n "$(filter cmd-fd,$(COMMAND_PACKAGES))"
	@test -n "$(filter cmd-rg,$(COMMAND_PACKAGES))"
	@echo "Command package dispatch mappings verified"

# Rust fixture for exact parent->child fd-map parity. It lives outside the
# production workspace so it cannot become a shipped command accidentally;
# the C conformance build invokes these targets and mounts the resulting binary
# from c/build alongside the other native/WASM fixtures.
FD_MAPPING_CONTRACT := test-programs/fd-mapping-contract/Cargo.toml
FD_MAPPING_CONTRACT_TARGET := target/fd-mapping-contract

fd-mapping-contract-wasm: c/sysroot/lib/wasm32-wasi/libc.a patch-std
	CC_wasm32_wasip1="$(WASI_C_CC)" \
	AR_wasm32_wasip1="$(WASI_C_AR)" \
	CFLAGS_wasm32_wasip1="--sysroot=$(WASI_C_SYSROOT)" \
	RUSTFLAGS="$(WASI_RUSTFLAGS)" \
	cargo build --manifest-path $(FD_MAPPING_CONTRACT) \
		--target-dir $(FD_MAPPING_CONTRACT_TARGET) \
		--target $(WASM_TARGET) \
		-Z build-std=std,panic_abort \
		--release
	@mkdir -p c/build
	@cp $(FD_MAPPING_CONTRACT_TARGET)/$(WASM_TARGET)/release/fd-mapping-contract.wasm \
		c/build/fd_mapping_contract

fd-mapping-contract-native:
	cargo build --manifest-path $(FD_MAPPING_CONTRACT) \
		--target-dir $(FD_MAPPING_CONTRACT_TARGET)/native \
		--release
	@mkdir -p c/build/native
	@cp $(FD_MAPPING_CONTRACT_TARGET)/native/release/fd-mapping-contract \
		c/build/native/fd_mapping_contract

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

# Build C programs to WASM (downloads wasi-sdk if needed)


test:
	cargo test --workspace

clean:
	cargo clean
	rm -rf host/dist

clean-vendor:
	rm -rf vendor .cargo/config.toml

patch-check:
ifneq ($(PATCHES),)
	@if [ -x scripts/test-patch-std-check.sh ]; then \
		./scripts/test-patch-std-check.sh; \
	fi
	@if [ -x "$(PATCH_SCRIPT)" ]; then \
		./$(PATCH_SCRIPT) --check; \
	else \
		echo "Warning: $(PATCH_SCRIPT) not found or not executable"; \
	fi
else
	@echo "No patches found"
endif
