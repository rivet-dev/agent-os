set positional-arguments := true

release *args:
	pnpm --filter=publish release "$@"

# Cut a release-preview (debug build, npm-only, branch dist-tag) — see the
# release-preview skill for the end-to-end flow.
release-preview REF:
	gh workflow run .github/workflows/publish.yaml --ref "{{ REF }}"

# --- @agentos-software/* software packages (independent, PER-PACKAGE versions) ---
toolchain-build:
	make -C toolchain commands
	make -C toolchain cmd/duckdb cmd/vim

toolchain-cmd name:
	make -C toolchain cmd/{{ name }}

# Pre-flight for the publish "WASM Commands" job's fragile state: build the C
# programs against the VANILLA wasi-sdk sysroot exactly like a fresh CI runner
# (a locally-built patched sysroot is moved aside for the run). Catches
# socket/netdb programs missing from PATCHED_PROGRAMS before CI does.
toolchain-preflight:
	#!/usr/bin/env bash
	set -euo pipefail
	cd toolchain/c
	if [ -e sysroot ]; then mv sysroot sysroot.preflight-stash; fi
	restore() { if [ -e sysroot.preflight-stash ]; then rm -rf sysroot; mv sysroot.preflight-stash sysroot; fi; }
	trap restore EXIT
	make wasi-sdk
	make programs

toolchain-copy-commands:
	node packages/runtime-core/scripts/copy-wasm-commands.mjs --require

toolchain-check-abi:
	node scripts/generate-wasm-abi-manifest.mjs

toolchain-audit-imports:
	just toolchain-check-abi
	node scripts/audit-wasm-imports.mjs

software-build:
	pnpm --filter @rivet-dev/agentos-toolchain build
	pnpm --filter '@agentos-software/*' build

# Rebuild and stage the complete default WASM tool set from source. All outputs
# land in ignored build/bin/commands directories and must not be committed.
tools-rebuild:
	just toolchain-build
	just toolchain-copy-commands
	just software-build
	just toolchain-audit-imports

install-shell:
	#!/usr/bin/env bash
	set -euo pipefail
	pnpm --filter @rivet-dev/agentos-shell build
	global_bin_dir="$(pnpm config get global-bin-dir)"
	if [[ -z "$global_bin_dir" || "$global_bin_dir" == "undefined" ]]; then
		global_bin_dir="${PNPM_HOME:-/tmp/pnpm}"
	fi
	mkdir -p "$global_bin_dir"
	for package in @rivet-dev/agentos-shell @rivet-dev/agent-os-shell @rivet-dev/agentos-workspace; do
		PATH="$global_bin_dir:$PATH" pnpm --global remove "$package" >/dev/null 2>&1 || true
	done
	(cd packages/shell && PATH="$global_bin_dir:$PATH" pnpm link --global)

install-gigacode:
	#!/usr/bin/env bash
	set -euo pipefail
	repo_root='{{justfile_directory()}}'
	pnpm --dir "$repo_root" install
	make -C "$repo_root/toolchain" wasm
	if [[ -n "${CODEX_REPO:-}" ]]; then
		make -C "$repo_root/toolchain" codex-required CODEX_REPO="$CODEX_REPO"
	else
		make -C "$repo_root/toolchain" codex-required
	fi
	if [[ -n "${AGENTOS_SIDECAR_BIN:-}" ]]; then
		export AGENTOS_SKIP_NATIVE_META_BUILD=1
	fi
	pnpm --dir "$repo_root" --filter '@rivet-dev/agentos-experiment-gigacode...' build
	pnpm --dir "$repo_root/experiments/gigacode" check-types
	pnpm --dir "$repo_root/experiments/gigacode" install-global
	"$HOME/.local/bin/gigacode" --version

shell *args:
	#!/usr/bin/env bash
	set -euo pipefail
	actor_mode=false
	for arg in "$@"; do
		if [[ "$arg" == "--actor" ]]; then
			actor_mode=true
		fi
	done
	if [[ ! -x packages/shell/node_modules/.bin/tsx \
		|| ! -e packages/shell/node_modules/@agentos-software/codex-cli \
		|| ! -d packages/build-tools/node_modules ]]; then
		pnpm install --force
	fi
	missing_registry_packages=()
	for package_json in packages/shell/node_modules/@agentos-software/*/package.json; do
		IFS=$'\t' read -r package_name package_main < <(node -e '
			const manifest = require(require("node:path").resolve(process.argv[1]));
			console.log(`${manifest.name}\t${manifest.main ?? ""}`);
		' "$package_json")
		package_dir="${package_json%/package.json}"
		if [[ -n "$package_main" && ( ! -e "$package_dir/${package_main#./}" \
			|| ! -e "$package_dir/dist/package.aospkg" ) ]]; then
			missing_registry_packages+=("$package_name")
		fi
	done
	if (( ${#missing_registry_packages[@]} > 0 )); then
		pnpm --filter @agentos-software/manifest build
		pnpm --filter @rivet-dev/agentos-toolchain build
		registry_filters=()
		for package_name in "${missing_registry_packages[@]}"; do
			registry_filters+=(--filter "$package_name")
		done
		pnpm "${registry_filters[@]}" build
	fi
	if [[ ! -e software/common/dist/index.js ]]; then
		pnpm --filter @agentos-software/common build
	fi
	if [[ ! -e packages/runtime-core/dist/index.js \
		|| ! -e packages/core/dist/index.js \
		|| ! -e packages/agentos/dist/index.js ]]; then
		pnpm --filter @rivet-dev/agentos-runtime-core build
		pnpm --filter @rivet-dev/agentos-core build
		pnpm --filter @rivet-dev/agentos build
	fi
	if [[ "$actor_mode" == true ]]; then
		r6_root="${AGENTOS_R6_ROOT:-$PWD/../r6}"
		rivetkit_loader="$r6_root/rivetkit-typescript/packages/rivetkit/node_modules/tsx/dist/loader.mjs"
		if [[ ! -e "$r6_root/pnpm-lock.yaml" ]]; then
			echo "just shell --actor requires the Rivet repo at $r6_root (override with AGENTOS_R6_ROOT)" >&2
			exit 1
		fi
		if [[ ! -e "$rivetkit_loader" ]]; then
			pnpm --dir "$r6_root" install --frozen-lockfile --filter 'rivetkit...'
		fi
		if [[ ! -e "$r6_root/shared/typescript/virtual-websocket/dist/mod.js" \
			|| ! -e "$r6_root/rivetkit-typescript/packages/traces/dist/tsup/index.js" \
			|| ! -e "$r6_root/rivetkit-typescript/packages/workflow-engine/dist/tsup/index.js" \
			|| ! -e "$r6_root/engine/sdks/typescript/envoy-protocol/dist/index.js" \
			|| ! -e "$r6_root/rivetkit-typescript/packages/rivetkit-wasm/pkg/rivetkit_wasm.js" ]]; then
			pnpm --dir "$r6_root" --filter 'rivetkit...' build
		fi
	fi
	CARGO_TARGET_DIR="$PWD/target" cargo build -p agentos-sidecar
	env \
		AGENTOS_SIDECAR_BIN="$PWD/target/debug/agentos-sidecar" \
		NODE_OPTIONS="--no-deprecation ${NODE_OPTIONS:-}" \
		pnpm --filter @rivet-dev/agentos-shell exec tsx src/main.ts "$@"

# --- agentos-sdk.dev docs site (landing + /docs) ---
# The site (packages under website/) depends on the private @rivet-dev/docs-theme
# and @rivet-gg/icons, which are NOT committed here. `dev-website-setup` vendors
# the theme from a sibling workspace, builds it, and links the site into the
# pnpm workspace. Then `dev-website` (or `dev-website-start`) serves it with hot
# reload. Building the icon set needs a Font Awesome Pro token exported as
# FONTAWESOME_PACKAGE_TOKEN (e.g. `source ~/misc/env.txt` first).

# Vendor + build the docs theme and link the site into the workspace (idempotent).
dev-website-setup:
	#!/usr/bin/env bash
	set -euo pipefail
	theme="website/vendor/theme"
	icons="$theme/vendor/icons"
	built=0

	# A symlink keeps Node's real module path in the source checkout, where the
	# workspace-installed build dependencies are not visible. Materialize it.
	if [ -L "$theme" ]; then
		src="$(readlink -f "$theme")"
		[ -f "$src/package.json" ] || { echo "error: docs-theme symlink target is invalid: $src" >&2; exit 1; }
		tmp="$(mktemp -d website/vendor/theme.XXXXXX)"
		cp -RL "$src/." "$tmp/"
		unlink "$theme"
		mv "$tmp" "$theme"
		echo "materialized docs-theme from $src"
	fi

	# 1. Vendor the private docs theme from a sibling workspace if absent.
	if [ ! -f "$theme/package.json" ]; then
		src=""
		for d in ../*/website/vendor/theme; do
			[ -f "$d/package.json" ] || continue
			v="$(node -p "require('$d/package.json').version" 2>/dev/null)" || continue
			case "$v" in *stub*) continue;; esac
			src="$d"; break
		done
		[ -n "$src" ] || { echo "error: no sibling docs-theme found under ../*/website/vendor/theme" >&2; exit 1; }
		echo "vendoring docs-theme from $src"
		mkdir -p website/vendor
		cp -R "$src" "$theme"
	fi

	# 2. Include the site + theme in the pnpm workspace (local-only; do not commit).
	if grep -qE '^[[:space:]]*# - website(/|$)' pnpm-workspace.yaml; then
		sed -i '/^[[:space:]]*# - website\(\/\|$\)/ s/# - /- /' pnpm-workspace.yaml
		echo "enabled website workspace globs in pnpm-workspace.yaml (local-only)"
	fi

	# 3. Install so workspace links + build deps (esbuild) exist.
	pnpm install --lockfile=false

	# 4. Build the theme's config-time modules (dist/) if missing.
	if [ ! -f "$theme/dist/mdx/remark.js" ]; then
		pnpm --filter @rivet-dev/docs-theme build
		built=1
	fi

	# 5. Build the icon set (dist/). Requires a Font Awesome Pro token.
	if [ ! -f "$icons/dist/index.js" ]; then
		if [ -z "${FONTAWESOME_PACKAGE_TOKEN:-}" ]; then
			echo "error: FONTAWESOME_PACKAGE_TOKEN is required to build @rivet-gg/icons." >&2
			echo "       export it (e.g. 'source ~/misc/env.txt') and re-run." >&2
			exit 1
		fi
		pnpm --filter @rivet-gg/icons generate
		built=1
	fi

	# 6. Re-sync the pnpm store with freshly built dist/ (file: deps are copied in).
	if [ "$built" = 1 ]; then
		pnpm install --lockfile=false
	fi

	# A workspace-wide install can leave website/node_modules partially linked
	# after generated theme packages are rebuilt. Refresh the site package last
	# so Astro renderers and the newly generated icon files resolve immediately.
	pnpm --dir website install --lockfile=false

	echo "dev-website-setup: ready"

# Start the docs site dev server with hot reload (run dev-website-setup first).
dev-website-start:
	pnpm --filter @rivet-dev/agentos-website dev

# Set up (if needed) and start the docs site dev server.
dev-website: dev-website-setup dev-website-start

# Set up (if needed) and build the agentos-sdk.dev site to website/dist.
dev-website-build: dev-website-setup
	pnpm --filter @rivet-dev/agentos-website build

# Run the agentos-sdk.dev site (landing + /docs) locally with hot reload
docs:
	pnpm --filter @rivet-dev/agentos-website dev

# Build the agentos-sdk.dev site to website/dist
docs-build:
	pnpm --filter @rivet-dev/agentos-website build

# Build and crawl the generated site for broken routes, anchors, and assets.
# Pass `true` to also check external URLs. Crawling the rendered output checks
# Astro's actual routing behavior instead of guessing routes from MDX paths.
docs-check-links external='false': docs-build
	#!/usr/bin/env bash
	set -euo pipefail
	command -v docker >/dev/null || {
		echo "error: docker is required to run the docs link checker" >&2
		exit 1
	}
	repo_root='{{justfile_directory()}}'
	external='{{ external }}'
	case "$external" in
		false) network_args=(--offline) ;;
		true) network_args=() ;;
		*)
			echo "error: external must be 'true' or 'false'" >&2
			exit 1
			;;
	esac
	docker_env=()
	if [[ "$external" == true && -n "${GITHUB_TOKEN:-}" ]]; then
		docker_env=(-e GITHUB_TOKEN)
	elif [[ "$external" == true && -n "${GH_TOKEN:-}" ]]; then
		export GITHUB_TOKEN="$GH_TOKEN"
		docker_env=(-e GITHUB_TOKEN)
	elif [[ "$external" == true ]] \
		&& command -v gh >/dev/null \
		&& GITHUB_TOKEN="$(gh auth token 2>/dev/null)"; then
		export GITHUB_TOKEN
		docker_env=(-e GITHUB_TOKEN)
	fi
	docker run --rm \
		"${docker_env[@]}" \
		-v "$repo_root/website/dist:/site:ro" \
		lycheeverse/lychee:0.24.2 \
		--root-dir /site \
		--index-files index.html \
		--include-fragments=anchor-only \
		--exclude-all-private \
		--max-concurrency 32 \
		--host-concurrency 2 \
		--host-request-interval 200ms \
		--max-retries 1 \
		--timeout 20 \
		--no-progress \
		"${network_args[@]}" \
		/site

test-bounded cmd='pnpm test':
	#!/usr/bin/env bash
	set -euo pipefail

	repo_root='{{justfile_directory()}}'
	cmd="${1:-pnpm test}"
	avail_kb="$(awk '/MemAvailable/ {print $2}' /proc/meminfo)"
	cpus="$(nproc --all)"

	if [[ -z "$avail_kb" || -z "$cpus" ]]; then
		echo "Could not determine CPU or memory budget." >&2
		exit 1
	fi

	mem_max_kb=$((avail_kb * 60 / 100))
	mem_high_kb=$((mem_max_kb * 85 / 100))
	cpu_quota="$((cpus * 60))%"

	printf 'Running with CPUQuota=%s MemoryHigh=%sK MemoryMax=%sK\n' \
		"$cpu_quota" "$mem_high_kb" "$mem_max_kb"

	# Resource limits are scoped to the whole transient unit, so test runners and
	# every child process they spawn share the same CPU, memory, IO, and task caps.
	#
	# MemoryHigh starts reclaim/throttling before the hard MemoryMax. MemoryMax is
	# based on currently available memory, not total memory, to avoid host pressure.
	# CPUQuota limits aggregate CPU to 60% of logical cores; CPUWeight and Nice make
	# other work win contention. IOWeight and idle IO scheduling keep large test
	# output/builds from making the host sticky. OOMScoreAdjust makes this bounded
	# run a preferred kill target under pressure, and TasksMax prevents runaway
	# process fan-out.
	exec systemd-run --user --wait --collect --pipe \
		-p MemoryAccounting=yes \
		-p MemoryHigh="${mem_high_kb}K" \
		-p MemoryMax="${mem_max_kb}K" \
		-p MemorySwapMax=0 \
		-p CPUAccounting=yes \
		-p CPUQuota="$cpu_quota" \
		-p CPUWeight=20 \
		-p Nice=10 \
		-p IOWeight=20 \
		-p IOSchedulingClass=idle \
		-p OOMScoreAdjust=500 \
		-p TasksMax=512 \
		bash -lc 'cd "$1" && exec bash -lc "$2"' bounded-test "$repo_root" "$cmd"

test-risky-probe *tests:
	./.agent/scripts/run-risky-test-probe.sh "$@"
