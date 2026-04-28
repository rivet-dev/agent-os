# Kernel And Runtime Subsystem Map

This is an internal inventory of the guest-visible VM/kernel plane.

Scope:
- Includes the Rust kernel, bridge contract, native execution engines, V8 runtime daemon, native/browser sidecars, ACP session layer, and first-party mount plugins.
- Includes the first-party TypeScript mount descriptor helpers for S3 and Google Drive because they are the public entrypoints to those filesystems.
- Does not try to inventory every registry software package, agent adapter, or test file.

Many subsystems span more than one file, and some very large files contain multiple logical subsystems. The tree below is intentionally file-free so it reads as a quick system overview. The detailed file mapping starts in the sections below.

## Tree

- Kernel / VM runtime plane
  - Shared bridge contract
    - Bridge traits and request types
    - Canonical bridge contract inventory and raw runtime syscall families
  - Kernel core
    - Kernel VM and syscall surface
    - Filesystem substrate
      - VFS trait and in-memory filesystem
      - Root filesystem bootstrap and snapshotting
      - Device filesystem layer
      - `/proc` pseudo-filesystem
      - Overlay filesystem
      - Mount routing and mount wrappers
      - Mount plugin interface
    - Process and I/O model
      - Process table, groups, waitpid, signals, zombies
      - File descriptor tables and open-file descriptions
      - Advisory file locking
      - Pipe manager
      - PTY manager and line discipline
      - poll-style readiness notifications
      - Command driver registry and stub population
      - Command resolution, direct path execution, and shebang handling
    - Security and tenancy
      - Permissions and permissioned VFS wrapper
      - Resource limits and accounting
      - User/passwd model
  - Native execution engines
    - Runtime exports and shared helpers
    - JavaScript runtime host orchestration
    - JavaScript module resolution and guest/host path translation
    - Node import cache and builtin/polyfill asset catalog
    - Embedded loader/bootstrap surfaces
      - Node loader/register templates
      - Node execution runner and timing bootstrap
      - Embedded WASM host runner
      - Pyodide asset materialization
    - Python / Pyodide runtime
    - WASM runtime
    - Guest bridge bundle and builtin asset bundles
    - Fetch/undici/web-stream compatibility shims
    - Execution-side V8 client transport and IPC
  - V8 isolate runtime daemon
    - Daemon entrypoint and UDS listener
    - Daemon/build bootstrap and ICU setup
    - Session manager, event loop, streams, and timeouts
    - Script/module execution engine
    - Host-call bridge injection and value serialization
    - Binary IPC protocol and client/server schema mirror
    - Snapshot creation and snapshot cache
  - Native sidecar
    - Sidecar composition layer
    - Transport, protocol, ownership, and callback state machine
    - Dispatch hub and ownership/permission routing
    - VM lifecycle, rootfs bootstrap, layers, overlays, and snapshots
    - Guest filesystem API
    - Shadow-root reconciliation and kernel/writeback sync
    - Tool registration and virtual-process dispatch
    - Process/runtime dispatch and runtime env assembly
    - Networking policy, DNS, and loopback translation
    - TCP/UDP/Unix socket transports and socket state
    - TLS bridge
    - HTTP/1 bridge
    - HTTP/2 bridge
    - Builtin service RPCs
    - Mount/plugin bridge and permission glue
    - ACP wire/client/compat/session types
    - ACP orchestration in the dispatch hub
  - Mounted and external filesystems
    - Host-backed mount family
    - Callback and remote mount family
    - Object-store-backed persisted filesystem family
    - Public TypeScript mount descriptor helpers
  - Browser-side sidecar variant
    - Browser sidecar scaffold and worker bridge

## Subsystems

### Shared bridge contract

This is the typed boundary between the host bridge, kernel-adjacent services, and execution engines.

Relevant files:
- `crates/bridge/src/lib.rs`
- `crates/bridge/bridge-contract.json`

What lives here:
- Filesystem bridge traits and request types.
- Permission bridge traits and decisions.
- Execution bridge traits for starting, polling, and killing guest runtimes.
- Persistence, lifecycle, clock, random, and structured-event bridge surfaces.
- The contract inventory used to keep bridge globals and calling conventions aligned.
- The raw runtime-facing syscall families for network, DNS, TLS, HTTP/HTTP2, UDP, child processes, signals, crypto, SQLite, PTY/raw mode, timers, stdin, and dynamic import.

### Kernel VM and syscall surface

This is the top-level VM object that composes the filesystem, process table, FD tables, pipes, PTYs, permissions, and resources into a POSIX-like kernel.

Relevant files:
- `crates/kernel/src/kernel.rs`

What lives here:
- `KernelVm`, `KernelVmConfig`, spawn/exec/open-shell APIs, and process handles.
- Command resolution, shebang parsing, and driver-dispatched commands.
- procfs synthesis for `/proc/self` and `/proc/[pid]/*`.
- Mount and plugin attachment points as seen by the kernel.
- Read/write/stat/open/close/dup/waitpid/poll style syscall plumbing across the other kernel managers.

### VFS and filesystem substrate

This is the baseline filesystem layer that everything else builds on.

Relevant files:
- `crates/kernel/src/vfs.rs`
- `crates/kernel/src/root_fs.rs`
- `packages/core/fixtures/base-filesystem.json`
- `crates/kernel/src/device_layer.rs`
- `crates/kernel/src/overlay_fs.rs`
- `crates/kernel/src/mount_table.rs`
- `crates/kernel/src/mount_plugin.rs`

What lives here:
- `VirtualFileSystem`, `VirtualStat`, path validation, and the in-memory filesystem in `vfs.rs`.
- Root filesystem descriptors, snapshot encode/decode, and base filesystem loading in `root_fs.rs`.
- Docker-like lower/upper overlay behavior, whiteouts, opaque directories, and out-of-band overlay metadata in `overlay_fs.rs`.
- Mount routing, read-only wrappers, mounted filesystem adapters, and cross-mount dispatch in `mount_table.rs`.
- The plugin factory interface and registry used to open first-party mounted filesystems in `mount_plugin.rs`.

### Pseudo-filesystems: `/dev` and `/proc`

These are guest-visible filesystem subsystems, but they do not live in their own top-level crate.

Relevant files:
- `crates/kernel/src/device_layer.rs`
- `crates/kernel/src/kernel.rs`

What lives here:
- Synthetic `/dev` device nodes such as `/dev/null`, `/dev/zero`, `/dev/urandom`, `/dev/std*`, `/dev/fd`, and `/dev/pts` in `device_layer.rs`.
- procfs path resolution and synthetic `/proc/self` and `/proc/[pid]/*` data in `kernel.rs`.

### Process, command, FD, pipe, PTY, and readiness model

This is the kernel’s process and I/O core.

Relevant files:
- `crates/kernel/src/process_table.rs`
- `crates/kernel/src/fd_table.rs`
- `crates/kernel/src/pipe_manager.rs`
- `crates/kernel/src/pty.rs`
- `crates/kernel/src/poll.rs`
- `crates/kernel/src/command_registry.rs`

What lives here:
- Process entries, parent/child relationships, process groups, sessions, wait queues, signal state, and zombie reaping.
- Per-process FD tables, refcounted open-file descriptions, and dup/dup2 behavior.
- Advisory `flock` state and lock-target tracking inside `fd_table.rs`.
- Kernel-managed pipes with blocking and non-blocking semantics.
- PTY master/slave pairs, termios state, canonical mode, echo, signal-generating control characters, and resize handling.
- `poll()` style readiness bits and notifier generation counters.
- The command registry that seeds `/bin/*` driver stubs for sidecar/tool commands.
- Direct command resolution, direct path execution, and shebang parsing in `kernel.rs`.

### Permissions, resource limits, and user identity

These subsystems enforce policy and kernel-visible identity.

Relevant files:
- `crates/kernel/src/permissions.rs`
- `crates/kernel/src/resource_accounting.rs`
- `crates/kernel/src/user.rs`

What lives here:
- Filesystem, network, command, and environment permission decisions plus the permissioned VFS wrapper.
- Resource limits for process counts, FDs, pipes, PTYs, sockets, filesystem bytes/inodes, read/write sizes, readdir batches, and WASM limits.
- The default VM user model, passwd rendering, home directory, shell, and UID/GID defaults.

### Execution crate runtime common layer

This is the shared scaffolding around the runtime-specific implementations.

Relevant files:
- `crates/execution/src/lib.rs`
- `crates/execution/src/common.rs`
- `crates/execution/src/runtime_support.rs`

What lives here:
- Runtime exports and type surface for JavaScript, Python, and WASM execution engines.
- Shared JSON/string encoding helpers and stable hashing.
- Compile-cache setup, import-cache roots, warmup marker paths, sandbox root calculation, and execution-path helpers.

### JavaScript runtime host path

This is the current Rust-side JavaScript execution manager.

Relevant files:
- `crates/execution/src/javascript.rs`
- `crates/execution/src/node_process.rs`

What lives here:
- JavaScript execution lifecycle and event stream handling.
- Warmup/prewarm flow and import-cache bootstrapping.
- Sync RPC request/response plumbing for guest builtin polyfills.
- Guest stdin, timer, and stream-event handling for the JS runtime path.
- Node process hardening, env filtering, permission flags, control channels, and exported child FDs.
- Guest/host path translation, builtin normalization, and module-resolution behavior that lives inside `javascript.rs`.

### Loader/materialization layer and builtin interception

This subsystem is the loader and asset materialization layer behind the JavaScript and Python runtimes.

Relevant files:
- `crates/execution/src/node_import_cache.rs`
- `crates/execution/src/runtime_support.rs`
- `crates/execution/src/node_process.rs`
- `crates/execution/assets/runners/python-runner.mjs`

What lives here:
- Node loader templates for builtin interception and builtin deny/allow behavior.
- Guest path scrubbing and host-path to guest-path mapping.
- Materialization of builtin/polyfill assets into temp import-cache roots.
- CommonJS/ESM compatibility shims and builtin wrappers.
- Embedded bootstrap/runtime surfaces inside `node_import_cache.rs`, including Node loader/register templates, timing bootstrap, and the embedded WASM host runner.
- Pyodide asset staging and Python runner materialization.

### Guest bridge bundles and fetch compatibility shims

These files are checked-in guest assets that support the runtime surface but are not Rust modules.

Relevant files:
- `crates/execution/assets/v8-bridge.source.js`
- `crates/execution/assets/v8-bridge.js`
- `crates/execution/assets/v8-bridge-zlib.js`
- `crates/execution/assets/polyfill-registry.json`
- `crates/execution/assets/undici-shims/*`

What lives here:
- The bundled guest bridge code that is loaded into the V8 runtime.
- The runtime-loadable builtin registry.
- The fetch, undici, stream, web-stream, HTTP, HTTPS, TLS, and related compatibility shims used by the guest runtime.

### Python / Pyodide runtime

This subsystem owns Python guest execution.

Relevant files:
- `crates/execution/src/python.rs`
- `crates/execution/assets/runners/python-runner.mjs`
- `crates/execution/assets/pyodide/*`

What lives here:
- Python execution lifecycle, stdout/stderr collection, timeout handling, and warmup flow.
- Python VFS RPCs for file I/O, HTTP, DNS, and subprocess bridging.
- Pyodide asset bundling, package wheel staging, and stdlib materialization.
- The Python runner script that boots Pyodide and translates bridge errors into Python-facing exceptions.

### WASM runtime

This subsystem owns WASM guest execution.

Relevant files:
- `crates/execution/src/wasm.rs`

What lives here:
- WASM execution lifecycle and warmup flow.
- WASI permission tier handling.
- Sync RPC use for filesystem and host services.
- Module parser hardening and size limits.
- Signal registration mapping and runtime limit env handling.

### Execution-side V8 client transport and IPC

These files are the client-side bridge from the execution crate into the separate V8 daemon.

Relevant files:
- `crates/execution/src/v8_host.rs`
- `crates/execution/src/v8_ipc.rs`
- `crates/execution/src/v8_runtime.rs`

What lives here:
- Spawning and authenticating the `agent-os-v8` process.
- Multiplexed session registration and per-session frame routing.
- The execution crate’s copy of the binary IPC framing and its schema mirror.

### V8 isolate runtime daemon

This is the separate process that actually owns the V8 isolates.

Relevant files:
- `crates/v8-runtime/src/main.rs`
- `crates/v8-runtime/build.rs`
- `crates/v8-runtime/src/isolate.rs`
- `crates/v8-runtime/src/session.rs`
- `crates/v8-runtime/src/execution.rs`
- `crates/v8-runtime/src/bridge.rs`
- `crates/v8-runtime/src/host_call.rs`
- `crates/v8-runtime/src/ipc_binary.rs`
- `crates/v8-runtime/src/ipc.rs`
- `crates/v8-runtime/src/snapshot.rs`
- `crates/v8-runtime/src/stream.rs`
- `crates/v8-runtime/src/timeout.rs`

What lives here:
- The daemon entrypoint, Unix-domain-socket listener, authentication, and connection loop in `main.rs`.
- V8 build/bootstrap support, ICU data setup, isolate creation, and promise-rejection tracking in `build.rs` and `isolate.rs`.
- Session ownership, thread-per-session execution, and concurrency slot control in `session.rs`.
- Script compilation, module execution, CJS/ESM handling, global injection, and error extraction in `execution.rs`.
- Value serialization, external refs, and injected bridge callbacks in `bridge.rs`.
- Sync-blocking bridge-call dispatch and `call_id` routing in `host_call.rs`.
- Binary wire protocol encode/decode in `ipc_binary.rs` and the older MessagePack IPC surface in `ipc.rs`.
- Snapshot creation and snapshot caching in `snapshot.rs`.
- Async stream-event dispatch back into V8 in `stream.rs`.
- Wall-clock timeout enforcement via `terminate_execution()` in `timeout.rs`.

### Native sidecar transport, protocol, ownership, and callback state machine

This is the framed control-plane state machine around the native sidecar.

Relevant files:
- `crates/sidecar/src/lib.rs`
- `crates/sidecar/src/protocol.rs`
- `crates/sidecar/src/state.rs`
- `crates/sidecar/src/stdio.rs`
- `crates/sidecar/src/main.rs`

What lives here:
- The top-level sidecar composition surface in `lib.rs`.
- The request/response/event wire schema, ownership scopes, VM/session/process payloads, permission policy payloads, root filesystem descriptors, tool payloads, and sidecar callback frames in `protocol.rs`.
- The long-lived in-memory state model for VMs, contexts, processes, listeners, sockets, sidecar callbacks, and tool executions in `state.rs`.
- The framed stdio host transport, callback routing, and event pump in `stdio.rs`.

### Native sidecar dispatch hub

This is the service-layer router that sits on top of the transport/state machine.

Relevant files:
- `crates/sidecar/src/service.rs`

What lives here:
- Request dispatch.
- Ownership enforcement.
- Permission-policy evaluation.
- Security audit/log/event emission.
- ACP orchestration paths that live in the service rather than in `acp/*`.

### Native sidecar VM lifecycle, rootfs bootstrap, and layering

This is the sidecar-owned VM construction and snapshot layer.

Relevant files:
- `crates/sidecar/src/vm.rs`
- `crates/sidecar/src/bootstrap.rs`

What lives here:
- VM creation and disposal.
- Root filesystem construction from descriptors and snapshots.
- Layer and overlay creation/sealing.
- Shadow-root creation and bootstrap directories.
- Mount reconciliation, module-access mount insertion, and command-path refresh.
- Snapshot import/export helpers and root-filesystem entry conversion.

### Native sidecar guest filesystem API

This is the direct guest filesystem API surface exposed by the sidecar.

Relevant files:
- `crates/sidecar/src/filesystem.rs`

What lives here:
- Guest filesystem request handling for read/write/mkdir/stat/readdir/etc.
- Content encoding/decoding between bytes and protocol payloads.

### Native sidecar shadow-root reconciliation

This subsystem keeps the kernel VFS and the sidecar’s host shadow tree aligned.

Relevant files:
- `crates/sidecar/src/filesystem.rs`
- `crates/sidecar/src/execution.rs`
- `crates/sidecar/src/service.rs`
- `crates/sidecar/src/vm.rs`

What lives here:
- Mirror-to-shadow on file writes.
- Sync-back from active shadow paths into the kernel before host reads.
- Host-directory, host-file, and host-symlink reconciliation into the kernel tree.
- Process-exit writeback and shadow-root bootstrap behavior that affects guest-visible state.

### Native sidecar tool virtualization

This is the subsystem that makes registered toolkits show up as VM commands.

Relevant files:
- `crates/sidecar/src/tools.rs`
- `crates/sidecar/src/execution.rs`
- `crates/sidecar/src/protocol.rs`

What lives here:
- Toolkit registration.
- Prompt/reference markdown generation for toolkits.
- CLI-style flag parsing from JSON Schema.
- Resolution of `agentos`, toolkit commands, and tool invocations into sidecar-dispatched virtual processes.

### Native sidecar process/runtime dispatch

This is the execution core that launches guest runtimes and sidecar-owned virtual processes.

Relevant files:
- `crates/sidecar/src/execution.rs`

What lives here:
- Runtime dispatch for JavaScript, Python, WASM, and sidecar-virtual tool processes.
- Runtime env assembly, entrypoint resolution, guest/host path mapping, and shadow materialization.
- JS child-process RPC handling and nested process management.

### Native sidecar networking policy and socket transports

This is the network policy and transport layer that sits on top of the runtime execution core.

Relevant files:
- `crates/sidecar/src/execution.rs`
- `crates/sidecar/src/state.rs`

What lives here:
- DNS resolution policy and resolver selection.
- Loopback policy, exempt-port handling, and guest-port to host-port translation.
- TCP, UDP, and Unix socket listen/connect/bind flows plus their state machines.
- Listener discovery, socket snapshots, and resource accounting for network objects.

### Native sidecar TLS, HTTP, and HTTP/2 planes

These are distinct guest-visible subsystems even though they share `execution.rs` and `state.rs`.

Relevant files:
- `crates/sidecar/src/execution.rs`
- `crates/sidecar/src/state.rs`

What lives here:
- TLS socket upgrade, client/server TLS state, cert material handling, and client-hello inspection.
- HTTP/1 loopback and outbound request bridging.
- HTTP/2 server/session/stream state, TLS handoff, event queues, and flow-control snapshots.

### Native sidecar builtin service RPCs

This is the sidecar-owned service surface behind some guest runtime builtin APIs.

Relevant files:
- `crates/sidecar/src/execution.rs`

What lives here:
- Guest crypto helper surfaces.
- SQLite bridge state.
- Kernel-stdin, PTY/raw-mode, and related runtime service RPC paths.

### Mount/plugin bridge and permission glue

This is the filesystem/permission adapter from the sidecar into the outer host bridge.

Relevant files:
- `crates/sidecar/src/bridge.rs`
- `crates/sidecar/src/plugins/mod.rs`

What lives here:
- The host-backed filesystem wrapper used for bridge-mounted filesystems.
- Host inode/link tracking and synthetic metadata overlay.
- Permission bridging from sidecar policies into kernel `Permissions`.
- Mount plugin registry construction and memory-mount helpers.

### ACP agent session layer

This is the sidecar-owned session-management surface for agent adapters that speak ACP over stdio.

Relevant files:
- `crates/sidecar/src/acp/client.rs`
- `crates/sidecar/src/acp/compat.rs`
- `crates/sidecar/src/acp/json_rpc.rs`
- `crates/sidecar/src/acp/session.rs`
- `crates/sidecar/src/service.rs`

What lives here:
- JSON-RPC message parsing and serialization.
- ACP request/response transport management, timeouts, notification fanout, and request dedupe.
- Compatibility shims for permission requests, cancel behavior, and agent-specific quirks.
- Session state, event sequencing, terminal capture, config/mode tracking, and compatibility-derived options.
- The ACP orchestration embedded in `service.rs`, including handshake, stdout prebuffering, permission-request normalization, terminal proxying, and close/kill wiring.

### First-party mount plugins

These are the mounted filesystems that the native sidecar can open through the kernel mount-plugin interface.

Relevant files:
- `crates/sidecar/src/plugins/mod.rs`
- `crates/sidecar/src/plugins/host_dir.rs`
- `crates/sidecar/src/plugins/module_access.rs`
- `crates/sidecar/src/plugins/js_bridge.rs`
- `crates/sidecar/src/plugins/sandbox_agent.rs`
- `crates/sidecar/src/plugins/s3.rs`
- `crates/sidecar/src/plugins/google_drive.rs`
- `registry/file-system/s3/src/index.ts`
- `registry/file-system/google-drive/src/index.ts`

What lives here:
- `mod.rs`: plugin registration order for the native sidecar.
- `host_dir.rs` and `module_access.rs`: the host-backed mount family, with `module_access` as a read-only policy wrapper around projected `node_modules`.
- `js_bridge.rs` and `sandbox_agent.rs`: callback-driven and remote-process-backed mounted filesystems.
- `s3.rs` and `google_drive.rs`: the object-store-backed persisted filesystem family, both with manifest/chunk storage over a `MemoryFileSystem` working tree.
- `registry/file-system/*/src/index.ts`: the public TypeScript helpers that emit declarative mount descriptors for the native `s3` and `google_drive` plugins.

### Browser-side sidecar variant

This is the alternate sidecar wrapper for browser-hosted execution.

Relevant files:
- `crates/sidecar-browser/src/lib.rs`
- `crates/sidecar-browser/src/service.rs`

What lives here:
- Browser-side bridge traits for worker creation and termination.
- A minimal sidecar service that manages VMs, contexts, and browser worker-backed executions on the main thread.

## Notes On Large Mixed Files

These files are single physical modules but contain multiple logical subsystems and should usually be split mentally when navigating the code:

- `crates/kernel/src/kernel.rs`
  - VM facade and syscall surface.
  - procfs synthesis.
  - command/shebang resolution.
  - mount and driver integration.
- `crates/execution/src/node_import_cache.rs`
  - Node loader templates.
  - builtin/polyfill asset materialization.
  - guest path scrubbing.
  - Pyodide asset staging.
- `crates/sidecar/src/service.rs`
  - protocol dispatch hub.
  - ownership enforcement.
  - permission-policy evaluation.
  - audit/log/event emission.
  - ACP orchestration.
- `crates/sidecar/src/execution.rs`
  - runtime dispatch.
  - runtime env/bootstrap and command resolution.
  - shadow sync/writeback.
  - networking and DNS.
  - TLS/HTTP/HTTP2.
  - JS child-process RPC.
  - crypto and SQLite bridge state.
  - PTY/kernel-stdin service RPCs.
  - process writeback to the kernel.

## Likely Future Splits

If this map is used as a refactor guide, the most obvious “too many systems in one file” candidates are:

- `crates/sidecar/src/execution.rs`
- `crates/sidecar/src/service.rs`
- `crates/execution/src/node_import_cache.rs`
- `crates/execution/src/javascript.rs`
- `crates/kernel/src/kernel.rs`
