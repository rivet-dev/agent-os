# Agent OS Kernel Security Audit Report

**Date:** 2026-04-05
**Scope:** Full adversarial review of kernel, execution engines, VFS, networking, permissions, and POSIX compliance
**Method:** 12 parallel adversarial review agents examining each subsystem independently

---

## Executive Summary

This audit examined the Agent OS kernel across 12 dimensions: VFS/overlay filesystem, Node.js isolation, WASM execution, process table/signals, network stack, permission system, Python/Pyodide isolation, sidecar RPC, POSIX edge cases, host information leakage, resource limits/DoS, and control channel security.

**Key findings:**
- **58 CRITICAL/HIGH issues** across all subsystems
- **Node.js isolation is the weakest link** -- many builtins fall through to real host modules
- **Network stack has zero permission enforcement** -- guest code can connect anywhere
- **Control channels are in-band** -- guest can inject fake control messages via stderr
- **POSIX compliance has major gaps** -- no fork(), no file locking, no signal handlers, no mmap
- **Python/Pyodide is the most secure subsystem** -- proper WASM sandboxing with defense-in-depth

---

## 1. Linux Kernel Compatibility Matrix

### 1.1 Syscall / Feature Implementation Status

| Feature | Linux | Agent OS | Status | Severity |
|---------|-------|----------|--------|----------|
| **Filesystem** | | | | |
| open/close/read/write | Full POSIX | Implemented | OK | - |
| pread/pwrite | Full POSIX | Implemented | OK | - |
| stat/lstat/fstat | Full POSIX | Implemented | Partial (missing blocks, dev fields) | LOW |
| readdir | Full POSIX | Implemented | OK (filters `.`/`..`) | - |
| mkdir/rmdir | Full POSIX | Implemented | OK | - |
| rename | Atomic | Non-atomic multi-step | BROKEN | HIGH |
| link/unlink | Full POSIX | Implemented | OK | - |
| symlink/readlink | Full POSIX | Implemented | OK | - |
| chmod/chown | Full POSIX | Implemented | Missing permission enforcement | MEDIUM |
| truncate/ftruncate | Full POSIX | Implemented | OK | - |
| O_APPEND | Atomic seek+write | Non-atomic (race condition) | BROKEN | CRITICAL |
| O_CREAT\|O_EXCL | Atomic create-if-not-exists | TOCTOU race (check then create) | BROKEN | CRITICAL |
| O_NONBLOCK | Per-FD flag | Not implemented | MISSING | HIGH |
| O_DIRECTORY | opendir validation | Not implemented | MISSING | LOW |
| O_NOFOLLOW | Symlink rejection | Not implemented | MISSING | MEDIUM |
| O_CLOEXEC / FD_CLOEXEC | Per-FD flag | Not implemented in kernel | MISSING | MEDIUM |
| flock / fcntl locking | Advisory/mandatory locks | Not implemented | MISSING | CRITICAL |
| mmap / munmap | Memory-mapped files | Not implemented | MISSING | HIGH |
| sendfile / splice | Zero-copy transfer | Not implemented | MISSING | LOW |
| sparse files | Hole-aware storage | Materialized as zeros | BROKEN | MEDIUM |
| xattr | Extended attributes | Not implemented | MISSING | LOW |
| umask | Default creation mask | Not implemented | MISSING | MEDIUM |
| sticky bit | /tmp protection | Not enforced | MISSING | MEDIUM |
| setgid on dirs | Group inheritance | Not implemented | MISSING | LOW |
| atime/mtime/ctime | Full tracking | Partial (atime only on pread) | BROKEN | LOW |
| inotify / fanotify | FS event monitoring | Not implemented | MISSING | LOW |
| **Process Management** | | | | |
| fork() | Full COW semantics | Not implemented (spawn only) | MISSING | CRITICAL |
| exec() | Replaces process image | Partial (no shebang parsing) | BROKEN | HIGH |
| waitpid() | Full flags (WNOHANG, etc.) | Blocking only, single PID | BROKEN | HIGH |
| kill() | Full signal delivery | Only SIGTERM/SIGKILL work | BROKEN | HIGH |
| getpid/getppid | Full | Virtualized (correct) | OK | - |
| setpgid/getpgid | Full | Implemented | OK | - |
| setsid/getsid | Full | Implemented (no orphan handling) | PARTIAL | MEDIUM |
| setuid/setgid/seteuid | Full | Not implemented | MISSING | LOW |
| process groups | Full signal delivery | Kill doesn't reach stopped processes | BROKEN | HIGH |
| sessions | Full with controlling TTY | Partial (no orphan group handling) | BROKEN | MEDIUM |
| reparenting to init | Automatic on parent death | Not implemented | MISSING | HIGH |
| zombie reaping | Via waitpid() | 60s TTL auto-reap (non-standard) | DIFFERENT | MEDIUM |
| **Signals** | | | | |
| SIGCHLD | On child exit | Not implemented | MISSING | CRITICAL |
| SIGPIPE | On broken pipe write | Not implemented (EPIPE only) | MISSING | HIGH |
| SIGWINCH | On terminal resize | Not implemented | MISSING | MEDIUM |
| SIGSTOP/SIGCONT | Job control | Not implemented | MISSING | HIGH |
| SIGINT/SIGQUIT/SIGTSTP | Terminal signals | PTY-only (correct) | OK | - |
| SIGTERM | Termination | Implemented | OK | - |
| SIGKILL | Forced kill | Implemented | OK | - |
| sigprocmask | Signal blocking | Not implemented | MISSING | HIGH |
| sigaction | Handler registration | Not implemented | MISSING | HIGH |
| SA_RESTART | Syscall restart | Not implemented | MISSING | MEDIUM |
| EINTR | Interrupted syscall | Not implemented | MISSING | HIGH |
| Real-time signals | SIGRTMIN-SIGRTMAX | Not implemented | MISSING | LOW |
| **Pipes & IPC** | | | | |
| pipe/pipe2 | 64KB buffer | 65KB buffer (close enough) | OK | - |
| PIPE_BUF atomicity | Writes <= 4096 atomic | Not atomic at any size | BROKEN | HIGH |
| Non-blocking pipes | O_NONBLOCK + EAGAIN | Not implemented | MISSING | HIGH |
| select/poll/epoll | FD multiplexing | Not implemented | MISSING | CRITICAL |
| Unix domain sockets | AF_UNIX | Not implemented | MISSING | MEDIUM |
| SCM_RIGHTS | FD passing | Not implemented | MISSING | LOW |
| **Networking** | | | | |
| TCP sockets | Full | Sidecar-managed (no kernel mediation) | BROKEN | CRITICAL |
| UDP sockets | Full | Sidecar-managed (no kernel mediation) | BROKEN | CRITICAL |
| DNS resolution | Full | Falls through to host resolver | BROKEN | CRITICAL |
| SO_REUSEADDR | Socket option | Not implemented in kernel | MISSING | MEDIUM |
| Non-blocking connect | O_NONBLOCK + EINPROGRESS | Not implemented in kernel | MISSING | MEDIUM |
| **TTY/PTY** | | | | |
| PTY pairs | Full | Implemented | OK | - |
| Canonical mode | Line editing | Partial | PARTIAL | LOW |
| Raw mode | Character-at-a-time | Partial (no full termios) | PARTIAL | MEDIUM |
| VMIN/VTIME | Read timing | Not implemented | MISSING | LOW |
| Echo control | Per-character | Basic flag only | PARTIAL | LOW |
| ^C/^D/^Z/^\ | Special chars | ^C/^Z/^\ work, ^D missing | PARTIAL | LOW |
| **Device Files** | | | | |
| /dev/null | Full | Implemented | OK | - |
| /dev/zero | Configurable read size | Fixed 4096 bytes always | BROKEN | LOW |
| /dev/urandom | Configurable read size | Fixed 4096 bytes always | BROKEN | LOW |
| /dev/full | ENOSPC on write | Not implemented | MISSING | LOW |
| /dev/random | Blocking entropy | Not implemented | MISSING | LOW |
| /dev/fd/N | FD directory | Stub (empty listing) | BROKEN | MEDIUM |
| /dev/tty | Controlling terminal | Not implemented | MISSING | MEDIUM |
| /dev/pts/* | PTY devices | Stub | PARTIAL | LOW |
| **/proc Filesystem** | | | | |
| /proc/self | Symlink to PID | Not implemented | MISSING | MEDIUM |
| /proc/[pid]/stat | Process status | Not implemented | MISSING | MEDIUM |
| /proc/[pid]/status | Process info | Not implemented | MISSING | MEDIUM |
| /proc/[pid]/fd/ | Open FDs | Not implemented | MISSING | MEDIUM |
| /proc/[pid]/cmdline | Command line | Not implemented | MISSING | LOW |
| /proc/[pid]/environ | Environment | Not implemented | MISSING | LOW |
| /proc/[pid]/cwd | Working dir link | Not implemented | MISSING | LOW |
| /proc/[pid]/exe | Executable link | Not implemented | MISSING | LOW |
| /proc/cpuinfo | CPU info | Not implemented | MISSING | LOW |
| /proc/meminfo | Memory info | Not implemented | MISSING | LOW |
| /proc/mounts | Mount table | Not implemented | MISSING | MEDIUM |
| /proc/sys/* | Sysctl | Not implemented | MISSING | LOW |

### 1.2 Error Code Coverage

| errno | Linux | Agent OS | Status |
|-------|-------|----------|--------|
| EACCES | Permission denied | Implemented | OK |
| EAGAIN | Try again | Implemented | OK |
| EBADF | Bad FD | Implemented | OK |
| EEXIST | File exists | Implemented | OK |
| EINTR | Interrupted syscall | Not implemented | MISSING |
| EINVAL | Invalid argument | Implemented | OK |
| EIO | I/O error | Implemented | OK |
| EISDIR | Is a directory | Partial (not on write) | BROKEN |
| ELOOP | Symlink loop | Implemented (40 depth) | OK |
| EMFILE | Too many FDs | Implemented | OK |
| ENAMETOOLONG | Path too long | Not implemented | MISSING |
| ENOENT | No such file | Implemented | OK |
| ENOSPC | No space | Implemented | OK |
| ENOSYS | Not implemented | Implemented | OK |
| ENOTDIR | Not a directory | Partial | BROKEN |
| ENOTEMPTY | Dir not empty | Implemented | OK |
| EPERM | Not permitted | Implemented | OK |
| EPIPE | Broken pipe | Implemented (no signal) | PARTIAL |
| EROFS | Read-only FS | Not implemented | MISSING |
| ESRCH | No such process | Implemented | OK |
| EXDEV | Cross-device link | Implemented in mount_table | OK |
| EBUSY | Resource busy | Not implemented | MISSING |
| EWOULDBLOCK | Would block | Not implemented | MISSING |

---

## 2. Security & Sandboxing Gaps

### 2.1 CRITICAL: Node.js Builtin Fallthrough to Host

**Severity: CRITICAL**
**Location:** `crates/execution/src/node_import_cache.rs`

The ESM loader only explicitly handles ~15 Node.js builtins. All others fall through to `nextResolve()`, which returns the real host module. Critical uncovered builtins include:

- `node:crypto` -- Host cryptography, random sources
- `node:wasi` -- WebAssembly System Interface (host system access)
- `node:sqlite` -- Direct host database access
- `node:perf_hooks` -- Timing attacks, host uptime measurement
- `node:tty` -- Host terminal I/O
- `node:async_hooks` -- Internal state introspection
- `node:stream`, `node:buffer`, `node:zlib` -- No hardening

**Impact:** Guest code can `import crypto from 'node:crypto'` and get the REAL host module.

### 2.2 CRITICAL: Network Operations Bypass Permission System

**Severity: CRITICAL**
**Location:** `crates/sidecar/src/service.rs` lines 6027-6245

The kernel has `check_network_access()` in `permissions.rs` but it is NEVER called for socket/DNS operations in the sidecar RPC handlers. Guest code can:

- Connect to ANY host/port (including cloud metadata at 169.254.169.254)
- Bind to ANY interface including 0.0.0.0 (exposing to all VMs)
- Perform DNS lookups against host resolver
- Send UDP datagrams anywhere
- Bypass `fetch()` hardening via `http.request()`, `net.connect()`, etc.

### 2.3 CRITICAL: Control Channel Message Injection

**Severity: CRITICAL**
**Location:** `crates/execution/src/javascript.rs`, `crates/execution/src/node_process.rs`

Guest code can write magic-prefixed lines to stderr to:
- Inject fake warmup metrics (`__AGENT_OS_NODE_WARMUP_METRICS__:`)
- Inject fake exit codes (`__AGENT_OS_PYTHON_EXIT__:`)
- Inject fake signal state (`__AGENT_OS_SIGNAL_STATE__:`)
- Suppress arbitrary stderr output

```javascript
// Guest can write:
console.error('__AGENT_OS_PYTHON_EXIT__:{"exitCode":0}');
```

### 2.4 CRITICAL: WASM Memory Limits Not Enforced at Runtime

**Severity: CRITICAL**
**Location:** `crates/execution/src/wasm.rs` lines 840-843, 876-916

`WASM_MAX_MEMORY_BYTES_ENV` is only used for compile-time validation at module load. It is NOT passed to the Node.js runtime. Guest WASM code can grow memory beyond any configured limit at runtime, causing host OOM.

### 2.5 CRITICAL: WASI Unconditionally Enabled

**Severity: CRITICAL**
**Location:** `crates/execution/src/wasm.rs` line 612

`allow_wasi = true` is hardcoded for all WASM execution regardless of permission tier. Even "Isolated" tier gets WASI access.

### 2.6 HIGH: Unvalidated FD Access for RPC Channels

**Severity: HIGH**
**Location:** `crates/execution/src/javascript.rs` lines 725-730, 953-960

RPC channel FD numbers are passed via environment variables with `FD_CLOEXEC` explicitly cleared. Guest code can:
- Close RPC FDs to break sidecar communication
- Read/write to manipulate RPC messages
- Redirect them with dup2() to other FDs

### 2.7 HIGH: Unmount Has No Permission Check

**Severity: HIGH**
**Location:** `crates/kernel/src/kernel.rs` lines 1425-1432

`unmount_filesystem()` bypasses all permission checks. Guest can unmount any filesystem including `/`, `/etc`, `/proc`.

### 2.8 HIGH: Symlink Resolution Bypass in Permission System

**Severity: HIGH**
**Location:** `crates/kernel/src/permissions.rs` lines 484-491

`read_link()` and `lstat()` use `normalize_path()` instead of `check_subject()`, skipping symlink resolution before permission checks. Guest can create symlinks to forbidden paths and read targets.

### 2.9 HIGH: Host Information Leakage via Path Fallbacks

**Severity: HIGH**
**Location:** `crates/execution/src/node_import_cache.rs`

- `guestVisiblePathFromHostPath()` falls back to raw host path when mapping fails
- `INITIAL_GUEST_CWD` falls back to `HOST_CWD` if not in path mappings
- `os.homedir()`, `os.userInfo()`, `os.tmpdir()` fall back to host values
- `process.config`, `process.versions` expose host build info
- `AGENT_OS_*` variables passed through to child processes

### 2.10 HIGH: process Object Properties Leak Host Info

**Severity: HIGH**
**Location:** `crates/execution/src/node_import_cache.rs` lines 6176-6223

The guest process proxy only overrides 5 properties. All others pass through via `Reflect.get()`:
- `process.version` -- Host Node version
- `process.config` -- Complete host build configuration
- `process.versions` -- Host module versions (openssl, v8, zlib)
- `process.memoryUsage()` -- Host memory usage
- `process.uptime()` -- Host uptime

### 2.11 HIGH: CJS require() Loads from Host node_modules

**Severity: HIGH**
**Location:** `crates/execution/src/node_import_cache.rs` lines 6225-6271

`createGuestRequire()` uses `Module.createRequire()` + `baseRequire()` which resolves packages from HOST `node_modules`. Guest code can load arbitrary host packages.

### 2.12 HIGH: Default Permissions Are allow_all

**Severity: HIGH (footgun)**
**Location:** `crates/kernel/src/kernel.rs` line 101

`KernelVmConfig::new()` defaults to `Permissions::allow_all()` instead of deny-by-default. Any code creating a VM without explicit permissions gets unrestricted access.

---

## 3. Node.js / WASM Bridge Issues

### 3.1 Node.js Builtin Coverage

| Builtin | Has Polyfill? | Routes Through Kernel? | Security Status |
|---------|--------------|----------------------|----------------|
| `fs` / `fs/promises` | Yes (sync RPC) | Yes (VFS) | PARTIAL -- path-translating, not full polyfill |
| `child_process` | Yes (sync RPC) | Yes (process table) | PARTIAL -- wraps real spawn |
| `net` | Yes (sidecar RPC) | NO -- direct host sockets | BROKEN |
| `dgram` | Yes (sidecar RPC) | NO -- direct host sockets | BROKEN |
| `dns` | Yes (sidecar RPC) | NO -- direct host resolver | BROKEN |
| `http` / `https` | Yes (layered on net) | NO -- inherits net bypass | BROKEN |
| `http2` | Yes (layered on net) | NO -- inherits net bypass | BROKEN |
| `tls` | Yes (layered on net) | NO -- inherits net bypass | BROKEN |
| `os` | Yes (full polyfill) | Yes (virtualized values) | OK |
| `path` | Passthrough | N/A (pure computation) | OK |
| `url` | Passthrough | N/A (pure computation) | OK |
| `crypto` | NO | Falls through to host | CRITICAL |
| `wasi` | NO | Falls through to host | CRITICAL |
| `sqlite` | NO | Falls through to host | CRITICAL |
| `perf_hooks` | NO | Falls through to host | HIGH |
| `tty` | NO | Falls through to host | HIGH |
| `async_hooks` | NO | Falls through to host | MEDIUM |
| `stream` | NO | Falls through to host | MEDIUM |
| `buffer` | NO | Falls through to host | LOW |
| `zlib` | NO | Falls through to host | LOW |
| `vm` | Should be denied | Falls through if in ALLOWED | CRITICAL |
| `worker_threads` | Should be denied | Falls through if in ALLOWED | CRITICAL |
| `inspector` | Should be denied | Falls through if in ALLOWED | CRITICAL |
| `v8` | Should be denied | Falls through if in ALLOWED | CRITICAL |

### 3.2 WASM Execution Gaps

| Issue | Severity | Details |
|-------|----------|---------|
| Memory limits not runtime-enforced | CRITICAL | Only compile-time validation, no runtime cap |
| Fuel limits are coarse timeouts | CRITICAL | Fuel = millisecond timeout, not per-instruction |
| WASI always enabled | CRITICAL | Hardcoded `allow_wasi = true` regardless of tier |
| Module parser DoS | HIGH | Unbounded section iteration, no module size limit |
| Symlink TOCTOU in module path | HIGH | Different resolution at validation vs execution |
| Stack limit overflow | MEDIUM | No upper bound on `--stack-size` parameter |
| Prewarm phase no timeout | MEDIUM | `ensure_materialized()` can hang forever |
| File fingerprint TOCTOU | MEDIUM | size+mtime race for warmup cache |

### 3.3 Sync RPC Bridge Vulnerabilities

| Issue | Severity | Details |
|-------|----------|---------|
| No RPC authentication | MEDIUM | Simple integer IDs, no HMAC |
| Guest can forge RPC requests | MEDIUM | Write arbitrary JSON to request FD |
| Response writer can deadlock | MEDIUM | Guest slow-read causes sidecar hang |
| FD reservation race window | HIGH | Reservation dropped before clear_cloexec |

---

## 4. POSIX Edge Cases That Will Break

### 4.1 Things That Work on Linux But Break Here

| Scenario | What Linux Does | What Agent OS Does | Impact |
|----------|----------------|-------------------|--------|
| `git commit` | Atomic O_CREAT\|O_EXCL for refs | TOCTOU race, can corrupt refs | git broken |
| `npm install` | fcntl locking for package-lock | No locking, concurrent installs corrupt | npm broken |
| `python -c "import mmap"` | Memory maps files | No mmap, ImportError | Python broken |
| Concurrent log writes | O_APPEND atomic | Race condition, interleaved data | Data corruption |
| Shell job control (^Z, bg, fg) | SIGTSTP/SIGCONT | Not implemented | Shell broken |
| `make -j4` | fork() for parallel jobs | No fork, must use spawn | make broken |
| `#!/bin/sh` scripts | Kernel parses shebang | Not parsed | Scripts fail |
| Pipe write <= 4KB | Atomic (PIPE_BUF guarantee) | Not atomic, interleaved | IPC corruption |
| `select()` on multiple FDs | Multiplexed I/O | Not implemented | Event loops broken |
| Parent gets SIGCHLD | Signal on child exit | Not delivered | Cannot async-reap children |
| `flock /tmp/lockfile` | Advisory file lock | Not implemented | Lock files useless |
| Non-blocking I/O | O_NONBLOCK + EAGAIN | Not implemented | Async I/O broken |

### 4.2 Standard Tool Compatibility

| Tool | Will It Work? | Why Not |
|------|--------------|---------|
| git | NO | No atomic O_CREAT\|O_EXCL, no flock |
| npm/yarn/pnpm | NO | No fcntl locking |
| python | PARTIAL | No mmap, no fork, no fcntl |
| curl/wget | NO | Network bypasses kernel |
| tar | PARTIAL | Sparse files materialized, timestamps incomplete |
| grep | YES | Basic file I/O works |
| sed/awk | YES | Basic file I/O works |
| make | NO | No fork() for parallel jobs |
| docker | NO | No fork, no namespace, no cgroups |
| ssh | NO | Network bypasses kernel |
| vim/nano | PARTIAL | PTY works, but missing VMIN/VTIME |

---

## 5. Filesystem Deep Dive

### 5.1 Overlay FS Issues

| Issue | Severity | Details |
|-------|----------|---------|
| No opaque directory markers | HIGH | Lower layer entries leak through after copy-up |
| Whiteouts are in-memory only | HIGH | Lost on snapshot/persistence |
| No whiteout character devices | MEDIUM | Incompatible with standard OverlayFS tools |
| Copy-up TOCTOU race | MEDIUM | Symlink target can change between read and create |
| removeDir doesn't check lower children | HIGH | Can remove non-empty dir if children only in lower |
| Hardlink copy-up path resolution broken | HIGH | link() after copy-up references wrong path |
| Rename not atomic | HIGH | Read+write+delete pattern, crash-unsafe |

### 5.2 VFS Issues

| Issue | Severity | Details |
|-------|----------|---------|
| Hardlink across mounts not checked | HIGH | Should return EXDEV, currently allowed |
| Stat missing blocks/dev fields | LOW | Tools expecting `st_blocks` will get 0 |
| Time precision milliseconds only | LOW | Linux uses nanoseconds |
| No S_IFCHR/S_IFBLK/S_IFIFO/S_IFSOCK | MEDIUM | Missing file type bits in mode |
| /dev/zero returns fixed 4096 bytes | LOW | Should return requested length |
| /dev/urandom returns fixed 4096 bytes | LOW | Should return requested length |

### 5.3 Remote Filesystem / Mount Issues

| Issue | Severity | Details |
|-------|----------|---------|
| Mount permissions checked, unmount not | HIGH | Guest can unmount anything |
| TypeScript overlay has no resource limits | MEDIUM | Unlimited files/size in upper layer |
| Copy-up not counted against limits | MEDIUM | Large lower files can exhaust memory |
| S3 mount doesn't persist whiteouts | HIGH | Deleted files reappear |

---

## 6. Resource Limits & DoS Vectors

### 6.1 Properly Protected Resources

| Resource | Limit | Default | Status |
|----------|-------|---------|--------|
| Filesystem total size | max_filesystem_bytes | 64 MB | OK |
| Inode count | max_inode_count | 16,384 | OK |
| FDs per process | MAX_FDS_PER_PROCESS | 256 | OK |
| Pipe buffer | MAX_PIPE_BUFFER_BYTES | 65,536 | OK |
| PTY buffer | MAX_PTY_BUFFER_BYTES | 65,536 | OK |
| Symlink depth | MAX_SYMLINK_DEPTH | 40 | OK |
| Zombie TTL | ZOMBIE_TTL | 60s | OK |
| Python output buffer | max_bytes | 1 MB | OK |

### 6.2 Unbounded / Missing Limits

| Resource | Status | Attack |
|----------|--------|--------|
| pread() length | NO LIMIT | `pread(fd, 0, usize::MAX)` -- host OOM |
| fd_write() data size | NO PER-OP LIMIT | Single huge write can OOM before FS limit check |
| Environment variable size | NO LIMIT | Gigabyte env vars in spawn |
| Command argument size | NO LIMIT | Gigabyte argv lists |
| readdir result size | NO LIMIT | 16K entry directory returns all at once |
| Filesystem snapshot | NO LIMIT | Clones entire FS state to memory |
| File truncate | NO LIMIT | `truncate("/f", 1TB)` allocates and zeros 1TB |
| WASM runtime memory | NOT ENFORCED | Compile-time only, runtime unbounded |
| Socket count | FIELD EXISTS, NOT ENFORCED | No enforcement code found |
| Connection count | FIELD EXISTS, NOT ENFORCED | No enforcement code found |
| Network bandwidth | NOT IMPLEMENTED | Guest can flood network |
| Process spawn as zombies | ONLY RUNNING COUNTED | Create+exit loop bypasses max_processes |
| Path length | NOT CHECKED | Unbounded path strings |
| Symlink target length | NOT CHECKED | Huge symlink targets |
| Single file size | ONLY TOTAL FS CHECKED | One file can be entire 64MB |

---

## 7. Python/Pyodide Assessment (Best Secured)

The Pyodide engine is the most well-secured subsystem:

- Runs Python in WASM (not native), providing architectural isolation
- VFS RPC properly scoped to `/workspace` with path validation
- `js` and `pyodide_js` modules blocked (prevents WASM-JS interop escape)
- `os.system()` and `subprocess.*` monkey-patched to route through kernel
- `process.binding()` and `process.dlopen()` throw access denied
- `fetch()` restricted to `data:` URLs only
- Output buffers properly bounded (1MB default)
- ctypes neutered by WASM architecture (no native library loading)

**Remaining concerns:**
- No memory limit on Pyodide process
- No execution timeout at Python level
- Recursion depth only limited by Python's default ~1000 frames

---

## 8. Control Channel Security Summary

| Channel | Mechanism | In-Band? | Guest Can Forge? |
|---------|-----------|----------|-----------------|
| Node.js warmup metrics | stderr prefix `__AGENT_OS_NODE_WARMUP_METRICS__:` | YES | YES |
| Python exit code | stderr prefix `__AGENT_OS_PYTHON_EXIT__:` | YES | YES |
| WASM warmup metrics | stderr prefix `__AGENT_OS_WASM_WARMUP_METRICS__:` | YES | YES |
| Signal state | stderr prefix `__AGENT_OS_SIGNAL_STATE__:` | YES | YES |
| Node sync RPC | Dedicated FD pipes | No | YES (FD accessible) |
| Python VFS RPC | Dedicated FD pipes | No | YES (FD accessible) |
| Node control channel | Dedicated FD pipe | No | YES (FD accessible) |
| Sidecar stdio protocol | stdin/stdout framed | Parent-controlled | No (proper isolation) |

---

## 9. Priority Remediation Recommendations

### P0 -- Immediate (Security-Critical)

1. **Block all uncovered Node.js builtins** -- Every builtin not in BUILTIN_ASSETS must be in DENIED_BUILTINS. No fallthrough to `nextResolve()`.
2. **Add permission checks to network operations** -- All socket connect/bind/DNS operations must call `check_network_access()`.
3. **Move control messages out-of-band** -- Use dedicated FDs for all control signaling instead of stderr magic prefixes.
4. **Enforce WASM memory limits at runtime** -- Pass `WASM_MAX_MEMORY_BYTES_ENV` to Node.js runtime, not just compile-time validation.
5. **Make WASI conditional** -- Disable WASI for Isolated permission tier.
6. **Add permission check to unmount** -- `unmount_filesystem()` must check permissions.
7. **Fix symlink bypass in read_link/lstat** -- Use `check_subject()` not `check()`.

### P1 -- High Priority (Correctness/Isolation)

8. **Implement O_CREAT|O_EXCL atomicity** -- Single atomic create-if-not-exists operation.
9. **Implement O_APPEND atomicity** -- Atomic seek-to-end + write.
10. **Bound pread/fd_write per-operation size** -- Add max_read_length, max_write_length limits.
11. **Fix host info leakage** -- Never fall back to host paths; use safe defaults.
12. **Proxy all process properties** -- Block `process.config`, `process.versions`, `process.memoryUsage()`.
13. **Filter AGENT_OS_* from child processes** -- Strip internal vars before spawn.
14. **Fix overlay whiteout persistence** -- Store in durable layer, not in-memory Set.
15. **Add opaque directory support** -- Implement OverlayFS opaque markers.
16. **Fix hardlink across mounts** -- Return EXDEV.
17. **Default permissions to deny-all** -- Change `KernelVmConfig::new()` default.

### P2 -- Medium Priority (POSIX Compliance)

18. **Implement SIGCHLD** -- Deliver to parent on child exit.
19. **Implement SIGPIPE** -- Deliver on write to broken pipe.
20. **Implement waitpid flags** -- WNOHANG, WUNTRACED, WCONTINUED, negative PID.
21. **Implement file locking** -- At least advisory flock().
22. **Implement select/poll** -- FD multiplexing for event loops.
23. **Implement O_NONBLOCK** -- Non-blocking I/O with EAGAIN.
24. **Implement PIPE_BUF atomicity** -- Writes <= 4096 bytes must be atomic.
25. **Count zombies against process limits** -- Prevent zombie storms.
26. **Implement reparenting** -- Orphaned children go to init.
27. **Implement /proc filesystem** -- At least /proc/self, /proc/[pid]/fd, /proc/mounts.
28. **Fix /dev/zero and /dev/urandom** -- Return requested byte count, not fixed 4096.

### P3 -- Low Priority (Polish)

29. Implement shebang parsing for exec()
30. Add EISDIR for write-to-directory
31. Add ENOTDIR for path component checks
32. Add ENAMETOOLONG
33. Implement umask
34. Implement sticky bit enforcement
35. Add stat blocks/dev fields
36. Implement /dev/full, /dev/tty
37. Add nanosecond time precision
38. Implement SIGWINCH for PTY resize

---

## 10. Subsystem Security Scorecard

| Subsystem | Score | Assessment |
|-----------|-------|-----------|
| Python/Pyodide | A- | Strong WASM boundary, proper import blocking, VFS scoping |
| Permission System | C+ | Good design, but bypasses in read_link, lstat, unmount, network |
| Process Table | C | Basic functionality, missing signals/fork/reparenting |
| VFS Core | C+ | Correct for basic ops, missing atomicity guarantees |
| Overlay FS | C- | Missing opaque dirs, in-memory whiteouts, broken hardlink copy-up |
| Sidecar RPC | B- | Good auth/ownership checks, but info leaks and missing timeouts |
| WASM Engine | D+ | Limits not enforced at runtime, WASI always on |
| Node.js Isolation | D | Many builtins fall through, host info leaks everywhere |
| Network Stack | F | Zero permission enforcement, no address validation, full SSRF |
| Control Channels | D | All in-band via stderr, guest can forge messages |
| Resource Limits | C- | Some limits exist but many unbounded vectors |
| Host Info Protection | D+ | Good intent, but fallback-to-host pattern leaks everywhere |
