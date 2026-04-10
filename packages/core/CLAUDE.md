# agentOS Core Package

`@rivet-dev/agent-os-core` -- contains VM ops, ACP client, session management.

**⚠️ CRITICAL INVARIANT: ALL guest code MUST execute inside the kernel with ZERO host escapes.** The VM is a fully virtualized OS — every file read, network connection, and process spawn goes through the kernel. Guest code must never touch real host APIs. The Node.js execution engine is currently broken (spawns real host `node` processes instead of V8 isolates). See `crates/execution/CLAUDE.md`.

## AgentOs Class

- Wraps the kernel and proxies its API directly.
- **All public methods must accept and return JSON-serializable data.** No object references (Session, ManagedProcess, ShellHandle) in the public API. Reference resources by ID (session ID, PID, shell ID).
- Filesystem methods mirror the kernel API 1:1 (readFile, writeFile, mkdir, readdir, stat, exists, move, delete).
- Command execution mirrors the kernel API (exec, spawn).
- `fetch(port, request)` reaches services running inside the VM using the kernel network adapter pattern (`proc.network.fetch`).
- **Cron scheduling stays in the TypeScript layer.** The Rust sidecar has no concept of cron jobs. Cron expression parsing, timer management, overlap policies, and job execution dispatch all live in the TypeScript SDK.
- Native sidecar execution requests should stay unresolved on the TypeScript side. Forward `command`, `args`, `cwd`, and VM config through the wire payload, and let Rust own command lookup, guest-path to host-path mapping, shadow materialization, and `AGENT_OS_*` runtime env assembly.
- Native sidecar `exec()` should stay a thin `sh -c` wrapper when the guest shell exists. Do not reintroduce TypeScript tokenization or `node` special-casing in `src/sidecar/rpc-client.ts`.
- If a file must be visible to both `vm.readFile()` and guest shell commands, it cannot live only in a local compat mount. Put it on a real sidecar-visible path or mount, and keep any read-only guarantees enforced below the TypeScript proxy layer.
- Host tool registration is split across the boundary: TypeScript converts Zod schemas to JSON Schema, validates sidecar tool invocations, and runs the local `execute()` callbacks, while the sidecar owns CLI flag parsing, `agentos` command dispatch, and prompt-markdown generation via `register_toolkit`.
- The host-tool description limit is a cross-boundary contract: keep the 200-character maximum aligned between `src/host-tools.ts` and Rust `register_toolkit` validation in `crates/sidecar/src/tools.rs`, with boundary tests on both sides when changing it.
- `src/sidecar/rpc-client.ts` is the consolidated home for framed sidecar I/O, compat proxy helpers, and sidecar descriptor serializers. Keep shared/explicit sidecar pool and VM lease bookkeeping in `src/agent-os.ts` rather than reintroducing another sidecar lifecycle layer.
- Public SDK type exports now funnel through `src/types.ts`; keep legacy kernel/runtime implementation helpers behind `src/runtime-compat.ts` and avoid adding new public root exports directly from runtime internals.
- When adding a new public SDK option/result/helper type under `src/agent-os.ts`, `src/json-rpc.ts`, `src/host-dir-mount.ts`, or other root-facing modules, mirror it through `src/types.ts` and keep `tests/public-api-exports.test.ts` aligned so the package entrypoint stays truthful.

## Agent Sessions (ACP)

- Uses the **Agent Communication Protocol** (ACP) -- JSON-RPC 2.0 over stdio (newline-delimited)
- No HTTP adapter layer; communicate directly with agent ACP adapters over stdin/stdout
- Reference `~/sandbox-agent` for ACP integration patterns. Do not copy code from it.
- ACP docs: https://agentclientprotocol.com/get-started/introduction
- Session design is **agent-agnostic**: each agent type has a config specifying its ACP adapter package and main agent package name
- Currently configured agents: PI (`@rivet-dev/agent-os-pi`), PI CLI (`@rivet-dev/agent-os-pi-cli`), OpenCode (`@rivet-dev/agent-os-opencode`), Claude (`@rivet-dev/agent-os-claude`), and Codex (`@rivet-dev/agent-os-codex-agent` + `@rivet-dev/agent-os-codex`).
- **No host agent exceptions.** Host-native wrappers and host binary launch paths are not allowed. OpenCode support must use the real upstream OpenCode implementation rebuilt into the VM adapter package and executed inside the VM.
- `createSession("pi")` spawns the ACP adapter inside the VM, which calls the Pi SDK directly
- Keep `src/agents.ts` aligned with the shipped registry agent packages. Derive the built-in `AgentType` union from `AGENT_CONFIGS` instead of maintaining a separate manual list, and verify launch args/env with the mock-adapter session tests when adding or changing an agent.
- ACP agents that issue live `session/request_permission` calls during `session/prompt` cannot rely on queued session events alone. Route those permission round-trips through the sidecar callback channel (`SidecarRequestPayload`) so the host can answer them before the prompt request completes.
- On the native sidecar path, a top-level `session/cancel` request does not preempt an already running top-level `session/prompt` dispatch. If prompt callers must observe cancellation immediately, resolve the pending prompt request locally in `src/agent-os.ts` while still forwarding the real cancel RPC for eventual adapter/process cleanup.
- Native-sidecar ACP request timeouts should surface as JSON-RPC errors with `error.data.kind === "acp_timeout"` rather than string-only transport errors. Use `isAcpTimeoutErrorData()` from `src/json-rpc.ts` instead of parsing timeout messages.

### Agent Adapter Approaches

Each agent type can have two adapter approaches:
- **SDK adapter** (default) -- Embeds the agent SDK directly via library import (`createAgentSession()`). Lower memory footprint (~100MB less for Pi). Binary: `pi-sdk-acp`. Package: `@rivet-dev/agent-os-pi`. Agent ID: `pi`.
- **CLI adapter** -- Spawns the full agent CLI as a headless subprocess via its ACP adapter (`pi-acp` spawns `pi --mode rpc`). Higher memory overhead but provides full CLI feature set. Binary: `pi-acp`. Package: `@rivet-dev/agent-os-pi-cli`. Agent ID: `pi-cli`.

### Agent Configs

Each agent type needs:
- `acpAdapter`: npm package name for the ACP adapter (e.g., `@rivet-dev/agent-os-pi`)
- `agentPackage`: npm package name for the underlying agent (e.g., `@mariozechner/pi-coding-agent`)
- Any environment variables or flags needed
- Package-provided agent descriptors registered through `processSoftware()` override the hardcoded `AGENT_CONFIGS` entries at session launch time. If a default shell/env tweak matters for both built-in and packaged flows, keep the two config surfaces in sync.

## Testing

- **Framework**: vitest
- **Always run scoped tests, never the full suite.**
  - `pnpm --dir packages/core exec vitest run tests/path/to/file.test.ts` or `pnpm --dir packages/core exec vitest run -t "test name pattern"`
  - Never run bare `pnpm test` without a filter -- integration tests can hang indefinitely.
  - Use low timeouts for test commands (60000ms max).
- For `tests/wasm-commands.test.ts`, broad `-t "grep"` or `-t "sed"` filters can pull in unrelated `rg`, `gzip`, or cross-package pipeline coverage via substring matches. When a story only gates the `grep`/`sed` blocks, use the explicit case names or a narrower `--testNamePattern` that only matches those block entries.
- Cross-workspace suites like `registry/tests/*` import `@rivet-dev/agent-os-core` from `packages/core/dist`, not directly from `src/`. After changing exported test-runtime code such as `src/runtime-compat.ts`, rebuild `packages/core` before trusting registry/package Vitest results.
- **Always verify related tests pass before considering work done.**
- **All tests run inside the VM** -- network servers, file I/O, agent processes.
- For `vm.exec()` cwd/path tests, prefer setting up files from inside the guest shell when the assertion is about command resolution or relative paths. VM filesystem API writes becoming visible to host-backed runtimes is a separate shadow-sync surface and should be tested independently.
- For active agent-session/bash-tool filesystem regressions, cover the host read path in `tests/filesystem.test.ts` with a Claude llmock prompt. Long-lived session processes keep writing into the sidecar shadow root after a tool call returns, so `vm.readFile()`/`vm.stat()` need shadow reconciliation before the session itself exits.
- Session tests that need launch argv or OS-instruction assertions should inspect `getSessionAgentInfo(sessionId)` from sidecar state instead of spying on `kernel.spawn`; `createSession()` now launches through sidecar RPCs.
- `closeSession()` is intentionally fire-and-forget. Cleanup tests can await the internal `_sessionClosePromises` map when they need deterministic post-close assertions, but active-prompt cancellation cases should trigger the public close and then assert on resource release plus prompt error outcome separately, because the in-flight ACP request and the close request share the same sidecar connection.
- If you add or change a fire-and-forget session close path in `src/agent-os.ts`, attach a local `.catch(() => {})` to the dropped promise. The real close result is still exposed through `_sessionClosePromises`, and dropping the promise entirely turns shared-runtime close races into unhandled rejection noise in Vitest.
- Pi CLI session state currently reports the shared V8 host PID when multiple ACP sessions share one JavaScript runtime child. In cleanup tests, treat only host PIDs that are unique to a session as dedicated session roots; a shared PID is runtime-wide context, not three distinct leaked processes.
- Network tests on the native sidecar path should stick to listener bind/state assertions unless the bridge work explicitly targets guest HTTP/client round-trips. `vm.fetch()` does not currently translate arbitrary guest listener ports back to the host, and guest `net.connect()` coverage is still limited.
- For `tests/wasm-commands.test.ts` curl coverage, prefer a guest `net.createServer()` HTTP fixture over guest `http.createServer()` when the story is about the curl/WASM client path. The HTTP-server transport wrapper is a separate compatibility surface and can hide or conflate curl regressions.
- Layer lifecycle regressions should be covered in both `tests/layers.test.ts` for in-memory snapshot reuse/composition semantics and `crates/sidecar/tests/layer_management.rs` for VM-scoped layer RPC isolation; the package-level suite alone does not prove per-VM ownership boundaries.
- For guest-JavaScript startup diagnostics, isolate each suspect import or constructor in its own fresh VM. Once a V8-side probe wedges or times out, later `node` spawns in the same VM can degrade into generic broken-pipe noise instead of the original failure.
- Agent tests must be run sequentially in layers:
  1. PI headless mode (spawn pi directly, verify output)
  2. pi-acp manual spawn (JSON-RPC over stdio)
  3. Full `createSession()` API
- **API tokens**: All tests use `@copilotkit/llmock` with `ANTHROPIC_API_KEY='mock-key'`. No real API tokens needed. Do not load tokens from `~/misc/env.txt` or any external file.
- **Mock LLM testing**: Use `@copilotkit/llmock` to run a mock LLM server on the HOST (not inside the VM). Use `loopbackExemptPorts` in `AgentOs.create()` to exempt the mock port from SSRF checks. The kernel needs `permissions: allowAll` for network access.
- Compat-kernel loopback exemptions are sticky VM config. When `src/runtime-compat.ts` reconfigures a VM later to mount command directories, resend `loopbackExemptPorts` on every `configureVm()` call and seed the same port list into create-VM metadata so guest networking sees it before and after reconfiguration.
- **Pi SDK llmock setup**: Pi reads Anthropic endpoints from `~/.pi/agent/models.json`, not `ANTHROPIC_BASE_URL`. For `createSession("pi")` tests, write a provider override such as `{ "providers": { "anthropic": { "baseUrl": "<llmock-url>", "apiKey": "mock-key" } } }` inside the VM before creating the session.
- Pi headless llmock tests should still pass `ANTHROPIC_BASE_URL` through the session env even with the `~/.pi/agent/models.json` override, because some Pi SDK request paths still consult the env-configured base URL during ACP-driven tool turns.
- **Module access**: Set `moduleAccessCwd` in `AgentOs.create()` to a host dir with `node_modules/`. pnpm puts devDeps in `packages/core/node_modules/`.
- Pi bash-tool E2E coverage depends on registry WASM commands being built locally. Gate those tests with `tests/helpers/registry-commands.ts` `hasRegistryCommands` and include the `@rivet-dev/agent-os-common` software package only when the command artifacts exist.
- `tests/claude-session.test.ts` is the Claude SDK truth suite. It runs the real `@anthropic-ai/claude-agent-sdk` session path through llmock and covers PATH-backed `xu`, text-only replies, nested `node` `execSync` and `spawn`, metadata, lifecycle, and mode updates. Run it with `pnpm --dir packages/core exec vitest run tests/claude-session.test.ts --reporter=verbose` when verifying Claude regressions.
- **Kernel permissions are declarative pass-through config.** `AgentOsOptions.permissions` should stay JSON-serializable and be forwarded to the native sidecar without host-side probing or callback evaluation; Rust owns glob matching and policy decisions.

### Test Structure

See `.agent/specs/test-structure.md` for the full restructuring plan. Target layout:

- `unit/` -- no VM, no sidecar; pure logic (host-tools Zod conversion, descriptors, cron manager, etc.)
- `filesystem/` -- VFS CRUD, overlay, mount, layers, host-dir
- `process/` -- execution, signals, process tree, flat API wrappers
- `session/` -- ACP lifecycle, events, capabilities, MCP, cancellation
- `agents/{pi,claude,opencode,codex}/` -- per-agent adapter tests
- `wasm/` -- WASM command and permission tier tests
- `network/` -- connectivity and fetch behavior inside the VM
- Host tool command-path coverage belongs with VM-backed sidecar tests such as `tests/sidecar-tool-dispatch.test.ts`, not a standalone TypeScript RPC server suite.
- Shell-backed host-tool dispatch coverage in `tests/sidecar-tool-dispatch.test.ts` needs the `@rivet-dev/agent-os-common` software package in the test VM so `/bin/sh` exists; otherwise the suite only proves direct spawn/RPC dispatch and misses the guest-shell path.
- `sidecar/` -- sidecar client, native process
- `cron/` -- cron integration

### WASM Binaries and Quickstart Examples

- **WASM command binaries are not checked into git.** The `registry/software/*/wasm/` directories are build artifacts.
- **Quickstart examples that use `exec()` or shell commands require WASM binaries.** Without them, these fail with "No shell available."
- **To build WASM binaries locally:** Run `make` in `registry/native/`, then `make copy-wasm` and `make build` in `registry/`. Requires Rust nightly + wasi-sdk.
- **Examples that work without WASM binaries:** `hello-world.ts`, `filesystem.ts`, `cron.ts` (schedule/cancel only).
- **When testing quickstart examples**, don't treat WASM-dependent failures as regressions unless the WASM binaries are present.

### Known VM Limitations

- `globalThis.fetch` is hardened (non-writable) in the VM -- can't be mocked in-process
- Kernel child_process.spawn can't resolve bare commands from PATH (e.g., `pi`). Use `PI_ACP_PI_COMMAND` env var to point to the `.js` entry directly.
- `allProcesses()` / `processTree()` on the native sidecar path only surface the top-level tracked runtime processes. Guest-local `child_process.spawn()` children still report guest PIDs to user code, but they do not appear as separate kernel process-tree nodes yet.
- `kernel.readFile()` does NOT see the ModuleAccessFileSystem overlay -- read host files directly with `readFileSync` for package.json resolution
- Native ELF binaries cannot execute in the VM -- the kernel's command resolver only handles `.js`/`.mjs`/`.cjs` scripts and WASM commands.
- Projected native assets under `/root/node_modules` are readable through module access, but guest `child_process.spawn*()` still routes them through the VM command resolver; spawning a projected ELF currently fails during WASM warmup instead of executing host-native code.
- The native sidecar framed stdio client is bidirectional: host-originated `request`/`response` frames use positive `request_id` values, and sidecar-originated `sidecar_request`/`sidecar_response` frames use negative IDs. When adding host callbacks, register a sidecar request handler instead of assuming stdout only carries events plus responses.

### Debugging Policy

- **Never guess without concrete logs.** Every assertion about what's happening at runtime must be backed by log output. Add logs at every decision point and trace the full execution path before drawing conclusions. Never assume something is a timeout issue unless there are logs proving the system was actively busy for the entire duration.
- **Never use CJS transpilation as a workaround** for ESM module loading issues. Fix root causes in the ESM resolver, module access overlay, or V8 runtime.
- **Maintain a friction log** at `.agent/notes/vm-friction.md` for anything that behaves differently from a standard POSIX/Node.js system.
