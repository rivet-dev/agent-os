#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$REPO_ROOT"

RESULTS_DIR="$SCRIPT_DIR/results"
mkdir -p "$RESULTS_DIR"

BENCH_ONLY="${BENCH_ONLY:-}"
LANES=(
  "coldstart-sleep"
  "memory-sleep"
  "memory-pi-session"
  "session"
  "agent-session"
  "gigacode-session"
  "gigacode-agent-session"
)

should_run() {
  local name="$1"
  [[ -z "$BENCH_ONLY" || "$BENCH_ONLY" == "$name" ]]
}

should_build() {
  [[ -z "$BENCH_ONLY" ]] && return 0
  local lane
  for lane in "${LANES[@]}"; do
    [[ "$BENCH_ONLY" == "$lane" ]] && return 0
  done
  return 1
}

run() {
  local name="$1"
  shift
  if ! should_run "$name"; then
    echo "=== Skipping $name (BENCH_ONLY=$BENCH_ONLY) ===" >&2
    return
  fi
  echo "" >&2
  echo "=== Running $name ===" >&2
  pnpm exec tsx "$@" \
    1> "$RESULTS_DIR/${name}.json" \
    2> >(tee "$RESULTS_DIR/${name}.log" >&2)
}

if should_build; then
  echo "=== Building benchmark TypeScript dependencies ===" >&2
  pnpm --dir packages/core build >&2

  # Benchmarks must run against an optimized sidecar. The debug build is several
  # times slower and inflates cold-start and memory numbers.
  echo "=== Building release sidecar ===" >&2
  cargo build --release -p agentos-sidecar >&2
  if [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
    export AGENTOS_SIDECAR_BIN="$CARGO_TARGET_DIR/release/agentos-sidecar"
  else
    export AGENTOS_SIDECAR_BIN="$REPO_ROOT/target/release/agentos-sidecar"
  fi
  echo "Using sidecar: $AGENTOS_SIDECAR_BIN" >&2
else
  echo "=== No matching benchmark lanes for BENCH_ONLY=$BENCH_ONLY ===" >&2
fi

# Cold start - minimal "sleep" VM (idle Node.js process). This is the workload
# cited on the product cold-start chart. 2000 iterations keeps p95/p99 meaningful.
run "coldstart-sleep" \
  scripts/benchmarks/coldstart.bench.ts --workload=sleep --iterations=2000

# Memory - simple shell workload.
run "memory-sleep" \
  --expose-gc scripts/benchmarks/memory.bench.ts --workload=sleep --count=20

# Memory - full Pi agent session.
run "memory-pi-session" \
  --expose-gc scripts/benchmarks/memory.bench.ts --workload=pi-session --count=10

# Session-creation VM-tax benchmark (deterministic, llmock-backed).
# Set BENCH_GATE=1 to fail on regression; set BENCH_UPDATE_BASELINE=1 to refresh
# scripts/benchmarks/baseline.json.
run "session" \
  scripts/benchmarks/session.bench.ts --iterations=5 \
    ${BENCH_GATE:+--gate} ${BENCH_UPDATE_BASELINE:+--update-baseline}

# Cross-agent ACP process spawn and first mocked message. Failures are recorded
# per agent so one broken adapter does not hide timings for the others.
run "agent-session" \
  scripts/benchmarks/agent-session.bench.ts --iterations=3 --warmup=1

# Direct AgentOS ACP versus the GigaCode HTTP + Rivet actor path. Both lanes
# reuse a warm VM/actor and talk to the same local mock model server.
run "gigacode-session" \
  scripts/benchmarks/gigacode-session.bench.ts --iterations=3 --warmup=1

# Cross-agent GigaCode startup using the same LLMock prompt as agent-session.
# Compare sessionCreate and firstPrompt against the raw ACP benchmark output.
run "gigacode-agent-session" \
  scripts/benchmarks/gigacode-agent-session.bench.ts --iterations=3 --warmup=1

echo "" >&2
echo "=== Done. Results in $RESULTS_DIR ===" >&2
