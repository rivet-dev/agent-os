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
| `vm` | Guest-owned compatibility shim for package loading | Guest-owned compatibility builtin for `Script`, `createContext`, `isContext`, `runInNewContext`, `runInThisContext` | Keep it limited to the compatibility surface; do not fall through to host `node:vm` |
| `worker_threads` | Guest-owned compatibility shim for package loading | Guest-owned compatibility builtin exposing `isMainThread` plus inert ports; `Worker` construction stays unavailable | Keep it importable for feature detection, but never spawn real threads |
| `inspector` | Must be denied | **No wrapper -- falls through to real module** | Must stay denied |
| `v8` | Guest-owned compatibility shim for package loading | Guest-owned compatibility builtin for safe inspection/serialization helpers | Keep it limited to the compatibility surface; do not fall through to host `node:v8` |

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
- `fs.promises` methods that need real concurrency must use dedicated async bridge globals in `crates/execution/assets/v8-bridge.source.js`; wrapping `fs.*Sync` inside `async` functions still serializes `Promise.all(...)` behind the first sidecar response.
- fd-based APIs (`open`, `read`, `write`, `close`, `fstat`) plus `createReadStream`/`createWriteStream` should ride the same bridge.
- Guest `fs.watch` / `fs.watchFile` currently stay guest-owned polling wrappers over `fs.statSync`; keep them in `v8-bridge.source.js` unless the kernel grows a real notification API.
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
- The stdlib-backed V8 bridge bundle should be generated from `crates/execution/assets/v8-bridge.source.js` via `pnpm --dir packages/core build:v8-bridge`; keep the heavier assert/util/zlib payload in `v8-bridge-zlib.js` so the main `v8-bridge.js` stays below the 500KB cap.
- If you change generated builtin asset source in `crates/execution/src/node_import_cache.rs`, bump `NODE_IMPORT_CACHE_ASSET_VERSION` in the same file or stale materialized assets under `/tmp/agent-os-node-import-cache-*` will keep serving the old code.
- The embedded WASM runner's `buildPreopens()` map must mirror `AGENT_OS_GUEST_PATH_MAPPINGS`, not just `.` / `/workspace`; otherwise kernel-visible host-dir mounts like `/etc/agentos` or `/hostmnt` can succeed through `vm.readFile()` while the same path fails under `vm.exec("cat ...")`.
- Treat `crates/bridge/bridge-contract.json` as the canonical inventory for host bridge globals and calling conventions, and treat `crates/execution/assets/polyfill-registry.json` as the canonical inventory for guest `_loadPolyfill` module names. When adding or renaming a bridge global, update those files together with `crates/v8-runtime/src/session.rs`, and when exposing a new runtime-loadable builtin, update the polyfill registry together with the `_loadPolyfill` handler in `crates/execution/src/javascript.rs`.
- Guest builtin availability must stay aligned across `polyfill-registry.json`, `normalize_builtin_specifier()` in `crates/execution/src/javascript.rs`, `Module.builtinModules` plus `loadBuiltinModule()` in `crates/execution/assets/v8-bridge.source.js`, and the host-node import-cache assets in `crates/execution/src/node_import_cache.rs`; if one surface still treats a denied builtin as unknown, guests will see `MODULE_NOT_FOUND` or host fallthrough instead of the intended `ERR_ACCESS_DENIED` or compatibility stub.
- The shared-runtime `node:stream` compatibility surface for sidecar/builtin-conformance tests currently comes from the inline mini-stream module in `crates/execution/src/javascript.rs`, not the stdlib-backed `crates/execution/assets/v8-bridge.source.js` path. Stream iterator/parity fixes for guest `require("stream")` need to land in that inline module and should be covered in `crates/sidecar/tests/builtin_conformance.rs`.
- Bootstrap globals injected by `packages/core/scripts/build-v8-bridge.mjs` exist only to let the bundle initialize during snapshot creation. If that bootstrap layer defines `URL` or `URLSearchParams`, mark them as bootstrap stubs and have `v8-bridge.source.js` ignore or replace them once the stdlib polyfills load, or the runtime can silently keep the incomplete bootstrap implementation.
- If guest `fetch()` is powered by bundled undici, the aliased `node:stream` helpers in `crates/execution/assets/undici-shims/stream.js` must understand the bundled web-streams ponyfill too; undici's fetch path calls `finished()`, `isReadable()`, `isErrored()`, and `isDisturbed()` on `ReadableStream` response bodies, not just Node event-emitter streams.
- When testing import-cache temp-root cleanup, use a dedicated `NodeImportCache::new_in(...)` base dir so the one-time sweep stays isolated to that root.
- Active JavaScript/Python/WASM executions must hold a `NodeImportCache` cleanup guard until the child exits; otherwise dropping the engine can delete `timing-bootstrap.mjs` and related assets while the host runtime is still importing them.
- Host-Node compatibility coverage should stay behind the `legacy-js-tests` feature. Default validation for JavaScript execution must target the V8 isolate path and its `javascript_v8.rs` tests.
- Shared-V8 JavaScript tests should assert `uses_shared_v8_runtime()` and the absence of host guest-node launches, not `child_pid() == 0`; shared isolates still report the host runtime PID so the sidecar can manage lifecycle signals.

## Guest Path Scrubbing

- Guest path scrubbing in `node_import_cache.rs` should treat the real `HOST_CWD` as an implicit runtime-only mapping to the virtual guest cwd (for example `/root`) so entrypoint imports and stack traces stay usable without leaking the host path.
- Reserve `/unknown` for absolute host paths outside visible mappings or the internal cache roots.

## CommonJS Module Isolation

- `node_import_cache.rs` has to patch `Module._resolveFilename` and the guest-facing `Module._cache` / `require.cache` view together; wrapping only `createGuestRequire()` does not constrain local `require()` inside already-loaded `.cjs` modules.
- The V8 bridge's guest-side CommonJS helpers in `crates/execution/assets/v8-bridge.source.js` must pass an explicit `"require"` mode into `_resolveModule`; omitting it falls back to import resolution and picks the wrong conditional export branch for dual packages.
- Keep `require.resolve()` parity between both CommonJS entrypoints in `crates/execution/assets/v8-bridge.source.js`: `createRequire()` and the per-module `require` created in `_compile()`. If one gains `resolve.paths()` or builtin handling changes without the other, guest packages behave differently depending on how they obtained `require`.
- For builtins that guest CommonJS should `require("node:...")`, update `createRequire()` builtin guards plus both `Module.builtinModules` and `loadBuiltinModule()` in `crates/execution/assets/v8-bridge.source.js`; changing only one surface leaves `require()` behavior out of sync with `_requireFrom()` and can degrade into `ERR_ACCESS_DENIED`, `MODULE_NOT_FOUND`, or host-fallthrough mismatches.
- `crates/v8-runtime/src/execution.rs` should only fall back to runtime CJS export enumeration (`Object.keys(module.exports)`) when static extraction finds zero names; eagerly requiring every CJS module during shim generation adds avoidable work and can trigger module side effects earlier than intended.
- Inline builtin wrappers in `crates/execution/src/javascript.rs` must not call `_requireFrom()` on the same builtin subpath they implement. Subpath wrappers like `node:fs/promises` should be built from the parent builtin (`node:fs`) or a direct object, not `_requireFrom("node:fs/promises")`.
- Resolver-only coverage for `javascript.rs` should use `javascript::ModuleResolutionTestHarness` with a temp-dir fixture instead of booting a V8 isolate; mapping `/root` plus `/root/node_modules` is enough to exercise exports/imports and pnpm `.pnpm` layouts.
- `crates/execution/tests/cjs_esm_interop.rs` is the desired-behavior matrix for CJS/ESM/runtime edge cases. If an interop gap is deferred to a follow-up story, keep the strong assertion in place and mark that test `#[ignore = "US-055: ..."]` instead of weakening it to match current behavior.

## Guest `process` Hardening

- Guest-visible `process` hardening in `node_import_cache.rs` should harden properties on the real host `process` before swapping in the guest proxy.
- The proxy fallback must resolve via the proxy receiver (`Reflect.get(..., proxy)`) so accessors inherit the virtualized surface instead of the raw host object.
- Per-process filesystem state such as `umask` belongs in `ProcessContext` / `ProcessTable`. Kernel create/write entrypoints should read it there, and any guest Node exposure must be threaded through the JavaScript sync-RPC bridge instead of inheriting host `process` behavior.

## Guest `child_process` Isolation

- Strip all `AGENT_OS_*` keys from the RPC `options.env` payload in `node_import_cache.rs`.
- Carry only the Node runtime bootstrap allowlist in `options.internalBootstrapEnv`.
- Re-inject that allowlisted map only when `crates/sidecar/src/service.rs` starts a nested JavaScript runtime.
- JavaScript child-process launches in `crates/sidecar/src/execution.rs` must call `prepare_javascript_runtime_env(...)` and set `AGENT_OS_SANDBOX_ROOT` just like top-level `execute()` does. If child V8 executions miss those runtime env entries, stack traces fall back to `/unknown/...`, bare-package ESM imports like `undici` stop resolving, and spawned JS CLIs (including `pi-acp` -> `pi --mode rpc`) silently diverge from top-level behavior.
- In `crates/execution/src/node_import_cache.rs`, WASM child-process stdio can target delegate-managed guest fds rather than real host OS fds. Keep synthetic-pipe routing aligned with `delegateManagedFdWrite`/`delegateManagedFdClose`, retain those delegate fds for the child lifetime, and only release the final close after child exit; writing streamed stdout/stderr with raw host `writeSync(fd, ...)` breaks redirected shell output.
- In the same WASM host-process path, synthetic pipes must initialize both `producers` and `consumers`, and consumer registration must flush any chunks buffered before the child attached. Shell builtins can write into a pipe before a spawned child like `wc` registers its stdin consumer, so registration also needs to close child stdin immediately when no writers or producers remain.
- The WASM runner's read-only `path_open` guard in `crates/execution/src/node_import_cache.rs` must allow non-mutating open flags such as `O_DIRECTORY`; only create/truncate/exclusive flags and write rights should return `EROFS`, or read-only traversal commands like `find`, `fd`, and `ls <dir>` will fail to enumerate directories.
- WASM execution tests that poll `WasmExecution::poll_event_blocking()` need to handle `WasmExecutionEvent::SyncRpcRequest(_)` explicitly unless the test is asserting that control-plane behavior; the runtime includes sync RPC traffic in the same event stream as stdout/stderr/signal/exit events, and `wait()` already treats those requests as ignorable noise for result aggregation.
- The host WASI runner's full-permission preopens must include both `'.'` and `'/workspace'` mapped to `process.cwd()`. Child commands that receive `cwd: "/workspace"` from the sidecar still resolve relative paths through the WASI `.` preopen, so omitting it makes `cat note.txt`/redirects fail even when the guest cwd is otherwise correct.
- WASM child-process launches should keep the guest command name in `ResolvedChildProcessExecution.process_args[0]` / WASI `argv[0]`; `execution_args` is the suffix after that command name. PATH-resolution tests for mounted commands should assert the full argv vector, not just the trailing args.

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
- Python RPC shims in `crates/execution/assets/runners/python-runner.mjs` should translate JS bridge failures into Python-native exceptions (`PermissionError`, `FileNotFoundError`, `OSError`) instead of leaking `JsException`, and Python `subprocess.run()` should inherit the VM cwd from sidecar process state rather than Pyodide's internal `/home/pyodide` working directory.
- Pyodide `micropip` support must keep guest `js` / `pyodide_js` imports blocked for user Python code while exposing only a narrow internal compat surface to `micropip` and `pyodide.http`; widening that exception re-opens host escape hatches.
- `python-runner.mjs` must suppress `loadPyodide()`/micropip progress banners such as `Loading ...` and `Loaded ...` from guest stdout; sidecar callers and tests often parse stdout as program output or JSON, so those bootstrap logs have to stay internal.
- When `python-runner.mjs` or other bundled execution assets change, bump `NODE_IMPORT_CACHE_ASSET_VERSION` in `node_import_cache.rs` if the temp materialization needs to refresh immediately; otherwise stale `/tmp/agent-os-node-import-cache-*` contents can mask the update during local test runs.
