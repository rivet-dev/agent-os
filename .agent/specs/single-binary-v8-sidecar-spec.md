# Single-Binary V8 Sidecar Spec

Move the `agent-os-v8` runtime into `agent-os-sidecar` so the system ships and runs as a single executable. Remove the out-of-process V8 daemon model, remove the client/server IPC layer built around that daemon, and replace it with a direct in-process V8 runtime interface.

## Summary

The current JavaScript, WebAssembly, and Python execution path still depends on a separate `agent-os-v8` binary. `crates/execution` spawns that binary, discovers it on disk, authenticates over a Unix domain socket, serializes `BinaryFrame` messages, and routes responses back into the sidecar. This adds packaging complexity, duplicated protocol code, daemon-specific failure modes, and sidecar logic that treats the V8 runtime like an external child process.

The target state is simpler:

- `agent-os-sidecar` is the only runtime executable.
- V8 initializes inside the sidecar process.
- Each JS/WASM/Python execution still gets its own V8 isolate session on a dedicated thread.
- `crates/execution` talks to V8 through direct Rust APIs and channels, not through socket framing.
- No `agent-os-v8` binary discovery, auth handshake, socket directory management, or mirrored IPC schema remains.
- `crates/execution` remains a separate native implementation crate; "single binary" refers to the final executable artifact, not a single crate.

This keeps the isolation model centered on V8 isolates and the kernel, while removing the unnecessary process boundary between the sidecar and the V8 runtime.

## Problem

Today the V8 stack is split across two transport-shaped halves:

- `crates/v8-runtime/src/main.rs` implements a daemon entrypoint, listener socket, connection auth, and frame dispatch.
- `crates/execution/src/v8_host.rs`, `v8_runtime.rs`, and `v8_ipc.rs` implement a client for that daemon.

That split creates work that does not help the VM model:

1. Binary management.
   `crates/execution` must locate `agent-os-v8` through env vars, sibling binaries, `target/`, or `PATH`.
2. Daemon-only startup logic.
   The system creates socket directories, prints socket paths over stdout, retries connect, and authenticates with `SECURE_EXEC_V8_TOKEN`.
3. Duplicated transport contracts.
   One side owns `BinaryFrame` encode/decode and the other side mirrors it.
4. Child-process-driven runtime control.
   Sidecar execution state tracks a V8 host PID and contains special-case signal logic for the shared runtime process.
5. Extra packaging surface.
   The project effectively ships two executables even though only the sidecar is the real product boundary.

None of that is required once the runtime is inside the sidecar process.

## Goals

1. One executable artifact: `agent-os-sidecar`.
2. No `agent-os-v8` subprocess anywhere in production execution paths.
3. No Unix socket, auth token, or frame serialization between execution code and V8 runtime code.
4. Preserve per-session V8 isolate ownership and concurrency limits.
5. Preserve existing bridge semantics for JS, WASM, and Python.
6. Simplify sidecar kill/liveness handling so it stops depending on a host child PID for V8-backed executions.
7. Reduce the number of modules a developer has to touch when adding a new V8 bridge capability.

## Non-Goals

1. Reworking the Node polyfill model.
   This spec removes the extra process boundary; it does not redesign builtin polyfills.
2. Preserving the current execution-runtime lifecycle API shape.
   This change may require an intentional breaking update to execution-facing lifecycle surfaces that currently expose the shared V8 host PID.
3. Collapsing every V8-related Rust file into one source file.
   The hard requirement is one executable, not one module.
4. Designing cooperative pause/resume for isolates.
   This spec preserves stop/continue semantics, but it does not require a general reusable pause/resume API beyond what sidecar process control needs.

## Compatibility

This migration is allowed to change the current execution-runtime lifecycle contract.

Explicitly:

- `JavascriptExecution::child_pid()`, `PythonExecution::child_pid()`, and `WasmExecution::child_pid()` may be removed or replaced.
- `ProcessStartedResponse.pid` must stop meaning "host runtime PID for the shared V8 subprocess".
- The sidecar-visible process identity for embedded runtimes should become the kernel PID, not a host subprocess PID.
- Tests and callers that currently inspect the host process state with tools like `ps` must be updated to validate sidecar/kernel-visible semantics instead.

## Portability Boundary

The browser/native portability seam is already defined by `agent-os-bridge`, especially `ExecutionBridge` and `HostBridge`.

This spec does not change that seam.

- `crates/execution` remains the native execution implementation crate.
- `crates/v8-runtime` remains a native implementation detail for this migration, but library-only rather than binary-backed.
- `crates/sidecar-browser` should continue to depend on `agent-os-bridge` and browser-worker bridge traits, not on `crates/execution`.
- No new `V8Runtime` trait is required for browser/native portability.

Any `EmbeddedV8Runtime` API introduced by this spec is native-internal only. It must not become a second portability contract that duplicates `ExecutionBridge`.

## Current State

Current path:

1. `JavascriptExecutionEngine::start_execution()` lazily spawns `V8RuntimeHost` from `crates/execution/src/v8_host.rs`.
2. `V8RuntimeHost` launches `agent-os-v8`, waits for a socket path on stdout, connects over UDS, and authenticates.
3. `crates/v8-runtime/src/main.rs` accepts the connection, owns `SessionManager`, and dispatches `BinaryFrame` messages to per-session isolate threads.
4. JS/WASM/Python execution objects retain the V8 child PID and expose it back to sidecar execution management.

This is why sidecar code currently has runtime-process-specific branches such as `SharedJavascriptSignalHost`, `SharedJavascriptTerminate`, and `HostPid(...)`.

## Target Architecture

### One process, same isolation model

The sidecar process owns V8 directly. The sidecar still creates one V8 isolate session per execution context and still runs each isolate on a dedicated thread. The removed boundary is only the host subprocess boundary between `agent-os-sidecar` and `agent-os-v8`.

### Embedded runtime service

Introduce an in-process runtime service, referred to here as `EmbeddedV8Runtime`.

Responsibilities:

- initialize the V8 platform once per process
- own the snapshot cache
- own the session registry and concurrency slot control
- create/destroy isolate sessions
- deliver stream events and bridge responses to the correct session

This service is created lazily inside the sidecar execution layer and never speaks over a socket.

### Keep session threads

The existing `crates/v8-runtime/src/session.rs` model is still useful. Each session already runs on its own thread and owns its own isolate. That model should stay. The simplification is that session threads receive typed Rust commands over channels instead of transport frames over an authenticated daemon connection.

### Replace transport-shaped APIs with runtime-shaped APIs

The current interface is built around `BinaryFrame`. That is the wrong abstraction once both sides live in one process.

Replace it with direct types such as:

- `V8SessionCreate`
- `V8SessionCommand`
- `V8SessionEvent`
- `V8BridgeResponse`
- `V8StreamEvent`

The new API should describe runtime intent, not wire format. Serialization should disappear from the hot path entirely.

### No connection auth or ownership layer

The daemon-only concepts below are removed:

- `SECURE_EXEC_V8_TOKEN`
- `SECURE_EXEC_V8_CODEC`
- `AGENT_OS_V8_RUNTIME_PATH`
- socket path discovery via stdout
- UDS retry/connect logic
- connection ids in the V8 session manager
- auth handshake code

Session ownership still exists, but it is expressed through Rust handles, not connection ids.

### Sidecar-native execution control

V8-backed executions no longer report a host child PID for the runtime process.

Instead:

- the sidecar-visible PID for embedded JS/WASM/Python executions becomes the kernel PID
- liveness checks use session state plus kernel process state
- termination uses an in-process terminate path on the session handle
- sidecar signal handling must stop calling host `kill(2)` for JS/WASM/Python runtime management

`signal 0` should become a runtime liveness check.
`SIGTERM` and `SIGKILL` should map to runtime termination.

### Signal model after embedding

Embedding V8 removes the old shortcut where some signals were forwarded to the shared runtime subprocess. The guest-visible signal model must still be preserved.

- `SIGSTOP` and `SIGCONT` remain supported.
- For embedded runtimes, stop/continue must be implemented as sidecar-managed session suspension/resumption aligned with kernel `ProcessTable` state, including `waitpid` stop/continue notifications.
- `SIGCHLD` remains supported.
- Nested child-process exit, stop, and continue transitions must still notify the parent guest process, but the notification must be delivered through an in-process runtime signal path rather than by sending `SIGCHLD` to a host runtime PID.
- `dispatch_v8_process_signal(...)` or its replacement becomes the canonical path for delivering runtime-owned signals to embedded JS sessions.

## Code Layout

### Required end state

- `crates/v8-runtime/Cargo.toml` becomes library-only. Remove the `[[bin]]` target.
- `crates/v8-runtime/src/main.rs` is deleted.
- `crates/execution/src/v8_host.rs` is deleted.
- `crates/execution/src/v8_runtime.rs` is deleted as a process launcher. Any helper functions worth keeping should move into a small support module with a non-launcher name.
- `crates/execution/src/v8_ipc.rs` is deleted.

### Runtime ownership

The simplest ownership model is:

- `JavascriptExecutionEngine` owns a lazily initialized `EmbeddedV8Runtime`
- `WasmExecutionEngine` and `PythonExecutionEngine` continue to route through `JavascriptExecutionEngine`
- sidecar execution management deals with session handles and execution ids, not runtime host PIDs

### File movement

For this migration, keep the existing crate boundary:

- `crates/execution` stays a separate native implementation crate
- `crates/v8-runtime` stays a separate native support crate, but library-only

Do not combine the embedding change with a crate-layout refactor.

The V8 runtime implementation modules stay in `crates/v8-runtime/src/` for this change:

- `bridge.rs`
- `execution.rs`
- `host_call.rs`
- `isolate.rs`
- `session.rs`
- `snapshot.rs`
- `stream.rs`
- `timeout.rs`

That keeps the change focused on removing the binary boundary first rather than mixing in a file-move refactor.

Important constraint:

- `session.rs` and `host_call.rs` must not be reused unchanged.
- The current versions are transport-shaped around `BinaryFrame`, `connection_id`, `IpcSender`, and `CallIdRouter`.
- The embedded design must rewrite those pieces around direct runtime commands, per-session ownership, and in-process bridge-response routing.
- If `crates/v8-runtime` stays as a library crate, its `build.rs` and ICU-data bootstrap responsibilities must remain valid there or move explicitly to the crate that becomes the final link owner.

## Native Runtime API

`crates/execution` should interact with V8 through a small concrete native API, not a new portability trait.

Preferred shape:

```rust
pub struct EmbeddedV8Runtime { ... }

impl EmbeddedV8Runtime {
    pub fn create_session(&self, request: CreateV8Session) -> Result<EmbeddedV8Session, V8RuntimeError>;
}

pub struct EmbeddedV8Session { ... }

impl EmbeddedV8Session {
    pub fn inject_globals(&self, payload: Vec<u8>) -> Result<(), V8RuntimeError>;
    pub fn execute(&self, request: ExecuteV8) -> Result<V8ExecutionStream, V8RuntimeError>;
    pub fn send_bridge_response(&self, response: V8BridgeResponse) -> Result<(), V8RuntimeError>;
    pub fn send_stream_event(&self, event: V8StreamEvent) -> Result<(), V8RuntimeError>;
    pub fn terminate(&self) -> Result<(), V8RuntimeError>;
    pub fn destroy(self) -> Result<(), V8RuntimeError>;
}
```

The exact type names can change, but the key constraint is:

- use concrete native types unless and until there is a real need for multiple interchangeable native V8 backends
- do not introduce a new abstraction that blurs the existing `agent-os-bridge` portability seam

`JavascriptExecutionEngine` should not know how a socket path is printed, how a frame is encoded, or how a token is compared in constant time, because those behaviors no longer exist.

## Migration Plan

### Phase 1: Embed without redesigning isolate internals

1. Remove the `agent-os-v8` binary entrypoint.
2. Lift the useful daemon internals into a library-owned `EmbeddedV8Runtime`.
3. Replace UDS reader/writer threads with in-process channels.
4. Replace connection-bound session ownership with direct handle ownership.

This phase should preserve most of the existing session-thread logic.

### Phase 2: Delete transport mirrors

1. Delete `v8_host.rs`, `v8_runtime.rs`, and `v8_ipc.rs`.
2. Delete `ipc_binary` framing from the execution-facing interface.
3. Rewrite `SessionManager` and `BridgeCallContext` so they no longer depend on connection ids, frame serialization, or daemon-style response routing.

### Phase 3: Simplify sidecar process management

1. Replace `child_pid` with the embedded-runtime identity model defined above.
2. Replace sidecar runtime-child signal/liveness logic with embedded-runtime control paths.
3. Preserve `SIGSTOP`, `SIGCONT`, and `SIGCHLD` guest semantics through direct sidecar/runtime integration.
4. Remove any remaining assumptions that JS/WASM/Python executions correspond to a separate host process.

### Phase 4: Remove obsolete packaging and docs

1. Remove references to rebuilding or locating `agent-os-v8`.
2. Remove platform package metadata under `crates/v8-runtime/npm/` if it only exists to distribute the old binary.
3. Update internal subsystem maps and execution docs to describe the embedded runtime.
4. Update authoritative instruction surfaces that currently encode the daemon model, including the repo-root `CLAUDE.md` and `crates/execution/CLAUDE.md`.

## Testing

The daemon and transport test surfaces should be retired and replaced with embedded-runtime tests.

Add coverage for:

1. lazy runtime initialization inside the sidecar process
2. session create/execute/terminate/destroy without any subprocess spawn
3. concurrent session execution with preserved isolate ownership
4. bridge response routing without global connection ids
5. stream event ordering and termination races
6. snapshot initialization and invalidation in the embedded path
7. sidecar kill/liveness behavior for JS/WASM/Python without host PID signaling
8. preserved `SIGSTOP`, `SIGCONT`, and `SIGCHLD` guest-visible behavior for embedded runtimes
9. `ProcessStartedResponse.pid` semantics after the switch to kernel PID identity

Update existing tests to assert the new invariant:

- V8-backed guest execution does not spawn `agent-os-v8`
- V8-backed guest execution remains fully functional inside the single `agent-os-sidecar` binary
- stale launcher code paths are gone or unreachable, including `AGENT_OS_V8_RUNTIME_PATH`, `SECURE_EXEC_V8_TOKEN`, `SECURE_EXEC_V8_CODEC`, and any binary discovery logic for `agent-os-v8`

## Risks

1. V8 global-state issues become sidecar-process issues.
   Teardown and test isolation must stay disciplined.
2. Some current signal behaviors rely on the runtime being a separate OS process.
   Those paths must be made explicit rather than accidentally preserved.
3. Migration churn can touch JS, WASM, and Python together because they all route through the shared V8 runtime.

## Decision

The sidecar remains the only runtime executable. V8 becomes an embedded sidecar subsystem, not a sibling daemon. The implementation should keep isolate sessions and snapshot behavior, but delete the daemon transport layer entirely.
