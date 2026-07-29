# ACP adapter migration status

Last updated: 2026-07-14

This is the execution checklist for replacing AgentOS's harness-specific ACP
forks with maintained/native adapters. The architecture and acceptance details live in
`docs-internal/acp-adapter-upstream-plan.md`.

## Completion definition

The migration is complete only when all four harnesses use the selected ACP
boundary, host and AgentOS behavior pass the same black-box parity cases, the
Gigacode terminal suite passes, and Gigacode has been rebuilt and reinstalled
globally. A package build or unit-test-only result is not complete.

## Pi — standard `pi-acp`

Current status: **AgentOS pins the published `pi-acp@0.0.31` Node adapter and
`@earendil-works/pi-coding-agent@0.80.6`. The adapter is launched unchanged and
uses its documented `PI_ACP_PI_COMMAND` override to spawn the packaged Pi CLI.
Packed LLMock validation is in progress**.

- [x] Pin the published `pi-acp@0.0.31` package.
- [x] Keep the native Pi CLI at `@earendil-works/pi-coding-agent@0.80.6`.
- [x] Remove the embedded SDK/snapshot adapter.
- [x] Remove the custom Rust/WASI adapter, source downloader, patch, and build.
- [x] Use the standard adapter's documented Pi executable override.
- [ ] Pass the packed initialize and real LLMock prompt lifecycle.
- [ ] Pass shared multi-turn, resume, cancel, tools, commands, and shutdown parity.

## Claude Code — maintained Claude Agent ACP

Current status: **official adapter packaged; host and packed AgentOS prompt,
tool, lifecycle, configuration, multi-turn, resume, and cancellation tests
pass**.

- [x] Update to the current stable
  `@agentclientprotocol/claude-agent-acp` and its supported Claude Agent SDK/CLI.
- [x] Make the published adapter the unmodified ACP entrypoint.
- [x] Point it at the byte-identical upstream JavaScript Claude executable
  through its supported `CLAUDE_CODE_EXECUTABLE` override.
- [x] Inventory every minified Claude CLI/SDK rewrite and classify it as ACP
  semantics, AgentOS runtime compatibility, upstream defect, observability, or
  packaging.
- [x] Remove custom ACP translation and trace-only/minified patches.
- [x] Reproduce every still-needed Node/POSIX workaround as a focused AgentOS
  runtime test, fix core behavior, then run the unmodified current Claude code.
- [ ] Verify native host credentials and writable refresh persistence.
- [ ] Pass models/effort, session lifecycle/resume, multi-turn, cancel/queue,
  permissions, tools, terminals, subagents, commands, EOF, and turn-end parity.

## OpenCode — native `opencode acp`

Current status: **unmodified current upstream ACP packaged; initialize,
directory bootstrap, native session creation, model selection, and a real
LLMock-backed OpenCode turn pass in a packed AgentOS VM. The remaining active
gap is that the native ACP ends the turn without forwarding the streamed text
events to the AgentOS client**.

- [x] Pin upstream OpenCode 1.17.20 (`4473fc3`) with source checksums.
- [x] Build upstream's native ACP for Node with byte-identical prepared source.
- [x] Remove the downstream patch and all provider/model/session/storage/plugin
  source rewrites.
- [x] Fix valid Node behavior missing from AgentOS in runtime core with
  focused tests; do not patch OpenCode around it.
- [x] Package and run without Bun at runtime (upstream Bun build tooling is
  allowed).
- [ ] Verify native OpenCode config/auth mounts and writable refresh state.
- [ ] Pass providers/models/options/modes/agents, lifecycle/resume, multi-turn,
  cancel/queue, tools, commands, MCP, EOF, and turn-end parity.

## Codex — maintained Codex ACP

Current status: **official ACP package wired; the official Codex App Server
builds, projects, rewrites its roughly 149 MiB optimized module within bounded
memory, instantiates, enters `_start`, opens its official SQLite state database,
and runs cooperative Tokio blocking work. The next WASI gate is Rust's
`File::lock()` during installation-ID persistence**.

- [x] Pin the current stable `@agentclientprotocol/codex-acp` and compatible
  official Codex App Server.
- [x] Package the adapter unchanged and register `codex-acp` as ACP entrypoint.
- [x] Point `CODEX_PATH` at the packaged official Codex command only when normal
  resolution is insufficient.
- [ ] Complete/test the official Codex App Server execution path in AgentOS
  WASM rather than emulating it in the adapter.
- [ ] Verify native host Codex credentials and writable refresh persistence.
- [ ] Pass models/reasoning, approvals/modes, lifecycle/resume, multi-turn,
  cancel/queue, tools/plans/terminals, MCP/skills/review/commands, EOF, and
  turn-end parity.

## Shared validation and delivery

Current status: **the SDK and terminal contracts are implemented and continue
to grow while the final OpenCode and Codex runtime gates are resolved**.

- [ ] Run the same deterministic mock-provider/RPC black-box suite against each
  host reference and packaged AgentOS entrypoint.
- [ ] Run opt-in live credential smoke tests for all four harnesses.
- [ ] Test at least three ordered turns, resume plus another turn, cancellation,
  queued input, commands, tool ordering, session ordering, and clean shutdown.
- [ ] Exercise Gigacode through its real OpenCode TypeScript SDK boundary.
- [ ] Exercise the interactive Gigacode CLI through the terminal driver,
  including model list, new/list/resume sessions, prompts, commands, cancel,
  queueing, and output order.
- [ ] Run scoped Rust, TypeScript, registry, core, sidecar, protocol, and docs
  checks, followed by the expensive end-to-end suite.
- [ ] Rebuild native AgentOS plugin/sidecar artifacts needed by Gigacode.
- [ ] Rebuild and reinstall global `gigacode`/`gc`, stop any stale daemon, and
  manually verify the installed command in a fresh terminal session.
- [ ] Record final versions, release URLs, benchmark output, test commands, and
  any external npm authorization step here.

## Activity log

- 2026-07-13: Started standalone Rust repository work in parallel with a
  read-only audit of the four current AgentOS packages.
- 2026-07-13: Added the Pi native/WASM release contract, required feature
  surface, reproducible benchmark rules, and AgentOS migration acceptance to
  the architecture plan.
- 2026-07-13: Replaced Claude's custom ACP and patched minified CLI with the
  maintained ACP package plus a byte-identical upstream CLI. Moved required
  system-prompt injection to the adapter's supported session metadata and fixed
  AgentOS child stdin close semantics exposed by the unmodified adapter.
- 2026-07-13: Replaced the OpenCode 1.3.13 patch fork with checksummed upstream
  1.17.20 native ACP source and added generic AgentOS Node PTY compatibility.
- 2026-07-13: Wired the maintained Codex ACP package and its supported
  `CODEX_CONFIG`/`CODEX_PATH` inputs while the official App Server WASI artifact
  is being completed in the Rivet Codex fork.
- 2026-07-13: Rust Pi native/deterministic E2E and WASI compilation pass. The
  final reproducible 30-sample interleaved benchmark measures median startup at
  20.1ms and 3.7 MiB adapter RSS (15.6 MiB process-tree RSS) versus 356.2ms and
  91.6 MiB adapter RSS (103.6 MiB process-tree RSS) for the pinned Node
  reference; AgentOS integration remains in progress.
- 2026-07-14: Claude's official ACP 0.59.0 now passes packaged CLI
  initialization, real AgentOS text prompting, PATH-backed WASM tools, sync and
  async child processes, cancellation, modes, raw ACP, three configuration
  option groups (model/mode/effort), two turns, and native close/load/resume.
  Runtime fixes are covered independently for late stdin close and callable
  `Intl.DateTimeFormat`/`Intl.NumberFormat` semantics.
- 2026-07-14: The standalone Pi adapter's current 0.80.6 LLMock run reports 14
  models and two successful turns across close/resume. Its README now contains
  the required feature matrix and reproducible 30-sample startup/RSS benchmark;
  release publication and AgentOS package replacement remain in progress.
- 2026-07-14: Published `rivet-dev/pi-acp-rust` v0.1.0 at commit `c5f6a066`
  with five native targets and `pi-acp-agentos.wasm`. The release-downloaded
  WASM SHA-256 is
  `b9f3a815b79035c2c860b0ab59b6bcd96c15a561e90d77fd167999eb94ca48ef`.
  All Rust/WASI and real Pi jobs pass; the release workflow is red only at npm
  publication because the local account lacks `@rivet-dev` trusted-publisher
  authorization. AgentOS integration therefore pins the checksummed GitHub
  release asset rather than a developer-local build.
- 2026-07-14: OpenCode's pristine 1.17.20 Node bundle initializes from the
  packed AgentOS package in about 5.1 seconds. Packed `session/new`/prompt is
  still the active OpenCode gate. The official Codex App Server 0.144.3 WASI
  port is through its major dependency crates but has not emitted the final
  checksummed artifact yet.
- 2026-07-14: Fixed generic ACP configuration-state replacement in the AgentOS
  sidecar. A model switch may replace the complete `configOptions` list (for
  example, removing Claude effort controls for a model that does not advertise
  them); AgentOS now persists and emits that authoritative response instead of
  retaining stale conditional controls. The focused Rust sidecar regression and
  Claude model/effort lifecycle test pass.
- 2026-07-14: Replaced AgentOS's hard-coded JavaScript ACP launch choice with
  executable-header resolution. ACP entrypoints now follow the same shebang and
  WebAssembly-magic semantics as other VM commands, without package runtime
  metadata.
- 2026-07-14: OpenCode bootstrap instrumentation proved that config, plugins,
  LSP, formatting, VCS, snapshots, and project services all initialize. The
  remaining internal-route stall is a generic Node HTTP request-stream issue:
  AgentOS emitted body EOF before Effect's delayed consumer entered flowing
  mode. The buffered pull/flowing-state fix is in packed validation.
- 2026-07-14: The official Codex App Server now compiles and instantiates as a
  packed WASI command without raw POSIX listener imports. Large-module memory
  instrumentation used a JavaScript numeric array for the complete 142 MiB
  module; the generic runtime rewrite now concatenates bounded `Uint8Array`
  chunks while preserving the configured memory limit. Exact `codex
  app-server` JSONL validation is pending the optimized relink.
- 2026-07-14: Claude's official adapter passed its 10-case full AgentOS suite
  (including PATH tools, synchronous and asynchronous child processes,
  configuration replacement, two turns, native close/load/resume, and cancel)
  plus the four-case host package suite and upstream byte-integrity check.
- 2026-07-14: Packed Pi now launches its native CLI far enough to expose two
  generic Node-runtime gaps: the bounded module-resolution cache was too small
  for Pi's dependency graph, and `node:path.toNamespacedPath` was absent. Both
  require focused core fixes before the concurrent, cancellable adapter can be
  patch-released.
- 2026-07-14: Packed Codex reaches the official App Server `_start`; its first
  runtime failure is Tokio filesystem code attempting to create a blocking
  worker thread on `wasm32-wasip1`. The WASI port must remove that unsupported
  thread dependency without changing the App Server JSONL surface.
- 2026-07-14: OpenCode's five native directory bootstrap calls (providers,
  skills, commands, agents, and configuration) now all return HTTP 200 and
  resolve in the pristine upstream Node build. Focused AgentOS regressions cover
  late `server.on("request")` listeners, delayed request-body consumption, and
  zlib synchronous methods receiving `Uint8Array`; the next trace boundary is
  directory snapshot completion into native session creation.
- 2026-07-14: The OpenCode trace now reaches directory `Promise.all`, directory
  build, snapshot completion, and `sdk.session.create` in order. The generated
  SDK sends the first session POST as `fetch(new Request(...))`; that request
  does not reach the server although the already-covered `fetch(url, init)`
  form does, so the exact Request-object form is now the focused runtime gate.
- 2026-07-14: Codex no longer panics in Tokio filesystem initialization. The
  exact literal `codex app-server` packed smoke reaches the official
  `state_5.sqlite` open and now reports the full chain: SQLx cannot communicate
  with its SQLite worker because the underlying WASI operation returns
  `ENOTSUP`. The single-threaded WASI SQLx transport is the next fork fix.
- 2026-07-14: Gigacode now projects OpenCode's native host XDG configuration
  and data directories alongside Claude, Codex, and Pi credentials. The mounts
  are writable for native OAuth/API credential refresh persistence; the E2E
  contract reads both native OpenCode files and verifies data-directory
  write-through. Type and formatting checks pass, with packed validation still
  gated on native OpenCode session creation.
- 2026-07-14: Strengthened the installed Gigacode SDK contract: provider
  discovery must expose more than a placeholder model for every harness, and
  the suite creates and prompts independent Claude, Codex, OpenCode, and Pi
  sessions through the OpenCode TypeScript SDK against one deterministic
  Anthropic/OpenAI LLMock endpoint. Static checks pass; execution remains part
  of the final rebuilt global-install run after all four packages are green.
- 2026-07-14: Extended the opt-in published-package/live-credential matrix from
  Pi, Claude, and OpenCode to all four harnesses. Codex now installs its real
  `@agentos-software/codex` package and requires native `OPENAI_API_KEY` auth,
  while the other three retain native Anthropic/config auth; the fixture no
  longer silently discards session-close or VM-dispose failures. Core type and
  formatting checks pass.
- 2026-07-14: Packed Pi completed direct native CLI version execution, Rust
  WASM ACP launch, Pi RPC session creation, a real LLMock prompt, and close in
  the AgentOS client E2E (about 6.25 seconds). The adapter then restored
  concurrent ACP dispatch required for cancellation/steering and bounded child
  stderr/status handling; the final released-WASM packed rerun is pending.
- 2026-07-14: Kept the package manifest at v1. Adapter runtime metadata was
  removed because the executable header is authoritative; Rust and TypeScript
  packers continue to emit and decode the same v1 schema.
- 2026-07-14: OpenCode's session POST starvation is now reproduced by a generic
  six-connection regression. Completed bootstrap connections retained Undici
  core drain state `2` while the pool remained marked as needing drain, so no
  connection became selectable beside the long-lived event stream; the generic
  drain-state correction is in packed validation.
- 2026-07-14: Codex passed its SQLite state-database initialization after the
  WASI SQLx correction. The next official App Server call uses Tokio
  `spawn_blocking` for installation-ID work, exposing the remaining unsupported
  worker-thread operation; a WASI-only cooperative implementation is relinking.
- 2026-07-14: Rechecked the selected upstream versions against npm's current
  `latest` dist-tags: Claude Agent ACP 0.59.0, Codex ACP 1.1.2, Pi CLI 0.80.6,
  and OpenCode 1.17.20 remain current.
- 2026-07-14: The generic OpenCode Undici correction now passes the exact
  saturated-pool regression (one live event stream, five completed bootstrap
  clients, and repeated `Request`-object POSTs). Packed native OpenCode creates
  a session, selects `anthropic/claude-sonnet-4-6`, and completes a real mock
  provider turn; forwarding the resulting native stream updates is the active
  OpenCode gate.
- 2026-07-14: The deterministic Gigacode SDK test now runs two ordered turns
  for each of Claude, Codex, OpenCode, and Pi, restarts the single daemon once,
  verifies every persisted transcript, and runs a third turn through the same
  logical sessions. It also requires one new underlying ACP session and an
  explicit bounded transcript handoff per harness after restart.
- 2026-07-14: Superseded the v0.1.0 mixed-artifact release with native-only
  `pi-acp-rust` v0.1.2 at commit
  `341fed0c145325d492b25c8ad4ef961177b7aa9f`. The release contains five native
  target archives, checksums, the npm launcher, and source; it contains no
  AgentOS, WASM, or WASI artifact. Public `@rivet-dev/pi-acp@0.1.2` installs and
  runs `pi-acp --version` successfully. The OIDC retry reached npm with publish
  permission and failed only because that version already existed, leaving a
  fresh-version provenance publish as the remaining automation proof.
- 2026-07-14: AgentOS now pins the v0.1.2 source tarball (SHA-256
  `625e3027dcf68d6ebe4835fe00f63bae238e39a1b6521cb48d754a2953d6df09`),
  verifies and applies its registry-owned WASI-only patch (SHA-256
  `5134fa1303f5a2e8afbf7e6f968974594102047a7aa35e52a0407415d84ce10c`),
  and reproducibly cross-compiles the packed artifact (SHA-256
  `6591e2582a40bc02614cc9c5b697d2e958d359b27ef06f87f27131563a5ecc49`).
  A clean registry build/test and the packed AgentOS real-LLMock Pi session E2E
  pass. The standalone repository remains strictly native-only.
