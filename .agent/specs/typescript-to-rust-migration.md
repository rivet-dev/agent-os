# Spec: TypeScript-to-Rust Migration

Gut the TypeScript layer. Rust owns everything. TypeScript is a thin SDK that spawns the sidecar, forwards RPC calls, and dispatches events to user callbacks. Nothing else.

## Philosophy

**No legacy support. No backwards compatibility. No migration shims.** This is a clean break. We are designing the system we want, not preserving the system we have. If an existing API was wrong, delete it. If a type was over-exported, stop exporting it. If a pattern was a workaround, don't port the workaround — fix the underlying problem.

Specifically:
- **Breaking changes are free.** Every downstream package (`secure-exec`, `dev-shell`, actor layer, registry packages) gets rewritten to the new API in the same change. No compatibility layers, no deprecation warnings, no dual-code-path transitions.
- **Delete, don't deprecate.** If something is removed, it's gone. No `@deprecated` annotations, no tombstone re-exports, no "legacy" mode.
- **Smallest correct implementation.** Every line of code must justify its existence. No defensive programming against hypothetical future requirements. No abstraction layers "in case we need to swap this out later." No extension points that have zero current users.
- **Port behavior, not code.** When moving logic from TypeScript to Rust, don't transliterate line-by-line. Understand what the code does, why, and implement the cleanest Rust version. Many TypeScript patterns exist because of JS limitations (callback-based permissions, synthetic PIDs, shadow directories) — these problems may not exist in Rust.
- **Agent compatibility workarounds are ported faithfully.** The one exception to "don't port workarounds" is agent protocol compatibility (ACP deduplication, OpenCode synthetic events, cancel fallback). These exist because real agents have real quirks. Port them, but isolate them behind a per-agent compatibility layer so they can be removed when agents fix their implementations.

## Design Principle

The question is not "what can we move to Rust?" — it's "what must stay in TypeScript?" The answer is short:

1. **User callbacks** — `tool.execute()`, `onProcessStdout()`, `onSessionEvent()`, `onCronEvent()`. These are user-provided TypeScript functions. They run in the host Node.js process.
2. **Zod validation** — Users define tool schemas with Zod. Validation stays in TypeScript.
3. **Sidecar lifecycle** — Spawning/killing the Rust binary, IPC setup. ~50 lines.
4. **npm package resolution** — Walking `node_modules/` to find agent packages, reading `package.json` `bin` fields. This is host Node.js filesystem work. Resolved paths are passed to the sidecar during `ConfigureVm`.
5. **Public SDK types** — TypeScript interfaces and type exports for consumers.
6. **JS-bridge filesystem mounts** — Users can mount custom TypeScript `VirtualFileSystem` implementations into the VM. These run in the host process and must be dispatched from TypeScript. (See "JS-Bridge Mounts" section.)
7. **Agent `prepareInstructions` callbacks** — Per-agent instruction preparation is a TypeScript callback that may call back into the sidecar for VFS reads/writes. This stays as a TypeScript callback invoked during session creation. (See "Session Creation Flow" section.)

8. **Cron scheduling** — Timer management, overlap policies, job execution dispatch all stay in TypeScript. The Rust sidecar has no concept of cron. Cron uses sidecar primitives (spawn, createSession) but the scheduling orchestration is TypeScript.

Everything else moves to Rust: ACP protocol, session state, filesystem overlay/layers/snapshots, process management, command resolution, path mapping, kernel permissions, tool virtual processes, shim generation, prompt generation, socket tracking, signal state, process trees.

## Prerequisite: Split service.rs + Async Migration

**Do this before anything else.** `service.rs` is 14,247 lines in a single file. It must be split into focused modules and converted to async before adding any new functionality.

### Step 0a: Split service.rs

Break the monolith into domain modules:

```
crates/sidecar/src/
  service.rs            — top-level dispatch (request routing only, ~500 lines)
  vm.rs                 — VM lifecycle (create, configure, dispose, ~1,500 lines)
  filesystem.rs         — guest filesystem call dispatch (~1,500 lines)
  execution.rs          — process spawn, stdin, kill, networking, event pump (~4,000 lines)
                          Networking (TCP/UDP/Unix sockets, DNS) stays co-located with
                          execution because ActiveProcess owns socket state and sync RPC
                          handlers mutate both process and socket state simultaneously.
  plugins/
    mod.rs              — plugin trait + factory (~100 lines)
    host_dir.rs         — host directory mount plugin
    s3.rs               — S3 mount plugin
    google_drive.rs     — Google Drive mount plugin
    sandbox_agent.rs    — Sandbox Agent mount plugin
    js_bridge.rs        — JS-bridge mount plugin (new — dispatches to TypeScript)
  bootstrap.rs          — root filesystem construction, snapshots (~1,000 lines)
  bridge.rs             — host filesystem, permission bridge (~500 lines)
  protocol.rs           — wire types (already separate, expanded — see "Wire Protocol")
  state.rs              — VmState, SessionState, shared state types (~500 lines)
  acp/                  — added in step 6
    mod.rs              — ACP client, JSON-RPC 2.0 codec
    session.rs          — session state machine
    compat.rs           — per-agent compatibility workarounds
  tools.rs              — added in step 5, virtual process dispatch + shim/prompt gen
```

No behavior changes in 0a. Pure mechanical extraction. Every function keeps its exact signature. Tests must pass identically before and after.

Also extract any `#[cfg(test)] mod tests` blocks from `service.rs` into `crates/sidecar/tests/` files. Inline tests in a 14k-line file are unmaintainable. Only trivial unit tests of private helpers stay inline.

Note: `handle_javascript_sync_rpc_request` is a cross-cutting dispatch hub (~700 lines) that routes to filesystem, networking, child_process, and process operations. It stays in `service.rs` as the coordinator — it delegates to domain modules but owns the routing.

### Step 0b: Async migration

Convert from synchronous `nix::poll` loop to `tokio::select!`:

**Current (`stdio.rs`):**
```rust
loop {
    poll(stdin_fd, timeout);          // blocks
    let request = read_request();     // synchronous
    let response = dispatch(request); // synchronous
    write_response(response);
    poll_execution_events();          // synchronous
}
```

**Target:**
```rust
loop {
    tokio::select! {
        request = stdin.next() => { handle_rpc(request).await }
        event = process_events.recv() => { push_event(event).await }
        // Future steps add more branches:
        // notification = acp_events.recv() => { ... }
        // sidecar_response = sidecar_resp_rx.recv() => { ... }
    }
}
```

**Concurrency model:** Single-task `select!` loop. Only one branch runs at a time. `&mut self` on the sidecar state is sufficient — no `Arc<Mutex<_>>` needed. This is the same concurrency model as the current synchronous loop, just with proper async wakeup instead of polling.

The existing `tokio` dependency (used by S3/sandbox agent plugins) extends to the main event loop. All request handlers become `async fn`.

**TypeScript client change:** The `NativeSidecarProcessClient` must be updated to handle unsolicited push frames from the sidecar (today it only reads events after sending a request). This is part of step 0b, not a later step.

**Combined estimate for 0a + 0b:** ~800-1,200 lines of refactoring. No net-new functionality. All existing tests must pass.

## Target TypeScript Architecture

```
packages/core/src/
  index.ts              — public exports
  agent-os.ts           — SDK class. RPC calls + event dispatch + callback storage.
  types.ts              — public TypeScript types (VirtualStat, ProcessInfo, etc.)
                          Re-exports from runtime.ts that survive: VirtualFileSystem interface,
                          VirtualStat, VirtualDirEntry, ProcessInfo, Permissions types,
                          ExecResult, SessionEvent types, CronEvent types.
  host-tools.ts         — HostTool/ToolKit types with Zod schemas
  host-tools-zod.ts     — Zod validation + zodToJsonSchema() for registration
  packages.ts           — npm package resolution (node_modules walks, ~150 lines)
  agents.ts             — agent configs with prepareInstructions callbacks (~100 lines)
  js-bridge.ts          — VirtualFileSystem dispatch for JS-bridge mounts (~150 lines)
  cron/                 — stays as-is (cron-manager.ts, timer-driver.ts, schedule-driver.ts, types.ts)
  sidecar/
    rpc-client.ts       — wire protocol client (frames, serialization, event stream)
    process.ts          — spawn/kill sidecar binary
```

~2,500 lines total, down from ~10,400. (Revised up from 2,000 to account for JS-bridge mounts, agent callbacks, and proper event routing.)

## Downstream Packages

Everything gets rewritten to the new API in the same change. No compatibility layers.

- **`secure-exec` / `@secure-exec/typescript`** — Re-export the new types from `types.ts`. Delete re-exports of internals (`InMemoryFileSystem`, `NodeRuntime`, `BindingTree`, etc.). These were implementation details that leaked into the public API. Gone.
- **`dev-shell`** — Rewrite to use `AgentOs.create()`. Delete `createKernel()` / `createInMemoryFileSystem()` usage.
- **`packages/browser`** — Has its own independent copies. No immediate change needed.
- **Actor layer (Rivet repo)** — Rewrite to match new `AgentOs` API. Updated in the same change.
- **Registry packages** — `defineSoftware`, `HostTool`, `ToolKit`, `NativeMountPluginDescriptor` survive. `registry/tests/` migrates to new test helpers.

## API Changes

The new API is the smallest correct surface. No `Session` class, no `ManagedProcess` object, no `CronJob` handle. Everything is IDs and flat methods.

1. **`Permissions`** — declarative only. `{ fs: "allow" }` or `{ fs: { mode: "allow", paths: [...] } }`. No callbacks.
2. **`Session` class** — deleted. Flat methods on `AgentOs`: `prompt(sessionId, text)`, `cancelSession(sessionId)`, `closeSession(sessionId)`, `onSessionEvent(sessionId, handler)`.
3. **`spawn()`** — returns `Promise<number>` (pid). Stdin/kill/wait are flat methods: `writeProcessStdin(pid, data)`, `killProcess(pid, signal)`, `waitProcess(pid)`.
4. **`createSession()`** — returns `Promise<{ sessionId: string }>`.
5. **Event handlers** — all `on*` methods return `() => void` (unsubscribe). Multiple subscribers supported.
6. **`mountFs()`** — still works via JS-bridge.
7. **`scheduleCron()`** — returns `Promise<{ id: string }>`. Cancel via `cancelCron(id)`.
8. **`prompt()`** — returns `Promise<PromptResult>`. Blocks until agent completes.
9. **`rawSend(sessionId, method, params)`** — preserved for custom ACP methods.
10. **Batch methods** (`readdirRecursive`, `readFiles`, `writeFiles`) — thin TypeScript wrappers over multiple RPCs. Add batch RPCs to sidecar if latency matters.
11. **Shell/terminal methods** — `openShell`, `writeShell`, `onShellData`, `resizeShell`, `closeShell`, `connectTerminal` become flat RPCs/events. Migrated in step 3 alongside process management.
12. **Process introspection** — `listProcesses`, `allProcesses`, `processTree`, `getProcess` become RPCs.
13. **Session introspection** — `listSessions`, `getSessionModes`, `getSessionConfigOptions`, `getSessionCapabilities`, `getSessionAgentInfo`, `setSessionMode`, `setSessionModel`, `setSessionThoughtLevel`, `respondPermission`, `onPermissionRequest` become RPCs/events.
14. **`fetch(port, request)`** — thin RPC wrapper to kernel network adapter.
15. **`unmountFs(path)`** — RPC.
16. **`dispose()`** — RPC that tears down all processes, sessions, mounts.

## Wire Protocol

The existing protocol uses length-prefixed binary frames over stdio (4-byte BE length + JSON payload). Three frame directions exist today:

- **Request** (TypeScript → Sidecar): `{ frame_type: "request", request_id, payload }`
- **Response** (Sidecar → TypeScript): `{ frame_type: "response", request_id, payload }`
- **Event** (Sidecar → TypeScript): `{ frame_type: "event", payload }` — fire-and-forget, no request_id

The migration adds a fourth direction: **sidecar-initiated requests** that require a response from TypeScript. This is needed for tool invocations, JS-bridge calls, cron session preparation, and permission request forwarding.

### New frame types

- **SidecarRequest** (Sidecar → TypeScript): `{ frame_type: "sidecar_request", request_id, payload }`
- **SidecarResponse** (TypeScript → Sidecar): `{ frame_type: "sidecar_response", request_id, payload }`

Request ID namespacing: TypeScript-initiated request IDs are positive integers (1, 2, 3...). Sidecar-initiated request IDs are negative integers (-1, -2, -3...). No collision possible.

### SidecarRequest payload types

```rust
enum SidecarRequestPayload {
    // Tool system (step 5)
    ToolInvocation {
        invocation_id: String,
        tool_key: String,       // "toolkit_name:tool_name"
        input: serde_json::Value,
        timeout_ms: u64,
    },

    // JS-bridge mounts (step 2)
    JsBridgeCall {
        call_id: String,
        mount_id: String,
        operation: String,      // "readFile", "writeFile", "stat", etc.
        args: serde_json::Value,
    },

}
```

### Push event payload types (fire-and-forget, no response needed)

```rust
enum EventPayload {
    // Existing
    ProcessOutput { process_id: String, stream: String, data: Vec<u8> },
    ProcessExited { process_id: String, exit_code: i32 },
    VmLifecycle { vm_id: String, state: String },

    // New (step 3)
    ShellData { shell_id: String, data: Vec<u8> },

    // New (step 6)
    SessionEvent { session_id: String, notification: serde_json::Value },
    PermissionRequest { session_id: String, request: serde_json::Value },
}
```

### SidecarResponse payload types (TypeScript → Sidecar)

```rust
enum SidecarResponsePayload {
    ToolInvocationResult {
        invocation_id: String,
        result: Option<serde_json::Value>,
        error: Option<String>,
    },
    JsBridgeResult {
        call_id: String,
        result: Option<serde_json::Value>,
        error: Option<String>,  // maps to errno: EIO for generic errors, ENOENT for not-found
    },
}
```

### TypeScript client changes (step 0b)

The `rpc-client.ts` must handle both directions concurrently:
- **Reading:** parse incoming frames, dispatch based on `frame_type`:
  - `response` → resolve pending TypeScript-initiated request
  - `event` → emit to event router
  - `sidecar_request` → dispatch to callback handler, send `sidecar_response`
- **Writing:** send `request` frames (TypeScript-initiated) and `sidecar_response` frames (replies to sidecar requests)

Single stdio pipe supports this because reads and writes are independent. Request ID sign distinguishes direction.

## Two Permission Systems (Kernel vs ACP)

The spec's declarative permissions replace **kernel-level permissions** — can this process access this path / this network host / this env var. These are syscall-time checks evaluated by the sidecar.

**ACP permission requests** are a completely separate system. When an agent asks "may I delete this file?", it sends a `session/request_permission` JSON-RPC request. The sidecar forwards this to TypeScript as a `PermissionRequest` push event. TypeScript dispatches to the user's `onPermissionRequest` handler (which might show a UI dialog). The user calls `respondPermission(sessionId, requestId, reply)` which RPCs back to the sidecar, which sends the JSON-RPC response to the agent.

These are not conflated. Kernel permissions are static policy. ACP permissions are interactive approval.

## Session Creation Flow

Session creation involves both TypeScript and Rust in a specific order:

```
TypeScript                              Rust Sidecar
──────────                              ────────────

1. Resolve agent package paths
   (walk node_modules, read
    package.json bin field)
                                        
2. Call prepareInstructions()
   callback. This may RPC back
   to sidecar for VFS reads.
   Returns { args, env }.
   ─── RPC: readFile ──────────────►   3. Serve VFS read
   ◄── response ───────────────────

4. CreateSession RPC ──────────────►   5. Spawn agent process with
   { agent_type, adapter_bin_path,        resolved bin path, args, env.
     args, env, instructions }            Speak ACP JSON-RPC over stdio.
                                          Send initialize + session/new.
   ◄── { session_id } ────────────     6. Return session ID.

7. Register event handlers.            8. Push SessionEvent, PermissionRequest
   ◄── push events ───────────────        as agent sends notifications.
```

Key: TypeScript resolves the package path and runs `prepareInstructions`. Everything after that is Rust. The `prepareInstructions` callback pattern stays in TypeScript because:
- It's defined per-agent in registry packages (`registry/agent/*/src/index.ts`)
- It contains agent-specific logic (Pi: `--append-system-prompt`, OpenCode: `OPENCODE_CONTEXTPATHS`, Claude: `--append-system-prompt`)
- New agents add new callbacks without modifying Rust code
- The callback may call back to sidecar for VFS reads — this is fine, it's just RPC

## JS-Bridge Mounts

Users can mount custom TypeScript `VirtualFileSystem` implementations into the VM:

```typescript
const myFs: VirtualFileSystem = { readFile: ..., writeFile: ..., ... };
vm.mountFs("/custom", myFs);
```

This cannot move to Rust because the `VirtualFileSystem` implementation runs user TypeScript code.

**Design:** The sidecar registers a `js_bridge` mount plugin at the given path. When the kernel accesses a path under that mount, the sidecar pushes a `JsBridgeCall { mount_id, operation, args }` event to TypeScript. TypeScript dispatches to the user's `VirtualFileSystem` implementation and returns the result via `JsBridgeResult` RPC. The sidecar holds the kernel operation until the result arrives.

This is the same pattern as tool invocations: Rust holds a pending operation, pushes an event, TypeScript runs the callback, returns via RPC.

**Latency:** Every VFS operation on a JS-bridge mount requires a round-trip to TypeScript. This is acceptable because JS-bridge mounts are the escape hatch, not the common case. Native mount plugins (host_dir, S3, Google Drive) run entirely in Rust.

**Error mapping:** JS-bridge errors map to POSIX errno: generic errors → `EIO`, "not found" / "ENOENT" → `ENOENT`, "permission denied" → `EACCES`, "already exists" → `EEXIST`. The sidecar inspects the error string for known patterns.

**Timeout:** Per-call timeout of 30s. On timeout, returns `EIO` to the kernel. The mount stays usable for future calls.

## ModuleAccessFileSystem

The `ModuleAccessFileSystem` overlay projects host `node_modules/` into the VM read-only so agents can access their dependencies. This moves to the Rust sidecar as a native mount plugin (`plugins/module_access.rs`). The sidecar already has host filesystem access via `host_dir` — `ModuleAccessFileSystem` is essentially a read-only `host_dir` mount scoped to `node_modules/` directories. TypeScript passes the `moduleAccessCwd` host path during `ConfigureVm`; the sidecar handles the rest.

## Scoping

All new subsystems are scoped **per-VM**, not per-sidecar:
- Tool registrations and virtual processes are per-VM
- ACP sessions are per-VM
- JS-bridge mounts are per-VM
- Permissions are per-VM
- Layer/snapshot state is per-VM

A single sidecar process can host multiple VMs, each with independent state.

## ACP in Rust — Complexity Acknowledgment

Moving ACP to Rust is the largest single piece of work. The existing `acp-client.ts` (564 lines) and `session.ts` (493 lines) contain battle-tested edge cases:

- Permission request deduplication (`_seenInboundRequestIds` — VM stdout can duplicate NDJSON lines)
- Legacy permission method shimming (`request/permission` vs `session/request_permission`)
- Cancel fallback (request → notification when agent returns -32601)
- Permission option normalization (`always`/`allow_always`, `once`/`allow_once`, `reject`/`reject_once`)
- Exit drain grace period (50ms delay before rejecting pending requests)
- Timeout diagnostics (last 20 activity entries in error messages)
- Synthetic session update injection for OpenCode
- Local mode/config state tracking with optimistic updates

All of this must be faithfully ported to Rust. Not simplified, not redesigned — ported. These are compatibility workarounds for real agent behaviors.

**ACP inbound requests** — agents send requests TO the sidecar (`fs/read_text_file`, `fs/write_text_file`, `terminal/create`, `terminal/output`, `terminal/wait_for_exit`, `terminal/kill`, `terminal/release`). The sidecar serves these directly from its own VFS and process table. No round-trip to TypeScript needed for these — the sidecar already has the data.

**Realistic estimate:** 1,200-1,500 lines of Rust for the full ACP implementation.

## Tool Invocation — Virtual Process Design

**No HTTP server.** Tools are invoked through virtual child processes, not HTTP. When an agent runs `agentos-mytoolkit mytool --flag value`, the sidecar intercepts the process spawn (it's a registered command), creates a virtual process backed by the tool system, and communicates over the process's stdio.

```
Agent spawns `agentos-mytoolkit` ──► Sidecar command resolver
                                      │
                                      ├─ Recognize as registered tool command
                                      ├─ Parse argv against JSON Schema
                                      ├─ Create invocation_id
                                      ├─ Push ToolInvocation SidecarRequest to TypeScript
                                      │
TypeScript event loop                 │  (sidecar holds virtual process)
  ├─ Receive ToolInvocation           │
  ├─ Zod validate input               │
  ├─ Call tool.execute()               │
  ├─ Send ToolInvocationResult ────────►  Sidecar
                                      │    ├─ Write result to virtual process stdout
                                      │    ├─ Exit virtual process with code 0 (or 1 on error)
                                      │
Agent reads stdout ◄──────────────────
```

The virtual process is a kernel process entry with a PID, stdin, stdout, and exit code — it just has no real host process behind it. The sidecar owns the process lifecycle. This is cleaner than HTTP because:
- No port allocation or discovery
- Tool commands appear as real executables in the VM's PATH
- Communication uses the existing process I/O infrastructure
- The tool shim scripts become trivial (just the command stub, no HTTP client code)
- `--json` output is just writing JSON to stdout

The sidecar needs:
- Virtual process creation in the kernel process table
- `HashMap<InvocationId, VirtualProcessHandle>` for correlation
- Per-invocation timeout (default 30s, configurable per tool)
- Error: write error message to stderr, exit code 1

Estimate: ~400-500 lines (virtual process + tool dispatch, no HTTP server complexity).

## What Gets Deleted from TypeScript

| File | Lines | Why It Goes |
|------|-------|------------|
| `overlay-filesystem.ts` | 758 | Rust already has `overlay_fs.rs` |
| `runtime.ts` (most of it) | ~1,600 | VFS/kernel implementations → Rust. Types survive in `types.ts` |
| `layers.ts` | 314 | Sidecar manages layers |
| `filesystem-snapshot.ts` | 164 | Sidecar handles snapshots |
| `base-filesystem.ts` | 253 | Sidecar loads base filesystem |
| `native-kernel-proxy.ts` | 1,858 | Replaced by thin RPC calls in agent-os.ts + js-bridge.ts |
| `session.ts` | 493 | Sidecar owns ACP sessions |
| `acp-client.ts` | 564 | Sidecar speaks JSON-RPC to agents |
| `protocol.ts` | 57 | JSON-RPC types move to Rust |
| `stdout-lines.ts` | 66 | Sidecar buffers/streams stdout |
| `host-tools-server.ts` | 424 | Sidecar handles tools via virtual processes |
| `host-tools-argv.ts` | 359 | Sidecar parses CLI args from JSON Schema |
| `host-tools-prompt.ts` | 132 | Sidecar generates prompt text |
| `host-tools-shims.ts` | 195 | Sidecar generates shim scripts |
| `permission-descriptors.ts` | 347 | Declarative permissions, no probing |
| ~~`cron/*`~~ | ~~350~~ | **STAYS** — cron scheduling remains in TypeScript |
| `sqlite-bindings.ts` | 470 | Sidecar exposes SQLite directly |
| `os-instructions.ts` | 19 | Sidecar loads instructions |
| `sidecar/handle.ts` | 237 | Simplified to ~30 lines |
| `sidecar/client.ts` | 421 | Merged into rpc-client.ts |
| `sidecar/in-process-transport.ts` | 88 | Replaced by sidecar test harness |
| `sidecar/mount-descriptors.ts` | 52 | Inlined into rpc-client.ts |
| `sidecar/root-filesystem-descriptors.ts` | 76 | Inlined into rpc-client.ts |

## What agent-os.ts Becomes

Every public method is an RPC call + event routing. The class stores:
- `rpc: RpcClient` — sidecar connection
- `eventRouter: EventRouter` — maps event keys to handler sets (supports multiple subscribers + unsubscribe)
- `toolExecutors: Map<string, { validate, execute }>` — tool callbacks
- `cronCallbacks: Map<string, () => void | Promise<void>>` — cron callbacks
- `jsBridgeMounts: Map<string, VirtualFileSystem>` — JS-bridge mount callbacks

```typescript
class AgentOs {
  // Filesystem — pure RPC pass-through
  async readFile(path: string): Promise<Uint8Array> {
    return this.rpc.call("read_file", { path });
  }
  async writeFile(path: string, content: string | Uint8Array): Promise<void> {
    return this.rpc.call("write_file", { path, content: encodeContent(content) });
  }
  // ... 20+ filesystem methods, all one-liners

  // Process — RPC + event dispatch
  async spawn(command: string, args: string[], opts?: SpawnOptions): Promise<number> {
    const { pid } = await this.rpc.call("spawn", { command, args, ...opts });
    return pid;
  }
  async exec(command: string, opts?: ExecOptions): Promise<ExecResult> {
    return this.rpc.call("exec", { command, ...opts });
  }
  async waitProcess(pid: number): Promise<number> {
    // RPC blocks in the sidecar until the process exits — no event race condition.
    // If process already exited, returns immediately with the exit code.
    const { exit_code } = await this.rpc.call("wait_process", { pid });
    return exit_code;
  }
  onProcessStdout(pid: number, handler: (data: Uint8Array) => void): () => void {
    return this.eventRouter.on(`process_output:${pid}:stdout`, handler);
  }
  // ... onProcessStderr, onProcessExit — all return unsubscribe functions

  // Sessions — RPC + event dispatch
  async createSession(agentType: string, opts?: SessionOptions): Promise<{ sessionId: string }> {
    const config = AGENT_CONFIGS[agentType];
    const adapterPath = await this.resolveAdapterBin(config.acpAdapter);
    const prepared = await config.prepareInstructions?.(this, opts);
    const { session_id } = await this.rpc.call("create_session", {
      agent_type: agentType,
      adapter_bin_path: adapterPath,
      args: prepared?.args,
      env: prepared?.env,
      ...opts,
    });
    return { sessionId: session_id };
  }
  async prompt(sessionId: string, text: string): Promise<PromptResult> {
    return this.rpc.call("session_prompt", { session_id: sessionId, text });
  }
  async rawSend(sessionId: string, method: string, params?: any): Promise<any> {
    return this.rpc.call("session_raw_send", { session_id: sessionId, method, params });
  }

  // JS-bridge mounts
  mountFs(path: string, driver: VirtualFileSystem, opts?: { readOnly?: boolean }): void {
    const mountId = crypto.randomUUID();
    this.jsBridgeMounts.set(mountId, driver);
    this.rpc.call("mount_js_bridge", { mount_id: mountId, path, read_only: opts?.readOnly });
  }

  // Tools — register with sidecar, keep execute callback locally
  registerToolkit(toolkit: ToolKit): void {
    const schema = toolkitToJsonSchema(toolkit);
    this.rpc.call("register_toolkit", schema);
    for (const [name, tool] of Object.entries(toolkit.tools)) {
      this.toolExecutors.set(`${toolkit.name}:${name}`, {
        validate: (input: unknown) => tool.inputSchema.safeParse(input),
        execute: tool.execute,
      });
    }
  }
}
```

The event loop reads events from the sidecar and dispatches:

```typescript
// Callbacks are dispatched concurrently — each sidecar request spawns its own
// async task so slow callbacks don't block event processing.
private async runEventLoop(): Promise<void> {
  for await (const event of this.rpc.events()) {
    switch (event.type) {
      // Fire-and-forget events — dispatch immediately, never block
      case "process_output":
        this.eventRouter.emit(`process_output:${event.pid}:${event.stream}`, event.data);
        break;
      case "process_exited":
        this.eventRouter.emit(`process_exited:${event.pid}`, event.exit_code);
        break;
      case "shell_data":
        this.eventRouter.emit(`shell_data:${event.shell_id}`, event.data);
        break;
      case "session_event":
        this.eventRouter.emit(`session_event:${event.session_id}`, event.notification);
        break;
      case "permission_request":
        this.eventRouter.emit(`permission_request:${event.session_id}`, event.request);
        break;

      // Sidecar requests — spawn concurrent handler, respond via sidecar_response
      case "sidecar_request":
        this.handleSidecarRequest(event).catch(console.error); // fire-and-forget
        break;
    }
  }
}

private async handleSidecarRequest(event: SidecarRequest): Promise<void> {
  const { request_id, payload } = event;
  try {
    switch (payload.type) {
      case "tool_invocation": {
        const executor = this.toolExecutors.get(payload.tool_key);
        if (!executor) throw new Error("unknown tool");
        const parsed = executor.validate(payload.input);
        if (!parsed.success) throw new Error(formatZodError(parsed.error));
        const result = await executor.execute(parsed.data);
        this.rpc.sendSidecarResponse(request_id, { result });
        break;
      }
      case "js_bridge_call": {
        const mount = this.jsBridgeMounts.get(payload.mount_id);
        if (!mount) throw new Error("unknown mount");
        const result = await dispatchVfsCall(mount, payload.operation, payload.args);
        this.rpc.sendSidecarResponse(request_id, { result });
        break;
      }
    }
  } catch (err) {
    this.rpc.sendSidecarResponse(request_id, { error: String(err) });
  }
}
```

## Test Structure

### Problem

The current test infrastructure depends heavily on deleted APIs:
- `createInMemoryFileSystem()` + `createKernel()` are the foundation of 50+ test files
- `registry/tests/helpers.ts` re-exports these for all wasmvm/kernel integration tests
- `packages/core/src/test/runtime.ts` provides `TerminalHarness`, `getAgentOsKernel`, etc.
- `InProcessSidecarTransport` allows running without a real sidecar binary
- Tests inspect TypeScript-side state (overlay layers, process entries, permission callbacks)

### Design: Two Test Tiers

**Tier 1: Rust unit/integration tests (`cargo test`)**

All kernel-internal logic moves to Rust, so its tests move too:
- VFS operations (read, write, mkdir, stat, symlink, etc.)
- Overlay filesystem (copy-up, whiteouts, opaque markers)
- Layer management (create, seal, import, export)
- Process table (spawn, wait, signals, process tree)
- Command resolution (Node vs WASM, entrypoint resolution)
- Path mapping (guest↔host, shadow directory)
- ACP protocol (JSON-RPC parsing, request correlation, timeouts, deduplication)
- Session state machine (initialize, prompt, cancel, close, modes, capabilities)
- Permission evaluation (declarative rules, glob matching)
- Tools: virtual process dispatch, shim generation, prompt markdown, argv parsing from JSON Schema
- SQLite (query execution, value encoding, WAL sync)

These tests are fast (no sidecar process spawn), run in `cargo test`, and cover the vast majority of logic. They replace the deleted TypeScript tests 1:1.

**Test layout:**
```
crates/kernel/tests/
  vfs/               — filesystem operations (existing + expanded)
  overlay/           — overlay filesystem tests (port from overlay-backend.test.ts)
  layers/            — layer lifecycle tests (port from layers in agent-os tests)
  process/           — process table, signals, tree (existing + expanded)

crates/sidecar/tests/
  acp/               — ACP protocol tests (port from pi-acp-adapter.test.ts, pi-sdk-adapter.test.ts)
    json_rpc.rs      — JSON-RPC 2.0 parsing, serialization
    session.rs       — session lifecycle, state tracking
    permissions.rs   — permission request deduplication, compatibility
    inbound.rs       — inbound request handling (fs/read_text_file, terminal/*)
  tools/             — tool system tests
    virtual_process.rs — virtual process creation, dispatch, lifecycle
    shim_gen.rs      — CLI shim script generation
    prompt_gen.rs    — markdown prompt generation
    argv_parse.rs    — CLI argument parsing from JSON Schema
  permissions/       — declarative permission evaluation
    glob_match.rs    — path glob matching
    policy.rs        — policy evaluation (allow/deny per operation)
  sqlite/            — SQLite binding tests

crates/execution/tests/
  javascript/        — existing, expanded for command resolution
    command_resolution.rs  — Node vs WASM dispatch
    env_construction.rs    — AGENT_OS_* env var building
    path_mapping.rs        — guest↔host path resolution
  (existing test files preserved)
```

**Tier 2: TypeScript SDK integration tests (`pnpm test`)**

These test the SDK ↔ sidecar boundary end-to-end. They spawn a real sidecar binary and exercise the full stack through the `AgentOs` API.

```
packages/core/tests/
  sdk/
    filesystem.test.ts     — readFile, writeFile, mkdir, stat via AgentOs API
    process.test.ts        — spawn, exec, stdin, kill, waitProcess via AgentOs API
    session.test.ts        — createSession, prompt, events, permissions via AgentOs API
    cron.test.ts           — scheduleCron, cancelCron, callback/exec/session actions
    tools.test.ts          — registerToolkit, tool invocation round-trip
    js-bridge.test.ts      — mountFs with custom VirtualFileSystem, read/write through bridge
    shell.test.ts          — openShell, writeShell, onShellData, resizeShell
    snapshot.test.ts       — snapshotRootFilesystem, layer create/seal/import
    permissions.test.ts    — declarative permissions, ACP permission requests
    network.test.ts        — fetch, socket lookup

  agents/
    pi.test.ts             — Pi SDK adapter end-to-end
    pi-cli.test.ts         — Pi CLI adapter end-to-end
    claude.test.ts         — Claude adapter end-to-end
    opencode.test.ts       — OpenCode adapter end-to-end

  compat/
    secure-exec.test.ts    — secure-exec public API smoke test
    dev-shell.test.ts      — dev-shell integration
```

**Test helpers:**

```typescript
// packages/core/src/test/helpers.ts
export async function createTestVm(opts?: Partial<AgentOsOptions>): Promise<AgentOs> {
  // Spawns real sidecar, creates VM with test defaults
  // Uses moduleAccessCwd for node_modules access
  // Returns disposable AgentOs instance
}

export async function withTestVm(
  fn: (vm: AgentOs) => Promise<void>,
  opts?: Partial<AgentOsOptions>,
): Promise<void> {
  const vm = await createTestVm(opts);
  try { await fn(vm); } finally { await vm.dispose(); }
}
```

**No in-process transport.** All TypeScript tests use a real sidecar binary. This is slower but tests the real system. `cargo build -p agent-os-sidecar` runs automatically when the binary is stale (this already exists in `agent-os.ts`).

**Sidecar test mode.** Add a `--test` flag to the sidecar binary that:
- Enables verbose logging for test diagnostics
- Reduces timeouts (faster failure detection)
- Exposes internal state query RPCs for test assertions (e.g., `GetInternalState` to inspect overlay layers, process table, cron jobs)

### Migration of Existing Tests

| Current Test File | Destination | Notes |
|---|---|---|
| `overlay-backend.test.ts` | `crates/kernel/tests/overlay/` | Port to Rust |
| `mount-descriptors.test.ts` | `crates/sidecar/tests/` | Port to Rust |
| `mount.test.ts` | `packages/core/tests/sdk/filesystem.test.ts` | Keep as SDK integration |
| `native-sidecar-process.test.ts` | `packages/core/tests/sdk/` | Keep as SDK integration |
| `pi-acp-adapter.test.ts` | `crates/sidecar/tests/acp/session.rs` + `packages/core/tests/agents/pi.test.ts` | Split: protocol → Rust, e2e → TS |
| `pi-sdk-adapter.test.ts` | Same split as above | |
| `host-tools-argv.test.ts` | `crates/sidecar/tests/tools/argv_parse.rs` | Port to Rust |
| `registry/tests/wasmvm/*.test.ts` | Stay as-is but use `createTestVm()` | Update imports |
| `registry/tests/kernel/*.test.ts` | Stay as-is but use `createTestVm()` | Update imports |

### Test Coverage Requirements

**Rust tests must cover:**
- Every VFS operation (read, write, mkdir, stat, symlink, link, chmod, chown, utimes, truncate, pread, pwrite, readdir, exists, realpath, lstat, readlink, removeFile, removeDir, rename) — positive + error cases
- Overlay: copy-up, whiteout, opaque directory, multi-layer resolution, metadata hiding
- Layers: create, seal, import, export, overlay construction from layers
- ACP: JSON-RPC request/response, notification forwarding, permission dedup, cancel fallback, drain grace, timeout diagnostics, inbound fs/terminal requests
- Session: initialize, new, prompt completion, cancel, close, mode tracking, capability parsing, event history with sequence numbers, synthetic updates for OpenCode
- Permissions: allow/deny per operation, path glob matching, per-env-var rules
- Tools: virtual process lifecycle, JSON Schema → CLI argv parsing, command stub generation, prompt markdown, invocation correlation with timeout
- SQLite: query, exec, prepared statements, value encoding (bigint, Uint8Array), WAL checkpoint
- Command resolution: node script, node -e inline, WASM command, PATH lookup, unknown command error
- Path mapping: guest↔host resolution, shadow symlink creation, node_modules ancestor expansion
- Process: spawn, stdin buffering, kill, wait, exit code, process tree query

**TypeScript tests must cover:**
- Every public `AgentOs` method works end-to-end through the sidecar
- Event subscription + unsubscription + multiple subscribers
- JS-bridge mount read/write round-trip
- Tool invocation with Zod validation (success + failure)
- Cron callback invocation (cron stays in TypeScript)
- Session event + permission request dispatch
- Error propagation from sidecar to SDK
- Concurrent operations (multiple spawns, multiple sessions)
- Disposal cleanup (processes killed, sessions closed, mounts unmounted)

### Mock LLM Server for Rust ACP Tests

Rust ACP tests need a mock agent that speaks JSON-RPC over stdio. Use `@copilotkit/llmock` (the same mock library the TypeScript tests use) via a Node.js subprocess:

1. Write a small Node.js script (`crates/sidecar/tests/fixtures/mock-acp-adapter.mjs`) that:
   - Starts an llmock server on a random port
   - Speaks ACP JSON-RPC over stdio (initialize → session/new → session/prompt)
   - Routes prompts to the llmock endpoint
   - Returns structured responses

2. Rust tests spawn this script via `Command::new("node").arg("mock-acp-adapter.mjs")` and communicate over its stdio, exactly matching how the real sidecar talks to real agents.

3. This is 1:1 with how TypeScript tests work today — same mock library, same protocol, just invoked from Rust instead of TypeScript.

## Migration Order

Each step is independently shippable. The system works at every intermediate state. Ordered by easiest-first and fewest dependencies.

### Step 1: Declarative permissions

**Why first:** Simplest logic to port — no callbacks to coordinate, no new event types, no async complexity beyond what step 0 provides.

**Rust:** Declarative permission evaluation with glob matching. ~200-300 lines.

**TypeScript:** Replace `permission-descriptors.ts` (347 lines) with declarative permission serialization (~20 lines). Update `AgentOs.create()` to accept declarative permissions.

**Test:** Existing permission tests rewritten as Rust unit tests + TypeScript integration tests against new API.

### Step 2: Filesystem — delete overlay, layers, snapshots, base

**Why second:** Large LoC reduction, no async coordination complexity. The Rust kernel already has `overlay_fs.rs` and `MemoryFileSystem`. This is wiring, not invention.

**Rust:** Add layer management RPCs (`CreateLayer`, `SealLayer`, `ImportSnapshot`, `ExportSnapshot`, `CreateOverlay`). Bundle `base-filesystem.json` into sidecar. ~150-200 lines.

**TypeScript:** Delete `overlay-filesystem.ts` (758), `layers.ts` (314), `filesystem-snapshot.ts` (164), `base-filesystem.ts` (253), `InMemoryFileSystem` from `runtime.ts` (~360). Update `agent-os.ts` to call layer RPCs instead of managing TypeScript filesystem objects. Extract surviving types to `types.ts`.

**Test:** Port `overlay-backend.test.ts` to `crates/kernel/tests/overlay/`. TypeScript filesystem tests switch to `createTestVm()` + sidecar.

### Step 3: Process management + shell/terminal — delete synthetic PIDs, stdin buffering, process tree

**Why third:** Depends on async sidecar (step 0) for event push. Eliminates the split-brain process table. Shell/terminal operations are tightly coupled with process management so they migrate together.

**Rust:** Extend process table with stdin buffering, process tree query RPC, kernel-assigned PIDs. `ProcessOutput`/`ProcessExited` events already exist in the protocol — update them to use kernel PIDs instead of synthetic IDs. Add `ShellData` push event. Add RPCs: `WaitProcess`, `OpenShell`, `WriteShell`, `ResizeShell`, `CloseShell`, `ConnectTerminal`, `ListProcesses`, `GetProcessTree`. ~300-400 lines.

**TypeScript:** Delete synthetic PID system, `TrackedProcessEntry`, `flushPendingStdin`, `buildProcessSnapshot`, `readHostProcesses`, `openShell`, `connectTerminal` wrappers from `native-kernel-proxy.ts`. `spawn()` becomes a thin RPC returning kernel PID. Shell methods become thin RPCs. Add event routing for process + shell events. ~600 lines deleted.

**Test:** Process and shell tests rewritten against new flat API.

### Step 4: Command resolution + path mapping — delete shadow directory

**Why fourth:** Depends on process management (step 3) since command resolution feeds into `Spawn`. Eliminates the largest chunk of proxy complexity.

**Rust:** Add command resolver to sidecar (Node vs WASM dispatch, `node -e` inline handling, entrypoint resolution). Move shadow directory management and `expandHostAccessPaths` to sidecar. Build `AGENT_OS_*` env vars internally. ~350-400 lines.

**TypeScript:** Delete `resolveExecution`, `buildNodeExecutionEnv`, `resolveNodeEntrypoint`, `resolveNodeCwd`, `resolveHostPath`, `shadowPathForGuest`, `materializeGuestFile`, `materializeHostPathMappings`, `expandHostAccessPaths`, `tokenizeCommand`, `resolveExecCommand` from `native-kernel-proxy.ts`. ~400 lines deleted.

**After step 4:** `native-kernel-proxy.ts` is reduced to ~400 lines (filesystem view dispatch, socket cache, shell/terminal wrappers). Can be inlined into `agent-os.ts` or kept as `rpc-client.ts`.

### Step 5: Tools — virtual process dispatch, shim gen, prompt gen, argv parsing

**Why fifth:** Depends on async sidecar (step 0) for sidecar request push + process management (step 3) for virtual processes. No dependency on ACP.

**Rust:** Virtual process tool dispatch, JSON Schema → CLI flags, command stub generation, prompt markdown generation, invocation correlation with `oneshot` channels. ~800-1,100 lines.

**TypeScript:** Delete `host-tools-server.ts` (424), `host-tools-argv.ts` (359), `host-tools-prompt.ts` (132), `host-tools-shims.ts` (195). Add `zodToJsonSchema()` converter (~40 lines) and tool invocation event handler (~30 lines). Keep `host-tools.ts` (types) and `host-tools-zod.ts` (validation).

**Test:** Port `host-tools-argv.test.ts` to Rust. Add TypeScript integration test for tool invocation round-trip.

### ~~Step 6: Cron~~ — STAYS IN TYPESCRIPT

Cron scheduling stays in TypeScript. The sidecar has no concept of cron. The `cron/` directory stays as-is. Cron actions call sidecar primitives (`spawn`, `createSession`) via RPC.

### Step 6: ACP + sessions — move to sidecar

**Why sixth:** Largest single piece. Depends on async sidecar (step 0) and process management (step 3). Most agent-specific edge cases to port.

**Rust:** JSON-RPC 2.0 NDJSON codec, ACP client with request correlation + timeouts + deduplication, session state machine, inbound request handling (`fs/read_text_file`, `terminal/*`), agent compatibility layer (OpenCode synthetic events, cancel fallback, permission option normalization). ~1,500-1,900 lines.

**TypeScript:** Delete `session.ts` (493), `acp-client.ts` (564), `protocol.ts` (57), `stdout-lines.ts` (66). Session methods become flat RPC calls on `AgentOs`. `prepareInstructions` stays as TypeScript callback. ~1,180 lines deleted.

**Test:** Split existing `pi-acp-adapter.test.ts` / `pi-sdk-adapter.test.ts`: protocol tests → Rust, e2e agent tests → TypeScript.

### Step 7: SQLite + cleanup

**Rust:** Embed `rusqlite`, expose query/exec via sync RPC channel to guest processes. ~500-600 lines.

**TypeScript:** Delete `sqlite-bindings.ts` (470). Final cleanup: inline remaining `native-kernel-proxy.ts` into `agent-os.ts`, delete unused files, update all imports.

### Progress Tracking

After each step, the system is fully functional. Running `pnpm test` and `cargo test` should pass. The TypeScript line count monotonically decreases:

| After Step | TS Lines | Rust Lines Added | What's Gone |
|------------|----------|------------------|-------------|
| 0 (today) | ~10,400 | 0 | — |
| 0a+0b | ~10,400 | ~0 (refactor) | service.rs monolith, sync poll loop |
| 1 | ~10,050 | ~250 | Permission probing |
| 2 | ~8,560 | ~200 | Overlay, layers, snapshots, InMemoryFS |
| 3 | ~7,860 | ~350 | Synthetic PIDs, stdin buffering, process tree, shell/terminal wrappers |
| 4 | ~7,460 | ~375 | Command resolution, shadow dirs, path mapping |
| 5 | ~6,350 | ~950 | Tool HTTP server, shims, prompt gen, argv |
| 6 | ~5,170 | ~1,700 | ACP, sessions, protocol, stdout lines |
| 7 | ~2,850 | ~550 | SQLite, remaining native-kernel-proxy inlined into agent-os, sidecar client/handle/transport/descriptors consolidated into rpc-client, runtime.ts gutted to types.ts, agents.ts simplified, packages.ts simplified |

~5,375 lines new Rust total. Cron stays in TypeScript (~350 lines kept).

## Summary

| Component | Before (TS lines) | After (TS lines) | Moves to Rust |
|-----------|-------------------|-------------------|---------------|
| agent-os.ts | 2,948 | ~700 | Session mgmt, tool server, cron, filesystem setup |
| native-kernel-proxy.ts | 1,858 | 0 (deleted) | Everything — replaced by RPC calls + js-bridge.ts |
| native-process-client.ts | 1,593 | ~400 (rpc-client.ts) | Wire protocol simplifies |
| session.ts + acp-client.ts | 1,057 | 0 (deleted) | Sidecar owns ACP |
| runtime.ts | 2,173 | ~300 (types.ts) | VFS, kernel, InMemoryFS → Rust. Types survive. |
| Filesystem files | 1,489 | 0 (deleted) | Overlay, layers, snapshots, base |
| Host tools files | 1,110 | ~150 | Zod validation stays; server, shims, prompt, argv go |
| Cron files | 350 | ~350 (stays) | Cron stays in TypeScript |
| Permission files | 347 | 0 (deleted) | Declarative, no probing |
| JS-bridge + agents | 0 (new) | ~250 | New TS files for callback dispatch |
| Other (sqlite, packages, etc.) | 1,475 | ~700 | SQLite to sidecar; packages stays + simplifies |
| **Total** | **~10,400** | **~2,500** | **~7,900 lines deleted** |

**Rust additions (realistic):**

| Component | Lines | Notes |
|-----------|-------|-------|
| Async sidecar migration | 800-1,200 | Refactor, not net-new |
| ACP client + JSON-RPC | 1,200-1,500 | Largest single piece |
| Session state machine | 300-400 | Port from session.ts |
| Tool virtual process + dispatch | 400-500 | Virtual process in kernel, tool dispatch |
| Tool shim/prompt/argv | 500-700 | Port from host-tools-*.ts |
| ~~Cron scheduler~~ | ~~250-350~~ | Stays in TypeScript |
| Layer/snapshot RPCs | 150-200 | Wiring — kernel has primitives |
| Declarative permissions | 200-300 | Glob matching + evaluation |
| SQLite bindings | 500-600 | rusqlite + sync RPC handlers |
| JS-bridge mount support | 200-300 | Event push + correlation |
| Protocol additions | 300-400 | ~20 new RPC + event types |
| Agent configs + instruction prep | 200-250 | Port per-agent logic |
| **Total** | **4,900-6,600** | |

The TypeScript package becomes a thin SDK: spawn sidecar, forward RPCs, dispatch events to callbacks, validate tool input with Zod, resolve npm packages. ~2,500 lines, no kernel logic.
