SHELL := /bin/bash

XFSTESTS_URL := https://git.kernel.org/pub/scm/fs/xfs/xfstests-dev.git
PIN := $(shell tr -d '[:space:]' < xfstests.pin)
CACHE := $(CURDIR)/.cache/xfstests.git
WORK := $(CURDIR)/.work/xfstests
REPORT := $(CURDIR)/report
V8_BRIDGE_STAGE := $(CURDIR)/.cache/v8-bridge
PATCHES := $(sort $(wildcard $(CURDIR)/patches/*.patch))
TOOLCHAIN := $(CURDIR)/../../toolchain
NATIVE_C := $(TOOLCHAIN)/c
TOOLCHAIN_COMMANDS := $(TOOLCHAIN)/target/wasm32-wasip1/release/commands
WASI_CC := $(NATIVE_C)/vendor/wasi-sdk/bin/clang
WASI_SYSROOT := $(NATIVE_C)/sysroot
XFSTESTS_C_HELPERS := af_unix append_reader append_writer attr_replace_test devzero dirstress fill fs_perms locktest looptest \
	feature listxattr lstat64 nametest permname \
	multi_open_unlink preallo_rw_pattern_reader preallo_rw_pattern_writer pwrite_mmap_blocked \
	readdir-while-renames rename renameat2 rewinddir-test runas seek_copy_test \
	t_access_root t_attr_corruption t_dir_type t_futimens t_getcwd t_rename_overwrite t_truncate_cmtime \
	t_mmap_writev testx trunc truncate truncfile unlink-fsync \
	writev_on_pagefault
XFSTESTS_HELPER_INCLUDES := -D_GNU_SOURCE -I$(CURDIR)/runner/include -I$(WORK)/src \
	-I$(NATIVE_C)/include -include agentos_helper_compat.h
XFSTESTS_HELPER_SUPPORT := $(CURDIR)/runner/agentos_helper_support.c
CARGO_TARGET_DIR ?= $(CURDIR)/.cache/cargo-target
CARGO_BUILD_JOBS ?= 1
XFSTESTS_CONCURRENCY ?= 2
# `object_s3` remains in the harness for focused return-to-service work, but it is
# deliberately absent from the supported default matrix while its whole-object
# write-back, durability, and request-amplification contract is incomplete.
# XFSTESTS_BACKENDS ?= chunked_local,memory,chunked_s3,object_s3
XFSTESTS_BACKENDS ?= chunked_local,memory,chunked_s3
XFSTESTS_WASM_BACKENDS ?= v8 wasmtime
XFSTESTS_MAX_TEMP_BYTES ?= 8589934592
XFSTESTS_TEST_TIMEOUT_SECONDS ?= 3600

.PHONY: run native-commands c-probes helpers stage check clean

run: helpers
	mkdir -p "$(REPORT)"
	mkdir -p "$(CURDIR)/.cache/run-roots" && \
	run_tmp="$$(mktemp -d "$(CURDIR)/.cache/run-roots/agentos-xfstests-run.XXXXXX")" && \
	trap 'rm -rf -- "$$run_tmp"' EXIT && \
	cp -a "$(WORK)" "$$run_tmp/source" && \
	cd "$(CURDIR)/../.." && \
	(pnpm install --frozen-lockfile --offline || pnpm install --frozen-lockfile) && \
	node packages/build-tools/scripts/build-v8-bridge.mjs --out-dir "$(V8_BRIDGE_STAGE)" && \
	export XFSTESTS_ROOT="$$run_tmp/source" && \
	export XFSTESTS_EXCEPTIONS="$(CURDIR)/exceptions.toml" && \
	export XFSTESTS_CONCURRENCY="$(XFSTESTS_CONCURRENCY)" && \
	export XFSTESTS_BACKENDS="$(XFSTESTS_BACKENDS)" && \
	export XFSTESTS_MAX_TEMP_BYTES="$(XFSTESTS_MAX_TEMP_BYTES)" && \
	export XFSTESTS_TEST_TIMEOUT_SECONDS="$(XFSTESTS_TEST_TIMEOUT_SECONDS)" && \
	export XFSTESTS_TEMP_ROOT="$$run_tmp" && \
	export CARGO_TARGET_DIR="$(CARGO_TARGET_DIR)" && \
	export CARGO_BUILD_JOBS="$(CARGO_BUILD_JOBS)" && \
	export AGENTOS_V8_BRIDGE_PREBUILT_DIR="$(V8_BRIDGE_STAGE)" && \
	for wasm_backend in $(XFSTESTS_WASM_BACKENDS); do \
		case "$$wasm_backend" in v8|wasmtime) ;; *) echo "unsupported XFSTESTS_WASM_BACKENDS entry: $$wasm_backend" >&2; exit 2;; esac; \
		export AGENTOS_TEST_WASM_BACKEND="$$wasm_backend"; \
		export XFSTESTS_REPORT_DIR="$(REPORT)/$$wasm_backend"; \
		mkdir -p "$$XFSTESTS_REPORT_DIR"; \
		cargo test --release --manifest-path "$(CURDIR)/../../Cargo.toml" \
				-p agentos-native-sidecar --test xfstests_correctness \
				xfstests_wasi_helper_ports -- --ignored --exact --nocapture && \
		cargo test --release --manifest-path "$(CURDIR)/../../Cargo.toml" \
				-p agentos-native-sidecar --test xfstests_correctness \
				xfstests_generic_quick_matrix -- --ignored --exact --nocapture || exit $$?; \
	done

native-commands:
	$(MAKE) -C "$(TOOLCHAIN)" wasm
	$(MAKE) -C "$(TOOLCHAIN)" cmd/grep

c-probes:
	$(MAKE) -C "$(NATIVE_C)" sysroot
	rm -f "$(NATIVE_C)/build/credentials_test" "$(NATIVE_C)/build/credentials_test.wasm" \
		"$(NATIVE_C)/build/xattr_test" "$(NATIVE_C)/build/xattr_test.wasm" \
		"$(NATIVE_C)/build/xattr_tools" "$(NATIVE_C)/build/xattr_tools.wasm" \
		"$(NATIVE_C)/build/acl_tools" "$(NATIVE_C)/build/acl_tools.wasm" \
		"$(NATIVE_C)/build/xfs_io" "$(NATIVE_C)/build/xfs_io.wasm" \
		"$(NATIVE_C)/build/mknod" "$(NATIVE_C)/build/mknod.wasm" \
		"$(NATIVE_C)/build/fifo_test" "$(NATIVE_C)/build/fifo_test.wasm" \
		"$(NATIVE_C)/build/mmap_test" "$(NATIVE_C)/build/mmap_test.wasm" \
		"$(NATIVE_C)/build/openat_test" "$(NATIVE_C)/build/openat_test.wasm" \
		"$(NATIVE_C)/build/pwritev_test" "$(NATIVE_C)/build/pwritev_test.wasm" \
		"$(NATIVE_C)/build/sync_test" "$(NATIVE_C)/build/sync_test.wasm" \
		"$(NATIVE_C)/build/flock_test" "$(NATIVE_C)/build/flock_test.wasm" \
		"$(NATIVE_C)/build/signal_tests" "$(NATIVE_C)/build/signal_tests.wasm" \
		"$(NATIVE_C)/build/waitpid_status" "$(NATIVE_C)/build/waitpid_status.wasm" \
		"$(NATIVE_C)/build/self_stop_status" "$(NATIVE_C)/build/self_stop_status.wasm"
	$(MAKE) -C "$(NATIVE_C)" build/credentials_test build/xattr_test build/xattr_tools build/acl_tools build/xfs_io build/mknod build/fifo_test build/mmap_test build/openat_test build/pwritev_test build/sync_test build/flock_test build/signal_tests build/waitpid_status build/self_stop_status
	chmod +x "$(NATIVE_C)"/build/{credentials_test,xattr_test,xattr_tools,acl_tools,xfs_io,mknod,fifo_test,mmap_test,openat_test,pwritev_test,sync_test,flock_test}
	mkdir -p "$(TOOLCHAIN_COMMANDS)"
	for spec in \
		attr:xattr_tools getfattr:xattr_tools setfattr:xattr_tools \
		chacl:acl_tools getfacl:acl_tools setfacl:acl_tools \
		xfs_io:xfs_io mknod:mknod mkfifo:mknod; do \
		command=$${spec%%:*}; source=$${spec##*:}; \
		cp "$(NATIVE_C)/build/$$source" "$(TOOLCHAIN_COMMANDS)/$$command"; \
		chmod +x "$(TOOLCHAIN_COMMANDS)/$$command"; \
	done

helpers: stage native-commands c-probes
	test -x "$(WASI_CC)"
	set -e; for helper in $(XFSTESTS_C_HELPERS); do \
		source="$(WORK)/src/$$helper.c"; \
		if [[ "$$helper" == feature ]]; then source="$(CURDIR)/runner/feature_wasi.c"; fi; \
		"$(WASI_CC)" --target=wasm32-wasip1 --sysroot="$(WASI_SYSROOT)" -O2 \
			$(XFSTESTS_HELPER_INCLUDES) \
			-o "$(WORK)/src/$$helper" "$$source" \
			"$(XFSTESTS_HELPER_SUPPORT)"; \
		chmod +x "$(WORK)/src/$$helper"; \
	done
	"$(WASI_CC)" --target=wasm32-wasip1 --sysroot="$(WASI_SYSROOT)" -O2 \
		$(XFSTESTS_HELPER_INCLUDES) \
		-o "$(WORK)/src/fssum" "$(WORK)/src/fssum.c" "$(WORK)/src/md5.c" \
		"$(XFSTESTS_HELPER_SUPPORT)"
	chmod +x "$(WORK)/src/fssum"
	printf '%s\n' $(XFSTESTS_C_HELPERS) fssum > "$(WORK)/agentos-built-helpers"
	rm -rf "$(WORK)/agentos-command-bin"
	rm -rf "$(WORK)/agentos-command-packages"
	mkdir -p "$(WORK)/agentos-command-bin" "$(WORK)/agentos-command-packages"
	for package in acl attr coreutils diffutils findutils gawk grep sed xfsprogs; do \
		manifest="$(CURDIR)/../../software/$$package/agentos-package.json"; \
		test -f "$$manifest"; \
		mkdir -p "$(WORK)/agentos-command-packages/$$package"; \
		for command in $$(node -e 'const m=require(process.argv[1]); console.log([...(m.commands||[]),...(m.stubs||[]),...Object.keys(m.aliases||{})].join(" "))' "$$manifest"); do \
			test -f "$(TOOLCHAIN_COMMANDS)/$$command" || { echo "missing staged command: $$package/$$command" >&2; exit 1; }; \
			cp "$(TOOLCHAIN_COMMANDS)/$$command" "$(WORK)/agentos-command-packages/$$package/$$command"; \
			chmod +x "$(WORK)/agentos-command-packages/$$package/$$command"; \
			ln -sf "../agentos-command-packages/$$package/$$command" \
				"$(WORK)/agentos-command-bin/$$command"; \
		done; \
	done
	printf '%s\n' pwrite pread truncate falloc fpunch fzero finsert fcollapse fiemap fadvise flink utimes fsync s fdatasync syncfs stat \
		> "$(WORK)/agentos-xfs-io-commands"
	printf '%s\n' xattrs acls > "$(WORK)/agentos-built-features"
	python3 "$(CURDIR)/runner/surface_audit.py" \
		--root "$(WORK)" --pin "$(PIN)" \
		--operation-evidence "$(CURDIR)/operation-evidence.toml" \
		--output "$(WORK)/agentos-surface-audit.md"

stage: check
	command -v git >/dev/null
	command -v python3 >/dev/null
	mkdir -p "$(dir $(CACHE))" "$(dir $(WORK))"
	if [[ ! -d "$(CACHE)" ]]; then \
		git clone --bare "$(XFSTESTS_URL)" "$(CACHE)"; \
	fi
	if ! git -C "$(CACHE)" cat-file -e "$(PIN)^{commit}" 2>/dev/null; then \
		git -C "$(CACHE)" fetch --depth=1 origin "$(PIN)"; \
	fi
	if [[ ! -d "$(WORK)/.git" ]]; then \
		git clone --no-checkout "$(CACHE)" "$(WORK)"; \
	fi
	git -C "$(WORK)" checkout --detach --force "$(PIN)"
	git -C "$(WORK)" clean -ffdqx
	set -e; for patch in $(PATCHES); do \
		git -C "$(WORK)" apply --check "$$patch"; \
		git -C "$(WORK)" apply "$$patch"; \
	done
	cd "$(WORK)/tests/generic" && "$(WORK)/tools/mkgroupfile" group.list
	cp "$(CURDIR)/local.config.in" "$(WORK)/local.config"
	python3 "$(CURDIR)/runner/prepare.py" \
		--exceptions "$(CURDIR)/exceptions.toml" \
		--exclude "$(CURDIR)/exclude.generic" \
		--json "$(WORK)/agentos-exceptions.json"
	test "$$(git -C "$(WORK)" rev-parse HEAD)" = "$(PIN)"

check:
	test "$$(printf '%s' "$(PIN)" | wc -c)" -eq 40
	test "$$(find "$(CURDIR)/patches" -maxdepth 1 -name '*.patch' | wc -l)" -eq 27
	python3 "$(CURDIR)/runner/prepare.py" \
		--exceptions "$(CURDIR)/exceptions.toml" \
		--exclude "$(CURDIR)/exclude.generic" --check
	cd "$(CURDIR)/runner" && python3 -m unittest test_prepare.py test_surface_audit.py

clean:
	rm -rf "$(CURDIR)/.work" "$(CURDIR)/report"
