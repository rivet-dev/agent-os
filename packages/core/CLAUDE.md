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
- Keep cron schedule validation and `nextRun` computation on the shared helpers in `src/cron/parse-schedule.ts`; if `CronManager` and `TimerScheduleDriver` parse or reject schedules differently, `listCronJobs()` can advertise jobs the driver refuses (or immediately fires) and the API becomes self-contradictory.
- Native sidecar execution requests should stay unresolved on the TypeScript side. Forward `command`, `args`, `cwd`, and VM config through the wire payload, and let Rust own command lookup, guest-path to host-path mapping, shadow materialization, and `AGENT_OS_*` runtime env assembly.
- Native sidecar `exec()` should keep shell-sensitive commands on the `sh -c` wrapper path so cwd changes, pipelines, and other shell semantics stay truthful, but shell-free simple commands can use the direct spawn fast path regardless of driver. For Wasm commands in `src/sidecar/rpc-client.ts`, direct spawn preserves the real guest exit status for external-command failures like `cat /missing`, while the `sh -c` wrapper can swallow that non-zero status even when stderr is correct.
- In `src/sidecar/rpc-client.ts`, `&&` command chains must stay on a single guest `sh -c` execution. Splitting them into separate `exec()` calls loses shell state like `cd` and changes where relative redirects write.
- In `src/sidecar/rpc-client.ts`, only take the redirect fast path when the parsed command actually includes `<`, `>`, or `>>`. Bare WASM commands like `pwd` must stay on the shell-wrapper path or they bypass the explicit `cd` fixup and start in `/` instead of the VM home cwd.
- In `src/sidecar/rpc-client.ts`, keep the shell wrapper as `cd ... || exit` followed by the target command and trust the shell process exit code directly. Temp-file or assignment-based `$?` capture on the brush path is brittle: shell redirection can leave the file empty, inject `exit` parse errors into stderr, and silently turn failing guest commands green.
- In `src/sidecar/rpc-client.ts`, the simple-command parsers must preserve backslashes for non-shell-special escapes inside double quotes. Commands like `printf "a\\nb\\n" > file` rely on the guest command seeing the literal `\n` bytes; only `\"`, `\\`, `\$`, ``\` ``, and line-continuation newlines should collapse on the native-sidecar fast path.
- In `src/sidecar/rpc-client.ts`, treat bare unquoted `!` as shell syntax, not as a direct-fast-path token. Commands like `test ! -f /tmp/file` rely on guest shell semantics, and bypassing the shell can flip the observed exit code even when the underlying file operation succeeded.
- If a file must be visible to both `vm.readFile()` and guest shell commands, it cannot live only in a local compat mount. Put it on a real sidecar-visible path or mount, and keep any read-only guarantees enforced below the TypeScript proxy layer.
- Host tool registration is split across the boundary: TypeScript converts Zod schemas to JSON Schema, validates sidecar tool invocations, and runs the local `execute()` callbacks, while the sidecar owns CLI flag parsing, `agentos` command dispatch, and prompt-markdown generation via `register_toolkit`.
- Host-tool `inputSchema` conversion in `src/host-tools-zod.ts` is intentionally fail-closed. Support only the Zod subset that round-trips cleanly into the sidecar-facing JSON Schema contract; if a schema would degrade semantics or emit `$ref`/`$defs` (`discriminatedUnion`, `intersection`, `tuple`, `record`, `date`, `bigint`, custom refinements, metadata `id`, etc.), throw `HostToolSchemaConversionError` with the offending field path instead of coercing it to `{ type: "string" }`.
- The host-tool description limit is a cross-boundary contract: keep the 200-character maximum aligned between `src/host-tools.ts` and Rust `register_toolkit` validation in `crates/sidecar/src/tools.rs`, with boundary tests on both sides when changing it.
- `src/sidecar/rpc-client.ts` is the consolidated home for framed sidecar I/O, compat proxy helpers, and sidecar descriptor serializers. Keep shared/explicit sidecar pool and VM lease bookkeeping in `src/agent-os.ts` rather than reintroducing another sidecar lifecycle layer.
- In `src/agent-os.ts`, shell teardown is two-phase: public `_shells` entries can disappear immediately on `closeShell()`, but `dispose()` must still await the separate pending shell-exit set before dropping the sidecar event listener, or late shell stdout/exit delivery can race into a closed bridge.
- The native sidecar framed stdio path now defaults to the BARE payload codec. Keep any JSON payload support behind explicit migration-only opts such as `payloadCodec: "json"`, and remember that BARE structs need every positional field serialized explicitly across the Rust/TypeScript boundary rather than relying on JSON-style `skip_serializing_if` omissions.
- In `src/sidecar/native-process-client.ts`, treat `child.on("exit")` and `child.on("error")` as the authoritative terminal-disconnect path for framed stdio clients. `stdout` can close before Node fills in `exitCode`/`signalCode`, so reject in-flight RPCs with a typed disconnect immediately and upgrade the stored terminal error once the concrete exit metadata arrives.
- In the native-sidecar event path, long-lived background loops should call `waitForEvent()` in abortable no-timeout mode instead of parking a multi-hour timeout sentinel. The abort signal is the cancellation mechanism; the timeout itself becomes the regression surface on idle VMs.
- For native-sidecar BARE ACP session bootstrap payloads, keep `SessionCreatedResponse` aligned with `crates/sidecar/protocol/agent_os_sidecar_v1.bare`: `sessionId` is the first positional field on the wire, before optional `pid`, `modes`, `configOptions`, `agentCapabilities`, and `agentInfo`. If the TypeScript decoder reads `session_id` last, every `createSession()` response desynchronizes.
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
- In `createSession()`, treat `skipOsInstructions` as "skip the base `/etc/agentos/instructions.md` text" only. Still call agent `prepareInstructions(...)` when session-level `additionalInstructions` or tool-reference content exists, and forward `skipBase` through the options object instead of blanking out the caller's extra instructions.
- ACP agents that issue live `session/request_permission` calls during `session/prompt` cannot rely on queued session events alone. Route those permission round-trips through the sidecar callback channel (`SidecarRequestPayload`) so the host can answer them before the prompt request completes.
- Native-sidecar inbound ACP host callbacks are explicit sidecar-request payloads now. If Rust forwards an unknown ACP JSON-RPC request, answer it through `SidecarRequestPayload.type === "acp_request"` with an `acp_request_result` JSON-RPC response; otherwise the sidecar will only synthesize `-32601` after the callback transport is unavailable or times out.
- Host ACP callbacks in `src/agent-os.ts` are no longer a generic `-32601` stub: keep the dispatcher aligned with both the newer `fs/read` / `fs/write` / `fs/readDir` / `terminal/*` method names and the legacy aliases still exercised by native-sidecar tests such as `fs/read_text_file` and `fs/write_text_file`.
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
- **Prefer scoped tests while iterating.**
  - `pnpm --dir packages/core exec vitest run tests/path/to/file.test.ts` or `pnpm --dir packages/core exec vitest run -t "test name pattern"`
  - Repo-root `pnpm test` is the RC sweep and exits cleanly; it is still too broad for normal iteration.
  - `pnpm --dir packages/core test` intentionally uses Vitest's `verbose` reporter because `tests/wasm-commands.test.ts` and similar long-running VM suites otherwise sit silent for minutes and get misread as hangs during `US-088` sweeps.
  - Use low timeouts for test commands (60000ms max).
- The vitest setup file at `tests/helpers/default-vm-permissions.ts` patches `AgentOs.create()` and disposes every cached shared sidecar via `__disposeAllSharedSidecarsForTesting()` in `afterAll`. Workers can hang on exit if the shared sidecar's piped stdio handles stay open, so any new test entrypoints that bypass this setup file must dispose their sidecars themselves.
- `NativeSidecarProcessClient.dispose()` enforces a graceful exit window then `SIGKILL`s the child if it ignores stdin EOF; `tests/native-sidecar-process.test.ts` covers the regression so future changes cannot reintroduce an unbounded teardown wait.
- In `packages/core` tests that capture `spawn()` stdout/stderr via callbacks and then call `waitProcess(pid)`, drain one macrotask (`await new Promise((resolve) => setTimeout(resolve, 0))`) before asserting on the buffered strings. Native-sidecar `process_output` events can arrive one turn after the exit notification, and tiny outputs like `curl -s` bodies are the first thing to get lost if you snapshot immediately.
- `NativeSidecarProcessClient.waitForEvent(...)` supports indexed `SidecarEventSelector` objects; prefer selectors over ad hoc lambdas on shared sidecar clients so buffered events stay O(1) to retrieve and `ownership` can pin a wait to one VM/session.
- The native sidecar client's unmatched event buffer is intentionally bounded and fail-closed. If a test or runtime path can leave `runEventPump` idle while output events stream, expect `SidecarEventBufferOverflow` rather than unbounded buffering, and set a larger `eventBufferCapacity` explicitly only for cases that truly need it.
- When Node/Vitest code needs to shell out to Cargo, resolve it through `src/sidecar/cargo.ts` instead of assuming a login shell already put `~/.cargo/bin` on `PATH`.
- For `tests/wasm-commands.test.ts`, broad `-t "grep"` or `-t "sed"` filters can pull in unrelated `rg`, `gzip`, or cross-package pipeline coverage via substring matches. When a story only gates the `grep`/`sed` blocks, use the explicit case names or a narrower `--testNamePattern` that only matches those block entries.
- For `tests/wasm-commands.test.ts` and similar long-running VM truth suites, prefer one shared VM per `describe(...)` block over one VM per individual test unless the case truly needs pristine bootstrap state. Per-test VM boots push the file into multi-minute runtimes and make the RC sweep look hung even when it is still progressing.
- Cross-workspace suites like `registry/tests/*` import `@rivet-dev/agent-os-core` from `packages/core/dist`, not directly from `src/`. After changing exported test-runtime code such as `src/runtime-compat.ts`, rebuild `packages/core` before trusting registry/package Vitest results.
- The `examples/quickstart` package also resolves `@rivet-dev/agent-os-core` from `packages/core/dist`; after TypeScript changes in `packages/core/src`, rebuild `packages/core` before rerunning quickstart acceptance commands.
- The synthetic `openShell()` fallback in `src/sidecar/rpc-client.ts` needs PTY-style output semantics for xterm-based harnesses: normalize terminal-visible line endings to `\r\n`, and route command stderr through the main `onData` stream instead of treating it like a separate non-PTY stderr channel.
- **Always verify related tests pass before considering work done.**
- **All tests run inside the VM** -- network servers, file I/O, agent processes.
- For `vm.exec()` cwd/path tests, prefer setting up files from inside the guest shell when the assertion is about command resolution or relative paths. VM filesystem API writes becoming visible to host-backed runtimes is a separate shadow-sync surface and should be tested independently.
- For active agent-session/bash-tool filesystem regressions, cover the host read path in `tests/filesystem.test.ts` with a Claude llmock prompt. Long-lived session processes keep writing into the sidecar shadow root after a tool call returns, so `vm.readFile()`/`vm.stat()` need shadow reconciliation before the session itself exits.
- Session tests that need launch argv or OS-instruction assertions should inspect `getSessionAgentInfo(sessionId)` from sidecar state instead of spying on `kernel.spawn`; `createSession()` now launches through sidecar RPCs.
- `closeSession()` is intentionally fire-and-forget. Cleanup tests can await the internal `_sessionClosePromises` map when they need deterministic post-close assertions, but active-prompt cancellation cases should trigger the public close and then assert on resource release plus prompt error outcome separately, because the in-flight ACP request and the close request share the same sidecar connection.
- If you add or change a fire-and-forget session close path in `src/agent-os.ts`, attach a local `.catch(() => {})` to the dropped promise. The real close result is still exposed through `_sessionClosePromises`, and dropping the promise entirely turns shared-runtime close races into unhandled rejection noise in Vitest.
- Pi CLI session state currently reports the shared V8 host PID when multiple ACP sessions share one JavaScript runtime child. In cleanup tests, treat only host PIDs that are unique to a session as dedicated session roots; a shared PID is runtime-wide context, not three distinct leaked processes.
- For projected npm CLIs in package tests, prefer `node /root/node_modules/<pkg>/dist/<entry>.js` over `/root/node_modules/.bin/*`. pnpm's generated `.bin` wrappers embed host filesystem paths, which are not stable or guest-visible inside the VM.
- Browserbase VM tests should read credentials from host env as `BROWSER_BASE_API_KEY` / `BROWSER_BASE_PROJECT_ID`, alias them to `BROWSERBASE_API_KEY` / `BROWSERBASE_PROJECT_ID` in the guest env, and keep VM `network` permissions narrowed to `dns://*.browserbase.com` plus `tcp://*.browserbase.com:*` so remote Browserbase sessions work while direct guest egress stays denied.
- For Browserbase e2e flows inside the VM, prefer a small guest `fetch()` helper that creates/releases the Browserbase session plus `node /root/node_modules/@browserbasehq/browse-cli/dist/index.js --ws <connectUrl> ...` over the browse daemon session socket path. The direct `--ws` mode avoids a guest-local Unix-socket control hop and keeps the test focused on Browserbase API plus CDP connectivity.
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
- Compat-kernel `createKernel()` bootstraps sidecar VMs under a temporary internal `allowAll` only when the caller provided explicit permissions, then reapplies the requested policy in `configureVm()` after local mounts and `/bin/*` command stubs are in place. Skipping that handoff makes default-deny VMs block their own runtime/bootstrap writes before the guest policy ever takes effect.
- In `src/runtime-compat.ts`, `rootView.exists("/bin/<command>")` can return `true` from the kernel command registry before the sidecar shadow root has a real stub file. If a host-backed runtime needs the command visible on disk, materialize the stub unconditionally instead of skipping on `exists()`.
- In `src/runtime-compat.ts`, custom `createKernel({ filesystem })` snapshots need to be replayed through guest filesystem calls after `createVm()` when permissions allow it. Loading the root snapshot into the kernel alone is not enough for shell-launched WASM commands, because they read the sidecar shadow root and will miss pre-seeded files like `/hello.txt` unless those entries are mirrored there too.
- In `src/runtime-compat.ts`, `createWasmVmRuntime({ commandDirs })` is a stateful command-dir descriptor, not just a static command list: keep symlink-to-WASM alias discovery, basename-based `tryResolve()` for late-added binaries, and the descriptor’s internal command-path/module-cache bookkeeping aligned with the kernel mount path or the registry dynamic-module truth tests will drift out of sync.
- In `src/runtime-compat.ts`, `NativeKernel.processes` is not automatically shared with the native-sidecar proxy map. When `spawn()` wraps `proxy.spawn(...)`, mirror the proxy snapshot into `kernel.processes` immediately and after `wait()` so registry integration tests that read `kernel.processes.get(pid)` see the same root-process status transitions as the public compat kernel.
- Declarative sidecar permission rules must use explicit `["*"]` wildcards for rule `operations` and `paths`/`patterns`; empty arrays are rejected by the native sidecar instead of being treated as implicit wildcards.
- **Pi SDK llmock setup**: Pi reads Anthropic endpoints from `~/.pi/agent/models.json`, not `ANTHROPIC_BASE_URL`. For `createSession("pi")` tests, write a provider override such as `{ "providers": { "anthropic": { "baseUrl": "<llmock-url>", "apiKey": "mock-key" } } }` inside the VM before creating the session.
- Pi headless llmock tests should still pass `ANTHROPIC_BASE_URL` through the session env even with the `~/.pi/agent/models.json` override, because some Pi SDK request paths still consult the env-configured base URL during ACP-driven tool turns.
- `packages/core` agent-session tests execute registry agent workspaces through their built `dist`/bin artifacts. After changing an adapter under `registry/agent/*/src`, rebuild that workspace before trusting the core Vitest result.
- Keep Claude's default `CLAUDE_CODE_NODE_SHELL_WRAPPER` enabled (`"1"`) in both `src/agents.ts` and `registry/agent/claude/src/index.ts`. Forcing it to `"0"` breaks real Bash-tool execution under llmock-backed sessions: shell redirections can still create empty files, but the command output/tool result never lands, which regresses `tests/claude-session.test.ts` and filesystem visibility checks.
- Registry/kernel suites that import `@rivet-dev/agent-os-core/test/runtime` read `packages/core/dist/test/runtime.js`, not the TypeScript sources directly. After changing `src/runtime-compat.ts`, `src/sidecar/rpc-client.ts`, or other runtime-test surfaces, run `pnpm --dir packages/core build` before rerunning those registry Vitest files or they will keep exercising stale code.
- **Module access**: Set `moduleAccessCwd` in `AgentOs.create()` to a host dir with `node_modules/`. pnpm puts devDeps in `packages/core/node_modules/`.
- Quickstarts and integration tests that run full-tier registry commands (for example `@rivet-dev/agent-os-git`) should set both `moduleAccessCwd` and explicit `permissions` on `AgentOs.create()`. Relying on ambient `process.cwd()` can project broken workspace-root symlinks into the VM, and omitting permissions now defaults the native sidecar to deny-all instead of silently allowing access.
- S3-backed core tests can use `tests/helpers/mock-s3.ts` as the explicit local harness instead of Docker/MinIO; when the endpoint resolves to `127.0.0.1` or `localhost`, set `AGENT_OS_ALLOW_LOCAL_S3_ENDPOINTS=1` before creating the VM so the sidecar accepts the local test endpoint.
- Sandbox toolkit quickstarts/tests that depend on external Docker should use an explicit `SKIP_DOCKER=1` gate instead of `skipIf`, and the truthful host-tool path is to read `AGENTOS_TOOLS_PORT` inside the VM and `POST` `{ toolkit, tool, input }` to `http://127.0.0.1:$AGENTOS_TOOLS_PORT/call` from a guest Node script.
- Shared Vitest helpers under `src/test/` should register optional capability coverage conditionally in code instead of with `describe.skipIf` / `test.skipIf`; `US-088` treats those markers as product-debt skips even when they only guard backend capability differences.
- Pi bash-tool E2E coverage depends on registry WASM commands being built locally. Gate those tests with `tests/helpers/registry-commands.ts` `hasRegistryCommands` and include the `@rivet-dev/agent-os-common` software package only when the command artifacts exist.
- Registry package tests for C-built commands such as `duckdb` and `http_get` should also go through `tests/helpers/registry-commands.ts`: prefer copied `registry/software/*/wasm` artifacts, fall back to `registry/native/c/build` when available, and let the helper build missing C-source artifacts on demand before declaring the command unavailable. When bootstrapping from `registry/native/c`, build `make sysroot` first and then run a second `make` for the concrete `build/...` targets so `SYSROOT` resolves to the patched tree instead of the vanilla SDK sysroot chosen at parse time; in that second pass, treat `sysroot/lib/wasm32-wasi/libc.a` as already built so `make` does not loop back through the patch pipeline because of preserved sysroot timestamps.
- `tests/claude-session.test.ts` is the Claude SDK truth suite. It runs the real `@anthropic-ai/claude-agent-sdk` session path through llmock and covers PATH-backed `xu`, text-only replies, nested `node` `execSync` and `spawn`, metadata, lifecycle, and mode updates. Run it with `pnpm --dir packages/core exec vitest run tests/claude-session.test.ts --reporter=verbose` when verifying Claude regressions.
- **Kernel permissions are declarative pass-through config.** `AgentOsOptions.permissions` should stay JSON-serializable and be forwarded to the native sidecar without host-side probing or callback evaluation; Rust owns glob matching and policy decisions.
- ACP session event retention is ack-based now: `AgentOs` must track the highest sequenced ACP event seen per session separately from the truncated local `events` array, pass that cursor through `getSessionState(...)`, and keep its local retention cap aligned with the sidecar's bounded session buffer.
- Public `onSessionEvent()` now replays buffered `session/update` notifications synchronously before returning. Internal collectors that only want future chunks, such as `prompt()`, should subscribe through the private no-replay path or they will re-consume historical output.
- ACP initialize intent belongs in `AgentOs.createSession()`: when the caller's ACP `protocolVersion` or `clientCapabilities` change, pass them through `src/sidecar/native-process-client.ts` instead of re-hardcoding initialize defaults in the Rust sidecar.
- **Sidecar permission path patterns preserve `*` vs `**`.** Use single-segment globs such as `/workspace/*` only for direct children; use `/workspace/**` when the VM should reach nested paths through the native sidecar permission policy.
- **Native-sidecar socket/process inspection is explicit now.** If a `Kernel` or `NativeSidecarProcessClient` caller needs `findListener()`, `findBoundUdp()`, or `getProcessSnapshot()`, grant `network.inspect` and/or `process.inspect` in the forwarded permissions; broad `network.listen` or `childProcess` access is not enough on its own.
- **Host tool invocation is its own permission surface.** Guest `agentos-*`/tools-RPC calls must grant `permissions.tool` with `invoke` rules that match `<toolkit>:<tool>` patterns; if the same test/example also boots guest command software, keep `fs` and `childProcess` permissions explicit because command execution still needs those guest-visible capabilities.
- `packages/core` Vitest now patches `AgentOs.create()` in `tests/helpers/default-vm-permissions.ts` to inject explicit allow-all permissions only when a suite omits them. Permission-focused tests must still pass their own `permissions` object so they exercise the real default-deny path instead of the generic test harness default.

### Test Structure

See `.agent/specs/test-structure.md` for the full restructuring plan. Target layout:

- `unit/` -- no VM, no sidecar; pure logic (host-tools Zod conversion, descriptors, cron manager, etc.)
- `filesystem/` -- VFS CRUD, overlay, mount, layers, host-dir
- Shared filesystem conformance coverage in `src/test/file-system.ts` is fail-closed: backend-specific deviations must be modeled as explicit `capabilities` flags on the test descriptor, never with permissive `try/catch` branches that treat any thrown error as success.
- `process/` -- execution, signals, process tree, flat API wrappers
- `session/` -- ACP lifecycle, events, capabilities, MCP, cancellation
- `agents/{pi,claude,opencode,codex}/` -- per-agent adapter tests
- `wasm/` -- WASM command and permission tier tests
- `network/` -- connectivity and fetch behavior inside the VM
- `tests/migration-parity.test.ts` is the dedicated Rust/native migration gate. Keep it on the default `AgentOs.create()` sidecar path and make it cover filesystem, process, layer snapshot, tool dispatch, networking, and at least one real agent prompt/session flow together; the canonical invocation is `pnpm test:migration-parity` from the repo root.
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
- `allProcesses()` / `processTree()` on the native sidecar path should be derived from the VM's active-process snapshot rather than host `ps` output. Preserve the public `spawn()` PID for root processes by remapping the sidecar's kernel PID back through the root `process_id`, so nested guest `child_process.spawn()` children remain visible under the user-facing parent PID.
- `kernel.readFile()` does NOT see the ModuleAccessFileSystem overlay -- read host files directly with `readFileSync` for package.json resolution
- Native ELF binaries cannot execute in the VM -- the kernel's command resolver only handles `.js`/`.mjs`/`.cjs` scripts and WASM commands.
- Projected native assets under `/root/node_modules` are readable through module access, but guest `child_process.spawn*()` still routes them through the VM command resolver; spawning a projected ELF currently fails during WASM warmup instead of executing host-native code.
- The native sidecar framed stdio client is bidirectional: host-originated `request`/`response` frames use positive `request_id` values, and sidecar-originated `sidecar_request`/`sidecar_response` frames use negative IDs. When adding host callbacks, register a sidecar request handler instead of assuming stdout only carries events plus responses.

### Debugging Policy

- **Never guess without concrete logs.** Every assertion about what's happening at runtime must be backed by log output. Add logs at every decision point and trace the full execution path before drawing conclusions. Never assume something is a timeout issue unless there are logs proving the system was actively busy for the entire duration.
- **Never use CJS transpilation as a workaround** for ESM module loading issues. Fix root causes in the ESM resolver, module access overlay, or V8 runtime.
- **Maintain a friction log** at `.agent/notes/vm-friction.md` for anything that behaves differently from a standard POSIX/Node.js system.
