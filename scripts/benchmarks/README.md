# Agent OS Benchmarks

Agent OS keeps only product-surface benchmarks here:

- `session.bench.ts` - deterministic llmock-backed session VM tax (`vm` vs `bare-node` PI SDK session creation).
- `agent-session.bench.ts` - Claude Code, Pi, Codex, and OpenCode ACP process spawn plus first mocked prompt latency.
- `gigacode-session.bench.ts` - direct Pi ACP versus GigaCode ACP/session/first-prompt startup latency.
- `gigacode-agent-session.bench.ts` - Claude Code, Pi, Codex, and OpenCode startup and first-prompt latency through GigaCode, for comparison with `agent-session.bench.ts`.
- `coldstart.bench.ts` - Agent OS VM cold-start product workloads.
- `memory.bench.ts` - shared-sidecar per-VM memory overhead.
- `bench-utils.ts` - shared helpers for cold-start and memory workloads.
- `baseline.json` - committed baseline for the session VM-tax regression gate.

The differential matrix, focused runtime lanes, fuzz/perf harness, leak and
footprint probes, native comparisons, and ecosystem command benches now live in
agentos:

`/home/nathan/.herdr/workspaces/agent-os/agentos-perf-rules/packages/benchmarks`

Use that package for runtime-focused investigations; also follow its
`CLAUDE.md` Benchmarks section. `overlay-readdir` is deleted here too; its
agentos port is pending the API it needs.

## Standard Suite

Run the remaining product lanes through:

```bash
bash scripts/benchmarks/run-benchmarks.sh
```

Run a single lane with `BENCH_ONLY=<lane>`:

- `coldstart-sleep`
- `memory-sleep`
- `memory-pi-session`
- `session`
- `agent-session`
- `gigacode-session`
- `gigacode-agent-session`

Results are written under `scripts/benchmarks/results/` as `<lane>.json` and
`<lane>.log`.

## Session Baseline Gate

`session.bench.ts` compares Agent OS session creation against a bare Node PI SDK
baseline and reports:

- `vm.vmCreate.p50`
- `vm.sessionCreate.p50`
- `derived.vmTaxRatio`

Useful commands:

```bash
pnpm exec tsx scripts/benchmarks/session.bench.ts
pnpm exec tsx scripts/benchmarks/session.bench.ts --lanes=vm
pnpm exec tsx scripts/benchmarks/session.bench.ts --gate
pnpm exec tsx scripts/benchmarks/session.bench.ts --update-baseline
BENCH_GATE=1 bash scripts/benchmarks/run-benchmarks.sh
BENCH_UPDATE_BASELINE=1 bash scripts/benchmarks/run-benchmarks.sh
```

Only refresh `baseline.json` intentionally and review the resulting diff.

## Cross-Agent Session Startup

`agent-session.bench.ts` uses one shared AgentOS sidecar and a local LLMock
server. For every agent it reports VM creation, the first shell/filesystem
command, ACP `openSession`, and the first completed prompt separately.
`openSession` is the actual adapter process-spawn boundary: it includes ACP
initialize and `session/new`, not merely allocation of an HTTP session ID.

```bash
pnpm exec tsx scripts/benchmarks/agent-session.bench.ts
pnpm exec tsx scripts/benchmarks/agent-session.bench.ts --agents=claude,pi
pnpm exec tsx scripts/benchmarks/agent-session.bench.ts --software-dir="$HOME/.local/share/gigacode/software"
BENCH_ONLY=agent-session bash scripts/benchmarks/run-benchmarks.sh
```

Use `--software-dir` to benchmark installed packed `.aospkg` artifacts without
requiring generated `dist/package.aospkg` files in the source checkout.

## GigaCode vs Direct ACP

`gigacode-session.bench.ts` compares the same Pi + LLMock turn through direct
AgentOS ACP and through GigaCode. Its fresh state directory makes first-start
model discovery part of the daemon startup barrier, so catalog probes cannot
contaminate the later ACP samples; discovery time is reported separately.

```bash
pnpm exec tsx scripts/benchmarks/gigacode-session.bench.ts
BENCH_ONLY=gigacode-session bash scripts/benchmarks/run-benchmarks.sh
```

## Real Claude Production Comparison

`claude-real.bench.mjs` compares raw AgentOS ACP with the installed GigaCode
path using the real Claude provider, workspace, and Claude configuration. It
does not start LLMock. Every sample creates a fresh Claude ACP process, selects
the default effort, sends one minimal real prompt, and records sidecar phase
timings for process spawn, ACP initialize, and `session/new`.

```bash
node scripts/benchmarks/claude-real.bench.mjs --iterations=5 \
  --output=/tmp/claude-real-benchmark.json --keep-artifacts
```

Use this for production latency investigations. The LLMock benchmarks remain
the deterministic, credential-free regression lanes; their empty Claude home
means their timings are not a production baseline.

## Cross-Agent GigaCode Startup

`gigacode-agent-session.bench.ts` runs the same four supported harnesses and LLMock
response as `agent-session.bench.ts`, but through GigaCode's HTTP/Rivet path.
Compare `sessionCreate` and `firstPrompt` with the raw ACP result; the benchmark
reports GigaCode logical-session, model-selection, and HTTP round-trip overhead
separately.

```bash
pnpm exec tsx scripts/benchmarks/gigacode-agent-session.bench.ts
pnpm exec tsx scripts/benchmarks/gigacode-agent-session.bench.ts --agents=claude,pi
BENCH_ONLY=gigacode-agent-session bash scripts/benchmarks/run-benchmarks.sh
```
