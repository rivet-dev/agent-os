SHELL := /bin/bash

# Output directory (shared with Rust/C builds)
COMMANDS_DIR ?= ../target/wasm32-wasip1/release/commands

# Vendor directory for upstream Go sources
VENDOR_DIR := vendor

# --- slack-cli ---
SLACK_CLI_REPO    := https://github.com/slackapi/slack-cli.git
SLACK_CLI_COMMIT  := 24c1d2c50b7f438700596c4d1902e0838a42fab0
SLACK_CLI_DIR     := $(VENDOR_DIR)/slack-cli
SLACK_CLI_PATCHES := $(sort $(wildcard patches/slack-cli/*.patch))

.PHONY: wasm clean install

# ---------------------------------------------------------------------------
# Build all Go WASM commands
# ---------------------------------------------------------------------------
wasm: $(COMMANDS_DIR)/slack

# ---------------------------------------------------------------------------
# slack-cli
# ---------------------------------------------------------------------------

# Fetch upstream source at pinned commit
$(SLACK_CLI_DIR)/go.mod:
	@echo "=== Fetching slack-cli ==="
	@mkdir -p $(VENDOR_DIR)
	git clone $(SLACK_CLI_REPO) $(SLACK_CLI_DIR)
	git -C $(SLACK_CLI_DIR) checkout $(SLACK_CLI_COMMIT)

# Vendor Go dependencies
$(SLACK_CLI_DIR)/vendor/modules.txt: $(SLACK_CLI_DIR)/go.mod
	@echo "=== Vendoring slack-cli dependencies ==="
	cd $(SLACK_CLI_DIR) && go mod vendor

# Apply WASI patches to vendored dependencies
.PHONY: patch-slack-cli
patch-slack-cli: $(SLACK_CLI_DIR)/vendor/modules.txt
	@echo "=== Applying WASI patches to slack-cli ==="
	@for patch in $(SLACK_CLI_PATCHES); do \
		echo "  PATCH: $$patch"; \
		patch --forward -p1 -d $(SLACK_CLI_DIR) < "$$patch" || \
			echo "  (already applied)"; \
	done

# Build WASM binary
$(COMMANDS_DIR)/slack: $(SLACK_CLI_DIR)/vendor/modules.txt $(SLACK_CLI_PATCHES) | patch-slack-cli
	@echo "=== Building slack-cli WASM ==="
	@mkdir -p $(COMMANDS_DIR)
	cd $(SLACK_CLI_DIR) && GOOS=wasip1 GOARCH=wasm go build -mod=vendor -o $(abspath $@) .
	@echo "  Built: $@ ($$(du -h $@ | cut -f1))"

# ---------------------------------------------------------------------------
# Install (copy to COMMANDS_DIR, for parity with C Makefile)
# ---------------------------------------------------------------------------
install:
	@echo "Go binaries already output directly to COMMANDS_DIR"

# ---------------------------------------------------------------------------
# Clean
# ---------------------------------------------------------------------------
clean:
	rm -rf $(VENDOR_DIR)
	rm -f $(COMMANDS_DIR)/slack
