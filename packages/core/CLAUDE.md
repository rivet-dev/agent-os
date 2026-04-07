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

## Agent Sessions (ACP)

- Uses the **Agent Communication Protocol** (ACP) -- JSON-RPC 2.0 over stdio (newline-delimited)
- No HTTP adapter layer; communicate directly with agent ACP adapters over stdin/stdout
- Reference `~/sandbox-agent` for ACP integration patterns. Do not copy code from it.
- ACP docs: https://agentclientprotocol.com/get-started/introduction
- Session design is **agent-agnostic**: each agent type has a config specifying its ACP adapter package and main agent package name
- Currently configured agents: PI (`@rivet-dev/agent-os-pi`), PI CLI (`@rivet-dev/agent-os-pi-cli`), OpenCode (`@rivet-dev/agent-os-opencode`), Claude (`@rivet-dev/agent-os-claude`).
- **No host agent exceptions.** Host-native wrappers and host binary launch paths are not allowed. OpenCode support must use the real upstream OpenCode implementation rebuilt into the VM adapter package and executed inside the VM.
- `createSession("pi")` spawns the ACP adapter inside the VM, which calls the Pi SDK directly

### Agent Adapter Approaches

Each agent type can have two adapter approaches:
- **SDK adapter** (default) -- Embeds the agent SDK directly via library import (`createAgentSession()`). Lower memory footprint (~100MB less for Pi). Binary: `pi-sdk-acp`. Package: `@rivet-dev/agent-os-pi`. Agent ID: `pi`.
- **CLI adapter** -- Spawns the full agent CLI as a headless subprocess via its ACP adapter (`pi-acp` spawns `pi --mode rpc`). Higher memory overhead but provides full CLI feature set. Binary: `pi-acp`. Package: `@rivet-dev/agent-os-pi-cli`. Agent ID: `pi-cli`.

### Agent Configs

Each agent type needs:
- `acpAdapter`: npm package name for the ACP adapter (e.g., `@rivet-dev/agent-os-pi`)
- `agentPackage`: npm package name for the underlying agent (e.g., `@mariozechner/pi-coding-agent`)
- Any environment variables or flags needed

## Testing

- **Framework**: vitest
- **Always run scoped tests, never the full suite.**
  - `pnpm --dir packages/core exec vitest run tests/path/to/file.test.ts` or `pnpm --dir packages/core exec vitest run -t "test name pattern"`
  - Never run bare `pnpm test` without a filter -- integration tests can hang indefinitely.
  - Use low timeouts for test commands (60000ms max).
- **Always verify related tests pass before considering work done.**
- **All tests run inside the VM** -- network servers, file I/O, agent processes.
- Network tests: write a server script file, run it with `node` inside the VM, then `vm.fetch()` against it.
- Agent tests must be run sequentially in layers:
  1. PI headless mode (spawn pi directly, verify output)
  2. pi-acp manual spawn (JSON-RPC over stdio)
  3. Full `createSession()` API
- **API tokens**: All tests use `@copilotkit/llmock` with `ANTHROPIC_API_KEY='mock-key'`. No real API tokens needed. Do not load tokens from `~/misc/env.txt` or any external file.
- **Mock LLM testing**: Use `@copilotkit/llmock` to run a mock LLM server on the HOST (not inside the VM). Use `loopbackExemptPorts` in `AgentOs.create()` to exempt the mock port from SSRF checks. The kernel needs `permissions: allowAll` for network access.
- **Module access**: Set `moduleAccessCwd` in `AgentOs.create()` to a host dir with `node_modules/`. pnpm puts devDeps in `packages/core/node_modules/`.
- **Kernel permissions are declarative pass-through config.** `AgentOsOptions.permissions` should stay JSON-serializable and be forwarded to the native sidecar without host-side probing or callback evaluation; Rust owns glob matching and policy decisions.

### Test Structure

See `.agent/specs/test-structure.md` for the full restructuring plan. Target layout:

- `unit/` -- no VM, no sidecar; pure logic (host-tools parsing, descriptors, cron manager, etc.)
- `filesystem/` -- VFS CRUD, overlay, mount, layers, host-dir
- `process/` -- execution, signals, process tree, flat API wrappers
- `session/` -- ACP lifecycle, events, capabilities, MCP, cancellation
- `agents/{pi,claude,opencode,codex}/` -- per-agent adapter tests
- `wasm/` -- WASM command and permission tier tests
- `network/` -- connectivity, host-tools server
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
- `kernel.readFile()` does NOT see the ModuleAccessFileSystem overlay -- read host files directly with `readFileSync` for package.json resolution
- Native ELF binaries cannot execute in the VM -- the kernel's command resolver only handles `.js`/`.mjs`/`.cjs` scripts and WASM commands.
- The native sidecar framed stdio client is bidirectional: host-originated `request`/`response` frames use positive `request_id` values, and sidecar-originated `sidecar_request`/`sidecar_response` frames use negative IDs. When adding host callbacks, register a sidecar request handler instead of assuming stdout only carries events plus responses.

### Debugging Policy

- **Never guess without concrete logs.** Every assertion about what's happening at runtime must be backed by log output. Add logs at every decision point and trace the full execution path before drawing conclusions. Never assume something is a timeout issue unless there are logs proving the system was actively busy for the entire duration.
- **Never use CJS transpilation as a workaround** for ESM module loading issues. Fix root causes in the ESM resolver, module access overlay, or V8 runtime.
- **Maintain a friction log** at `.agent/notes/vm-friction.md` for anything that behaves differently from a standard POSIX/Node.js system.
