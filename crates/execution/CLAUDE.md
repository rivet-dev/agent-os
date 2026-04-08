# Execution Engines

Runtime execution for Node.js (JavaScript/TypeScript) and Python (Pyodide) guest code.

**⚠️ ABSOLUTE RULE — NO EXCEPTIONS, NO FALLBACKS, NO "TEMPORARY" WORKAROUNDS:**

**ALL guest code MUST execute inside V8 isolates with kernel-backed polyfills. NEVER spawn real host Node.js processes for guest code. NEVER use `Command::new("node")` for guest execution. NEVER add a "legacy node mode", "host execution fallback", or "execution mode flag" that routes guest code through real host processes. There is exactly ONE execution path for guest JavaScript: V8 isolates managed by `crates/v8-runtime/` with polyfills that route through the kernel. Any code path where guest code reaches the real host — even as a "temporary" measure, even behind a flag, even for "compatibility" — is a critical security violation and MUST NOT be merged.**

If tests fail because they were written for the old `Command::new("node")` path, **fix or delete the tests** — do NOT restore host execution to make them pass.

## Node.js Isolation Model

**Desired state:** Guest JS/TS runs inside isolated V8 contexts managed by the execution engine. All Node.js builtins (`fs`, `net`, `child_process`, `dns`, `http`, `os`, etc.) are kernel-backed polyfills that route through the kernel VFS, socket table, and process table. Module loading is fully intercepted — guest code never touches real host APIs. The execution engine previously had this working via `@secure-exec/core` + `@secure-exec/nodejs` with full kernel-backed polyfills for all builtins.

**Current state (⚠️ COMPLETELY BROKEN -- see `.agent/todo/node-isolation-gaps.md`):**

Guest Node.js code currently runs as **real host Node.js child processes** spawned via `std::process::Command::new("node")` in `javascript.rs`. The ESM loader hooks intercept `require()`/`import` but most builtins either fall through to the real host module or are thin wrappers that call real host APIs. **This fundamentally violates the isolation model.** The execution engine must be rebuilt to use V8 isolates with kernel-backed polyfills instead of spawning real `node` processes. This is being actively worked on.

**Recovery reference:** The complete working polyfill + V8 isolate code from the original `@secure-exec/core` + `@secure-exec/nodejs` + `@secure-exec/v8` packages has been recovered to `.agent/recovery/secure-exec/`. Key files to port:
- `nodejs/src/bridge/fs.ts` (3,974 lines) -- full kernel-backed `fs`/`fs/promises` polyfill
- `nodejs/src/bridge/network.ts` (11,149 lines) -- full `net`/`dgram`/`dns` polyfill via kernel socket table
- `nodejs/src/bridge/child-process.ts` (1,058 lines) -- `child_process` polyfill via kernel process table
- `nodejs/src/bridge/process.ts` (2,251 lines) -- virtualized `process` global (env, cwd, pid, signals)
- `nodejs/src/bridge/polyfills.ts` (914 lines) -- polyfill registration and module hijacking
- `nodejs/src/bridge-handlers.ts` (6,405 lines) -- host-side bridge handlers for all kernel syscalls
- `nodejs/src/execution-driver.ts` (1,693 lines) -- V8 isolate session lifecycle + bridge setup
- `kernel/` -- the JS kernel (VFS, process table, socket table, PTY, pipes)
- `v8/` -- V8 runtime process manager, IPC binary protocol

The original source repo is at `/home/nathan/secure-exec-1/` (tagged `v0.2.1`).

**Prior art -- the original JS kernel had full polyfills:**

Before the Rust sidecar (commit `5a43882`), the JS kernel (`@secure-exec/core` + `@secure-exec/nodejs` + `packages/posix/`) had complete kernel-backed polyfills for all builtins. The pattern was:
- **Kernel socket table** -- `kernel.socketTable.create/connect/send/recv` managed all TCP/UDP. Loopback stayed in-kernel; external connections went through a `HostNetworkAdapter`.
- **Kernel VFS** -- All `fs` operations routed through the kernel VFS via syscall RPC.
- **Kernel process table** -- `child_process.spawn` routed through `kernel.spawn()`.
- **SharedArrayBuffer RPC** -- Synchronous syscalls from worker threads used `Atomics.wait` + shared memory buffers (same pattern the Pyodide VFS bridge uses today).
- **Module hijacking** -- `require('net')` returned the kernel-backed socket implementation, not real `node:net`.

The Rust sidecar kernel already has the VFS, process table, pipe manager, PTY manager, and permission system. What's missing is porting the **polyfill layer**. This is a port of proven patterns, not a greenfield design.

### Current reality vs required state

| Builtin | Required | Current | Gap |
|---------|----------|---------|-----|
| `fs` / `fs/promises` | Kernel VFS polyfill | Path-translating wrapper over real `node:fs` | Port: route through kernel VFS via RPC |
| `child_process` | Kernel process table polyfill | Path-translating wrapper over real `node:child_process` | Port: route through kernel process table |
| `net` | Kernel socket table polyfill | **No wrapper -- falls through to real `node:net`** | Port: kernel socket table polyfill |
| `dgram` | Kernel socket table polyfill | **No wrapper -- falls through to real `node:dgram`** | Port: kernel socket table polyfill |
| `dns` | Kernel DNS resolver polyfill | **No wrapper -- falls through to real `node:dns`** | Port: kernel DNS resolver polyfill |
| `http` / `https` / `http2` | Built on kernel `net` polyfill | **No wrapper -- falls through to real module** | Port: builds on `net` polyfill |
| `tls` | Kernel TLS polyfill | Guest-owned polyfill in `node_import_cache.rs` wraps the existing guest `net` transport with host TLS state | Keep client/server entrypoints on guest sockets and avoid direct host `node:tls` listeners/connections |
| `os` | Kernel-provided values | Guest-owned polyfill in `node_import_cache.rs` virtualizes hostname, CPU, memory, loopback networking, home, and user info | Keep future `os` additions aligned with VM defaults |
| `vm` | Must be denied | **No wrapper -- falls through to real `node:vm`** | Must stay denied |
| `worker_threads` | Must be denied | **No wrapper -- falls through to real module** | Must stay denied |
| `inspector` | Must be denied | **No wrapper -- falls through to real module** | Must stay denied |
| `v8` | Must be denied | **No wrapper -- falls through to real module** | Must stay denied |

### Loader interception (`node_import_cache.rs`)

ESM loader hooks (`loader.mjs`) and CJS `Module._load` patches (`runner.mjs`) are generated from Rust string templates. Every `import`/`require` is intercepted:
1. `resolveBuiltinAsset()` -- checks `BUILTIN_ASSETS` list. Redirects to a kernel-backed polyfill file.
2. `resolveDeniedBuiltin()` -- checks `DENIED_BUILTINS` set. Redirects to a stub that throws `ERR_ACCESS_DENIED`. A builtin is in `DENIED_BUILTINS` only if it is NOT in `ALLOWED_BUILTINS`.
3. **Fall through to `nextResolve()`** -- Node.js default resolution. Returns the real host module. **This must never happen for any builtin that guest code can import.**

`AGENT_OS_ALLOWED_NODE_BUILTINS` (JSON string array env var) controls which builtins are removed from the deny list. `DEFAULT_ALLOWED_NODE_BUILTINS` in `packages/core/src/sidecar/native-kernel-proxy.ts` currently includes all builtins -- this must be reduced to only builtins that have kernel-backed polyfills.

### Additional hardening layers (defense-in-depth, NOT primary isolation)

1. **`globalThis.fetch` hardening** -- Replaced with `restrictedFetch` (loopback-only on exempt ports). Does NOT cover `http.request()`, `net.connect()`, or `dgram.createSocket()`.
2. **Node.js `--permission` flag** -- OS-level backstop for filesystem and child_process only. No network restrictions. This is a safety net, not the isolation boundary.
3. **Guest env stripping** -- `NODE_OPTIONS`, `LD_PRELOAD`, `DYLD_INSERT_LIBRARIES`, `LD_LIBRARY_PATH` stripped before spawn.
4. **Permissioned Pyodide host launches still need `--allow-worker`.** `python.rs` bootstraps through Node's internal ESM loader worker, so the host process must keep `--allow-worker` enabled even while guest `worker_threads` stays denied.

## Guest `fs` and `fs/promises` Polyfill Rules

- Guest Node `fs` and `fs/promises` polyfills share the JavaScript sync-RPC transport between `node_import_cache.rs` and `crates/sidecar/src/service.rs`.
- Node-facing `readdir` results must filter `.`/`..`.
- Async methods should dispatch under `fs.promises.*`.
- fd-based APIs (`open`, `read`, `write`, `close`, `fstat`) plus `createReadStream`/`createWriteStream` should ride the same bridge.
- Runner-internal pipe/control writes must keep snapped host `node:fs` bindings because `syncBuiltinModuleExports(...)` mutates the builtin module for guests.

## JavaScript Sync RPC

- Timeouts and slow-reader backpressure should be enforced in `javascript.rs`, not in the generated runner.
- Track the pending request ID on the host, auto-emit `ERR_AGENT_OS_NODE_SYNC_RPC_TIMEOUT` after the configured wait.
- Queue replies through a bounded async writer so slow guest reads cannot block the sidecar thread.
- Have `crates/sidecar/src/service.rs` ignore stale `sync RPC request ... is no longer pending` races after the timeout fires.
- Guest V8 timers have two host paths in `javascript.rs`: `_scheduleTimer` is an async bridge call that resolves its pending Promise later, while `kernelTimerCreate`/`kernelTimerArm`/`kernelTimerClear` are local `_loadPolyfill` dispatches that must emit `"timer"` stream events back into the V8 session so `setTimeout`/`setInterval` callbacks fire.
- Live guest stdin also has two delivery paths: `AGENT_OS_KEEP_STDIN_OPEN` uses `"stdin"` / `"stdin_end"` stream events, while TTY-style reads use `_kernelStdinRead` and must stay forwarded to the sidecar-backed kernel fd `0` pipe so timeout and EOF remain distinguishable.
- Guest `stdin.setRawMode()` should follow the same bridge pattern as `_kernelStdinRead`: leave `_ptySetRawMode` unhandled in `LocalBridgeState`, map it to sidecar `__pty_set_raw_mode`, and have the sidecar toggle kernel PTY discipline on the guest process's fd `0` instead of keeping a local execution-only stub.
- The current V8 sync-RPC bridge effectively supports one in-flight request at a time. Do not leave long-lived network waits such as HTTP server close listeners parked on a pending sync-RPC Promise; use stream events plus short-lived follow-up RPCs so later bridge calls cannot deadlock behind the wait.

## Runner Script Assets

- Execution-host runner scripts materialized by `NodeImportCache` should live as checked-in assets under `crates/execution/assets/runners/` and be loaded via `include_str!`.
- When testing import-cache temp-root cleanup, use a dedicated `NodeImportCache::new_in(...)` base dir so the one-time sweep stays isolated to that root.
- Active JavaScript/Python/WASM executions must hold a `NodeImportCache` cleanup guard until the child exits; otherwise dropping the engine can delete `timing-bootstrap.mjs` and related assets while the host runtime is still importing them.
- Host-Node compatibility coverage should stay behind the `legacy-js-tests` feature. Default validation for JavaScript execution must target the V8 isolate path and its `javascript_v8.rs` tests.
- Shared-V8 JavaScript tests should assert `uses_shared_v8_runtime()` and the absence of host guest-node launches, not `child_pid() == 0`; shared isolates still report the host runtime PID so the sidecar can manage lifecycle signals.

## Guest Path Scrubbing

- Guest path scrubbing in `node_import_cache.rs` should treat the real `HOST_CWD` as an implicit runtime-only mapping to the virtual guest cwd (for example `/root`) so entrypoint imports and stack traces stay usable without leaking the host path.
- Reserve `/unknown` for absolute host paths outside visible mappings or the internal cache roots.

## CommonJS Module Isolation

- `node_import_cache.rs` has to patch `Module._resolveFilename` and the guest-facing `Module._cache` / `require.cache` view together; wrapping only `createGuestRequire()` does not constrain local `require()` inside already-loaded `.cjs` modules.
- Resolver-only coverage for `javascript.rs` should use `javascript::ModuleResolutionTestHarness` with a temp-dir fixture instead of booting a V8 isolate; mapping `/root` plus `/root/node_modules` is enough to exercise exports/imports and pnpm `.pnpm` layouts.

## Guest `process` Hardening

- Guest-visible `process` hardening in `node_import_cache.rs` should harden properties on the real host `process` before swapping in the guest proxy.
- The proxy fallback must resolve via the proxy receiver (`Reflect.get(..., proxy)`) so accessors inherit the virtualized surface instead of the raw host object.
- Per-process filesystem state such as `umask` belongs in `ProcessContext` / `ProcessTable`. Kernel create/write entrypoints should read it there, and any guest Node exposure must be threaded through the JavaScript sync-RPC bridge instead of inheriting host `process` behavior.

## Guest `child_process` Isolation

- Strip all `AGENT_OS_*` keys from the RPC `options.env` payload in `node_import_cache.rs`.
- Carry only the Node runtime bootstrap allowlist in `options.internalBootstrapEnv`.
- Re-inject that allowlisted map only when `crates/sidecar/src/service.rs` starts a nested JavaScript runtime.

## Guest Networking Rules

- Guest Node `net` Unix-socket support follows the same split as TCP: resolve guest socket paths against `host_dir` mounts when possible, otherwise map them under the VM sandbox root on the host, keep active Unix listeners/sockets in `crates/sidecar/src/service.rs`, and mirror non-mounted listener paths into the kernel VFS so guest `fs` APIs can see the socket file.
- When a guest Node networking port stops using real host listeners, mirror that state in `crates/sidecar/src/service.rs` `ActiveProcess` tracking and consult it from `find_listener`/socket snapshot queries before falling back to `/proc/[pid]/net/*`; procfs only sees host-owned sockets, not sidecar-managed polyfill listeners.
- Sidecar-managed loopback `net.listen` / `dgram.bind` listeners now use guest-port to host-port translation in `crates/sidecar/src/service.rs`: preserve guest-visible loopback addresses/ports in RPC responses and socket snapshots, but use the hidden host-bound port for external host-side probes and test clients.
- V8 `node:dgram` support in `crates/execution/assets/v8-bridge.js` depends on both `loadBuiltinModule("dgram")` and `"dgram"` appearing in `Module.builtinModules`; keep those lists aligned, and keep the bridge payloads aligned with the current sidecar RPC contract (`createSocket` object payload, `send` bytes plus `{ address, port }`, `poll` object-or-null responses).
- Sidecar JavaScript networking policy should read internal bootstrap env like `AGENT_OS_LOOPBACK_EXEMPT_PORTS` from `VmState.metadata` / `env.*`, not `vm.guest_env`; `guest_env` is permission-filtered and may be empty even when sidecar-only policy still needs the value.

## Guest `tls`

- Guest Node `tls` should stay layered on the guest `net` polyfill rather than importing host `node:tls` directly.
- Client connections must pass a preconnected guest socket into `tls.connect({ socket })`.
- Server handshakes should wrap accepted guest sockets with `new TLSSocket(..., { isServer: true })` and emit `secureConnection` from the wrapped socket's `secure` event.

## Guest `dns`

- When a newly allowed Node builtin still has bypass-capable host-owned helpers or constructors (for example `dns.Resolver` / `dns.promises.Resolver`), replace those entrypoints with guest-owned shims or explicit unsupported stubs before adding the builtin to `DEFAULT_ALLOWED_NODE_BUILTINS`; inheriting the host module is only safe for exports that cannot escape the kernel-backed port.

## Python Execution

- Python execution in `python.rs` should keep `poll_event()` blocked until a real guest-visible event arrives or the caller timeout expires; filtered stderr/control messages are internal noise.
- `wait(None)` should still enforce the per-run `AGENT_OS_PYTHON_EXECUTION_TIMEOUT_MS` cap.
- `wait()` should bound accumulated stdout/stderr via the hidden `AGENT_OS_PYTHON_OUTPUT_BUFFER_MAX_BYTES` env knob rather than growing buffers without limit.
- Node heap caps from `AGENT_OS_PYTHON_MAX_OLD_SPACE_MB` need to apply to both prewarm and execution launches without leaking those control vars into guest `process.env`.
- Warmup marker fingerprints for guest assets must include mutation data (`size` plus `mtime`/`mtime_nsec`), not just inode identity; in-place rewrites of Pyodide or WASM assets can preserve the inode and still need to invalidate prewarm stamps.
- Pyodide bootstrap hardening in `node_import_cache.rs` must stay staged: `globalThis` guards can go in before `loadPyodide()`, but mutating `process` before `loadPyodide()` breaks the bundled Pyodide runtime under Node `--permission`.
