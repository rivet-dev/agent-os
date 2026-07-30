#!/usr/bin/env bash
#
# Run agentOS runtime benchmarks against an optimized sidecar, saving JSON and
# logs to packages/benchmarks/results/.

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"
cd "$REPO_ROOT"

RESULTS_DIR="$HERE/results"
mkdir -p "$RESULTS_DIR"
STAMP="$(date +%Y%m%d-%H%M%S)"

if [[ -n "${BENCH_FAMILIES:-}" && -z "${BENCH_ONLY:-}" ]]; then
	BENCH_ONLY="matrix"
else
	BENCH_ONLY="${BENCH_ONLY:-}"
fi

should_run() {
	local name="$1"
	[[ -z "$BENCH_ONLY" || "$BENCH_ONLY" == "$name" ]]
}

echo "=== Building benchmark TypeScript dependencies ===" >&2
pnpm --dir packages/core build >&2

echo "=== Building release sidecar ===" >&2
cargo build --release -p agentos-sidecar >&2
if [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
	export AGENTOS_SIDECAR_BIN="${AGENTOS_SIDECAR_BIN:-$CARGO_TARGET_DIR/release/agentos-sidecar}"
else
	export AGENTOS_SIDECAR_BIN="${AGENTOS_SIDECAR_BIN:-$REPO_ROOT/target/release/agentos-sidecar}"
fi
echo "Using sidecar: $AGENTOS_SIDECAR_BIN" >&2

build_native_baseline() {
	echo "" >&2
	echo "=== Building native-baseline ===" >&2
	cargo build --release -p agentos-benchmark-baseline >&2
	if [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
		export NATIVE_BASELINE_BIN="${NATIVE_BASELINE_BIN:-$CARGO_TARGET_DIR/release/agentos-benchmark-baseline}"
	else
		export NATIVE_BASELINE_BIN="${NATIVE_BASELINE_BIN:-$REPO_ROOT/target/release/agentos-benchmark-baseline}"
	fi
	echo "Using native baseline: $NATIVE_BASELINE_BIN" >&2
}

build_native_baseline_wasm() {
	if command -v rustup >/dev/null 2>&1 && ! rustup target list --installed | grep -qx "wasm32-wasip1"; then
		echo "=== Skipping vm-wasm lane: Rust target wasm32-wasip1 is not installed ===" >&2
		echo "Install it with: rustup target add wasm32-wasip1" >&2
		export NATIVE_BASELINE_WASM=""
		return
	fi
	echo "" >&2
	echo "=== Building native-baseline wasm32-wasip1 ===" >&2
	cargo build --release --target wasm32-wasip1 -p agentos-benchmark-baseline >&2
	if [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
		export NATIVE_BASELINE_WASM="${NATIVE_BASELINE_WASM:-$CARGO_TARGET_DIR/wasm32-wasip1/release/agentos-benchmark-baseline.wasm}"
	else
		export NATIVE_BASELINE_WASM="${NATIVE_BASELINE_WASM:-$REPO_ROOT/target/wasm32-wasip1/release/agentos-benchmark-baseline.wasm}"
	fi
	echo "Using wasm native baseline: $NATIVE_BASELINE_WASM" >&2
}

run_tsx() {
	local name="$1"
	shift
	if ! should_run "$name"; then
		echo "=== Skipping $name (BENCH_ONLY=$BENCH_ONLY) ===" >&2
		return
	fi
	echo "" >&2
	echo "=== Running $name ===" >&2
	pnpm --dir packages/benchmarks exec tsx "$@" \
		1> "$RESULTS_DIR/${name}-$STAMP.json" \
		2> >(tee "$RESULTS_DIR/${name}-$STAMP.log" >&2)
}

run_node() {
	local name="$1"
	shift
	if ! should_run "$name"; then
		echo "=== Skipping $name (BENCH_ONLY=$BENCH_ONLY) ===" >&2
		return
	fi
	echo "" >&2
	echo "=== Running $name ===" >&2
	pnpm --dir packages/benchmarks exec node "$@" \
		1> "$RESULTS_DIR/${name}-$STAMP.json" \
		2> >(tee "$RESULTS_DIR/${name}-$STAMP.log" >&2)
}

run_tsx "coldstart" "$HERE/coldstart.bench.ts"
run_node "memory" --expose-gc --import tsx/esm "$HERE/memory.bench.ts"

run_tsx "echo-cold-warm" "$HERE/src/focused/echo.bench.ts"

run_tsx "ls-serial" \
	"$HERE/src/focused/ls.bench.ts" \
	--iterations="${BENCH_LS_ITERATIONS:-5}" \
	--warmup="${BENCH_LS_WARMUP:-1}" \
	--serial-runs="${BENCH_LS_SERIAL_RUNS:-5}" \
	--file-counts="${BENCH_LS_FILE_COUNTS:-0,100}" \
	${BENCH_LS_WASM_WARMUP_DEBUG:+--wasm-warmup-debug}

run_tsx "wasi-ls-scaling" \
	"$HERE/src/focused/wasi-ls-scaling.bench.ts" \
	--iterations="${BENCH_WASI_LS_ITERATIONS:-5}" \
	--warmup="${BENCH_WASI_LS_WARMUP:-1}" \
	--serial-runs="${BENCH_WASI_LS_SERIAL_RUNS:-3}" \
	--file-counts="${BENCH_WASI_LS_FILE_COUNTS:-0,1,32,100,1000}" \
	--ls-variants="${BENCH_WASI_LS_VARIANTS:-one}" \
	${BENCH_WASI_LS_WASM_WARMUP_DEBUG:+--wasm-warmup-debug} \
	${BENCH_WASI_LS_SYSCALL_COUNTERS:+--wasi-syscall-counters}

run_tsx "wasi-ls-scaling-counters" \
	"$HERE/src/focused/wasi-ls-scaling.bench.ts" \
	--iterations="${BENCH_WASI_LS_COUNTER_ITERATIONS:-3}" \
	--warmup="${BENCH_WASI_LS_COUNTER_WARMUP:-1}" \
	--serial-runs="${BENCH_WASI_LS_COUNTER_SERIAL_RUNS:-3}" \
	--file-counts="${BENCH_WASI_LS_COUNTER_FILE_COUNTS:-0,100,1000}" \
	--ls-variants="${BENCH_WASI_LS_COUNTER_VARIANTS:-one,fast-no-decor}" \
	--wasi-syscall-counters \
	${BENCH_WASI_LS_COUNTER_WASM_WARMUP_DEBUG:+--wasm-warmup-debug}

run_tsx "readdir-scaling" \
	"$HERE/src/focused/readdir.bench.ts" \
	--iterations="${BENCH_READDIR_ITERATIONS:-10}" \
	--warmup="${BENCH_READDIR_WARMUP:-2}" \
	--entry-counts="${BENCH_READDIR_ENTRY_COUNTS:-0,1,32,100,1000}" \
	--modes="${BENCH_READDIR_MODES:-plain,withFileTypes}" \
	--fixtures="${BENCH_READDIR_FIXTURES:-vm-shadow}" \
	--workloads="${BENCH_READDIR_WORKLOADS:-pure}" \
	${BENCH_READDIR_PREFLIGHT_OPS:+--preflight-ops="${BENCH_READDIR_PREFLIGHT_OPS}"} \
	${BENCH_READDIR_PREFLIGHT_COUNTS:+--preflight-counts="${BENCH_READDIR_PREFLIGHT_COUNTS}"} \
	${BENCH_READDIR_INCLUDE_READDIR:+--include-readdir="${BENCH_READDIR_INCLUDE_READDIR}"} \
	${BENCH_READDIR_PROBE_TARGETS:+--probe-targets="${BENCH_READDIR_PROBE_TARGETS}"}

run_tsx "readdir-probe" \
	"$HERE/src/focused/readdir.bench.ts" \
	--iterations="${BENCH_READDIR_PROBE_ITERATIONS:-5}" \
	--warmup="${BENCH_READDIR_PROBE_WARMUP:-1}" \
	--entry-counts="${BENCH_READDIR_PROBE_ENTRY_COUNTS:-32}" \
	--modes="${BENCH_READDIR_PROBE_MODES:-plain}" \
	--fixtures="${BENCH_READDIR_PROBE_FIXTURES:-vm-shadow,native-host-dir}" \
	--workloads=probe \
	--preflight-ops="${BENCH_READDIR_PROBE_PREFLIGHT_OPS:-none,existsSync,statSync}" \
	--preflight-counts="${BENCH_READDIR_PROBE_PREFLIGHT_COUNTS:-0,32,33}" \
	--include-readdir="${BENCH_READDIR_PROBE_INCLUDE_READDIR:-both}" \
	--probe-targets="${BENCH_READDIR_PROBE_TARGETS:-dir-plus-children}"

run_tsx "fs-sync-ops" \
	"$HERE/src/focused/fs-sync-ops.bench.ts" \
	--iterations="${BENCH_FS_SYNC_ITERATIONS:-10}" \
	--warmup="${BENCH_FS_SYNC_WARMUP:-2}" \
	--ops="${BENCH_FS_SYNC_OPS:-existsSync,statSync,openClose,mkdirRmdir,smallWrite,readFileSync,renameFile}" \
	--call-counts="${BENCH_FS_SYNC_CALL_COUNTS:-1,8,32}" \
	--fixtures="${BENCH_FS_SYNC_FIXTURES:-vm-shadow}" \
	--payload-bytes="${BENCH_FS_SYNC_PAYLOAD_BYTES:-8}" \
	${BENCH_FS_SYNC_RPC_LATENCY:+--sync-rpc-latency} \
	${BENCH_FS_SYNC_PHASES:+--fs-sync-phases}

run_tsx "fs-sync-ops-phases" \
	"$HERE/src/focused/fs-sync-ops.bench.ts" \
	--iterations="${BENCH_FS_SYNC_PHASE_ITERATIONS:-5}" \
	--warmup="${BENCH_FS_SYNC_PHASE_WARMUP:-1}" \
	--ops="${BENCH_FS_SYNC_PHASE_OPS:-existsSync,statSync,readFileSync}" \
	--call-counts="${BENCH_FS_SYNC_PHASE_CALL_COUNTS:-8,32}" \
	--fixtures="${BENCH_FS_SYNC_PHASE_FIXTURES:-vm-shadow}" \
	--payload-bytes="${BENCH_FS_SYNC_PHASE_PAYLOAD_BYTES:-8}" \
	--sync-rpc-latency \
	--fs-sync-phases

run_tsx "sync-bridge-floor" \
	"$HERE/src/focused/sync-bridge-floor.bench.ts" \
	--iterations="${BENCH_SYNC_BRIDGE_ITERATIONS:-10}" \
	--warmup="${BENCH_SYNC_BRIDGE_WARMUP:-2}" \
	--call-counts="${BENCH_SYNC_BRIDGE_CALL_COUNTS:-1,8,32}" \
	--payload-bytes="${BENCH_SYNC_BRIDGE_PAYLOAD_BYTES:-0}" \
	${BENCH_SYNC_BRIDGE_RPC_LATENCY:+--sync-rpc-latency} \
	${BENCH_SYNC_BRIDGE_PHASES:+--bridge-phases}

run_tsx "sync-bridge-floor-phases" \
	"$HERE/src/focused/sync-bridge-floor.bench.ts" \
	--iterations="${BENCH_SYNC_BRIDGE_PHASE_ITERATIONS:-5}" \
	--warmup="${BENCH_SYNC_BRIDGE_PHASE_WARMUP:-1}" \
	--call-counts="${BENCH_SYNC_BRIDGE_PHASE_CALL_COUNTS:-1,8,32}" \
	--payload-bytes="${BENCH_SYNC_BRIDGE_PHASE_PAYLOAD_BYTES:-0}" \
	--sync-rpc-latency \
	--bridge-phases

run_tsx "sync-bridge-floor-bigargs" \
	"$HERE/src/focused/sync-bridge-floor.bench.ts" \
	--iterations="${BENCH_SYNC_BRIDGE_BIGARGS_ITERATIONS:-10}" \
	--warmup="${BENCH_SYNC_BRIDGE_BIGARGS_WARMUP:-2}" \
	--call-counts="${BENCH_SYNC_BRIDGE_BIGARGS_CALL_COUNTS:-1,8,32}" \
	--payload-bytes="${BENCH_SYNC_BRIDGE_BIGARGS_PAYLOAD_BYTES:-65536}"

run_tsx "dns-lookup-floor" \
	"$HERE/src/focused/dns-lookup-floor.bench.ts" \
	--iterations="${BENCH_DNS_LOOKUP_ITERATIONS:-10}" \
	--warmup="${BENCH_DNS_LOOKUP_WARMUP:-2}" \
	--rows="${BENCH_DNS_LOOKUP_ROWS:-single_localhost,sequential_same_2,sequential_same_8,sequential_same_32,concurrent_same_4,concurrent_same_16,cold_process_single}"

run_tsx "net-tcp-event-floor" \
	"$HERE/src/focused/net-tcp-event-floor.bench.ts" \
	--iterations="${BENCH_NET_TCP_ITERATIONS:-20}" \
	--warmup="${BENCH_NET_TCP_WARMUP:-5}" \
	${BENCH_NET_TCP_ROWS:+--rows="${BENCH_NET_TCP_ROWS}"} \
	${BENCH_NET_TCP_POLL_DELAY_MS:+--net-poll-delay-ms="${BENCH_NET_TCP_POLL_DELAY_MS}"} \
	${BENCH_NET_TCP_TRACE:+--net-bridge-trace}

DEFAULT_NET_TCP_TRACE_ROWS="connect_close_1,connect_close_4,connect_close_8,echo_1x1,echo_1x5,echo_1x5_string,echo_1x4k,echo_1x64k,echo_1x256k,echo_1x1m,burst_4x1_echo_once,burst_16x1_echo_once,burst_16x1_string_echo_once,burst_64x1_echo_once,burst_16x4096_echo_once,burst_64x1024_echo_once,burst_256x256_echo_once,pingpong_1x1,pingpong_4x1,pingpong_8x1,pingpong_16x1,pingpong_32x1,concurrent_2x1,concurrent_4x1,concurrent_8x1,echo_4x1,echo_8x1"
run_tsx "net-tcp-cadence-trace" \
	"$HERE/src/focused/net-tcp-event-floor.bench.ts" \
	--iterations="${BENCH_NET_TCP_TRACE_ITERATIONS:-5}" \
	--warmup="${BENCH_NET_TCP_TRACE_WARMUP:-1}" \
	--rows="${BENCH_NET_TCP_TRACE_ROWS:-$DEFAULT_NET_TCP_TRACE_ROWS}" \
	--net-bridge-trace \
	${BENCH_NET_TCP_TRACE_POLL_DELAY_MS:+--net-poll-delay-ms="${BENCH_NET_TCP_TRACE_POLL_DELAY_MS}"}

run_tsx "concurrency-vms" "$HERE/src/focused/concurrency-vms.bench.ts"

run_tsx "interference" "$HERE/src/focused/interference.bench.ts"

run_tsx "concurrent-processes" "$HERE/src/focused/concurrent-processes.bench.ts"

run_tsx "wasm-command-floor" \
	"$HERE/src/focused/wasm-command-floor.bench.ts" \
	--iterations="${BENCH_WASM_COMMAND_FLOOR_ITERATIONS:-3}" \
	--warmup="${BENCH_WASM_COMMAND_FLOOR_WARMUP:-1}" \
	--serial-runs="${BENCH_WASM_COMMAND_FLOOR_SERIAL_RUNS:-3}" \
	--stdout-sizes="${BENCH_WASM_COMMAND_FLOOR_STDOUT_SIZES:-0,1,65536}" \
	${BENCH_WASM_COMMAND_FLOOR_WARMUP_DEBUG:+--wasm-warmup-debug}

run_tsx "wasm-command-floor-debug" \
	"$HERE/src/focused/wasm-command-floor.bench.ts" \
	--iterations="${BENCH_WASM_COMMAND_DEBUG_ITERATIONS:-2}" \
	--warmup="${BENCH_WASM_COMMAND_DEBUG_WARMUP:-0}" \
	--serial-runs="${BENCH_WASM_COMMAND_DEBUG_SERIAL_RUNS:-2}" \
	--stdout-sizes="${BENCH_WASM_COMMAND_DEBUG_STDOUT_SIZES:-0,1}" \
	--wasm-warmup-debug

run_tsx "mount-readdir" \
	"$HERE/src/focused/mount-readdir.bench.ts" \
	--iterations="${BENCH_MOUNT_READDIR_ITERATIONS:-20}" \
	--warmup="${BENCH_MOUNT_READDIR_WARMUP:-3}" \
	--mount-counts="${BENCH_MOUNT_READDIR_COUNTS:-0,10,100}" \
	--entry-count="${BENCH_MOUNT_READDIR_ENTRY_COUNT:-32}"

run_tsx "overlay-readdir" \
	"$HERE/src/focused/overlay-readdir.bench.ts"

if should_run "process-spawn"; then
	build_native_baseline
	export NODE_OPTIONS="${NODE_OPTIONS:---expose-gc}"
fi
run_tsx "process-spawn" "$HERE/src/focused/process-spawn.bench.ts"

if should_run "matrix"; then
	build_native_baseline
	build_native_baseline_wasm
fi
run_tsx "matrix" "$HERE/src/run-all.ts"

echo "" >&2
echo "Done. Results saved under $RESULTS_DIR" >&2
