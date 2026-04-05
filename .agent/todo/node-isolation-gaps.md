# Runtime Isolation Gaps

Agent OS is a fully virtualized operating system. Every guest syscall must go through the kernel — no guest operation may fall through to a real host syscall. The Node.js execution model currently spawns real host OS child processes (`std::process::Command::new("node")`) and most builtins either fall through to real host modules or are thin path-translating wrappers over real host APIs. This violates the virtualization model.

The original JS kernel (`@secure-exec/core` + `@secure-exec/nodejs`, deleted in commit `5a43882`) had full kernel-backed polyfills for `fs`, `net`, `http`, `dns`, `dgram`, `child_process`, and `os` using SharedArrayBuffer RPC and a kernel socket table. The work here is **porting those proven patterns** to the Rust sidecar, not designing from scratch.

## P0: Remove dangerous builtins from DEFAULT_ALLOWED_NODE_BUILTINS

**This is the single highest-value change. Zero effort, immediate security fix.**

`packages/core/src/sidecar/native-kernel-proxy.ts` sets `DEFAULT_ALLOWED_NODE_BUILTINS` to include everything. Every builtin without a kernel polyfill falls through to the real host module.

- [ ] Remove `dgram`, `dns`, `http`, `http2`, `https`, `net`, `tls`, `vm`, `worker_threads`, `inspector`, `v8` from `DEFAULT_ALLOWED_NODE_BUILTINS`. Only keep builtins that have kernel-backed polyfills.
- [ ] Add `os`, `cluster`, `diagnostics_channel` to `DENIED_BUILTINS`. `node:os` leaks host info (hostname, CPUs, memory, network interfaces).
- [ ] Make `ALLOWED_NODE_BUILTINS` configurable from `AgentOsOptions` (currently hardcoded).
- [ ] Fix `--allow-worker` inconsistency: currently always passed at `--permission` level even when `worker_threads` is denied at the loader level.

## P0: Pyodide sandbox escapes

### `import js` exposes all JS globals to Python (CRITICAL)
Python code can `import js` and access `js.process.env`, `js.process.kill()`, `js.require`, and any other JS global. Full sandbox escape.

- [ ] Block or proxy `js` and `pyodide_js` FFI modules so Python code cannot reach raw JS globals.

### Node.js `--permission` disabled for Python (CRITICAL)
`python.rs:622` sets `enable_permissions=false`. The `--permission` flag is not applied to the Pyodide host process.

- [ ] Enable `--permission` for the Python runtime's host Node.js process.

## P0: Port kernel-backed polyfills from original JS kernel

These builtins need kernel-backed polyfills ported from the original `@secure-exec/nodejs` patterns. The Rust kernel already has the VFS, process table, and pipe manager. The missing piece is the JS polyfill layer + RPC bridge (SharedArrayBuffer for sync calls, same pattern the Pyodide VFS bridge already uses).

### `fs` / `fs/promises` — port kernel VFS polyfill
Currently: `wrapFsModule()` translates paths then calls real `node:fs` (real host syscalls). Must route through kernel VFS via RPC instead.

- [ ] Replace `wrapFsModule` with kernel VFS polyfill using SharedArrayBuffer RPC for sync methods
- [ ] Async `fs.promises.*` methods: IPC message to sidecar kernel (straightforward, ~20 methods with direct kernel counterparts)
- [ ] Sync methods (`readFileSync`, etc.): SharedArrayBuffer + `Atomics.wait` bridge (proven pattern from Pyodide VFS bridge)
- [ ] Fd-based operations (`fs.open` → `kernel.fd_open`, `fs.read(fd)` → `kernel.fd_read`, etc.)
- [ ] Streams (`createReadStream`/`createWriteStream`): reimplement using polyfilled fd operations
- [ ] `fs.watch`/`fs.watchFile`: kernel has no file-watching API — stub or add kernel-side support

### `child_process` — port kernel process table polyfill
Currently: `wrapChildProcessModule()` translates paths but spawns real host processes. Must route through kernel process table.

- [ ] Replace with polyfill that routes `spawn`/`exec`/`execFile` through `kernel.spawn_process()`
- [ ] Build synthetic `ChildProcess` EventEmitter backed by kernel pipe fds for stdio
- [ ] Wire `waitpid` for exit/close events, `kill_process` for `.kill()`
- [ ] **Fix `exec`/`execSync` bypass**: currently passed through with zero interception — no path translation, no `--permission` injection. Guest can run `execSync('cat /etc/passwd')` on the host unmodified.

### `net` — port kernel socket table polyfill
Currently: no wrapper, falls through to real `node:net`. The kernel has a socket table and `HostNetworkAdapter` for external connections. The original JS kernel had `kernel.socketTable.create/connect/send/recv`.

- [ ] Polyfill `net.Socket` as a Duplex stream backed by kernel socket table operations via RPC
- [ ] Polyfill `net.createServer` backed by kernel socket `listen`/`accept`
- [ ] Loopback connections stay in-kernel; external connections go through `HostNetworkAdapter`

### `dgram` — port kernel UDP polyfill
- [ ] Polyfill `dgram.createSocket()` routed through kernel socket table

### `dns` — port kernel DNS resolver polyfill
- [ ] Polyfill `dns.resolve*()` and `dns.lookup()` routed through kernel DNS resolver
- [ ] Note: `dns.lookup()` uses libuv's `getaddrinfo` internally, not `net` — needs its own interception regardless of `net` polyfill

### `http` / `https` / `http2` — builds on `net` + `tls` polyfills
- [ ] Investigate: can real `node:http` use the polyfilled `net` module (loader hooks intercept `require('net')` inside `http` internals)? If yes, these may work automatically once `net` is polyfilled.
- [ ] If not: polyfill `http.request`/`http.get` directly as kernel-level fetch-style RPC calls (covers 95% of use cases without full streaming)

### `tls` — port kernel TLS polyfill
- [ ] Polyfill TLS socket creation routed through kernel networking

### `os` — polyfill with kernel-provided values (easy, ~100 lines)
- [ ] Return kernel hostname, configured CPU/memory values, etc. instead of real host info

### Builtins that must stay permanently denied
- [ ] **`vm`** — Creates V8 contexts without loader hooks. Must stay denied.
- [ ] **`worker_threads`** — Workers may not inherit loader hooks. Must stay denied.
- [ ] **`inspector`** — V8 debugger access. Must stay permanently denied.
- [ ] **`v8`** — Exposes heap internals. Must stay permanently denied.

### Safe builtins (no polyfill needed)
These are pure computation with no host I/O — safe to leave as real Node.js modules:
`stream`, `events`, `buffer`, `crypto`, `path`, `util`, `zlib`, `string_decoder`, `querystring`, `url`, `assert`, `timers`, `console`

### Native addons (.node files)
Native addons are shared objects loaded via `process.dlopen()` — arbitrary native code on the host. Cannot be sandboxed.
- [ ] Deny native addon loading by intercepting `process.dlopen` and `Module._extensions['.node']`.

### `process` global leaks host state
The `process` global is not virtualized. Multiple properties expose real host information:
- [ ] **`process.env`** — leaks all `AGENT_OS_*` internal env vars to guest. `AGENT_OS_GUEST_PATH_MAPPINGS` reveals real host paths where guest dirs are mapped. `AGENT_OS_NODE_IMPORT_CACHE_PATH` reveals host temp directory paths. Scrub `AGENT_OS_*` keys from guest-visible `process.env`.
- [ ] **`process.cwd()`** — returns real host path (e.g., `/tmp/agent-os-xxx/workspace`), not the guest's virtual path (e.g., `/root`). Must be virtualized to return the kernel CWD.
- [ ] **`process.execPath` / `process.argv[0]`** — exposes real host Node.js binary path (e.g., `/usr/local/bin/node`). Must be replaced with a virtual value.
- [ ] **`process.pid` / `process.ppid`** — returns real host OS PIDs. `process.ppid` leaks the sidecar's PID. Must be virtualized to return kernel PIDs.
- [ ] **`process.on('SIGINT'/'SIGTERM'/...)`** — guest can register signal handlers that prevent the sidecar from cleanly terminating the process. Must intercept `process.on()`/`process.once()` for signal events.
- [ ] **`process.chdir()`** — changes the real host CWD. Must be intercepted and routed through kernel.
- [ ] **`process.getuid()` / `process.getgid()`** — returns real host user IDs. Must be virtualized.

### `node:module` not denied — module resolution manipulation
`node:module` is not in DENIED_BUILTINS. Guest can `import { createRequire, Module } from 'node:module'` and access `Module._cache`, `Module._resolveFilename`, `Module._extensions` directly — bypassing the `_load` hook, probing host filesystem via `_resolveFilename`, and poisoning the module cache.
- [ ] Add `module` to DENIED_BUILTINS, or wrap it to remove dangerous APIs.

### `node:trace_events` not denied
Provides V8 tracing access. Not in DENIED_BUILTINS.
- [ ] Add `trace_events` to DENIED_BUILTINS.

### Host paths leak through errors and `require.resolve()`
- [ ] **`require.resolve()`** — returns real host filesystem paths (e.g., `/tmp/agent-os-node-import-cache-1/...`). Must translate resolved paths back to guest-visible paths.
- [ ] **Error messages / stack traces** — module-not-found errors, loader errors, etc. contain real host paths. Must scrub or translate host paths in error messages before they reach guest code.

### Loader metrics prefix injectable via guest stderr
Guest code can write `__AGENT_OS_NODE_IMPORT_CACHE_METRICS__:` to stderr to confuse the sidecar's metrics parsing (same class of issue as Pyodide exit code injection).
- [ ] Include in the side-channel fix for control messages.

## P1: Pyodide runtime gaps

### No `Drop` impl on `PythonExecution`
Orphaned Node+Pyodide processes (~200MB+ each) leak if caller drops without calling `wait()`.
- [ ] Implement `Drop` for `PythonExecution` that kills the child process.

### `wait()` has no timeout
Infinite hang on runaway Python code. No cancel mechanism.
- [ ] Add timeout parameter to Python `wait()`.
- [ ] Add a `cancel()`/`kill()` method for in-flight Python executions.

### No VFS RPC path validation
Python code can read/write any kernel VFS path. `service.rs:2394-2470` passes `request.path` directly to kernel.
- [ ] Scope VFS RPC operations to the guest's cwd or apply kernel permission checks.

### No `spawn_waiter` thread
Exit detection relies on fragile stderr parsing + `try_wait()` polling. Ungraceful deaths detected late.
- [ ] Add dedicated `spawn_waiter` thread matching JS/WASM pattern.

### Unbounded stdout/stderr buffering in `wait()`
All output accumulated in memory with no cap. OOM on large output.
- [ ] Cap buffer sizes or stream instead of accumulating. Use bounded mpsc channels.

### VFS RPC sync bridge can deadlock
`readSync()` blocks forever if Rust side never responds.
- [ ] Add timeout to synchronous VFS RPC bridge calls.

## P1: `options.permissions` not wired through

The TypeScript `AgentOsOptions.permissions` field is accepted but never consumed. The `LocalBridge` allows everything. The protocol has `PermissionDescriptor` on the Rust side but TS always sends an empty array.

- [ ] Wire `options.permissions` through to the sidecar bridge.
- [ ] Stop defaulting to `allowAll` in `LocalBridge`.

## P1: CWD passed directly as host filesystem path

`service.rs:2195-2206` uses the `Execute` request's `cwd` as the real host `current_dir()` AND adds it to `--allow-fs-read`/`--allow-fs-write`. No validation. Setting `cwd=/` grants host-wide access.

- [ ] Validate that the execution CWD is within the configured sandbox root.

## P1: `exec`/`execSync` bypass all child_process wrapping

`wrapChildProcessModule` passes `exec`/`execSync` through as bare `.bind()` calls — no path translation, no `--permission` injection. Guest code calling `child_process.execSync('cat /etc/passwd')` executes on the host unmodified.

- [ ] Wrap `exec`/`execSync` with the same interception as `spawn`/`execFile`.

## P1: Shared import cache enables cross-VM cache poisoning

`flushCacheState()` reads/merges/writes a shared on-disk cache. If two VMs share the same cache root, VM-A can write a poisoned resolution entry that VM-B picks up. `validateResolutionEntry` only checks file existence, not trust.

- [ ] Use per-VM cache paths, or validate that resolved files are within trusted locations.

## P1: `prependNodePermissionArgs` unconditionally passes `--allow-child-process`

When spawning child Node processes, the wrapper injects `--allow-child-process` and `--allow-worker` unconditionally. Every child of a guest process gets full child_process/worker permissions, enabling recursive escalation.

- [ ] Only pass `--allow-child-process` and `--allow-worker` if the parent was explicitly granted those permissions.

## P2: Kernel permission model gaps

### Permission bypass via symlinks (HIGH)
`PermissionedFileSystem` checks on caller-supplied path, then inner filesystem resolves symlinks independently. Only exploitable if mounts expose host paths.
- [ ] Resolve symlinks before permission checks, or check both raw and resolved paths.

### `link()` only checks destination permission (MEDIUM)
- [ ] Check permissions on both source and destination for `link()`.

### Symlinks can cross mount boundaries (HIGH)
`MountTable` enforces `EXDEV` for rename/link but not symlink.
- [ ] Enforce mount boundary checks for symlink targets.

### `exists()` bypasses EACCES (LOW)
When permission check returns EACCES, `exists()` falls through — leaks file existence.
- [ ] Return `false` on EACCES instead of falling through.

## P2: Process isolation gaps

### Host PID reuse in `signal_runtime_process` (HIGH)
Sidecar sends real `kill(2)` to host PIDs. PID reuse could kill wrong host process.
- [ ] Check child liveness before signaling.
- [ ] Whitelist allowed signals to `SIGTERM`/`SIGKILL`/`SIGINT`/`SIGCONT`/signal-0.

### PTY foreground PGID manipulation (MEDIUM)
Guest with PTY master FD can redirect signals to arbitrary process groups (guest-to-guest within same VM).
- [ ] Validate target PGID belongs to same session.

### `dup2` skips FD bounds check (MEDIUM)
- [ ] Validate `new_fd < MAX_FDS_PER_PROCESS` in `dup2` and `open_with`.

## P2: Resource exhaustion / DoS

### No filesystem total size limit (HIGH — guest-exploitable)
All file data in-memory with no cap. Guest writes to OOM.
- [ ] Add `max_filesystem_bytes` and `max_inode_count` to `ResourceLimits`.

### `truncate` / `pwrite` with large values cause OOM (MEDIUM)
- [ ] Validate against filesystem size limits before resizing.

### `read_frame` pre-validation OOM (MEDIUM)
`stdio.rs` allocates from 4-byte prefix before checking `max_frame_bytes`. Reachable only from local socket (trusted caller), but trivial fix.
- [ ] Check `declared_len` against `max_frame_bytes` before allocation.

### No WASM fuel/memory/stack limits (MEDIUM)
- [ ] Add execution fuel limits and memory growth caps.

### `pipe.read()` / `pty.read()` block forever if write end leaks (MEDIUM)
- [ ] Add timeout to pipe/PTY read operations.

### No socket/connection resource limits (MEDIUM)
- [ ] Add socket count and connection limits to `ResourceLimits`.

## P2: Pyodide-specific

### Exit code injection via stderr magic prefix (MEDIUM)
Guest can write `__AGENT_OS_PYTHON_EXIT__:0` to fake exit.
- [ ] Use side channel for control messages instead of in-band stderr parsing.

### Hardening runs AFTER `loadPyodide()` (MEDIUM)
Pyodide may cache references to dangerous APIs before hardening runs.
- [ ] Run hardening before `loadPyodide()`.

### Unbounded VFS RPC request queue (MEDIUM)
- [ ] Add bounded queue or rate limiting.

### Missing Pyodide tests
- [ ] Test frozen time — Phase 1 AC 1.4
- [ ] Test `node:child_process`/`node:vm` inaccessibility — Phase 1 AC 1.5
- [ ] Test zero network requests during init — Phase 1 AC 1.6
- [ ] Test kill (SIGTERM) — Phase 1 AC 1.7
- [ ] Test concurrent executions — Phase 1 AC 1.8
- [ ] Test cross-runtime file visibility — Phase 3 AC 3.5

## P2: Missing security infrastructure

### No security audit logging
Auth failures, permission denials, mount operations, kill-process calls — none are logged.
- [ ] Add structured security event logging for auth failures, permission denials, mount/unmount, process kills.

### Google Drive plugin SSRF via `token_url` and `api_base_url`
Mount config accepts arbitrary URLs. Can point `token_url` at internal services to exfiltrate JWTs.
- [ ] Validate URLs against expected hosts.

### S3 plugin SSRF via `endpoint`
S3 mount config accepts arbitrary endpoint URL. Can reach cloud metadata.
- [ ] Validate endpoint against private IP ranges.

### `mount_filesystem` has no permission checks
`kernel.rs` mount functions only check `assert_not_terminated()`. No path or caller validation.
- [ ] Add permission checks on mount operations.

## P3: Kernel correctness

### `host_dir` mount TOCTOU in path resolution (MEDIUM)
`fs::canonicalize()` + `ensure_within_root()` has race window for symlink swap.
- [ ] Use `O_NOFOLLOW`/`openat`-style resolution.

### `setpgid` allows cross-driver group joining (MEDIUM)
- [ ] Validate target PGID's owning driver matches requester.

### Poisoned mutex / `.expect()` inconsistency (MEDIUM)
`lock_or_recover()` in some modules, `.expect()` in others.
- [ ] Decide on single poison policy and apply consistently.

### `hardenProperty` falls back to mutable assignment (LOW)
- [ ] Throw instead of falling back.

### Signal/exit control messages via stderr (LOW)
Guest can emit magic prefixes on stderr to influence sidecar state.
- [ ] Use side channel for control messages.

### Zombie reaper loses exit codes (LOW)
- [ ] Don't reap zombies with living parent that hasn't called `waitpid`.

## P3: WASM permission tiers not enforced

- [ ] Restrict WASI preopens based on declared permission tier.
- [ ] Only provide `host_process` imports to `full` tier commands.

## P3: Pyodide code quality

- [ ] ~870 lines embedded JS — extract to `.js` file loaded at build time.
- [ ] ~300 lines duplicated across `python.rs`/`wasm.rs`/`javascript.rs` — extract shared code.
- [ ] `@rivet-dev/agent-os-python-packages` registry package not created.
- [ ] Cold/warm start times not documented.
- [ ] `NodeImportCache` temp directories never cleaned up on crash.

## P3: Low-priority robustness

- [ ] `read_dir` linear scan — use tree structure for directory children lookup.
- [ ] `collect_snapshot_entries` unbounded recursion — add depth limit or iterate.
- [ ] `nlink` underflow — use `saturating_sub`.
- [ ] `allocate_fd` potential infinite loop — bounded scan.
- [ ] SQLite WASM VFS deterministic randomness — wire to `random_get`.
- [ ] WASM FFI `poll()` buffer validation, `getpwuid` buffer trust, `usize`→`u32` truncation.
- [ ] SQL buffer overflow in `sqlite3_cli.c` (WASM-contained).
