#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${ROOT_DIR}"

if [[ -d /workspace/.cargo && -d /workspace/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin ]]; then
	export CARGO_HOME=/workspace/.cargo
	export RUSTUP_HOME=/workspace/.rustup
	export PATH="/workspace/.cargo/bin:${PATH}"
	export RUSTC=/workspace/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin/rustc
	export RUSTDOC=/workspace/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin/rustdoc
fi

run_step() {
	echo
	echo "==> $*"
	"$@"
}

if [[ "${CI_FORK_PULL_REQUEST:-0}" == "1" ]]; then
	NETWORK_ENV=()
else
	NETWORK_ENV=("AGENTOS_E2E_NETWORK=1")
fi

run_step pnpm install --frozen-lockfile
run_step pnpm build
run_step cargo fmt --check
run_step cargo clippy --workspace --all-targets -- -D warnings
run_step cargo test -p agent-os-v8-runtime -- --test-threads=1
run_step cargo test -p agent-os-v8-runtime snapshot::tests::snapshot_consolidated_tests -- --exact --ignored
run_step cargo test -p agent-os-execution -- --test-threads=1
run_step cargo test -p agent-os-sidecar -- --test-threads=1
run_step cargo test -p agent-os-kernel -- --test-threads=1
run_step cargo test -p agent-os-bridge -- --test-threads=1
run_step pnpm check-types
run_step pnpm lint

echo
if [[ ${#NETWORK_ENV[@]} -gt 0 ]]; then
	echo "==> AGENTOS_E2E_NETWORK=1 pnpm test"
	env "${NETWORK_ENV[@]}" pnpm test
else
	echo "==> pnpm test"
	pnpm test
fi
