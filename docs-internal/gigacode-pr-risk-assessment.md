# Current Branch Risk Assessment

Scope: working-copy change `lplyvpwl` on `workspace/brave-valley-8f38`, compared
with `main@origin` (`a624e8c4`) on 2026-07-21. This describes the current local
tree, not the stale published PR revision.

## Review snapshot

The branch currently changes **220 files**, with **26,150 insertions** and
**9,230 deletions**.

- GigaCode itself is 12 files and approximately 10,725 insertions.
- Everything outside `experiments/gigacode` is 208 files, approximately 15,425
  insertions, and 9,230 deletions.
- The counts below overlap when a package participates in more than one area;
  they are review aids, not values that should be summed.
- Package manifests remain on `.aospkg` schema v1. The proposed v2
  `agent.runtime` field was removed; ACP entrypoints use executable bits,
  shebangs, and WebAssembly magic like normal VM commands.

The earlier 1,027-file count was mostly generated noise. The 801-file Pi Rust
cache and six generated benchmark result/log files have been untracked and are
now ignored. A scan of all remaining added paths found no other cache, build,
binary, archive, log, database, or generated-result artifacts.

| Rank | Change area | Approximate churn | Behavioral risk | Blast radius |
| ---: | --- | --- | --- | --- |
| 1 | Generic runtime, kernel, VFS, and Node compatibility | 67 files, +3,842 / -615 | Very high | Every JS/WASM/Python VM workload |
| 2 | Durable ACP lifecycle and protocol | 26 files, +1,770 / -173 | Very high | Public session API and all agents |
| 3 | Claude, Codex, OpenCode, and Pi migrations plus Codex toolchain | 62 files, +3,342 / -8,164 | High; Codex toolchain is very high | Per-agent packages and WASI build chain |
| 4 | GigaCode OpenCode-compatible server/TUI experiment | 12 files, +10,725 | High complexity, more isolated | GigaCode users and OpenCode API compatibility |
| 5 | Node PTY and terminal attachment API | 11 files, +1,024 / -16 | Medium-high | New public Node API and process terminal state |
| 6 | Benchmark programs | 6 files, +2,368 | Low runtime risk, medium review cost | Developer performance tooling |
| 7 | Skills, migration notes, and website docs | Mostly additive | Low runtime risk | Maintenance and operator expectations |

## 1. Generic runtime, kernel, VFS, and Node compatibility

Primary areas:

- `crates/execution`
- `crates/kernel`
- `crates/native-sidecar` and `crates/native-sidecar-core`
- `crates/v8-runtime`
- `crates/vfs`
- `packages/build-tools` and the generated V8 bridge contract

This is the broadest risk in the branch. It changes child-process execution,
stdin/stdout and pipe scheduling, process events, JavaScript module behavior,
SQLite, filesystem calls, permissions, HTTP/TCP/Unix sockets, and V8/WASM
runtime behavior. These fixes were largely exposed by real upstream agents, but
the implementation is shared by every AgentOS workload.

The remaining VFS changes are not a format migration. They are runtime changes:

- Cache only successful lookups and `ENOENT`; do not cache transient VFS errors.
- Index mount paths for longest-prefix lookup while preserving exact Linux path
  separation such as `/data/file` versus `/data/data/file`.
- Add a bounded 32,768-entry tar realpath cache and change tar node lookup from
  `BTreeMap` to `HashMap`.
- Keep Rust and TypeScript v1 package packers cross-validated.

Review focus: security boundaries, queue/pipe liveness, path resolution,
symlink containment, cache invalidation, and generic regression tests that do
not depend on a particular agent.

## 2. Durable ACP lifecycle and protocol

Primary areas:

- `crates/agentos-sidecar/src/acp`
- `crates/agentos-sidecar-core`
- `crates/agentos-protocol`
- `packages/core/src/sidecar`
- AgentOS actor actions and session documentation

The active surface is `openSession`, `getSession`, `listSessions`, `prompt`,
`cancelPrompt`, `respondPermission`, session configuration, `readHistory`,
`unloadSession`, and `deleteSession`. The removed legacy methods
`sendPromptAsync`, `promptResult`, `cancelSession`, `setSessionModel`, and
`probeAgentConfig` must not return.

Important behavioral changes include:

- Persisting additional directories and MCP servers across restore.
- Adding an `eventCount` response barrier because terminal responses and ACP
  events travel on separate priority lanes.
- Waiting for prompt/cancellation quiescence and draining trailing adapter
  output before reporting completion.
- Replacing whole-turn and human permission deadlines with periodic inactivity
  warnings; lifecycle and machine-host RPCs remain bounded.
- Launching initial and restored ACP adapters through normal command/header
  resolution rather than forcing a package-declared runtime.
- Returning structured actor errors with the underlying AgentOS cause.

The main risk is lifecycle concurrency: prompt completion, cancellation,
adapter exit, restore, unload/delete, event delivery, and the terminal SQLite
commit must agree on one authoritative state. Failure modes include duplicate
output, late events, stuck busy state, lost permissions, or reusing an adapter
that is not quiescent.

Current validation status: package-format tests, focused Rust compilation, and
a synthetic end-to-end ACP create/prompt/history/unload/delete flow pass after
removing manifest runtime metadata. **Claude, Codex, OpenCode, and Pi have not
all been rebuilt and rerun through create/prompt/close since that final change.**
That four-harness sanity matrix remains a required review gate.

## 3. Agent packages and the Codex toolchain

Primary areas:

- `software/claude`
- `software/codex`
- `software/opencode`
- `software/pi`
- `toolchain/codex-ref`, Codex build scripts, and 11 Codex patches
- Three dependency/sysroot patches under `toolchain/std-patches/crates`

The package migrations remove large downstream adapters and move toward
maintained upstream boundaries:

- Claude stages the upstream CLI and uses its maintained ACP adapter.
- OpenCode runs an upstream Node build rather than the old source patch and
  transformation layer.
- Pi packages the pinned AgentOS-maintained `rivet-dev/pi-acp` fork rather than
  the removed embedded TypeScript adapter.
- Codex adds an ACP adapter around a patched WASI App Server build, including
  tool events, approvals, filesystem writes, reasoning effort, message
  normalization, and WASI runtime fixes.

The deletions are mostly removal of old embedded adapters and patches, but that
does not make the migration low risk. Each package combines source provenance,
checksums, dependency closure, executable projection, credentials, model and
reasoning configuration, tools, and lifecycle behavior.

Codex is the highest-risk individual harness because it also changes the Rust
WASI toolchain and carries 11 ordered source patches. Review its build
reproducibility, pinned upstream commit, patch ordering, clean-checkout build,
and runtime behavior independently from the JavaScript agent packages.

## 4. GigaCode

`experiments/gigacode` is the largest source addition: approximately 10,725
lines across 12 files. It implements an OpenCode-compatible HTTP/event surface
over Rivet actors and durable AgentOS ACP sessions, plus the local daemon,
startup/model discovery, queueing, permissions, tool normalization, TUI launch,
logging, and installation.

Its code is isolated under `experiments`, but it is still high complexity. The
highest-risk paths are cancellation quiescence, queued-turn ordering,
permission identity, actor/session ownership, reconnect/event replay, model and
reasoning-option projection, tool/file part normalization, and daemon startup.
The large single-file implementation in `gigacode.ts` also raises review and
maintenance cost even when its runtime blast radius is limited.

Review GigaCode separately from AgentOS core. Compatibility workarounds must not
be used to hide AgentOS defects; generic lifecycle/runtime defects belong in
AgentOS with focused tests.

## 5. Node PTY and terminal attachment API

Primary areas:

- `packages/node-pty`
- `packages/agentos/src/node.ts`
- Associated tests and process documentation

This adds a public Node helper that attaches an AgentOS shell to local
stdin/stdout, including raw mode, resize, signals, output buffering, limits,
and cleanup. It mutates process-global terminal and signal state, so failures
can leave the caller's terminal in raw mode or leak listeners. Review it as an
isolated public API with explicit cleanup, backpressure, output-limit, and
concurrent-shell tests.

## 6. Benchmarks, skills, and documentation

The benchmark source additions measure direct ACP and GigaCode session startup
for Claude, Pi, Codex, and OpenCode, plus a real-Claude comparison. They are
useful regression tools but should not block runtime review. Generated files
under `scripts/benchmarks/results/` are ignored; only intentionally updated
baseline files should be committed.

Skills and migration documents are low runtime risk but can encode stale
architecture. GigaCode-specific policy should remain under the experiment, and
dated migration logs are not proof that the current tree passes.

## Required review gates

1. Rebuild current v1 packages and run create/prompt/close for Claude, Codex,
   OpenCode, and Pi.
2. Run focused generic tests for every runtime/VFS change, especially process
   output, stdin, cancellation, mount lookup, tar symlinks, networking, and
   SQLite.
3. Exercise ACP cancellation, restore, unload/delete, concurrent permissions,
   event ordering, and adapter exit without GigaCode.
4. Rebuild Codex from a clean checkout using only the pinned source and ordered
   patches, then test tools, file edits, approvals, models, and reasoning levels.
5. Run GigaCode API sanity across all four harnesses, including mid-flight
   queues, cancellation followed by another prompt, tools, file edits, model
   variants, and session reconnect.
6. Review and test the Node PTY API independently.

## Recommended review boundaries

1. Durable ACP API/protocol and lifecycle.
2. Generic execution/kernel/VFS fixes, grouped by subsystem.
3. One boundary per agent package; keep Codex toolchain work explicit.
4. GigaCode HTTP/OpenCode compatibility and daemon behavior.
5. Node PTY/terminal API.
6. Benchmarks and documentation.
