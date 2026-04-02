SHELL := /bin/bash

# Native build directory (source lives in this repo now)
NATIVE_DIR  := native
COMMANDS_DIR := $(NATIVE_DIR)/target/wasm32-wasip1/release/commands

# Marker files for incremental builds
MARKER_DIR  := .build-markers
RUST_MARKER := $(MARKER_DIR)/rust-wasm-built
C_MARKER    := $(MARKER_DIR)/c-wasm-built
GO_MARKER   := $(MARKER_DIR)/go-wasm-built
COPY_MARKER := $(MARKER_DIR)/wasm-copied
META_MARKER := $(MARKER_DIR)/meta-generated
TS_MARKER   := $(MARKER_DIR)/ts-built

# Command packages (excludes _types and meta-packages)
CMD_PACKAGES := coreutils sed grep gawk findutils diffutils tar gzip \
                curl http-get wget zip unzip jq ripgrep fd tree file sqlite3 duckdb yq codex git slack-cli

# Meta-packages
META_PACKAGES := common build-essential everything

# All publishable packages
ALL_PACKAGES := $(CMD_PACKAGES) $(META_PACKAGES)

.PHONY: all build-wasm build-wasm-rust build-wasm-c build-wasm-go copy-wasm generate-meta build test \
        publish publish-dry publish-force publish-clean clean rebuild

all: build

# ---------------------------------------------------------------------------
# Marker directory
# ---------------------------------------------------------------------------
$(MARKER_DIR):
	@mkdir -p $(MARKER_DIR)

# ---------------------------------------------------------------------------
# Build WASM binaries from source (in native/)
# ---------------------------------------------------------------------------
build-wasm: build-wasm-rust build-wasm-c build-wasm-go

build-wasm-rust: $(RUST_MARKER)
$(RUST_MARKER): $(MARKER_DIR)
	@echo "=== Building Rust WASM commands ==="
	cd $(NATIVE_DIR) && make wasm
	@touch $(RUST_MARKER)

build-wasm-c: $(C_MARKER)
$(C_MARKER): $(MARKER_DIR)
	@echo "=== Building C WASM commands ==="
	cd $(NATIVE_DIR)/c && make programs
	cd $(NATIVE_DIR)/c && make install COMMANDS_DIR=../target/wasm32-wasip1/release/commands
	@touch $(C_MARKER)

build-wasm-go: $(GO_MARKER)
$(GO_MARKER): $(MARKER_DIR)
	@echo "=== Building Go WASM commands ==="
	cd $(NATIVE_DIR)/go && make wasm
	@touch $(GO_MARKER)

# ---------------------------------------------------------------------------
# Copy WASM binaries from native build output into per-package wasm/ dirs
# ---------------------------------------------------------------------------
copy-wasm: $(COPY_MARKER)
$(COPY_MARKER): $(RUST_MARKER) $(C_MARKER) $(GO_MARKER)
	@echo "=== Copying WASM binaries to packages ==="

	@# --- coreutils ---
	@mkdir -p software/coreutils/wasm
	@for cmd in \
		sh arch b2sum base32 base64 basename basenc cat chmod cksum column comm \
		cp cut date dd dircolors dirname du echo env expand expr factor false \
		fmt fold head join link ln logname ls md5sum mkdir mktemp mv nice nl \
		nohup nproc numfmt od paste pathchk printenv printf ptx pwd readlink \
		realpath rev rm rmdir seq sha1sum sha224sum sha256sum sha384sum sha512sum \
		shred shuf sleep sort split stat stdbuf strings _stubs sum tac tail tee \
		test timeout touch tr true truncate tsort uname unexpand uniq unlink wc \
		xu \
		which \
		whoami yes; do \
		if [ -f "$(COMMANDS_DIR)/$$cmd" ]; then \
			cp "$(COMMANDS_DIR)/$$cmd" software/coreutils/wasm/; \
		else \
			echo "  WARN: $$cmd not found"; \
		fi; \
	done
	@# coreutils aliases
	@cd software/coreutils/wasm && \
		[ -f sh ] && cp -f sh bash || true; \
		[ -f cat ] && cp -f cat more || true; \
		[ -f ls ] && cp -f ls dir || true; \
		[ -f ls ] && cp -f ls vdir || true; \
		[ -f test ] && cp -f test '[' || true; \
		if [ -f _stubs ]; then \
			for stub in chcon runcon chgrp chown chroot df groups id hostname \
				hostid install kill mkfifo mknod pinky who users uptime stty \
				sync tty; do \
				cp -f _stubs "$$stub"; \
			done; \
		fi

	@# --- sed ---
	@mkdir -p software/sed/wasm
	@[ -f "$(COMMANDS_DIR)/sed" ] && cp -f "$(COMMANDS_DIR)/sed" software/sed/wasm/ || echo "  WARN: sed not found"

	@# --- grep + aliases ---
	@mkdir -p software/grep/wasm
	@[ -f "$(COMMANDS_DIR)/grep" ] && cp -f "$(COMMANDS_DIR)/grep" software/grep/wasm/ || echo "  WARN: grep not found"
	@[ -f "$(COMMANDS_DIR)/grep" ] && cp -f "$(COMMANDS_DIR)/grep" software/grep/wasm/egrep || true
	@[ -f "$(COMMANDS_DIR)/grep" ] && cp -f "$(COMMANDS_DIR)/grep" software/grep/wasm/fgrep || true

	@# --- gawk ---
	@mkdir -p software/gawk/wasm
	@[ -f "$(COMMANDS_DIR)/awk" ] && cp -f "$(COMMANDS_DIR)/awk" software/gawk/wasm/ || echo "  WARN: awk not found"

	@# --- findutils ---
	@mkdir -p software/findutils/wasm
	@[ -f "$(COMMANDS_DIR)/find" ] && cp -f "$(COMMANDS_DIR)/find" software/findutils/wasm/ || echo "  WARN: find not found"
	@[ -f "$(COMMANDS_DIR)/xargs" ] && cp -f "$(COMMANDS_DIR)/xargs" software/findutils/wasm/ || echo "  WARN: xargs not found"

	@# --- diffutils ---
	@mkdir -p software/diffutils/wasm
	@[ -f "$(COMMANDS_DIR)/diff" ] && cp -f "$(COMMANDS_DIR)/diff" software/diffutils/wasm/ || echo "  WARN: diff not found"

	@# --- tar ---
	@mkdir -p software/tar/wasm
	@[ -f "$(COMMANDS_DIR)/tar" ] && cp -f "$(COMMANDS_DIR)/tar" software/tar/wasm/ || echo "  WARN: tar not found"

	@# --- gzip + aliases ---
	@mkdir -p software/gzip/wasm
	@[ -f "$(COMMANDS_DIR)/gzip" ] && cp -f "$(COMMANDS_DIR)/gzip" software/gzip/wasm/ || echo "  WARN: gzip not found"
	@[ -f "$(COMMANDS_DIR)/gzip" ] && cp -f "$(COMMANDS_DIR)/gzip" software/gzip/wasm/gunzip || true
	@[ -f "$(COMMANDS_DIR)/gzip" ] && cp -f "$(COMMANDS_DIR)/gzip" software/gzip/wasm/zcat || true

	@# --- curl (C build, needs patched sysroot) ---
	@mkdir -p software/curl/wasm
	@[ -f "$(COMMANDS_DIR)/curl" ] && cp -f "$(COMMANDS_DIR)/curl" software/curl/wasm/ || echo "  WARN: curl not found (needs patched sysroot)"

	@# --- http-get ---
	@mkdir -p software/http-get/wasm
	@[ -f "$(COMMANDS_DIR)/http_get" ] && cp -f "$(COMMANDS_DIR)/http_get" software/http-get/wasm/ || echo "  WARN: http_get not found"

	@# --- wget (C build, needs patched sysroot) ---
	@mkdir -p software/wget/wasm
	@[ -f "$(COMMANDS_DIR)/wget" ] && cp -f "$(COMMANDS_DIR)/wget" software/wget/wasm/ || echo "  WARN: wget not found (needs patched sysroot)"

	@# --- zip (C build) ---
	@mkdir -p software/zip/wasm
	@[ -f "$(COMMANDS_DIR)/zip" ] && cp -f "$(COMMANDS_DIR)/zip" software/zip/wasm/ || echo "  WARN: zip not found"

	@# --- unzip (C build) ---
	@mkdir -p software/unzip/wasm
	@[ -f "$(COMMANDS_DIR)/unzip" ] && cp -f "$(COMMANDS_DIR)/unzip" software/unzip/wasm/ || echo "  WARN: unzip not found"

	@# --- jq ---
	@mkdir -p software/jq/wasm
	@[ -f "$(COMMANDS_DIR)/jq" ] && cp -f "$(COMMANDS_DIR)/jq" software/jq/wasm/ || echo "  WARN: jq not found"

	@# --- ripgrep ---
	@mkdir -p software/ripgrep/wasm
	@[ -f "$(COMMANDS_DIR)/rg" ] && cp -f "$(COMMANDS_DIR)/rg" software/ripgrep/wasm/ || echo "  WARN: rg not found"

	@# --- fd ---
	@mkdir -p software/fd/wasm
	@[ -f "$(COMMANDS_DIR)/fd" ] && cp -f "$(COMMANDS_DIR)/fd" software/fd/wasm/ || echo "  WARN: fd not found"

	@# --- tree ---
	@mkdir -p software/tree/wasm
	@[ -f "$(COMMANDS_DIR)/tree" ] && cp -f "$(COMMANDS_DIR)/tree" software/tree/wasm/ || echo "  WARN: tree not found"

	@# --- file ---
	@mkdir -p software/file/wasm
	@[ -f "$(COMMANDS_DIR)/file" ] && cp -f "$(COMMANDS_DIR)/file" software/file/wasm/ || echo "  WARN: file not found"

	@# --- sqlite3 (C build, needs patched sysroot) ---
	@mkdir -p software/sqlite3/wasm
	@[ -f "$(COMMANDS_DIR)/sqlite3" ] && cp -f "$(COMMANDS_DIR)/sqlite3" software/sqlite3/wasm/ || echo "  WARN: sqlite3 not found (needs patched sysroot)"

	@# --- duckdb (upstream C++ build, needs patched sysroot) ---
	@mkdir -p software/duckdb/wasm
	@[ -f "$(COMMANDS_DIR)/duckdb" ] && cp -f "$(COMMANDS_DIR)/duckdb" software/duckdb/wasm/ || echo "  WARN: duckdb not found (needs patched sysroot)"

	@# --- yq ---
	@mkdir -p software/yq/wasm
	@[ -f "$(COMMANDS_DIR)/yq" ] && cp -f "$(COMMANDS_DIR)/yq" software/yq/wasm/ || echo "  WARN: yq not found"

	@# --- codex ---
	@mkdir -p software/codex/wasm
	@[ -f "$(COMMANDS_DIR)/codex" ] && cp -f "$(COMMANDS_DIR)/codex" software/codex/wasm/ || echo "  WARN: codex not found"
	@[ -f "$(COMMANDS_DIR)/codex-exec" ] && cp -f "$(COMMANDS_DIR)/codex-exec" software/codex/wasm/ || echo "  WARN: codex-exec not found"

	@# --- git ---
	@mkdir -p software/git/wasm
	@[ -f "$(COMMANDS_DIR)/git" ] && cp -f "$(COMMANDS_DIR)/git" software/git/wasm/ || echo "  WARN: git not found"

	@# --- slack-cli (Go build, GOOS=wasip1) ---
	@mkdir -p software/slack-cli/wasm
	@[ -f "$(COMMANDS_DIR)/slack" ] && cp -f "$(COMMANDS_DIR)/slack" software/slack-cli/wasm/ || echo "  WARN: slack not found (Go WASI build)"

	@echo "=== Copy complete ==="
	@touch $(COPY_MARKER)

# ---------------------------------------------------------------------------
# Generate agent-os-package.meta.json with artifact sizes
# ---------------------------------------------------------------------------
generate-meta: $(META_MARKER)
$(META_MARKER): $(COPY_MARKER)
	@echo "=== Generating agent-os-package.meta.json ==="
	@for pkg in $(CMD_PACKAGES); do \
		wasmdir="software/$$pkg/wasm"; \
		metafile="software/$$pkg/agent-os-package.meta.json"; \
		if [ ! -d "$$wasmdir" ] || [ -z "$$(ls -A $$wasmdir 2>/dev/null)" ]; then \
			continue; \
		fi; \
		total_size=0; \
		total_gzip=0; \
		artifacts=""; \
		for f in $$wasmdir/*; do \
			name=$$(basename "$$f"); \
			raw=$$(wc -c < "$$f"); \
			gz=$$(gzip -c "$$f" | wc -c); \
			total_size=$$((total_size + raw)); \
			total_gzip=$$((total_gzip + gz)); \
			if [ -n "$$artifacts" ]; then artifacts="$$artifacts,"; fi; \
			artifacts="$${artifacts}\"$$name\":{\"size\":$$raw,\"sizeGzip\":$$gz}"; \
		done; \
		echo "{\"artifacts\":{$$artifacts},\"totalSize\":$$total_size,\"totalSizeGzip\":$$total_gzip}" \
			| node -e "process.stdout.write(JSON.stringify(JSON.parse(require('fs').readFileSync('/dev/stdin','utf8')),null,2)+'\n')" \
			> "$$metafile"; \
		echo "  META: $$metafile"; \
	done
	@touch $(META_MARKER)

# ---------------------------------------------------------------------------
# Build TypeScript
# ---------------------------------------------------------------------------
build: $(TS_MARKER)
$(TS_MARKER): $(META_MARKER)
	pnpm install
	pnpm -r run build
	@touch $(TS_MARKER)

# ---------------------------------------------------------------------------
# Run tests
# ---------------------------------------------------------------------------
test: build
	pnpm -r run test

# ---------------------------------------------------------------------------
# Publish (builds everything first, skips empty and unchanged packages)
# ---------------------------------------------------------------------------
publish: build
	$(eval VERSION := 0.0.$(shell date -u +%y%m%d%H%M%S))
	@echo "=== Publishing with version $(VERSION) ==="
	@for pkg in $(ALL_PACKAGES); do \
		node -e " \
			const fs = require('fs'); \
			const p = JSON.parse(fs.readFileSync('software/$$pkg/package.json', 'utf8')); \
			p.version = '$(VERSION)'; \
			fs.writeFileSync('software/$$pkg/package.json', JSON.stringify(p, null, 2) + '\n'); \
		"; \
	done
	@for pkg in $(CMD_PACKAGES) $(META_PACKAGES); do \
		if [ -d "software/$$pkg/wasm" ] && [ -z "$$(ls -A software/$$pkg/wasm 2>/dev/null)" ]; then \
			echo "SKIP: @rivet-dev/agent-os-$$pkg (empty wasm/, binary not built)"; \
			continue; \
		fi; \
		HASH=$$(find software/$$pkg/src software/$$pkg/wasm -type f 2>/dev/null | sort | xargs sha256sum 2>/dev/null | sha256sum | cut -d' ' -f1); \
		LAST=$$(cat software/$$pkg/.last-publish-hash 2>/dev/null || echo ""); \
		if [ "$$HASH" = "$$LAST" ]; then \
			echo "SKIP: @rivet-dev/agent-os-$$pkg (unchanged)"; \
		else \
			echo "PUBLISH: @rivet-dev/agent-os-$$pkg@$(VERSION)"; \
			(cd software/$$pkg && pnpm publish --access public --no-git-checks); \
			echo "$$HASH" > software/$$pkg/.last-publish-hash; \
		fi; \
	done
	@echo "=== Publish complete ==="

publish-dry: build
	@for pkg in $(CMD_PACKAGES) $(META_PACKAGES); do \
		if [ -d "software/$$pkg/wasm" ] && [ -z "$$(ls -A software/$$pkg/wasm 2>/dev/null)" ]; then \
			echo "SKIP: @rivet-dev/agent-os-$$pkg (empty wasm/)"; \
			continue; \
		fi; \
		echo "=== Dry run: @rivet-dev/agent-os-$$pkg ==="; \
		(cd software/$$pkg && pnpm publish --dry-run --no-git-checks) || true; \
	done

publish-force: build
	$(eval VERSION := 0.0.$(shell date -u +%y%m%d%H%M%S))
	@echo "=== Force publishing all with version $(VERSION) ==="
	@for pkg in $(ALL_PACKAGES); do \
		node -e " \
			const fs = require('fs'); \
			const p = JSON.parse(fs.readFileSync('software/$$pkg/package.json', 'utf8')); \
			p.version = '$(VERSION)'; \
			fs.writeFileSync('software/$$pkg/package.json', JSON.stringify(p, null, 2) + '\n'); \
		"; \
	done
	@for pkg in $(CMD_PACKAGES) $(META_PACKAGES); do \
		if [ -d "software/$$pkg/wasm" ] && [ -z "$$(ls -A software/$$pkg/wasm 2>/dev/null)" ]; then \
			echo "SKIP: @rivet-dev/agent-os-$$pkg (empty wasm/)"; \
			continue; \
		fi; \
		echo "PUBLISH: @rivet-dev/agent-os-$$pkg@$(VERSION)"; \
		(cd software/$$pkg && pnpm publish --access public --no-git-checks); \
	done

publish-clean:
	@rm -f software/*/.last-publish-hash
	@echo "Publish cache cleared"

# ---------------------------------------------------------------------------
# Rebuild: clear markers to force full rebuild
# ---------------------------------------------------------------------------
rebuild:
	@rm -f $(MARKER_DIR)/*
	@echo "Build markers cleared. Next build will rebuild everything."

# ---------------------------------------------------------------------------
# Clean
# ---------------------------------------------------------------------------
clean:
	@rm -rf software/*/dist
	@rm -rf software/*/wasm
	@rm -f software/*/agent-os-package.meta.json
	@rm -rf $(MARKER_DIR)
	@echo "Cleaned dist/, wasm/, meta, and build markers"

clean-native:
	@rm -rf $(NATIVE_DIR)/target
	@rm -rf $(NATIVE_DIR)/vendor
	@rm -rf $(NATIVE_DIR)/c/build $(NATIVE_DIR)/c/vendor $(NATIVE_DIR)/c/libs $(NATIVE_DIR)/c/.cache
	@rm -rf $(NATIVE_DIR)/go/vendor
	@rm -rf $(MARKER_DIR)
	@echo "Cleaned native build artifacts"

clean-all: clean clean-native
