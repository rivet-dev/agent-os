# agentOS

A high-level wrapper around the Agent OS runtime that provides a clean API for running coding agents inside isolated VMs via the Agent Communication Protocol (ACP).

## Agent OS Runtime

Agent OS is a **fully virtualized operating system**. The kernel, written as a Rust sidecar, provides a complete POSIX-like environment — virtual filesystem, process table, socket table, pipe/PTY management, and permission system. Guest code sees a self-contained OS and must never interact with the host directly. Every system call (file I/O, networking, process spawning, DNS resolution) must be mediated by the kernel. No guest operation may fall through to a real host syscall.

The kernel orchestrates three execution environments:

- **WASM processes** — A custom libc and Rust toolchain compile a full suite of POSIX utilities (coreutils, sh, grep, etc.) to WebAssembly. All WASM execution happens within the sidecar's managed runtime.
- **Node.js** — JS/TS runs inside Node.js child processes with ESM loader hooks that intercept `require()`/`import` for builtins. Every Node.js builtin module that guest code can import must be a **kernel-backed polyfill** — never the real Node.js module. See "Node.js Isolation Model" below.
- **Python (Pyodide)** — CPython compiled to WASM via Pyodide, running within the sidecar with kernel-backed file/network I/O.

All runtimes are managed by the sidecar's execution engines and kernel process table. Processes can spawn children across runtimes (e.g., a Node process can spawn a WASM shell). Guest code must never escape the sidecar's isolation boundary to run on the host.

### Virtualization invariants

These are hard rules with no exceptions:

1. **Every guest syscall goes through the kernel.** File reads go through the kernel VFS, not real `node:fs`. Network connections go through the kernel socket table, not real `node:net`. Process spawning goes through the kernel process table, not real `node:child_process`. DNS resolution goes through the kernel's DNS resolver, not real `node:dns`. There are no shortcuts where guest code touches host resources directly.
2. **No real host builtins.** When a guest does `require('fs')` or `import net from 'node:net'`, the module loader must return a kernel-backed polyfill. If no polyfill exists yet, the builtin must be denied (`ERR_ACCESS_DENIED`). The loader must never fall through to Node.js's default resolution and hand the guest the real host module.
3. **The host is an implementation detail.** Guest code should not be able to observe that it is running on a host Node.js process. `process.pid` should be the kernel PID, `os.hostname()` should be the kernel hostname, `fs.readdirSync('/')` should show the kernel VFS root. `process.cwd()` should return the kernel CWD, not a host path. `process.env` must not contain internal `AGENT_OS_*` control variables. Error messages and stack traces must not reveal host filesystem paths. `require.resolve()` must return guest-visible paths, not host paths. Any host state leaking through to the guest is a bug.
4. **Polyfills are ports, not wrappers.** A path-translating shim over real `node:fs` is not a polyfill — it is a wrapper around a host API. A real polyfill implements the API semantics using only kernel primitives (VFS, socket table, process table, pipe manager). The original JS kernel (`@secure-exec/core` + `@secure-exec/nodejs`, deleted in commit `5a43882`) had full kernel-backed polyfills for `fs`, `net`, `http`, `dns`, `dgram`, `child_process`, and `os`. The Rust sidecar must reach the same level of isolation.
5. **Control channels must be out-of-band.** The sidecar must not use in-band magic prefixes on stdout/stderr for control signaling (exit codes, metrics, signal registration). Guest code can write these prefixes to inject fake control messages. Use dedicated file descriptors, separate pipes, or a side-channel protocol for all sidecar-internal communication.
6. **Resource consumption must be bounded.** Every guest-allocatable resource must have a configurable limit enforced by the kernel: filesystem total size, inode count, process count, open FDs, pipes, PTYs, sockets, connections. Unbounded allocation from guest input is a DoS vector. The kernel's `ResourceLimits` must cover all resource types, not just processes and FDs.
   Sidecar metadata parsing should start from `ResourceLimits::default()` and only override keys that are actually present; rebuilding the struct from sparse metadata drops default filesystem byte/inode caps.
   Per-operation memory guards also live in `ResourceLimits`: bound `pread`, `fd_write`/`fd_pwrite`, merged spawn `argv`/`env`, and `readdir` batches in `crates/kernel/src/kernel.rs`, and keep the matching `resource.max_*` metadata keys in `crates/sidecar/src/service.rs` in sync so the limits remain configurable.
   WASM runtime caps are also carried through `ResourceLimits`: `crates/sidecar/src/service.rs` maps the configured `max_wasm_*` fields into reserved `AGENT_OS_WASM_*` env keys, and `crates/execution/src/wasm.rs` is responsible for enforcing the resulting fuel/memory/stack limits before guest code runs.
   WebAssembly parser hardening in `crates/execution/src/wasm.rs` must stat module files before `fs::read()`, cap import/memory section entry counts before iterating them, and bound varuint encodings by byte length so malformed or oversized modules fail closed without parser DoS.
7. **Permission checks must use resolved paths.** Whenever the kernel checks permissions on a path, it must resolve symlinks first and check the resolved path. Checking the caller-supplied path and then operating on a symlink-resolved target is a TOCTOU bypass. Similarly, `link()` must check permissions on both source and destination.
8. **The VM must behave like a standard Linux environment.** Agents are written to target Linux. The kernel should implement POSIX semantics faithfully — correct `errno` values, proper signal delivery, standard `/proc` layout, expected filesystem behavior. Deviations from standard Linux behavior cause agent failures and must be documented in the friction log (`.agent/notes/vm-friction.md`). When in doubt, match Linux kernel behavior, not a simplified model.

### Key subsystems

- **Virtual filesystem (VFS)** — Layered chunked architecture: `ChunkedVFS` composes `FsMetadataStore` (directory tree, inodes, chunk mapping) + `FsBlockStore` (key-value blob store) into a `VirtualFileSystem`. Tiered storage keeps small files inline in metadata; larger files are split into chunks in the block store. The device layer (`/dev/null`, `/dev/urandom`, `/dev/pts/*`, etc.), proc layer (`/proc/[pid]/*`), and permission wrapper sit on top. All layers implement the `VirtualFileSystem` interface with full POSIX semantics.
- **Process management** — Kernel-wide process table tracks PIDs across all runtimes. Full POSIX process model: parent/child relationships, process groups, sessions, signals (SIGCHLD, SIGTERM, SIGWINCH), zombie cleanup, and `waitpid`. Each process gets its own FD table (0-255) with refcounted file descriptions supporting dup/dup2.
  Advisory `flock` state should stay kernel-global but be owned by the shared open-file-description (`FileDescription.id()`), keyed by the opened file identity, and released only when the last refcounted FD closes; dup/fork inheritance must see the same lock while separate opens still conflict.
  Per-FD status bits such as `O_NONBLOCK` belong on `FdEntry` / `ProcessFdTable`, while shared `FileDescription.flags()` should stay limited to open-file-description semantics such as access mode and `O_APPEND`; `/dev/fd/N` duplication can layer new per-FD flags without mutating the shared description.
  Host-side liveness probes that must not reap runtime children should use `waitid(..., WNOWAIT | WNOHANG | WEXITED | WSTOPPED | WCONTINUED)` rather than `waitpid`; the sidecar uses that non-reaping check before signaling host child PIDs to avoid PID-reuse races.
  Parent-aware `waitpid` state tracking belongs in `crates/kernel/src/process_table.rs`: queue stop/continue notifications there, and only let `crates/kernel/src/kernel.rs` clean up process resources after an exited child is actually reaped.
  Process exit handling in `crates/kernel/src/process_table.rs` has to keep child reparenting, orphaned stopped-process-group `SIGHUP`/`SIGCONT` delivery, and zombie-aware `max_processes` accounting aligned; changing only one of those paths breaks Linux-style lifecycle semantics.
  POSIX signal side effects that depend on the calling PID should stay at `KernelVm` syscall entrypoints instead of low-level primitives: `PipeManager` only reports broken-pipe `EPIPE`, while `crates/kernel/src/kernel.rs` `fd_write` is responsible for turning that into guest-visible `SIGPIPE` delivery.
  Job-control signal state transitions should stay aligned across `crates/kernel/src/process_table.rs` and `crates/kernel/src/kernel.rs`: `ProcessTable::kill(...)` owns `SIGSTOP`/`SIGTSTP`/`SIGCONT` status changes and `waitpid` notifications, while PTY resize should emit `SIGWINCH` from the `KernelVm` entrypoint after the PTY layer reports the foreground process group.
- **Pipes & PTYs** — Kernel-managed pipes (64KB buffers) enable cross-runtime IPC. PTY master/slave pairs with line discipline support interactive shells. `openShell()` allocates a PTY and spawns sh/bash.
- **Networking** — Socket table manages TCP/UDP/Unix domain sockets. Loopback connections stay entirely in-kernel. External connections delegate to a `HostNetworkAdapter` (implemented via `node:net`/`node:dgram` on the host). DNS resolution also goes through the adapter.
- **Permissions** — Deny-by-default access control. Four permission domains: `fs`, `network`, `childProcess`, `env`. Each is a function that returns `{allow, reason}`. The `allowAll` preset grants everything (used in agentOS). See "Node.js Builtin Permission Model" for how these interact with the Node.js builtin interception layer.
- **Kernel VM configs must opt into broad access explicitly.** `KernelVmConfig::new()` should stay deny-all by default; tests, browser scaffolds, or other callers that need unrestricted behavior must set `config.permissions = Permissions::allow_all()` themselves.
- **Sensitive mount policy is a separate filesystem capability.** Kernel mount APIs check normal `fs.write` permission on the mount path, and mounts targeting `/`, `/etc`, or `/proc` also require `fs.mount_sensitive`. In the Rust sidecar, `configure_vm` reconciles mounts before it applies `payload.permissions`, so mount-time policy must already be present on the VM (or be injected directly in tests) before `ConfigureVm` runs.

### Node.js Isolation Model

**Current state (KNOWN DEFICIENT — see `.agent/todo/node-isolation-gaps.md`):**

Guest Node.js code currently runs as **real host Node.js child processes** spawned via `std::process::Command::new("node")` in the Rust sidecar (`crates/execution/src/javascript.rs`). The ESM loader hooks intercept `require()`/`import` but most builtins either fall through to the real host module or are thin wrappers that call real host APIs. This violates the virtualization invariants above.

**Prior art — the original JS kernel had full polyfills:**

Before the Rust sidecar (commit `5a43882`), the JS kernel (`@secure-exec/core` + `@secure-exec/nodejs` + `packages/posix/`) had complete kernel-backed polyfills for all builtins. The pattern was:
- **Kernel socket table** — `kernel.socketTable.create/connect/send/recv` managed all TCP/UDP. Loopback stayed in-kernel; external connections went through a `HostNetworkAdapter`.
- **Kernel VFS** — All `fs` operations routed through the kernel VFS via syscall RPC.
- **Kernel process table** — `child_process.spawn` routed through `kernel.spawn()`.
- **SharedArrayBuffer RPC** — Synchronous syscalls from worker threads used `Atomics.wait` + shared memory buffers (same pattern the Pyodide VFS bridge uses today).
- **Module hijacking** — `require('net')` returned the kernel-backed socket implementation, not real `node:net`.

The Rust sidecar kernel already has the VFS, process table, pipe manager, PTY manager, and permission system. What's missing is porting the **polyfill layer** — the code that makes `require('fs')` return a kernel-backed implementation instead of real `node:fs`. This is a port of proven patterns, not a greenfield design.

**Current reality vs required state:**

| Builtin | Required | Current | Gap |
|---------|----------|---------|-----|
| `fs` / `fs/promises` | Kernel VFS polyfill | Path-translating wrapper over real `node:fs` | Port: route through kernel VFS via RPC |
| `child_process` | Kernel process table polyfill | Path-translating wrapper over real `node:child_process` | Port: route through kernel process table |
| `net` | Kernel socket table polyfill | **No wrapper — falls through to real `node:net`** | Port: kernel socket table polyfill |
| `dgram` | Kernel socket table polyfill | **No wrapper — falls through to real `node:dgram`** | Port: kernel socket table polyfill |
| `dns` | Kernel DNS resolver polyfill | **No wrapper — falls through to real `node:dns`** | Port: kernel DNS resolver polyfill |
| `http` / `https` / `http2` | Built on kernel `net` polyfill | **No wrapper — falls through to real module** | Port: builds on `net` polyfill |
| `tls` | Kernel TLS polyfill | Guest-owned polyfill in `node_import_cache.rs` wraps the existing guest `net` transport with host TLS state (`tls.connect({ socket })`, `new TLSSocket(socket, { isServer: true, ... })`) | Keep client/server entrypoints on guest sockets and avoid direct host `node:tls` listeners/connections |
| `os` | Kernel-provided values | Guest-owned polyfill in `node_import_cache.rs` virtualizes hostname, CPU, memory, loopback networking, home, and user info | Keep future `os` additions aligned with VM defaults and kernel-backed resource config |
| `vm` | Must be denied | **No wrapper — falls through to real `node:vm`** | Must stay denied |
| `worker_threads` | Must be denied | **No wrapper — falls through to real module** | Must stay denied |
| `inspector` | Must be denied | **No wrapper — falls through to real module** | Must stay denied |
| `v8` | Must be denied | **No wrapper — falls through to real module** | Must stay denied |

**How the loader interception works** (`crates/execution/src/node_import_cache.rs`):

ESM loader hooks (`loader.mjs`) and CJS `Module._load` patches (`runner.mjs`) are generated from Rust string templates. Every `import`/`require` is intercepted:
1. `resolveBuiltinAsset()` — checks `BUILTIN_ASSETS` list. Redirects to a kernel-backed polyfill file.
2. `resolveDeniedBuiltin()` — checks `DENIED_BUILTINS` set. Redirects to a stub that throws `ERR_ACCESS_DENIED`. A builtin is in `DENIED_BUILTINS` only if it is NOT in `ALLOWED_BUILTINS`.
3. **Fall through to `nextResolve()`** — Node.js default resolution. Returns the real host module. **This must never happen for any builtin that guest code can import.**

`AGENT_OS_ALLOWED_NODE_BUILTINS` (JSON string array env var) controls which builtins are removed from the deny list. `DEFAULT_ALLOWED_NODE_BUILTINS` in `packages/core/src/sidecar/native-kernel-proxy.ts` currently includes all builtins — this must be reduced to only builtins that have kernel-backed polyfills.

**Additional hardening layers (defense-in-depth, NOT primary isolation):**
1. **`globalThis.fetch` hardening** — Replaced with `restrictedFetch` (loopback-only on exempt ports). Does NOT cover `http.request()`, `net.connect()`, or `dgram.createSocket()`.
2. **Node.js `--permission` flag** — OS-level backstop for filesystem and child_process only. No network restrictions. This is a safety net, not the isolation boundary.
3. **Guest env stripping** — `NODE_OPTIONS`, `LD_PRELOAD`, `DYLD_INSERT_LIBRARIES`, `LD_LIBRARY_PATH` stripped before spawn.

### What agentOS adds on top

agentOS wraps the kernel and adds: a high-level filesystem/process API, ACP agent sessions (JSON-RPC over stdio), and a `ModuleAccessFileSystem` overlay that projects host `node_modules/` into the VM read-only so agents have access to their dependencies.

## Project Structure

- **Monorepo**: pnpm workspaces + Turborepo + TypeScript + Biome
- **Core package**: `@rivet-dev/agent-os` in `packages/core/` -- contains everything (VM ops, ACP client, session management)
- **Registry types**: `@rivet-dev/agent-os-registry-types` in `packages/registry-types/` -- shared type definitions for WASM command package descriptors. The registry software packages link to this package. When changing descriptor types, update here and rebuild the registry.
- **npm scope**: `@rivet-dev/agent-os-*`
- **Actor integration** lives in the Rivet repo at `rivetkit-typescript/packages/rivetkit/src/agent-os/`, not as a separate package
- **The actor layer must maintain 1:1 feature parity with AgentOs.** Every public method on the `AgentOs` class (`packages/core/src/agent-os.ts`) must have a corresponding actor action in the Rivet repo's `rivetkit-typescript/packages/rivetkit/src/agent-os/`. Subscription methods (onProcessStdout, onShellData, onCronEvent, etc.) are wired through actor events. Lifecycle methods (dispose) are handled by the actor's onSleep/onDestroy hooks. When adding a new public method to AgentOs, add the corresponding actor action in the same change. This includes changes to method signatures, option types, return types, and configuration interfaces -- any API surface change in AgentOs must be mirrored in the actor layer. **Always ask the user which Rivet repo/path to update** (e.g., `~/r-aos`, `~/r16`, etc.) before making changes there.
- **The RivetKit driver test suite must have full feature coverage of all agent-os actor actions.** Tests live in the Rivet repo's `rivetkit-typescript/packages/rivetkit/src/driver-test-suite/tests/`. When adding a new actor action, add a corresponding driver test in the same change.
- **The core quickstart (`examples/quickstart/`) and the RivetKit example (in the Rivet repo at `examples/agent-os/`) must stay in sync.** Both cover the same set of features (hello-world, filesystem, processes, network, cron, tools, agent-session, sandbox) with identical behavior, just different APIs. Core uses `AgentOs.create()` directly; RivetKit uses `agentOs()` actor with client-server split. When adding or changing a quickstart example, update both.

## Registry

The `registry/` directory contains four categories of extension packages, all published under `@rivet-dev/agent-os-*`:

1. **Agents** (`registry/agent/`) — ACP adapter packages that let specific coding agents run inside the VM. Each agent package wraps an agent SDK or CLI with an ACP adapter binary. Examples: `@rivet-dev/agent-os-pi`, `@rivet-dev/agent-os-pi-cli`, `@rivet-dev/agent-os-opencode`.
2. **File systems** (`registry/file-system/`) — First-party filesystem helpers and storage integrations. Migrated drivers like `@rivet-dev/agent-os-s3` now emit declarative native mount descriptors, while remaining storage packages can still expose lower-level block-store helpers until their native cutovers land.
3. **Tools** (`registry/tool/`) — Extension toolkits that add capabilities to the VM. Example: `@rivet-dev/agent-os-sandbox` (Sandbox Agent SDK integration with `createSandboxFs()` and `createSandboxToolkit()`).
4. **Software** (`registry/software/`) — Pre-built WASM command binaries (coreutils, grep, sed, etc.) compiled from Rust and C source in `registry/native/`. See `registry/CLAUDE.md` for naming conventions, package types, and how to add new packages.

### Release and publishing

**The main release script (`scripts/release.ts`) only handles the core TypeScript packages** (`packages/core/`, `packages/registry-types/`, etc.). It bumps the version in `packages/core/package.json`, commits, tags, and triggers the `release.yml` CI workflow which builds and publishes via `npm publish`.

**Registry packages (agents, file-systems, tools) are normal TypeScript packages** that follow the same semver versioning as core. They are currently published manually but share the same release cadence.

**Software packages are on a separate track.** They require a native build step (Rust nightly + wasi-sdk for C) to compile WASM binaries before they can be published. They use date-based versioning (`0.0.YYMMDDHHmmss`) instead of semver and are published via `make publish` from `registry/`. The software publish pipeline skips unchanged packages via content hashing. Software packages have no dependency on the core release cycle.

**Publish timing:** Core TypeScript packages (10 packages) take ~50 seconds via CI (`release.yml`). Software packages (19 packages with WASM binaries) take ~3 minutes via `make publish` locally. A full fresh publish of all 29 packages takes ~4 minutes total.

The registry software packages depend on `@rivet-dev/agent-os-registry-types` (in `packages/registry-types/`) via workspace link. This is the single source of truth for descriptor types like `WasmCommandPackage` and `WasmMetaPackage`.

## Terminology

- Call instances of the OS **"VMs"**, never "sandboxes"

## Architecture

- **The VM base filesystem artifact is derived from Alpine Linux, but runtime source should stay generic.** `packages/core/src/` must not hardcode Alpine-specific defaults or import Alpine-named helpers. The runtime consumes `packages/core/fixtures/base-filesystem.json` as the default root layer.
- **Base filesystem rebuild flow:** first capture a fresh Alpine snapshot with `pnpm --dir packages/core snapshot:alpine-defaults`, which writes `packages/core/fixtures/alpine-defaults.json`. Then run `pnpm --dir packages/core build:base-filesystem`, which rewrites the required AgentOs-specific values (for example `HOSTNAME=agent-os` and `/etc/hostname`) and emits `packages/core/fixtures/base-filesystem.json`. AgentOs uses that built artifact as the lower layer of an overlay-backed root filesystem.
- **The default VM filesystem model should be Docker-like.** The root filesystem should be a layered overlay view with one writable upper layer on top of one or more immutable lower snapshot layers. The base filesystem artifact is the initial lower layer; additional frozen lower layers may be stacked beneath the writable upper if needed. Do not design the default VM root as a pile of ad hoc post-boot mutations.
- **Everything runs inside the VM.** Agent processes, servers, network requests -- all spawned inside the Agent OS kernel, never on the host. This is a hard rule with no exceptions.
- **All guest code must execute within the kernel's isolation boundary (WASM or in-kernel isolate).** No runtime may escape to a host-native process. If a language runtime requires a JavaScript host (e.g., Emscripten-compiled WASM like Pyodide), the JS host must itself run inside the kernel — not as a host-side Node.js subprocess. Spawning an unsandboxed host process to run guest code is never acceptable, even as a convenience shortcut. New runtimes must either compile to WASI (so they run in the kernel's WASM engine directly) or run inside an already-sandboxed in-kernel isolate.
- **Guest code must never touch real host APIs.** Every `require('fs')`, `require('net')`, `require('child_process')`, `require('dns')`, `require('dgram')`, `require('http')`, etc. must return a kernel-backed polyfill that routes operations through the kernel's VFS, socket table, process table, and DNS resolver respectively. Path-translating wrappers over real `node:fs` or real `node:child_process` are NOT acceptable — they call real host syscalls. The original JS kernel had full polyfills for all of these; the Rust sidecar must match that level of isolation. If a polyfill does not exist yet for a builtin, that builtin must be denied at the loader level until one is built.
- **Native sidecar permission policy has to be available during `create_vm`, not just `configure_vm`.** Guest env filtering and kernel bootstrap driver registration happen while the VM is being constructed, so `AgentOsOptions.permissions` must be serialized into the `CreateVmRequest`; `configure_vm` can only mirror or refine that policy after the fact.
- **Permissioned Pyodide host launches still need `--allow-worker`.** `crates/execution/src/python.rs` bootstraps through Node's internal ESM loader worker, so the host process must keep `--allow-worker` enabled even while guest `worker_threads` stays denied.
- **WASM permission tiers must gate host Node WASI access as well as guest-side preopens.** In `crates/execution/src/wasm.rs`, keep `Isolated` executions off `--allow-wasi` entirely, and let `ReadOnly` / `ReadWrite` / `Full` differentiate the read/write scope through the guest WASI layer rather than a blanket host flag.
- **`sandbox_agent` mounts on `sandbox-agent@0.4.2` only get basic file endpoints (`entries`, `file`, `mkdir`, `move`, `stat`) from the HTTP fs API.** When the sidecar needs symlink/readlink/realpath/link/chmod/chown/utimes semantics, it must use the remote process API as a fallback and return `ENOSYS` when that helper path is unavailable.
- The `AgentOs` class wraps the kernel and proxies its API directly
- **All public methods on AgentOs must accept and return JSON-serializable data.** No object references (Session, ManagedProcess, ShellHandle) in the public API. Reference resources by ID (session ID, PID, shell ID). This keeps the API flat and portable across serialization boundaries (HTTP, RPC, IPC).
- Filesystem methods mirror the kernel API 1:1 (readFile, writeFile, mkdir, readdir, stat, exists, move, delete)
- **Per-process filesystem state such as `umask` belongs in `ProcessContext` / `ProcessTable`.** Kernel create/write entrypoints should read it there, and any guest Node exposure must be threaded through the JavaScript sync-RPC bridge (`crates/sidecar/src/service.rs` and `crates/execution/src/node_import_cache.rs`) instead of inheriting host `process` behavior.
- **`VirtualStat` additions must be propagated end-to-end.** When stat grows new fields, update kernel-backed storage stats, synthetic `/proc` and `/dev` stats, sidecar mount/plugin conversions, sidecar protocol serialization, and the TypeScript `VirtualStat` / `GuestFilesystemStat` adapters together or some callers will silently keep incomplete metadata.
- **readdir returns `.` and `..` entries** — always filter them when iterating children to avoid infinite recursion
- Guest Node `fs` and `fs/promises` polyfills share the JavaScript sync-RPC transport between `crates/execution/src/node_import_cache.rs` and `crates/sidecar/src/service.rs`; Node-facing `readdir` results must filter `.`/`..`, async methods should dispatch under `fs.promises.*`, fd-based APIs (`open`, `read`, `write`, `close`, `fstat`) plus `createReadStream`/`createWriteStream` should ride the same bridge, and runner-internal pipe/control writes must keep snapped host `node:fs` bindings because `syncBuiltinModuleExports(...)` mutates the builtin module for guests.
- JavaScript sync RPC timeouts and slow-reader backpressure should be enforced in `crates/execution/src/javascript.rs`, not in the generated runner: track the pending request ID on the host, auto-emit `ERR_AGENT_OS_NODE_SYNC_RPC_TIMEOUT` after the configured wait, queue replies through a bounded async writer so slow guest reads cannot block the sidecar thread, and have `crates/sidecar/src/service.rs` ignore stale `sync RPC request ... is no longer pending` races after the timeout fires.
- Execution-host runner scripts that are materialized by `NodeImportCache` should live as checked-in assets under `crates/execution/assets/runners/` and be loaded via `include_str!`; when testing import-cache temp-root cleanup, use a dedicated `NodeImportCache::new_in(...)` base dir so the one-time sweep stays isolated to that root.
- Active JavaScript/Python/WASM executions must hold a `NodeImportCache` cleanup guard until the child exits; otherwise dropping the engine can delete `timing-bootstrap.mjs` and related assets while the host runtime is still importing them.
- Sidecar-owned JavaScript, Python, and WASM engines should also get distinct per-VM import-cache base dirs under the sidecar cache root; sharing one temp root lets one VM sweep another VM's runner assets during stale-cache cleanup.
- Guest path scrubbing in `crates/execution/src/node_import_cache.rs` should treat the real `HOST_CWD` as an implicit runtime-only mapping to the virtual guest cwd (for example `/root`) so entrypoint imports and stack traces stay usable without leaking the host path, and reserve `/unknown` for absolute host paths outside visible mappings or the internal cache roots.
- Native-sidecar Node launches need one shared host shadow root across `CreateVm.metadata.cwd` and `packages/core/src/sidecar/native-kernel-proxy.ts` `shadowRoot`; if those diverge, sidecar `execute` rejects the Node cwd as escaping the VM sandbox root.
- CommonJS module isolation in `crates/execution/src/node_import_cache.rs` has to patch `Module._resolveFilename` and the guest-facing `Module._cache` / `require.cache` view together; wrapping only `createGuestRequire()` does not constrain local `require()` inside already-loaded `.cjs` modules.
- Guest-visible `process` hardening in `crates/execution/src/node_import_cache.rs` should harden properties on the real host `process` before swapping in the guest proxy, and the proxy fallback must resolve via the proxy receiver (`Reflect.get(..., proxy)`) so accessors inherit the virtualized surface instead of the raw host object.
- Node import prewarm in `crates/execution/src/javascript.rs` should stick to `node:` builtin specifiers instead of `agent-os:` synthetic URLs; newer Node warmup paths can reject custom schemes before the loader resolves them.
- Guest `child_process` launches should keep public child env and Node bootstrap internals separate: strip all `AGENT_OS_*` keys from the RPC `options.env` payload in `crates/execution/src/node_import_cache.rs`, carry only the Node runtime bootstrap allowlist in `options.internalBootstrapEnv`, and re-inject that allowlisted map only when `crates/sidecar/src/service.rs` starts a nested JavaScript runtime.
- Guest Node `net` Unix-socket support follows the same split as TCP: resolve guest socket paths against `host_dir` mounts when possible, otherwise map them under the VM sandbox root on the host, keep active Unix listeners/sockets in `crates/sidecar/src/service.rs`, and mirror non-mounted listener paths into the kernel VFS so guest `fs` APIs can see the socket file.
- When a guest Node networking port stops using real host listeners, mirror that state in `crates/sidecar/src/service.rs` `ActiveProcess` tracking and consult it from `find_listener`/socket snapshot queries before falling back to `/proc/[pid]/net/*`; procfs only sees host-owned sockets, not sidecar-managed polyfill listeners.
- Sidecar-managed loopback `net.listen` / `dgram.bind` listeners now use guest-port to host-port translation in `crates/sidecar/src/service.rs`: preserve guest-visible loopback addresses/ports in RPC responses and socket snapshots, but use the hidden host-bound port for external host-side probes and test clients.
- Sidecar JavaScript networking policy should read internal bootstrap env like `AGENT_OS_LOOPBACK_EXEMPT_PORTS` from `VmState.metadata` / `env.*`, not `vm.guest_env`; `guest_env` is permission-filtered and may be empty even when sidecar-only policy still needs the value.
- Timer-driven guest Node networking can outlive top-level module evaluation, so the `node_import_cache.rs` runner must keep the sync-RPC bridge alive until process exit instead of disposing it as soon as the entry module resolves.
- Guest Node `tls` should stay layered on the guest `net` polyfill rather than importing host `node:tls` directly: client connections must pass a preconnected guest socket into `tls.connect({ socket })`, and server handshakes should wrap accepted guest sockets with `new TLSSocket(..., { isServer: true })` and emit `secureConnection` from the wrapped socket's `secure` event.
- When a newly allowed Node builtin still has bypass-capable host-owned helpers or constructors (for example `dns.Resolver` / `dns.promises.Resolver`), replace those entrypoints with guest-owned shims or explicit unsupported stubs before adding the builtin to `DEFAULT_ALLOWED_NODE_BUILTINS`; inheriting the host module is only safe for exports that cannot escape the kernel-backed port.
- Command execution mirrors the kernel API (exec, spawn)
- `fetch(port, request)` reaches services running inside the VM using the kernel network adapter pattern (`proc.network.fetch`)
- Python execution in `crates/execution/src/python.rs` should keep `poll_event()` blocked until a real guest-visible event arrives or the caller timeout expires; filtered stderr/control messages are internal noise, `wait(None)` should still enforce the per-run `AGENT_OS_PYTHON_EXECUTION_TIMEOUT_MS` cap, `wait()` should bound accumulated stdout/stderr via the hidden `AGENT_OS_PYTHON_OUTPUT_BUFFER_MAX_BYTES` env knob rather than growing buffers without limit, and Node heap caps from `AGENT_OS_PYTHON_MAX_OLD_SPACE_MB` need to apply to both prewarm and execution launches without leaking those control vars into guest `process.env`.
- Pyodide bootstrap hardening in `crates/execution/src/node_import_cache.rs` must stay staged: `globalThis` guards can go in before `loadPyodide()`, but mutating `process` before `loadPyodide()` breaks the bundled Pyodide runtime under Node `--permission`.

## Linux Compatibility

The VM must behave like a standard Linux environment. Agents are written to target Linux and will break on non-standard behavior.

- **Target: Linux userspace compatibility.** The kernel is not reimplementing the Linux kernel — it is providing a POSIX-like userspace environment. The goal is that a program written for Linux should run inside the VM without modification, subject to the execution runtimes available (Node.js, WASM, Python).
- **Correct errno values.** Every kernel operation that fails must return the correct POSIX errno (`ENOENT`, `EACCES`, `EEXIST`, `EISDIR`, `ENOTDIR`, `EXDEV`, `EBADF`, `EPERM`, `ENOSYS`, etc.). Agents check errno values to decide control flow — wrong errnos cause cascading failures.
- **Standard `/proc` layout.** `/proc/self/`, `/proc/[pid]/`, `/proc/[pid]/fd/`, `/proc/[pid]/environ`, `/proc/[pid]/cwd`, `/proc/[pid]/cmdline` should contain the expected content. Many tools and runtimes read `/proc` to discover their own state.
- **Synthetic procfs paths use guest-visible permission subjects.** Kernel-owned `/proc/...` entries are virtual, so permission checks for procfs access should authorize the guest-visible proc path directly rather than resolving through the backing VFS realpath. Otherwise procfs availability silently depends on whether the mounted root happens to contain a physical `/proc` directory.
- **Standard `/dev` devices.** `/dev/null`, `/dev/zero`, `/dev/urandom`, `/dev/stdin`, `/dev/stdout`, `/dev/stderr`, `/dev/fd/*`, `/dev/pts/*` must exist and behave correctly. `/dev/urandom` must return cryptographically random bytes, not deterministic values.
- **Stream-device byte counts belong on length-aware read paths.** For unbounded devices such as `/dev/zero` and `/dev/urandom`, exact Linux-style byte-count assertions should target `pread` / `fd_read` in `crates/kernel/src/device_layer.rs` and kernel FD tests; `read_file()` has no byte-count parameter and is only a bounded helper for whole-file-style callers.
- **Correct signal semantics.** `SIGCHLD` must be delivered to parent on child exit. `SIGPIPE` must be generated on write to broken pipe. `SIGWINCH` must be delivered on terminal resize. Signal delivery must respect process groups and sessions.
- **Standard filesystem paths.** `/tmp` must be writable. `/etc/hostname`, `/etc/resolv.conf`, `/etc/passwd`, `/etc/group` should contain valid content. `/usr/bin/env` should exist for shebangs. Shell (`/bin/sh`, `/bin/bash`) must be available.
- **Direct script exec should resolve registered stubs before reparsing files.** When the kernel executes a path under `/bin/` or `/usr/bin/` that corresponds to a registered command driver, dispatch that driver directly before falling back to shebang parsing; otherwise command stubs like `/bin/sh` recurse into their own `#!` wrapper instead of behaving like the real executable.
- **Environment variable conventions.** `HOME`, `USER`, `PATH`, `SHELL`, `TERM`, `HOSTNAME`, `PWD`, `LANG` must be set to reasonable values. `PATH` must include standard directories where commands are found.
- **Document deviations in the friction log.** Any behavior that differs from standard Linux must be documented in `.agent/notes/vm-friction.md` with the deviation, root cause, and whether a fix exists or is planned.

## Virtual Filesystem Design Reference

- The VFS chunking and metadata architecture is modeled after **JuiceFS** (https://juicefs.com/docs/community/architecture/). Reference JuiceFS docs when designing chunk/block storage, metadata engine separation, or read/write data paths.
- Key JuiceFS concepts that apply: three-tier data model (Chunk/Slice/Block), pluggable metadata engines (SQLite, Redis, PostgreSQL), fixed-size block storage in object stores (S3), and metadata-data separation.
- For detailed design analysis: https://juicefs.com/en/blog/engineering/design-metadata-data-storage

### Agent-OS filesystem packages

- The old `fs-sqlite` and `fs-postgres` packages were deleted. They are replaced by the Agent OS `SqliteMetadataStore` and the `ChunkedVFS` composition layer.
- File system drivers live in `registry/file-system/` (see Registry section above). Prefer their declarative mount helpers when available; the legacy custom-`VirtualFileSystem` path is only for arbitrary caller-supplied filesystems and compatibility fallbacks.
- The Rivet actor integration (in the Rivet repo at `rivetkit-typescript/packages/rivetkit/src/agent-os/`) currently uses `ChunkedVFS(InMemoryMetadataStore + InMemoryBlockStore)` as legacy temporary infrastructure. This is not an acceptable long-term model for filesystem correctness. Filesystem semantics must move to durable metadata and block storage rather than transient in-memory state.

## Filesystem Conventions

- **OS-level content uses mounts, not post-boot writes.** If agentOS needs custom directories in the VM (e.g., `/etc/agentos/`), mount a pre-populated filesystem at boot — don't create the kernel and then write files into it afterward. This keeps the root filesystem clean and makes OS-provided paths read-only so agents can't tamper with them.
- **Filesystem semantics must be durable.** Any state that changes filesystem behavior — including overlay deletes, whiteouts, tombstones, copy-up state, directory entries, inode metadata, or file contents — must be represented in durable filesystem or metadata storage. Do not implement correctness-critical filesystem behavior with in-memory side tables, in-memory whiteout sets, or other transient hacks.
- **Overlay metadata must stay out-of-band from the merged tree.** If an overlay implementation persists whiteouts or opaque-directory markers in the writable upper, store them under a reserved hidden metadata root and make every merged overlay read/snapshot path filter that root back out of user-visible results.
- **Overlay mutating ops need raw-layer checks plus upper-layer moves.** Once copy-up marks directories opaque, merged `read_dir()` no longer tells you whether lower layers still hold children, so `rmdir`-style emptiness checks must inspect raw upper and lower entries directly. For identity-preserving ops like `rename`, stage the source into the writable upper first and then call the upper filesystem's native `rename` so hardlinks and inode identity survive the move.
- **Overlay filesystem behavior must match Linux OverlayFS as closely as possible, including mount-boundary semantics.** Treat the kernel OverlayFS docs as normative. OverlayFS overlays directory trees, not the mount table: the merged hierarchy is its own standalone mount, not a bind mount over underlying mounts. Do not design root overlay logic that "sees through" or absorbs unrelated mounted filesystems. Mounted filesystems remain separate mount boundaries, and cross-mount operations must keep normal mount semantics (`EXDEV`, separate identity, separate read-only rules). If we want overlay behavior inside a mounted filesystem such as an S3-backed or host-backed mount, that mounted filesystem must implement the layered metadata semantics itself rather than relying on the parent/root overlay to compose across the mount boundary.
- **User-facing filesystem APIs should distinguish mounts from layers.** Mounts are separate mounted filesystems presented to the kernel VFS. Layers are overlay-building blocks used to construct a layered filesystem. Do not collapse those into one generic concept. A plain mounted `VirtualFileSystem` is not automatically a valid overlay layer. Overlay construction should consume explicit layer handles: one writable upper layer plus zero or more immutable lower snapshot layers.
- **Middle layers in a Docker-like stack should be frozen layers, not extra writable uppers.** Linux OverlayFS supports one writable upper per overlay mount. Additional stacked layers should be represented as immutable snapshot/materialized lower layers. They may share the same layer-handle interface as the upper layer, but their state must mark them frozen/read-only. Any live whiteouts, opaque markers, or copy-up bookkeeping belong only to the active writable upper; once a layer is sealed into a reusable lower snapshot, it must be materialized into an ordinary read-only tree.
- **Never interfere with the user's filesystem or code.** Don't write config files, instruction files, or metadata into the user's working directory or project tree. Use dedicated OS paths (`/etc/`, `/var/`, etc.) or CLI flags instead. If an agent framework requires a file in the project directory (e.g., OpenCode's context paths), prefer absolute paths to OS-managed locations over creating files in cwd.
- **Agent prompt injection must be non-destructive.** Each agent has its own mechanism for loading instructions (CLI flags, env vars, config files). When injecting OS instructions: preserve the agent's existing user-provided instructions (CLAUDE.md, AGENTS.md, etc.), append rather than replace, and always provide `skipOsInstructions` opt-out. User configuration is never clobbered — user env vars override ours via spread order.

## Dependencies

- **Rivet repo** — A modifiable copy lives at `~/r-aos`. Use this when you need to make changes to the Rivet codebase.
- Mount host `node_modules` read-only for agent packages (pi-acp, etc.)

## Agent Sessions (ACP)

- Uses the **Agent Communication Protocol** (ACP) -- JSON-RPC 2.0 over stdio (newline-delimited)
- No HTTP adapter layer; communicate directly with agent ACP adapters over stdin/stdout
- Reference `~/sandbox-agent` for ACP integration patterns (how pi-acp is spawned, JSON-RPC protocol, session lifecycle). Do not copy code from it.
- ACP docs: https://agentclientprotocol.com/get-started/introduction
- Session design is **agent-agnostic**: each agent type has a config specifying its ACP adapter package and main agent package name
- Currently configured agents: PI (`@rivet-dev/agent-os-pi`), PI CLI (`@rivet-dev/agent-os-pi-cli`), OpenCode (`@rivet-dev/agent-os-opencode`), Claude (`@rivet-dev/agent-os-claude`).
- **No host agent exceptions.** Host-native wrappers and host binary launch paths are not allowed. OpenCode support must use the real upstream OpenCode implementation rebuilt into the VM adapter package and executed inside the VM.
- `createSession("pi")` spawns the ACP adapter inside the VM, which calls the Pi SDK directly

### Agent Adapter Approaches

Each agent type can have two adapter approaches:
- **SDK adapter** (default) — Embeds the agent SDK directly via library import (`createAgentSession()`). Lower memory footprint (~100MB less for Pi) because it skips loading CLI/TUI code. Binary: `pi-sdk-acp`. Package: `@rivet-dev/agent-os-pi`. Agent ID: `pi`.
- **CLI adapter** — Spawns the full agent CLI as a headless subprocess via its ACP adapter (`pi-acp` spawns `pi --mode rpc`). Higher memory overhead but provides full CLI feature set. Binary: `pi-acp`. Package: `@rivet-dev/agent-os-pi-cli`. Agent ID: `pi-cli`.

The `pi` agent type defaults to the SDK adapter for reduced memory overhead. Use `pi-cli` when the full CLI-based ACP adapter is needed.

### Agent Configs

Each agent type needs:
- `acpAdapter`: npm package name for the ACP adapter (e.g., `@rivet-dev/agent-os-pi`)
- `agentPackage`: npm package name for the underlying agent (e.g., `@mariozechner/pi-coding-agent`)
- Any environment variables or flags needed

## Testing

- **Framework**: vitest
- **All tests run inside the VM** -- network servers, file I/O, agent processes
- Network tests: write a server script file, run it with `node` inside the VM, then `vm.fetch()` against it
- Agent tests must be run sequentially in layers:
  1. PI headless mode (spawn pi directly, verify output)
  2. pi-acp manual spawn (JSON-RPC over stdio)
  3. Full `createSession()` API
- **API tokens**: All tests use `@copilotkit/llmock` with `ANTHROPIC_API_KEY='mock-key'`. No real API tokens needed. Do not load tokens from `~/misc/env.txt` or any external file.
- **Mock LLM testing**: Use `@copilotkit/llmock` to run a mock LLM server on the HOST (not inside the VM). Use `loopbackExemptPorts` in `AgentOs.create()` to exempt the mock port from SSRF checks. The kernel needs `permissions: allowAll` for network access.
- **Module access**: Set `moduleAccessCwd` in `AgentOs.create()` to a host dir with `node_modules/`. pnpm puts devDeps in `packages/core/node_modules/` which are accessible via the ModuleAccessFileSystem overlay.

### Test Quality Requirements

- **Security tests must verify outcomes, not just mechanisms.** Don't just assert that the right flags/args are passed — write adversarial tests that attempt the forbidden action (read a host file, spawn an unauthorized process, connect to a blocked address) and verify it fails. Testing that `--allow-fs-read=/sandbox` is passed to Node is not the same as testing that guest code cannot read `/etc/passwd`.
- **Never use fake binaries or mock bridges for security-critical assertions.** If a test claims to verify isolation or permission enforcement, it must exercise the real code path end-to-end. Mock/recording bridges are acceptable for protocol and event-shape tests, but not for proving security properties.
- **Assert response contents, not just response shapes.** `ResponsePayload::GuestFilesystemResult(_) => {}` with a wildcard discard is a no-op assertion. Always check the payload matches expected values.
- **SSRF/network filtering must cover all RFC-specified private and special-purpose ranges.** This includes 0.0.0.0/8, 127.0.0.0/8, 169.254.0.0/16, 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16, 224.0.0.0/4 (multicast), 255.255.255.255/32, ::1, fe80::/10, fc00::/7, ff00::/8 (IPv6 multicast), and IPv6-mapped IPv4 equivalents. Apply the same filter to all network paths (TCP connect, DNS results, UDP send).
- **Every network RPC path (TCP, UDP, Unix, DNS) must enforce bridge-level permission checks.** If a new socket type or protocol is added, it must call `bridge.require_network_access()` before connecting/binding — don't skip it for Unix sockets or other "internal" paths.
- **Host DNS must not fall through by default.** Guest DNS resolution must go through sidecar-controlled resolvers. If no explicit DNS override is configured, the default behavior must still be auditable and not leak host infrastructure.
- **Error messages returned to guest code must not contain host paths, IPs, or infrastructure details.** Scrub host-specific information from all error responses, DNS event emissions, and stack traces before they reach guest-visible surfaces.
- **Tests must run against the full test suite, not just focused subsets.** If `cargo test -p <crate>` fails, fix the failures before marking a story as passing. Verifying with `cargo test -p <crate> --test <specific_test> <specific_case>` alone hides integration regressions.
- **Concurrency-sensitive kernel code (waitpid, flock, dup/close, resource limits) must have race-condition tests.** Use multi-threaded test scenarios with concurrent operations to verify correctness under contention, not just sequential happy paths.
- **Shared mutable test state (env vars, temp dirs, global caches) must be isolated.** Tests that mutate `AGENT_OS_*` env vars or share temp directories must use `--test-threads=1` or per-test isolation to prevent flaky failures.

### WASM Binaries and Quickstart Examples

- **WASM command binaries are checked into git via Git LFS.** The `registry/software/*/wasm/` directories contain pre-built WASM binaries (~63MB total, 138 files) tracked by Git LFS (see `.gitattributes`). They are available immediately after `git clone` / `git lfs pull` — no local Rust/WASM build step needed for development or testing.
- **To rebuild WASM binaries from source:** Run `make` in `registry/native/`, then `make copy-wasm` in `registry/`. This requires Rust nightly + wasi-sdk. After rebuilding, commit the updated binaries (they'll go through LFS automatically).
- **Quickstart examples that use `exec()` or shell commands require WASM binaries.** Examples like `processes.ts`, `bash.ts`, `git.ts`, `nodejs.ts`, and `tools.ts` import `@rivet-dev/agent-os-common` which resolves to local `registry/software/*/wasm/` directories in a dev checkout.
- **Examples that work without WASM binaries:** `hello-world.ts`, `filesystem.ts`, `cron.ts` (schedule/cancel only). These only use the Node runtime and don't need shell commands.

### Known VM Limitations

- `globalThis.fetch` is hardened (non-writable) in the VM — can't be mocked in-process
- Kernel child_process.spawn can't resolve bare commands from PATH (e.g., `pi`). Use `PI_ACP_PI_COMMAND` env var to point to the `.js` entry directly. The Node runtime resolves `.js`/`.mjs`/`.cjs` file paths as node scripts.
- `kernel.readFile()` does NOT see the ModuleAccessFileSystem overlay — read host files directly with `readFileSync` for package.json resolution
- Native ELF binaries cannot execute in the VM — the kernel's command resolver only handles `.js`/`.mjs`/`.cjs` scripts and WASM commands. `child_process.spawnSync` returns `{ status: 1, stderr: "ENOENT: command not found" }` for native binaries.

### Debugging Policy

- **Never guess without concrete logs.** Every assertion about what's happening at runtime must be backed by log output. If you don't have logs proving something, add them before making claims. Use logging liberally when debugging -- add logs at every decision point and trace the full execution path before drawing conclusions. Never assume something is a timeout issue unless there are logs proving the system was actively busy for the entire duration. An idle hang and a slow operation look the same from the outside -- only logs can distinguish them.
- **Native sidecar security/audit telemetry should use structured bridge events, not ad hoc strings.** In `crates/sidecar/src/service.rs`, emit security-relevant records with `bridge.emit_structured_event(...)` and include a `timestamp` field plus stable keys such as `policy`, `path`, `source_pid`, `target_pid`, or `reason` so tests and downstream aggregation can assert on them directly.
- **Never use CJS transpilation as a workaround** for ESM module loading issues. The VM must use V8's native ESM module system and Node.js native imports. Fix root causes in the ESM resolver, module access overlay, or V8 runtime instead of transforming ESM to CJS. The correct approach is to implement proper CJS/ESM interop in the V8 module resolver (wrapping CJS modules in ESM shims with named exports).
- **Maintain a friction log** at `.agent/notes/vm-friction.md` for anything that behaves differently from a standard POSIX/Node.js system. Document the deviation, the root cause, and whether a fix exists.

## Documentation

- **Keep docs in `~/r-aos/docs/docs/agent-os/` up to date** when public API methods or types are added, removed, or changed on AgentOs or Session classes.
- **Keep the standalone `secure-exec` docs repo up to date** when exported API methods, types, or package-level behavior change for public `secure-exec` compatibility packages. The source of truth is the repo that contains `docs/docs.json`.
- **The active public `secure-exec` package scope is currently `secure-exec` and `@secure-exec/typescript`.** Do not assume other legacy `@secure-exec/*` packages are still part of the maintained public surface unless the user explicitly says so.
- **If a user asks for a `secure-exec` change without naming the package, prompt them to choose the target public package when it is ambiguous.** Specifically, ask whether the change belongs in `secure-exec` or `@secure-exec/typescript` before editing code if the target is not clear from the symbol or file path.
- **Keep `website/src/data/registry.ts` up to date.** When adding, removing, or renaming a package, update this file so the website reflects the current set of available apps (agents, file-systems, software, and sandbox providers). Every new agent-os package or registry software package must have a corresponding entry.
- **No implementation details in user-facing docs.** Never mention WebAssembly, WASM, V8 isolates, Pyodide, or SQLite VFS in documentation outside of `architecture.mdx`. These are internal implementation details. Use user-facing language instead: "persistent filesystem" not "SQLite VFS", "JavaScript, TypeScript, Python, and shell commands" not "WASM, V8 isolates, and Pyodide", "sandboxed execution" not "WebAssembly and V8 isolates". The `architecture.mdx` page is the only place where internals are appropriate.

## Agent Working Directory

All agent working files live in `.agent/` at the repo root.

- **Specs**: `.agent/specs/` -- design specs and interface definitions for planned work.
- **Research**: `.agent/research/` -- research documents on external systems, prior art, and design analysis.
- **Todo**: `.agent/todo/*.md` -- deferred work items with context on what needs to be done and why.
- **Notes**: `.agent/notes/` -- general notes and tracking.

When the user asks to track something in a note, store it in `.agent/notes/` by default. When something is identified as "do later", add it to `.agent/todo/`. Design documents and interface specs go in `.agent/specs/`.

## CLAUDE.md Convention

- Every directory that has a `CLAUDE.md` must also have an `AGENTS.md` symlink pointing to it (`ln -s CLAUDE.md AGENTS.md`). This ensures other AI agents that look for `AGENTS.md` find the same instructions.

## Git

- **Commit messages**: Single-line conventional commits (e.g., `feat: add host tools RPC server`). No body, no co-author trailers.

## Build & Dev

```bash
pnpm install
pnpm build        # turbo run build
pnpm test         # turbo run test
pnpm check-types  # turbo run check-types
pnpm lint         # biome check
```
