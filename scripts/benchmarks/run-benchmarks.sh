#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$REPO_ROOT"

echo "=== Building ===" >&2
pnpm build >&2

RESULTS_DIR="$SCRIPT_DIR/results"
mkdir -p "$RESULTS_DIR"

run() {
  local name="$1"
  shift
  echo "" >&2
  echo "=== Running $name ===" >&2
  pnpm exec tsx "$@" \
    1> "$RESULTS_DIR/${name}.json" \
    2> >(tee "$RESULTS_DIR/${name}.log" >&2)
}

# Cold-start benchmarks
run "coldstart-echo" \
  scripts/benchmarks/coldstart.bench.ts --workload=echo

run "coldstart-pi-prompt-turn" \
  scripts/benchmarks/coldstart.bench.ts --workload=pi-prompt-turn --iterations=3

# Memory benchmarks
# run "memory-sleep" \
#   --expose-gc scripts/benchmarks/memory.bench.ts --workload=sleep --count=5

run "memory-pi-session" \
  --expose-gc scripts/benchmarks/memory.bench.ts --workload=pi-session --count=3

run "memory-claude-session" \
  --expose-gc scripts/benchmarks/memory.bench.ts --workload=claude-session --count=3

run "memory-codex-session" \
  --expose-gc scripts/benchmarks/memory.bench.ts --workload=codex-session --count=3

echo "" >&2
echo "=== Done. Results in $RESULTS_DIR ===" >&2
