# Migration Gap Stories: TypeScript → Rust Parity

Generated from a comprehensive diff between the last TypeScript commit (`6694bf5`) and current Rust HEAD.

The TypeScript worktree is at `/tmp/a5-typescript/` for reference. The original secure-exec source is at `/home/nathan/secure-exec-1/` (tagged `v0.2.1`), and recovered polyfill/bridge code is at `.agent/recovery/secure-exec/`.

## Existing incomplete stories (not duplicated here)

- **US-066** - POSIX compliance tests (/proc, /dev, signals, process model)
- **US-067** - Security isolation test suite (guest escape prevention)
- **US-068** - Missing POSIX fs ops (ftruncate, mkdtemp, access, flock)
- **US-076** - Claude agent E2E
- **US-077** - OpenCode agent E2E
- **US-078** - Codex agent E2E
- **US-079** - Session cleanup and resource leak prevention

---

## SECTION 1: AGENT CONFIGURATION SYSTEM (P1)

These must land before US-076/077/078 can pass — those agent E2E stories depend on proper per-agent configuration.

### US-080: Port per-agent AGENT_CONFIGS to Rust compat layer

**Description:** The TypeScript version had a full `AGENT_CONFIGS` map (`agents.ts:40-201`) defining per-agent `acpAdapter`, `agentPackage`, `launchArgs`, `defaultEnv`, and an async `prepareInstructions()` hook. The Rust `compat.rs` only distinguishes `Generic` vs `OpenCode`. Port all agent configs so each agent type gets correct launch arguments, environment variables, and system prompt injection.

**Reference files:**
- TS source: `/tmp/a5-typescript/packages/core/src/agents.ts` (232 lines) — `AGENT_CONFIGS` map with PI, PI-CLI, OpenCode, Claude configs
- TS source: `/tmp/a5-typescript/packages/core/src/os-instructions.ts` (19 lines) — `getOsInstructions()` for system prompt generation
- Rust target: `crates/sidecar/src/acp/compat.rs` — currently 313 lines, only Generic/OpenCode

**Acceptance criteria:**
- `AgentCompatibilityKind` enum includes variants for Pi, PiCli, OpenCode, Claude (matching TS `AgentType`)
- Each agent variant carries: adapter package name, agent package name, optional launch args, optional default env vars
- `prepareInstructions()` equivalent implemented: reads VM OS instructions, generates tool reference, injects via `--append-system-prompt` or env files as appropriate per agent
- OpenCode-specific: creates `/tmp/agentos-additional-instructions.md` and `/tmp/agentos-tool-reference.md`, sets `OPENCODE_CONTEXTPATHS` env var (TS agents.ts:116-166)
- Claude-specific: sets 12 default env vars including `CLAUDE_CODE_SIMPLE=1`, `CLAUDE_CODE_SHELL=/bin/bash`, etc. (TS agents.ts:168-199)
- Pi/Pi-CLI: passes instructions via `--append-system-prompt` flag (TS agents.ts:73-114)
- `cargo test -p agent-os-sidecar` passes

### US-081: Test agent configuration produces correct launch environment

**Description:** Verify each agent type's configuration is correctly applied during session creation.

**Reference files:**
- TS source: `/tmp/a5-typescript/packages/core/src/agents.ts` — expected env vars and args per agent type

**Acceptance criteria:**
- Test: Pi agent session includes `--append-system-prompt` in launch args
- Test: Claude agent session includes all 12 default env vars
- Test: OpenCode agent session creates context path files and sets `OPENCODE_CONTEXTPATHS`
- Test: Unknown agent type falls back to Generic config
- Test: `toolReference` parameter is correctly injected into instructions
- Test: `skipBase` option suppresses base OS instructions
- Test file: `crates/sidecar/tests/agent_config.rs`
- `cargo test -p agent-os-sidecar --test agent_config` passes

---

## SECTION 2: KERNEL SOCKET TABLE (P2)

The TypeScript kernel had a unified `SocketTable` (~1,500 lines in `secure-exec-1/packages/core/src/kernel/socket-table.ts`) that all runtimes used. The Rust kernel has zero socket support — JavaScript gets networking only via the V8 bridge, and WASM/Python get nothing.

### US-082: Implement kernel SocketTable with TCP support (AF_INET, SOCK_STREAM)

**Description:** Add a `socket_table.rs` to the kernel with TCP socket lifecycle: socket creation, bind, listen, accept, connect, send, recv, shutdown, close. Integrate with the FD table so sockets are addressable by file descriptor.

**Reference files:**
- TS source: `/home/nathan/secure-exec-1/packages/core/src/kernel/socket-table.ts` — full SocketTable implementation
- TS source: `/home/nathan/secure-exec-1/packages/nodejs/src/bridge/network.ts` (11,149 lines) — polyfill using socket table
- Rust target: `crates/kernel/src/` — new `socket_table.rs`
- Rust integration: `crates/kernel/src/fd_table.rs` — FD entries must support socket type

**Acceptance criteria:**
- `crates/kernel/src/socket_table.rs` exists with `SocketTable` struct
- `socket()` creates AF_INET + SOCK_STREAM entries, returns socket ID
- `bind(addr, port)` assigns local address (port 0 = ephemeral)
- `listen(backlog)` transitions to listening state
- `accept()` returns new connected socket + peer address
- `connect(host, port)` initiates outbound TCP connection
- `send(data)` / `recv(max_len)` for data transfer
- `shutdown(how)` for half-close (SHUT_RD, SHUT_WR, SHUT_RDWR)
- `close()` releases socket resources
- Socket options: SO_REUSEADDR, SO_KEEPALIVE, TCP_NODELAY via `setsockopt`/`getsockopt`
- Sockets integrated with FD table — `open_socket_fd()` returns usable file descriptor
- `HostNetworkAdapter` trait for actual I/O delegation to host (TS: `socket-table.ts` adapter pattern)
- Resource accounting: `check_socket_allocation()` enforced from `resource_accounting.rs`
- `cargo test -p agent-os-kernel` passes

### US-083: Add UDP datagram support to kernel SocketTable

**Description:** Extend SocketTable with SOCK_DGRAM support for UDP communication.

**Reference files:**
- TS source: `/home/nathan/secure-exec-1/packages/core/src/kernel/socket-table.ts` — UDP socket handling with datagram queue and message boundaries
- Rust target: `crates/kernel/src/socket_table.rs`

**Acceptance criteria:**
- `socket()` supports AF_INET + SOCK_DGRAM
- `bind(addr, port)` assigns local UDP address
- `sendto(data, dest_addr, dest_port)` sends datagram
- `recvfrom(max_len)` receives datagram with source address
- Message boundary preservation (each send = one recv)
- MAX_DATAGRAM_SIZE = 65535 enforced
- Non-blocking mode support (EAGAIN on empty receive)
- `cargo test -p agent-os-kernel` passes

### US-084: Add Unix domain socket support to kernel SocketTable

**Description:** Extend SocketTable with AF_UNIX support for local IPC.

**Reference files:**
- TS source: `/home/nathan/secure-exec-1/packages/core/src/kernel/socket-table.ts` — AF_UNIX handling with path-based addresses
- Rust target: `crates/kernel/src/socket_table.rs`

**Acceptance criteria:**
- `socket()` supports AF_UNIX + SOCK_STREAM and SOCK_DGRAM
- `bind(path)` creates socket file in VFS
- `connect(path)` connects to path-based socket
- `listen()` + `accept()` work for SOCK_STREAM
- Socket file cleaned up on close (or unlink)
- Permissions checked via kernel VFS permission system
- `cargo test -p agent-os-kernel` passes

### US-085: Implement kernel DNS resolver

**Description:** Add a DNS resolution syscall to the kernel so all runtimes can resolve hostnames without host fallthrough.

**Reference files:**
- TS source: `/home/nathan/secure-exec-1/packages/nodejs/src/bridge/network.ts` — DNS resolution via kernel adapter
- Rust existing: `crates/sidecar/src/execution.rs` lines ~8236 — `dns.lookup`/`dns.resolve` exist but use host `hickory-resolver` directly, not through kernel
- Rust target: `crates/kernel/src/` — DNS interface on kernel or socket_table

**Acceptance criteria:**
- Kernel exposes `dns_resolve(hostname, family)` returning list of IP addresses
- Kernel exposes `dns_reverse(ip)` returning list of hostnames
- Resolution delegates to a `DnsAdapter` trait (host provides real DNS; tests can mock)
- Network permissions checked before resolution (`NetworkAccessRequest::Dns`)
- Integration with socket table: `connect(hostname, port)` resolves automatically
- `cargo test -p agent-os-kernel` passes

### US-086: Implement kernel loopback routing for 127.0.0.1

**Description:** When a guest connects to `127.0.0.1:<port>`, route the connection to another guest listener on the same VM — no host network needed.

**Reference files:**
- TS source: `/home/nathan/secure-exec-1/packages/core/src/kernel/socket-table.ts` — in-kernel loopback routing
- Rust existing: `crates/sidecar/src/execution.rs` — V8 bridge has loopback routing for JS only

**Acceptance criteria:**
- Kernel socket table maintains a listener registry (port → socket ID)
- `connect("127.0.0.1", port)` checks listener registry first, routes in-kernel if found
- In-kernel connection creates a pair of bidirectional buffers (like socketpair)
- If no kernel listener, falls through to HostNetworkAdapter
- Works for both TCP and UDP
- Test: two guest processes communicate via loopback TCP
- Test: loopback connection does not touch host network
- `cargo test -p agent-os-kernel` passes

### US-087: Wire V8 bridge net.* calls through kernel SocketTable

**Description:** Currently, V8 bridge `net.*` syscalls in `execution.rs` create real host sockets directly. Rewire them to go through the kernel SocketTable, so JS networking is kernel-mediated.

**Reference files:**
- TS source: `/home/nathan/secure-exec-1/packages/nodejs/src/bridge/network.ts` — all net calls go through kernel
- Rust current: `crates/sidecar/src/execution.rs` lines 8239-8329 — `net.*`, `dgram.*` handlers create host sockets directly

**Acceptance criteria:**
- `net.socket_create` calls `kernel.socket_table.create_socket()` instead of host socket
- `net.socket_connect` calls `kernel.socket_table.connect()` → delegates to host adapter
- `net.socket_listen` calls `kernel.socket_table.listen()` → host adapter binds real port
- `net.socket_read/write` go through kernel socket buffers
- `dgram.*` calls use kernel UDP sockets
- `dns.*` calls use kernel DNS resolver
- Network permissions enforced at kernel level, not sidecar level
- Existing V8 networking tests still pass
- `cargo test -p agent-os-sidecar` passes

### US-088: Wire WASM WASI sock_* calls through kernel SocketTable

**Description:** WASM guest processes currently have zero networking. Wire WASI socket syscalls through the kernel SocketTable so WASM commands (curl, git, wget) can use the network.

**Reference files:**
- TS source: `/home/nathan/secure-exec-1/packages/posix/src/driver.ts` — WASM syscall routing through kernel
- Rust current: `crates/execution/src/wasm.rs` — no socket support

**Acceptance criteria:**
- WASI `sock_open`, `sock_bind`, `sock_listen`, `sock_accept`, `sock_connect` routed to kernel SocketTable
- WASI `sock_send`, `sock_recv` use kernel socket buffers
- WASI `sock_shutdown` maps to kernel shutdown
- Test: WASM `curl` command can fetch an HTTP URL
- Test: WASM `git clone` over HTTP works
- Permission tier checked: only WASM commands at appropriate tier get network access
- `cargo test -p agent-os-execution` passes

### US-089: Cross-runtime networking test suite

**Description:** Verify that networking works identically across all three runtimes and that cross-runtime communication works via loopback.

**Reference files:**
- TS source: would have been tested via kernel socket table unifying all runtimes

**Acceptance criteria:**
- Test: JS guest creates TCP server, WASM guest connects to it via 127.0.0.1
- Test: WASM guest creates TCP server, JS guest connects to it
- Test: JS guest sends UDP datagram, WASM guest receives it
- Test: Python guest can make HTTP request through kernel TCP
- Test: Two JS guests in the same VM communicate via loopback
- Test: DNS resolution returns same results from JS, WASM, and Python
- Test file: `crates/sidecar/tests/cross_runtime_networking.rs`
- `cargo test -p agent-os-sidecar --test cross_runtime_networking -- --test-threads=1` passes

---

## SECTION 3: CENTRALIZED KERNEL poll(2) (P2)

### US-090: Implement centralized kernel poll(2) syscall

**Description:** The kernel has poll primitives (`poll.rs`, 157 lines) and individual subsystems (pipes, PTYs) can poll, but there's no centralized `poll()` syscall that multiplexes across all FD types. Implement it.

**Reference files:**
- TS source: `/home/nathan/secure-exec-1/packages/core/src/kernel/` — poll across pipes, sockets, PTYs
- Rust existing: `crates/kernel/src/poll.rs` — PollEvents, PollFd, PollNotifier exist
- Rust existing: `crates/kernel/src/pipe_manager.rs` — has internal `poll()` method
- Rust existing: `crates/kernel/src/pty.rs` — has internal poll readiness

**Acceptance criteria:**
- `kernel.poll(fds: &mut [PollFd], timeout_ms: i32) -> Result<usize>` implemented
- Multiplexes across: pipes, PTYs, sockets (once socket table exists), regular files
- POLLIN, POLLOUT, POLLERR, POLLHUP, POLLNVAL set correctly per FD type
- Timeout semantics: -1 = block forever, 0 = non-blocking, >0 = ms timeout
- Returns number of ready FDs
- Wakes up when any monitored FD becomes ready (via PollNotifier)
- Test: poll pipe read end after write
- Test: poll PTY master after slave write
- Test: poll with timeout expiry
- Test: poll returns POLLNVAL for closed FD
- `cargo test -p agent-os-kernel` passes

### US-091: Wire poll(2) to V8 bridge and WASM WASI

**Description:** Expose the kernel poll(2) to guest runtimes so they can do I/O multiplexing.

**Reference files:**
- TS source: WASM syscall routing included poll
- Rust target: `crates/sidecar/src/execution.rs` — add `poll` sync RPC handler

**Acceptance criteria:**
- V8 bridge: `__kernel_poll` sync RPC method calls `kernel.poll()`
- WASM: `poll_oneoff` WASI syscall routes through kernel poll
- Test: JS guest code uses poll to wait on multiple pipes
- Test: WASM command uses poll_oneoff for I/O multiplexing
- `cargo test -p agent-os-sidecar` passes

---

## SECTION 4: LAYER MANAGEMENT API (P2)

### US-092: Implement LayerStore with create/seal/import/compose operations

**Description:** The TypeScript version had a full `LayerStore` abstraction (`layers.ts:44-307`) enabling Docker-like layer management. The Rust kernel only has `RootFileSystem` with a single overlay. Implement the layer lifecycle API.

**Reference files:**
- TS source: `/tmp/a5-typescript/packages/core/src/layers.ts` (314 lines) — `LayerStore`, `WritableLayerHandle`, `SnapshotLayerHandle`, `createWritableLayer()`, `importSnapshot()`, `sealLayer()`, `createOverlayFilesystem()`
- Rust existing: `crates/kernel/src/root_fs.rs` — `RootFileSystem` with single overlay
- Rust existing: `crates/kernel/src/overlay_fs.rs` — `OverlayFilesystem`

**Acceptance criteria:**
- `LayerStore` struct in kernel with:
  - `create_writable_layer()` → `WritableLayerHandle` (allocates fresh writable FS with lease ID)
  - `import_snapshot(source)` → `SnapshotLayerHandle` (creates immutable layer from snapshot entries)
  - `open_snapshot_layer(layer_id)` → `SnapshotLayerHandle` (reopens existing snapshot)
  - `seal_layer(writable)` → `SnapshotLayerHandle` (freezes writable → immutable, invalidates lease)
  - `create_overlay_filesystem(upper, lowers)` → `OverlayFilesystem` (composes layer stack)
- Lease tracking prevents concurrent writes to same layer
- Layer identified by `{store_id, layer_id, kind}`
- Sidecar exposes layer RPCs matching TypeScript API
- `cargo test -p agent-os-kernel` passes

### US-093: Test multi-layer composition and snapshot lifecycle

**Description:** Verify layer operations produce correct filesystem state.

**Reference files:**
- TS source: `/tmp/a5-typescript/packages/core/src/layers.ts` — test scenarios implied by API

**Acceptance criteria:**
- Test: create writable layer, write files, seal → snapshot contains written files
- Test: import snapshot, overlay with new writable → reads see snapshot + new files
- Test: seal writable with deletes → snapshot has whiteouts for deleted files
- Test: compose 3-layer stack → reads correctly merge all layers
- Test: double-seal same writable → error (lease invalidated)
- Test: concurrent writable layers on same store → independent writes
- Test file: `crates/kernel/tests/layer_store.rs`
- `cargo test -p agent-os-kernel --test layer_store` passes

---

## SECTION 5: USER/GROUP IDENTITY (P3)

### US-094: Implement full user/group identity syscalls

**Description:** The Rust kernel `user.rs` is only 49 lines with a basic `User` struct. The TS version had full `getuid`/`getgid`/`geteuid`/`getegid`/`isatty`/`getpwuid` support.

**Reference files:**
- TS source: `/home/nathan/secure-exec-1/packages/posix/src/user.ts` — full user identity
- Rust existing: `crates/kernel/src/user.rs` (49 lines) — minimal User struct

**Acceptance criteria:**
- `kernel.getuid()` / `kernel.getgid()` return process UID/GID
- `kernel.geteuid()` / `kernel.getegid()` return effective UID/GID
- `kernel.setuid()` / `kernel.setgid()` change identity (root only)
- `kernel.isatty(fd)` checks if FD is a PTY
- Default user: uid=0, gid=0 (root) — matching TS behavior
- User info queryable: username, homedir, shell
- Wire to V8 bridge as `process.getuid()` etc.
- Wire to WASM WASI as `getuid` import
- `cargo test -p agent-os-kernel` passes

### US-095: Test user/group identity from guest code

**Description:** Verify guest processes see correct virtualized identity.

**Acceptance criteria:**
- Test: JS guest `process.getuid()` returns kernel UID (0), not host UID
- Test: JS guest `process.getgid()` returns kernel GID (0), not host GID
- Test: JS guest `os.userInfo()` returns kernel user info
- Test: WASM `id` command returns "uid=0(root) gid=0(root)"
- Test: `isatty(fd)` returns true for PTY FDs, false for pipe FDs
- Test file: `crates/sidecar/tests/user_identity.rs`
- `cargo test -p agent-os-sidecar --test user_identity -- --test-threads=1` passes

---

## SECTION 6: BOOTSTRAP & FILESYSTEM HARDENING (P3)

### US-096: Add bootstrap path suppression for kernel-reserved directories

**Description:** The TypeScript version suppressed writes to specific directories during bootstrap (`base-filesystem.ts:32-40`). The Rust version only does mode-level locking.

**Reference files:**
- TS source: `/tmp/a5-typescript/packages/core/src/base-filesystem.ts` (253 lines) — `SUPPRESSED_KERNEL_BOOTSTRAP_DIRS`, `SUPPRESSED_KERNEL_BOOTSTRAP_FILES`
- Rust target: `crates/kernel/src/root_fs.rs`

**Acceptance criteria:**
- During bootstrap, writes to `/boot`, `/usr/games`, `/usr/include`, `/usr/libexec`, `/usr/man` are silently suppressed
- During bootstrap, writes to `/usr/bin/env` are suppressed
- After `finish_bootstrap()`, all paths writable again (unless read-only mode)
- Test: bootstrap snapshot with suppressed paths → suppressed entries absent
- Test: post-bootstrap writes to previously-suppressed paths succeed
- `cargo test -p agent-os-kernel` passes

### US-097: Add fcntl operations to kernel FD table

**Description:** The FD table is missing `fcntl(F_GETFL, F_SETFL, F_GETFD, F_SETFD, F_DUPFD)` operations.

**Reference files:**
- TS source: implied by POSIX compliance
- Rust existing: `crates/kernel/src/fd_table.rs` (588 lines) — no fcntl

**Acceptance criteria:**
- `fcntl(fd, F_GETFL)` returns status flags (O_NONBLOCK, O_APPEND, etc.)
- `fcntl(fd, F_SETFL, flags)` modifies status flags
- `fcntl(fd, F_GETFD)` returns FD flags (FD_CLOEXEC)
- `fcntl(fd, F_SETFD, flags)` modifies FD flags
- `fcntl(fd, F_DUPFD, min_fd)` duplicates to lowest available >= min_fd
- Wire to V8 bridge and WASM WASI
- `cargo test -p agent-os-kernel` passes

### US-098: Add pwrite to HostDirFilesystem

**Description:** The host directory mount plugin lacks an explicit `pwrite()` implementation — it falls back to the default trait impl which does read-modify-write. Add a proper implementation.

**Reference files:**
- TS source: `/tmp/a5-typescript/packages/core/src/backends/host-dir-backend.ts:326-338` — explicit pwrite
- Rust target: `crates/sidecar/src/plugins/host_dir.rs`

**Acceptance criteria:**
- `HostDirFilesystem::pwrite(path, data, offset)` writes directly at offset
- Does not read the entire file first
- Handles sparse files correctly (seek + write)
- Test: pwrite at offset 1000 in a 500-byte file extends it
- `cargo test -p agent-os-sidecar` passes

---

## SECTION 7: HOST TOOLS COMPLETENESS (P3)

### US-099: Add MAX_TOOL_DESCRIPTION_LENGTH enforcement in Rust tools.rs

**Description:** TypeScript enforced `MAX_TOOL_DESCRIPTION_LENGTH = 200` characters. Rust validation (`tools.rs`) checks non-empty descriptions but has no length limit.

**Reference files:**
- TS source: `/tmp/a5-typescript/packages/core/src/host-tools.ts:58-73` — `validateToolkits()` with length check
- Rust target: `crates/sidecar/src/tools.rs` lines 49-68

**Acceptance criteria:**
- Tool descriptions exceeding 200 characters are rejected with a clear error
- Toolkit descriptions exceeding 200 characters are rejected
- Test: registering a tool with 201-char description returns error
- `cargo test -p agent-os-sidecar` passes

### US-100: Test shell invocation of agentos-* tools from guest scripts

**Description:** Verify that when guest code runs `/bin/sh -c "agentos-browser screenshot --url ..."`, the kernel intercepts the command and dispatches it through the tool system.

**Reference files:**
- TS source: `/tmp/a5-typescript/packages/core/src/host-tools-shims.ts` (195 lines) — generated Node.js shims in `/usr/local/bin/`
- Rust existing: `crates/sidecar/src/tools.rs` — command resolution via `resolve_tool_command()`
- Rust existing: `crates/kernel/src/command_registry.rs` — COMMAND_STUB in `/bin/{command}`

**Acceptance criteria:**
- Test: guest JS code runs `child_process.execSync('agentos --help')` → returns master help text
- Test: guest JS code runs `child_process.execSync('agentos-{toolkit} {tool} --json "{...}"')` → tool executes and returns result
- Test: guest WASM `sh` command can invoke `agentos-{toolkit} {tool}` via PATH
- If shell invocation fails because kernel doesn't intercept `execve("/bin/agentos")`, fix the COMMAND_STUB to either be a proper binary or ensure kernel intercepts before filesystem lookup
- Test file: `crates/sidecar/tests/tool_shell_invocation.rs`
- `cargo test -p agent-os-sidecar --test tool_shell_invocation -- --test-threads=1` passes

---

## SECTION 8: PACKAGE EXPORTS (P3)

### US-101: Restore missing public type exports in packages/core/src/index.ts

**Description:** The TypeScript `index.ts` exported 60+ types. The current Rust-era `index.ts` exports a minimal subset. Restore exports needed by downstream consumers.

**Reference files:**
- TS source: `/tmp/a5-typescript/packages/core/src/index.ts` (132 lines) — full export list
- Rust current: `/home/nathan/a5/packages/core/src/index.ts` (~20 lines)

**Acceptance criteria:**
- These types/values are exported from `@rivet-dev/agent-os-core`:
  - Session types: `Session`, `CreateSessionOptions`, `SessionInfo`, `SessionMode`, `SessionConfigOption`, `AgentCapabilities`, `AgentInfo`, `PermissionRequest`, `PermissionReply`
  - Protocol: `JsonRpcRequest`, `JsonRpcResponse`, `JsonRpcNotification`, `JsonRpcError`, `serializeMessage`, `deserializeMessage`
  - Mount types: `MountConfig`, `PlainMountConfig`, `OverlayMountConfig`, `RootFilesystemConfig`, `RootLowerInput`
  - Batch ops: `BatchReadResult`, `BatchWriteResult`, `BatchWriteEntry`
  - OS instructions: `getOsInstructions`
  - Other: `DirEntry`, `ReaddirRecursiveOptions`, `SpawnedProcessInfo`, `ProcessTreeNode`
- Types that no longer exist in the new architecture (e.g., `createOverlayBackend`) are NOT re-added
- `pnpm check-types` passes
- `pnpm build` passes

---

## SECTION 9: SQLITE KERNEL POLYFILL (P3)

### US-102: Verify SQLite works from guest V8 code via sidecar rusqlite

**Description:** TypeScript had kernel-backed SQLite bindings with VFS sync (`sqlite-bindings.ts`, 471 lines). Rust uses `rusqlite` in the sidecar with RPC. Verify the current approach provides equivalent functionality, including filesystem sync.

**Reference files:**
- TS source: `/tmp/a5-typescript/packages/core/src/sqlite-bindings.ts` (471 lines) — kernel-backed SQLite with VFS sync, statement pooling, transaction tracking
- Rust existing: `crates/sidecar/src/execution.rs` lines 8330-8346 — `sqlite.*` sync RPC handlers
- Rust existing: US-026 (already embedded rusqlite)

**Acceptance criteria:**
- Test: guest JS `new DatabaseSync('/tmp/test.db')` creates database file visible in kernel VFS
- Test: guest JS executes SQL (CREATE TABLE, INSERT, SELECT) and gets correct results
- Test: prepared statements work (prepare, bind, step, finalize)
- Test: database file persists across multiple open/close cycles within same VM
- Test: database file is visible via `fs.existsSync('/tmp/test.db')` from guest code
- Test: BLOB and INTEGER types correctly round-trip (matching TS bigint/Uint8Array encoding in sqlite-bindings.ts:44-133)
- Test: WAL checkpoint works (`sqlite.checkpoint` RPC)
- Test file: `crates/sidecar/tests/sqlite_guest.rs`
- `cargo test -p agent-os-sidecar --test sqlite_guest -- --test-threads=1` passes

---

## SECTION 10: PROCESS ENVIRONMENT HARDENING (P2)

### US-103: Filter AGENT_OS_* env vars and virtualize host identity in guest environment

**Description:** Guest `process.env` must not leak `AGENT_OS_*` control variables or host identity (HOME, USER, PATH). US-045 ported env virtualization but the subagent analysis found gaps.

**Reference files:**
- TS source: `/home/nathan/secure-exec-1/packages/nodejs/src/bridge/process.ts` (2,251 lines) — full process.env filtering
- TS source: `/tmp/a5-typescript/packages/core/src/agents.ts:168-199` — Claude agent sets 12 env vars
- Rust existing: US-045 (ported process.env virtualization)
- Rust target: `crates/execution/src/runtime_support.rs` — centralized env hardening

**Acceptance criteria:**
- Guest `process.env` does not contain any key starting with `AGENT_OS_`
- Guest `process.env` does not contain `NODE_SYNC_RPC_*`, `NODE_SANDBOX_ROOT`, or other internal control vars
- Guest `process.env.HOME` returns `/root` (or kernel-configured home), not host home
- Guest `process.env.USER` returns `root` (or kernel-configured user), not host user
- Guest `process.env.PATH` returns kernel PATH, not host PATH
- All three runtimes (JS, Python, WASM) apply the same filtering
- Centralized in a single function in `runtime_support.rs` (not duplicated per runtime)
- Test: verify 0 leaked `AGENT_OS_*` vars in guest env (this may already exist in US-067 scope — if so, just verify it passes)
- `cargo test -p agent-os-sidecar` passes

---

## SECTION 11: ACP CLIENT EDGE CASES (P3)

### US-104: Add process exit code to ACP client timeout diagnostics

**Description:** TypeScript ACP client included `process.exitCode` and `process.killed` in timeout error messages. Rust only tracks `transport_state` string.

**Reference files:**
- TS source: `/tmp/a5-typescript/packages/core/src/acp-client.ts:370-384` — exit code and killed flag in error
- Rust target: `crates/sidecar/src/acp/client.rs` lines 85-88, 317-336

**Acceptance criteria:**
- `AcpClientError::Timeout` message includes agent process exit code (if exited)
- `AcpClientError::Timeout` message includes whether process was killed vs. exited naturally
- Transport state tracks `exit_code: Option<i32>` and `killed: bool` separately (not just a string)
- `cargo test -p agent-os-sidecar` passes

### US-105: Implement synthetic session/update for agents that don't emit notifications

**Description:** When `setMode()` succeeds but the agent doesn't send a `session/update` notification (e.g., OpenCode), the TypeScript session emitted a synthetic notification. Rust doesn't do this.

**Reference files:**
- TS source: `/tmp/a5-typescript/packages/core/src/session.ts:350-469` — synthetic update generation with event stream dedup check
- Rust target: `crates/sidecar/src/acp/session.rs` — `apply_request_success()` exists but needs synthetic notification emission

**Acceptance criteria:**
- After a successful `setMode()` RPC, if no `session/update` notification arrives within 500ms, emit a synthetic one
- Synthetic notification contains `current_mode_update` with the new mode
- Dedup check: if a real notification arrived in the meantime, suppress the synthetic one (TS session.ts:456-463)
- Same logic for `setModel()` and `setThoughtLevel()` RPCs
- `cargo test -p agent-os-sidecar` passes

---

## SECTION 12: COMPREHENSIVE PARITY VALIDATION (FINAL)

### US-106: End-to-end migration parity test — exercise every major subsystem

**Description:** A comprehensive integration test that exercises every subsystem migrated from TypeScript to Rust, confirming the full stack works together.

**Acceptance criteria:**
- Test creates a VM with base filesystem, overlay layer, and host directory mount
- Test registers a toolkit with 3 tools, verifies help/describe/invoke all work
- Test spawns a JS guest process that:
  - Reads/writes files via `fs` (kernel VFS)
  - Creates a TCP server and connects to it via loopback (kernel socket table)
  - Resolves DNS (kernel DNS)
  - Spawns a child process and communicates via pipe (kernel process table + pipes)
  - Opens a PTY and writes to it (kernel PTY)
  - Uses `poll()` to wait on multiple FDs (kernel poll)
  - Creates and queries a SQLite database (sidecar rusqlite)
  - Calls `process.getuid()` (kernel user identity)
  - Calls `agentos-{toolkit} {tool}` via child_process.exec (tool dispatch)
- Test spawns a WASM command that reads a file and writes output (kernel VFS via WASI)
- Test creates an ACP session with a mock agent, sends a prompt, receives events
- Test seals the writable layer into a snapshot, imports it as a new layer
- Test verifies resource cleanup after dispose
- Test file: `crates/sidecar/tests/migration_parity.rs`
- `cargo test -p agent-os-sidecar --test migration_parity -- --test-threads=1` passes

---

## PRIORITY & DEPENDENCY ORDER

```
P1 (Agent E2E blockers):
  US-080 → US-081 → (then US-076, US-077, US-078 can proceed)

P2 (Kernel completeness):
  US-082 → US-083 → US-084 → US-085 → US-086  (socket table build-up)
  US-087 (wire V8 bridge) depends on US-082
  US-088 (wire WASM) depends on US-082
  US-089 (cross-runtime test) depends on US-087 + US-088
  US-090 → US-091 (poll)
  US-092 → US-093 (layers)
  US-103 (env hardening)

P3 (Feature parity):
  US-094 → US-095 (user/group)
  US-096 (bootstrap suppression)
  US-097 (fcntl)
  US-098 (pwrite)
  US-099 (tool description length)
  US-100 (tool shell invocation)
  US-101 (package exports)
  US-102 (sqlite verification)
  US-104 (ACP diagnostics)
  US-105 (synthetic updates)

Final:
  US-106 (comprehensive parity) depends on all above
```
