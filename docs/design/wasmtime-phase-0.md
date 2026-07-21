# Wasmtime Phase 0: ABI Inventory, Baseline, and Locked Decisions

Status: complete; normative input to the runtime-neutral refactor and Wasmtime
executor revisions

Audience: AgentOS kernel, sidecar runtime, execution, VFS, toolchain, test, and
registry-software owners

## 1. Purpose and completion statement

This document closes Phase 0 of the Wasmtime executor project. It records:

- every import exposed by the current standalone V8-WASM runner;
- the semantic owner, authority, bounds, waiting behavior, guest-memory
  direction, shared operation, and parity proof for each import;
- compatibility aliases, hard stubs, and unsupported imports;
- the current V8-WASM cold/warm latency and memory baseline; and
- the implementation decisions that must not be reopened implicitly during the
  runtime-neutral refactor.

The inventory was checked against:

- `crates/execution/assets/agentos-wasm-abi.json`;
- `crates/execution/assets/runners/wasm-runner.mjs`;
- `crates/execution/assets/runners/wasi-module.js`;
- `toolchain/crates/wasi-ext/src/lib.rs`;
- `toolchain/std-patches/` and `toolchain/std-patches/wasi-libc-overrides/`;
- kernel process, VFS, fd, socket, PTY, identity, and resource-accounting APIs;
  and
- native-sidecar execution, filesystem, and network services.

No import below permits Wasmtime to use ambient host resources. `wasmtime-wasi`
does not own the context. Both engines call the same AgentOS host services.

## 2. Inventory notation and cross-cutting contract

The tables use these abbreviations:

- **C**: canonical current ABI; implement for both engines.
- **A**: active compatibility alias/version; keep the linker function but map it
  to the canonical operation.
- **S**: current hard stub; Phase 1 either implements it in the shared owner or
  removes it only after a rebuilt-artifact import audit proves it unused.
- **U**: intentionally unsupported; validation/linking fails deterministically.
- **all/full/limited**: permission tier availability. Authority is checked again
  in the shared operation even when an import is omitted at link time.
- **sync**: bounded non-waiting semantic operation.
- **async**: readiness, timer, network, lock, FIFO, PTY, child, or cancellation
  wait. The V8 adapter may expose it through its existing synchronous bridge;
  the shared operation itself is asynchronous.
- **in/out**: bytes copied from/to guest memory. All ranges and aggregate sizes
  are validated before a side effect. No guest-memory borrow crosses an await.

Common default limits are `maxOpenFds=1024`, `maxPipes=128`, `maxPtys=128`,
`maxSockets=256`, `maxConnections=256`, `maxSocketBufferedBytes=4 MiB`,
`maxSocketDatagramQueueLen=1024`, `maxPreadBytes=64 MiB`,
`maxFdWriteBytes=64 MiB`, `maxProcessArgvBytes=1 MiB`,
`maxProcessEnvBytes=1 MiB`, `maxReaddirEntries=4096`, a 4096-byte path, 40
symlink traversals, 64 supplementary groups, 255-byte xattr names, and 64 KiB
xattr values. Every adapter additionally caps one decoded iovec, pollfd, or
poll-subscription array at the Linux-aligned 1024 entries and its encoded
descriptor bytes at 1 MiB before copying. Spawn file actions have their own
4096-entry and 1 MiB encoded-byte caps; SCM_RIGHTS carries at most 253
descriptors. These adapter caps become named configurable limits if real
software reaches them; they never silently become unbounded.

Multi-output imports prevalidate every output range before committing a side
effect. Operations that allocate guest fds reserve fd capacity before the host
operation and roll back completely if result encoding fails. Errors remain
typed errno/status values across the shared boundary.

## 3. Preview1 inventory

The runner exposes the same object as `wasi_snapshot_preview1` and
`wasi_unstable`; the latter is an ABI alias, not another implementation.

| Imports | Status; mode; memory | Shared semantic owner and operation | Authority, bounds, and parity proof |
| --- | --- | --- | --- |
| `args_sizes_get`, `args_get` | C; sync; out | `GuestProcessHost::image(pid).argv` from the committed kernel process image | 1 MiB argv cap; prevalidate pointer table and strings; direct-WAT exact bytes/order/OOB plus spawn/exec corpus |
| `environ_sizes_get`, `environ_get` | C; sync; out | `GuestProcessHost::image(pid).env`, after the one sidecar-owned guest filtering/default pass | 1 MiB env cap; deterministic ordering; internal-key exclusion and exec replacement tests |
| `clock_time_get`, `clock_res_get` | C; sync; out | `GuestClockHost`; frozen execution realtime plus monotonic | One u64 output; exact realtime/resolution, monotonicity, invalid-id and OOB tests. Process/thread CPU clock IDs retain the current stable `ENOTSUP` behavior; implementing per-executor CPU clocks is beyond the current V8-WASM parity target. |
| `random_get` | C; sync/chunked; out | `GuestEntropyHost`, same provider as kernel `/dev/urandom` | Validate full guest range first; fill in at most 64 KiB host chunks with a 16 MiB per-call cap; zero/OOB/limit/provider-failure tests |
| `fd_close` | C; sync; none | `GuestFdHost::close` on the kernel fd table | fd ownership; reuse/refcount/EBADF parity |
| `fd_datasync`, `fd_sync` | C; sync; none | `GuestFdHost::sync(DataOnly/All)` | writable/open fd; synchronous-VFS commit and exact errno tests |
| `fd_fdstat_get` | C; sync; out | `GuestFdHost::status -> FdStatus`, then Preview1 encoding | Kernel-derived type/flags/rights; exact byte-layout tests |
| `fd_fdstat_set_flags` | C; sync; scalar in | `GuestFdHost::set_status_flags` | append/nonblock only; shared-open-description parity |
| `fd_filestat_get` | C; sync; out | `GuestMetadataHost::stat_fd` | Exact dev/ino/type/nlink/size/time encoding; open-unlinked fd test |
| `fd_filestat_set_size` | C; sync; scalar in | `GuestFdHost::set_len` | write right, read-only mount, filesystem quota; position and rollback tests |
| `fd_filestat_set_times` | C compatibility export; sync; scalar in | `GuestMetadataHost::set_times_fd` | NOW/OMIT validation; open-unlinked fd and DAC tests |
| `fd_pread` | C; sync; in iovecs/out bytes+count | `GuestFdHost::read_at` | 1024 iovecs/1 MiB descriptors/64 MiB payload; offset unchanged and malformed-iovec tests |
| `fd_pwrite` | C; sync; in iovecs/out count | `GuestFdHost::write_at` | Pre-copy 64 MiB cap, quota and read-only checks; vector ordering/rollback tests |
| `fd_read` | C; async for pipe/PTY/socket/FIFO; in iovecs/out bytes+count | `GuestFdHost::read` | Same iovec caps, configured blocking deadline, signal/cancel; EOF/EAGAIN/EINTR/backpressure tests |
| `fd_write` | C; async for backpressured pipe/socket/PTY; in iovecs/out count | `GuestFdHost::write` or sidecar `write_stdio` for host-visible stdio | Pre-copy 64 MiB cap; partial write, EPIPE/SIGPIPE, ordering and backpressure tests |
| `fd_readdir` | C; sync; out dirents+count | `GuestFdHost::read_dir` | 4096 entries/call and caller buffer; cookie/partial-record/Linux dot ordering tests |
| `fd_seek`, `fd_tell` | C; sync; out offset | `GuestFdHost::seek/tell` | Checked signed offsets; ESPIPE/overflow/positioned-I/O tests |
| `fd_prestat_get`, `fd_prestat_dir_name` | C; sync; out | Adapter reads immutable process preopen capability descriptors | Hidden capabilities never enter guest fd namespace; list/name/short-buffer tests |
| `fd_allocate` | C compatibility export; sync; scalar in | `GuestExtentHost::allocate` | write right and filesystem quota; sparse/overflow/rollback tests |
| `fd_renumber` | C compatibility export; sync; none | `GuestFdHost::renumber` | fd and rlimit bounds; source consumed, target closed atomically |
| `sock_shutdown` | C compatibility export; sync command; none | `GuestNetworkHost::shutdown(fd, how)` for canonical socket descriptions | Validate direction; socketpair half-close/EOF, bad fd, unsupported direction and host-net parity tests |
| `path_open` | C; async only for blocking FIFO rendezvous; path in/fd out | `GuestPathHost::open_at` | directory-fd rights, DAC, symlink, permission tier, read-only mount, fd/quota limits; openat/FIFO/escape tests |
| `path_create_directory` | C; sync; path in | `GuestPathHost::mkdir_at` | parent DAC, umask, read-only/quota; mkdirat parity |
| `path_filestat_get` | C; sync; path in/stat out | `GuestMetadataHost::stat_at` | traversal/follow rights; symlink and exact metadata tests |
| `path_filestat_set_times` | C compatibility export; sync; path in | `GuestMetadataHost::set_times_at` | DAC/read-only/NOW/OMIT/follow tests |
| `path_link` | C; sync; two paths in | `GuestPathHost::link_at` using process-aware kernel checks | Phase 1 fixes current generic-kernel DAC bypass; hardlink/sticky/read-only tests |
| `path_readlink` | C; sync; path in/target+count out | `GuestPathHost::readlink_at` | traversal/output bound; truncation/proc-fd/OOB tests |
| `path_remove_directory` | C; sync; path in | `GuestPathHost::remove_dir_at` | Phase 1 replaces current generic `remove_dir`; DAC/sticky/read-only tests |
| `path_rename` | C; sync; two paths in | `GuestPathHost::rename_at` | Phase 1 replaces current generic `rename`; atomic/DAC/sticky/cross-mount tests |
| `path_symlink` | C; sync; target/path in | `GuestPathHost::symlink_at` | Phase 1 replaces current generic `symlink`; parent DAC/read-only/dangling tests |
| `path_unlink_file` | C; sync; path in | `GuestPathHost::unlink_at` | Phase 1 replaces current generic `remove_file`; sticky/open-description tests |
| `poll_oneoff` | C; async; subscriptions in/events+count out | `GuestProcessHost::poll` over kernel readiness, shared clock, signal and cancel broker | 1024 subscriptions/1 MiB descriptors; fd+clock, HUP, timeout, signal race and lost-wakeup tests |
| `proc_exit` | C; terminal control flow; none | `GuestProcessHost::exit(status) -> !`; sidecar finalizes kernel process once | Normal exit distinct from trap/signal; 0/42/137, output-drain and child-wait tests |
| `sched_yield` | C; async yield/checkpoint; none | VM executor yield plus cancellation/signal checkpoint | Fairness, STOP/terminate observation and no-busy-spin tests |
| `fd_advise`, `fd_fdstat_set_rights` | U | No current runner function | Direct import must fail with stable unsupported-import validation error |

## 4. `host_process` inventory

Full permission links the whole module. Read-only/read-write link only
`fd_dup_min`, `fd_flock`, `fd_getfd`, `fd_setfd`, `fd_record_lock`,
`proc_getrlimit`, `proc_setrlimit`, `proc_umask`, and `umask`. Isolated links no
`host_process`. Kernel authority remains mandatory in every case.

| Imports | Status; mode; memory | Canonical operation | Bounds, required correction, and parity proof |
| --- | --- | --- | --- |
| `proc_spawn`, `proc_spawn_v2`, `proc_spawn_v3`, `proc_spawn_v4` | A/A/A/C; sync setup with async lifecycle; in/out | Decode all versions to one `GuestProcessHost::spawn(SpawnRequest)` | 256 processes, 1 MiB argv/env, 4096 actions/1 MiB action bytes, fd limits; legacy artifact plus full posix_spawn actions/masks/groups tests |
| `proc_exec`, `proc_fexec` | C; terminal control flow; in | `GuestProcessHost::prepare_exec -> ExecPlan`; validate/compile before atomic kernel commit, then replace Store outside import | 256 MiB module, argv/env/fd limits; failed-exec atomicity, CLOEXEC, fd-offset and deleted-fd tests |
| `proc_waitpid`, `proc_waitpid_v2`, `proc_waitpid_v3` | A/A/C; async; out | `GuestProcessHost::wait(selector, flags) -> WaitEvent`, with three encoders | Child-count bound; exit-vs-signal/core/stopped/continued/selectors/EINTR tests |
| `proc_kill` | C; sync; none | `GuestSignalHost::send` through kernel signal broker | Raw owned ABI and kernel accept signals 0..64 with standard pending coalescing; the current libc deliberately exposes `_NSIG=32` and no realtime-signal API, which remains the parity target until a separate sysroot expansion; self/child/group/permission/default/caught and raw 64/65 boundary tests |
| `proc_getpid`, `proc_getppid` | C; sync; out | Kernel process identity accessors | Spawn/exec/reparent stability tests; no environment-derived PID state |
| `proc_getrlimit`, `proc_setrlimit` | C; sync; out/none | Kernel-owned `GuestProcessHost::get/set_rlimit` | Initial `RLIMIT_NOFILE`; lowering/inheritance/EMFILE/EPERM tests; no runner shadow |
| `proc_umask`, `umask` | C/A; sync; out | Kernel `GuestProcessHost::set/query_umask` | 0777 mask; create/mkdir/spawn/exec/query tests |
| `proc_itimer_real` | C; async delivery; out | Shared timer service plus `GuestSignalHost` SIGALRM path | One bounded timer/process; arm/disarm/interval/blocked/coalesced/exec tests |
| `proc_getpgid`, `proc_setpgid` | C; sync; out/none | Kernel process-group operations | Session/leader/child/cross-group/ESRCH tests |
| `fd_pipe` | C; sync allocation, async I/O; out | Kernel `GuestFdHost::pipe` | Pipe/fd limits; EOF/refcounts/EPIPE/SIGPIPE/nonblock/inheritance tests |
| `fd_dup`, `fd_dup2`, `fd_dup_min` | C; sync; out/none | Kernel `GuestFdHost::dup/dup_to/dup_min` | One fd namespace and RLIMIT; shared offset, target replacement, CLOEXEC and EMFILE tests |
| `fd_getfd`, `fd_setfd` | C; sync; out/none | Kernel descriptor flags | Supported bits and exact exec-close tests |
| `fd_flock`, `fd_record_lock` | C; async when waiting; out for GETLK | Kernel lock manager with durable waiter and cancellation guard | Lock/waiter tables bounded from fd limit and blocking deadline; contention/EINTR/deadlock/cleanup tests |
| `proc_closefrom` | C; sync; none | Kernel fd table `closefrom`; private preopens are not guest fds | Sparse/high fds, stdio, resource/lock release tests |
| `fd_socketpair` | C with Phase 1 ABI fix; sync; out | Kernel `GuestProcessHost::socketpair(kind, flags)` | Current Rust wrapper mistakenly treats `(kind, nonblock, cloexec)` as `(domain,type,protocol)`; fix and test stream/datagram/seqpacket/flags/limits |
| `fd_sendmsg_rights`, `fd_recvmsg_rights` | C; send sync/async-capable, recv async; in/out | Kernel Unix-socket SCM_RIGHTS operations on canonical fd descriptions | 253 rights and 64 MiB payload caps; atomic EBADF/EMFILE rollback, PEEK/WAITALL/CLOEXEC/EINTR tests |
| `sleep_ms` | A; async; none | Shared clock/timer wait raced with signal/cancel | Deadline bound; zero/duration/EINTR/restart/frozen-realtime tests |
| `pty_open` | S, but active library surface; sync allocation; out | Phase 1 wires existing kernel/sidecar `open_pty` | 128 PTYs and fd limits; master/slave/isatty/spawn/resize/SIGWINCH tests |
| `proc_sigaction` | C; sync; none | Kernel-owned dispositions/masks/flags; guest handler pointer remains adapter state | KILL/STOP rejection, IGN/DFL/user, NODEFER/RESETHAND/RESTART/exec tests |
| `proc_signal_mask_v2` | C; sync; out | Kernel signal broker `update_mask` | KILL/STOP filtering, pending-on-unblock and future per-thread tests |
| `proc_ppoll_v1` | C; async; in/out | Atomic temporary-mask registration plus shared poll/signal/timer broker | Same 1024-entry/1 MiB poll caps; signal/readiness ordering, restore and lost-wakeup tests |

## 5. `host_net` inventory

`host_net` is linked only for full permission. The current runner maintains a
second host-net fd map; Phase 1 removes it and places network descriptions in
the canonical kernel fd/resource namespace. The sidecar's one Tokio runtime
continues to own external DNS and socket I/O.

| Imports | Status; mode; memory | Canonical operation | Authority, bounds, and parity proof |
| --- | --- | --- | --- |
| `net_socket` | C; sync allocation; out fd | `GuestNetworkHost::socket` | Network policy, socket/fd limits; domain/type/protocol/permission tests |
| `net_set_nonblock` | C; sync; none | `GuestFdHost::set_status_flags` | Canonical open description; dup/fcntl/nonblock tests |
| `net_connect` | C; async; address in | `GuestNetworkHost::connect` on sidecar reactor | Network policy, connection/reactor/deadline limits; numeric/DNS/Unix/EINPROGRESS/cancel tests |
| `net_getaddrinfo` | C; async; name/service in, addresses+length out | `GuestNetworkHost::resolve_addresses` | Network/DNS policy, 4096-byte name/service and 256-result/64 KiB encoded-result caps; family/order/error tests |
| `net_dns_query_rr_v1` | C; async; query in/records out | `GuestNetworkHost::query_dns` | Same input/result bounds; A/AAAA/PTR/SSHFP, denied target, truncation and timeout tests |
| `net_bind` | C; async-capable sidecar operation; address in | `GuestNetworkHost::bind` | Listen policy, address/path ownership, reactor deadline; TCP/UDP/Unix/abstract/DAC tests |
| `net_listen` | C; sync command; none | `GuestNetworkHost::listen` | Backlog clamped to bounded accept capacity; state/error tests |
| `net_accept` | C; async; fd/address out | `GuestNetworkHost::accept` | Reserve connection+fd before wait; accept quantum/backlog/deadline; blocking/nonblock/cancel/rollback tests |
| `net_validate_socket`, `net_validate_accept` | A preflight helpers; sync; none | Fold into transactional socket/accept validation; keep aliases for existing libc | No state consumption; bad fd/type/state tests |
| `net_getsockname`, `net_getpeername` | C; sync snapshot; address out | `GuestNetworkHost::local/peer_address` | Caller capacity and 64 KiB response cap; IPv4/IPv6/Unix/truncation tests |
| `net_send` | C; async on backpressure; payload in/count out | `GuestNetworkHost::send` | Pre-copy at most 64 MiB and reactor byte quantum; partial/nonblock/EPIPE/cancel tests |
| `net_recv` | C; async; capacity in/data+count out | `GuestNetworkHost::recv` | Read at most min(caller, 64 KiB quantum) per completion; EOF/PEEK/WAITALL/nonblock tests |
| `net_sendto` | C; async; payload+address in/count out | `GuestNetworkHost::send_to` | UDP max 64 KiB, policy and datagram quotas; oversize/atomic-address tests |
| `net_recvfrom` | C; async; capacities in/data+address out | `GuestNetworkHost::recv_from` | UDP 64 KiB, queue/buffer limits; truncation/source/nonblock/cancel tests |
| `net_setsockopt` | C; sync command; option bytes in | `GuestNetworkHost::set_option` | Option payload capped at 64 KiB; exact supported level/name/type and timeout tests |
| `net_getsockopt` | C; sync snapshot; option bytes out | `GuestNetworkHost::get_option` | Caller/64 KiB cap; error/length/value parity |
| `net_poll` | C; async; pollfds in/out+ready count | Shared readiness broker, not a network-private loop | 1024 fds/1 MiB descriptors, bounded deadline; mixed fd, duplicate, signal/cancel/HUP tests |
| `net_close` | A explicit close; async-capable teardown; none | `GuestFdHost::close` | Idempotence is not promised; resource-release and waiter-cancel tests |
| `net_tls_connect` | C; async handshake; hostname in | `GuestNetworkHost::upgrade_tls` | TLS buffer/reactor/deadline limits, 4096-byte hostname, permission/cert/SNI/cancel tests |

## 6. `host_user`, `host_tty`, and remaining system services

Both modules are linked at every permission tier. The kernel process identity
and live fd/PTY state remain authoritative.

| Imports | Status; mode; memory | Canonical operation | Bounds, correction, and parity proof |
| --- | --- | --- | --- |
| `getuid`, `getgid`, `geteuid`, `getegid` | C; sync; out u32 | `GuestIdentityHost::identity` | Configured root/nonroot, child/exec, permission-tier and OOB tests |
| `getresuid`, `getresgid` | C; sync; three u32 out | Same typed identity snapshot | Prevalidate all outputs; real/effective/saved transition tests |
| `setuid`, `seteuid`, `setreuid`, `setresuid`, `setgid`, `setegid`, `setregid`, `setresgid` | C; sync; scalar in | Kernel Linux credential-transition operations | `u32::MAX` means unchanged where applicable; root/drop/restore/EPERM/inheritance tests |
| `getgroups` | C; sync; groups+count out | `GuestIdentityHost::supplementary_groups` | Maximum 64; sizing/exact/short/OOB/order tests |
| `setgroups` | C; sync; groups in | Kernel group mutation | Phase 1 rejects count >64 before reading; root/EPERM/dedup/65-entry tests |
| `getpwuid`, `getpwnam`, `getpwent` | C; sync; optional name in/record+length out | `GuestIdentityHost` over kernel `UserManager` | 4096-byte record/name and 256 enumeration cap; Phase 1 returns `ERANGE` rather than silent truncation; known/unknown/iteration/OOB tests |
| `getgrgid`, `getgrnam`, `getgrent` | C; sync; optional name in/record+length out | Same kernel account database | Same bounds; member/enumeration/ERANGE tests |
| `host_user.isatty` | A duplicate ABI; sync; bool out | `GuestTerminalHost::is_terminal(fd)` live kernel lookup | Remove fd<=2/cache restriction; duplicated/replaced/closed/pipe/master/slave tests |
| `host_tty.read` | A active crossterm ABI; async; bytes out | `GuestFdHost::read(fd=0, deadline)` | 64 KiB; legacy zero conflates EOF/timeout; byte/EOF/timeout/signal/OOB tests |
| `host_tty.isatty` | A; sync; no memory | Same live terminal lookup | Same fd identity tests; no runner cache |
| `host_tty.get_size` | A; sync; two u16 out | `GuestTerminalHost::window_size` | Prevalidate both outputs; resize/ENOTTY/OOB tests |
| `host_tty.set_size` | C; sync; scalar in | `GuestTerminalHost::set_window_size` | Validate u16 columns/rows; SIGWINCH, permission, ENOTTY and resize-observation tests |
| `host_tty.get_attr` | C; sync; flags plus seven control bytes out | `GuestTerminalHost::get_attributes` | Prevalidate both output ranges; live termios, dup/replacement, ENOTTY and OOB tests |
| `host_tty.set_attr` | C; sync; flags plus seven control bytes in | `GuestTerminalHost::set_attributes` | Snapshot bounded input before mutation; live termios, inheritance, ENOTTY and OOB tests |
| `host_tty.get_pgrp` | C; sync; pgid out | `GuestTerminalHost::foreground_process_group` | Prevalidate output; session/foreground-group, dup/replacement, ENOTTY and OOB tests |
| `host_tty.set_pgrp` | C; sync; pgid in | `GuestTerminalHost::set_foreground_process_group` | Kernel session/group authority; permission, orphan/session, ENOTTY and signal-routing tests |
| `host_tty.get_sid` | C; sync; sid out | `GuestTerminalHost::session` | Prevalidate output; controlling-terminal/session, dup/replacement, ENOTTY and OOB tests |
| `host_tty.set_raw_mode` | A; sync; none | Sidecar `GuestTerminalHost::set_raw_mode`, including generation lease bookkeeping | Enable/disable/nesting/exit restore/background/ENOTTY tests |
| `host_system.get_identity` | C; sync; field selector in/string+required length out | Kernel `SystemIdentity` snapshot (`sysname`, node name, release, version, machine, domain name) | 4096-byte result cap; exact Linux identity, short-buffer `ERANGE`, invalid selector, and OOB tests |

Phase 1 also replaces libc's process-global shadow termios, fixed foreground
process group, no-op `tcsetpgrp`, and missing resize path with live
`GuestTerminalHost` operations. Static libc passwd/group enumeration state is
acceptable only for the initial single-threaded ABI and is a Phase 4 threading
blocker.

## 7. `host_fs` inventory

`host_fs` is linked at every permission tier, so its shared implementation must
enforce all fd rights, process DAC, mount read-only policy, permission tiers,
and quotas. No Wasmtime linker availability check is treated as sufficient.

| Imports | Status; mode; memory | Canonical operation | Bounds, required correction, and parity proof |
| --- | --- | --- | --- |
| `open_tmpfile`, `fd_link` | C; sync; path in/fd out or path in | `GuestPathHost::open_tmpfile_at/link_tmpfile_at` | Path/fd/quota/DAC; O_EXCL, linkability, closed-source and rollback tests |
| `remount` | C; sync; path/options in | Sidecar mount host over kernel process check | euid 0, mount permission, 4096-byte path and 64 KiB options; nonroot/oversize tests |
| `path_mknod` | C; sync; path/scalars in | `GuestPathHost::mknod_at` | DAC/read-only/quota/type/rdev/umask tests |
| `path_renameat2` | C; sync; two paths in | `GuestPathHost::rename_at2` | Supported Linux flags, DAC/read-only/cross-mount tests |
| `path_statfs` | C; sync; path in/five u64 out | `GuestMetadataHost::statfs_at` | Prevalidate all outputs; authoritative quota/space tests |
| `fd_fiemap` | C; sync; three outputs | `GuestExtentHost::get(fd,index)` returns one bounded extent | Do not materialize all extents; sparse/unwritten/many-extent/index tests |
| `fd_punch_hole`, `fd_zero_range`, `fd_insert_range`, `fd_collapse_range` | C; sync; scalar in | `GuestExtentHost` range operations | Write right/read-only/quota/offset/alignment/overflow/rollback tests |
| `set_open_mode`, `set_open_direct` | A but architecturally obsolete; sync; scalar in | Phase 1 folds values into one atomic `OpenOptions`; legacy adapters use a generation-bound one-shot token | No process-global latch; nested/reentrant open tests now and interleaved-thread tests in Phase 4 |
| `path_owner`, `path_mode`, `path_size`, `path_blocks`, `path_rdev` | C; sync; path in/scalar outputs | One `GuestMetadataHost::stat_at` | Phase 1 removes ambient Node stat and sentinel errors; DAC/no-host-leak/symlink/sparse/device tests |
| `fd_owner`, `fd_mode`, `fd_size`, `fd_blocks` | C; sync; scalar outputs | One `GuestMetadataHost::stat_fd` | Phase 1 removes ambient fallbacks/caches/sentinels; open-unlinked/pipe/socket/truncate tests |
| `path_access` | C; sync; path in | `GuestPathHost::access_at(real_or_effective_ids)` | DAC/root/symlink/read-only tests |
| `path_chown`, `fd_chown` | A legacy names | Alias `GuestMetadataHost::chown_at/chown_fd`; remove only after rebuilt-artifact audit | Ownership/-1/symlink/open-unlinked tests |
| `chown`, `fchown` | C current names; sync; path or scalar in | Same process-aware metadata operations | Exact EPERM/DAC/ownership tests |
| `chmod`, `fchmod` | C; sync; path/scalar in | `GuestMetadataHost::chmod_at/chmod_fd` | Phase 1 removes mapped-host/ambient fallbacks; DAC/read-only/open-unlinked tests |
| `path_getxattr`, `path_listxattr`, `path_setxattr`, `path_removexattr` | C; sync; path/name/value in and value/list/size out | `GuestXattrHost::*_at` | 255-byte name/64 KiB value+list, DAC/read-only/quota; ERANGE/flags/namespace tests |
| `fd_getxattr`, `fd_listxattr`, `fd_setxattr`, `fd_removexattr` | C; sync; name/value in and value/list/size out | Real `GuestXattrHost::*_fd` operations | Phase 1 stops converting fd to a path; open-then-unlink/rename tests |
| `ftruncate` | A compatibility name; sync; scalar in | Alias `GuestFdHost::set_len` | Phase 1 removes ambient fallback and incorrect errno `1`; negative/overflow/quota tests |

## 8. Correctness fixes admitted into the prerequisite revision

The runtime-neutral revision intentionally changes current V8 behavior where
the inventory found a Linux/security defect:

1. canonical link, remove, rename, symlink, and unlink use process-aware kernel
   DAC/sticky/read-only checks;
2. all writes and decoded arrays are bounded before allocating or copying;
3. account lookup returns `ERANGE` and required length instead of successful
   truncation;
4. supplementary groups are capped before guest memory is read;
5. `socketpair` uses the actual `(kind, nonblock, cloexec)` ABI;
6. fd xattrs operate on the open file description, including after unlink;
7. terminal identity is a live fd lookup, not an fd<=2 cache;
8. metadata never falls through to ambient Node filesystem access or sentinel
   error values; and
9. `pty_open` calls the existing bounded kernel implementation instead of
   returning `FAULT`.

These fixes run through the V8-WASM adapter before Wasmtime work starts.

## 9. Required differential proof suites

| Suite | Required coverage |
| --- | --- |
| Raw ABI | One direct-WAT fixture per import row or grouped signature family; invalid pointers, overflow, undersized outputs, unsupported import, tier omission |
| Owned sysroot | Rebuild the complete default command set and inspect every module import; no undeclared import and no unexplained legacy version |
| Software | `ls`, `vim`, `grep`, `curl`, shell pipelines, sqlite, git, tar/gzip, xfs/metadata utilities, and registry software corpus |
| Filesystem | openat/path traversal, DAC/sticky/umask, quotas, fd lifetime, FIFO, xattrs, extents, metadata, read-only mounts, no ambient-host escape |
| Process/signal | spawn/exec/wait, fd actions/CLOEXEC, groups/sessions, rlimits, locks, SCM_RIGHTS, dispositions/masks/pending, stop/continue/kill |
| Network | TCP/UDP/Unix/DNS/TLS, policy denial, readiness, backpressure, cancellation, limits, fd duplication and mixed poll sets |
| Terminal | canonical/raw input, echo/control signals, duplicated fds, window resize/SIGWINCH, foreground group, raw-mode restoration, guest-created PTY |
| Identity/system | credential transitions, groups, passwd/group database, argv/env, clocks, entropy, `/proc`, `/dev`, hostname/system identity |
| Resource attacks | Every named count/byte/deadline cap at limit and limit+1; near-limit warning; transactional rollback and typed error |

Both backend selections execute the same corpus. Tests compare stdout, stderr,
bytes, errno, exit status, terminating signal, kernel side effects, and resource
accounting—not engine error strings.

### Resource-attack cap evidence map

This map scopes the Resource attacks row to the named executor-facing limits in
Sections 2–7 and the runtime-neutral request/reply paths. A checked row has
boundary, limit-plus-one, warning, typed-error, and rollback evidence either at
the semantic owner or through a shared admission primitive that the operation
is required to use. An unchecked row is a release-gate test gap; the existence
of a hard-coded rejection alone does not close it.

- [x] **Owned adapter payloads and counts.** `PayloadLimit` proves exact-limit
  admission, limit-plus-one rejection with structured `{limitName, limit,
  observed}` details, coalesced 80% warning/rearm, and allocation-free JSON
  measurement in
  `crates/execution/src/backend/payload.rs`. The bounded byte, string, vector,
  and count constructors are required to receive that named limit and reject
  before operation construction (`bounded_values_reject_before_admission` and
  `common_payload_constructors_require_named_limits`).
- [x] **Runtime-neutral retained resources.** `ResourceLedger` proves exact
  child-scope admission, 80% warning/rearm, limit-plus-one typed fields, parent
  rollback, and release-to-zero in
  `named_limit_proves_boundary_warning_typed_rejection_and_rollback` and
  `failed_child_admission_rolls_back_parent`. This is the shared evidence for
  reactor sockets/connections/buffers, blocking jobs/bytes, capabilities,
  tasks, completions, and other ledger-backed runtime resources.
- [x] **Common execution events and direct replies.** The backend submission
  tests prove a full count queue settles only the rejected call's waiter, the
  byte boundary remains charged after dequeue until settlement, limit-plus-one
  carries the configured name, and settlement releases the charge. Direct
  reply and common event tests prove bounded raw/JSON replies, stdout/stderr,
  warnings, and runtime faults, including near-limit delivery
  (`crates/execution/src/backend/{submission,reply,event}.rs` and
  `crates/execution/tests/backend_payload_bounds.rs`).
- [x] **Spawn file actions.** `wasm_spawn_action_decoder_enforces_typed_limits_with_e2big`
  covers the 4096-action and 1 MiB encoded-byte families with independent
  count/byte rejection and near-limit warnings. The raw ABI spawn-result
  prevalidation cases followed by `waitpid(...)=ECHILD` prove rejected requests
  do not create a child.
- [x] **Kernel saturation resources.** Kernel resource-accounting and socket
  table tests fill and exceed process, open-fd, pipe, PTY, socket, connection,
  socket-buffer-byte, and datagram-queue limits, verify stable usage after
  rejection, and verify capacity returns after close/drain/reap. The shared
  resource-gauge registration and `resource_gauges_track_usage_and_warn_on_approach`
  cover the common near-limit warning path.
- [x] **Fixed ABI table caps.**
  `raw_abi_fixed_tables_lists_and_strings_prove_boundary_plus_one_and_warning`
  proves exact-1024 admission, limit-plus-one rejection before table access,
  and the common warning contract for iovecs, pollfds, and Preview1
  subscriptions. `raw_abi_memory_directions_reject_hostile_ranges_before_host_work`
  proves malformed tables cannot partially copy out. Spawn actions remain 4096
  and SCM_RIGHTS remains 253.
- [x] **Point-in-time kernel byte/count caps.**
  `resource_limits_reject_oversized_spawn_payloads`,
  `resource_limits_reject_oversized_pread_and_write_operations`, and
  `resource_limits_reject_oversized_readdir_batches` prove exact-boundary
  admission, limit-plus-one rejection, and no file/process mutation for argv,
  environment, pread, write, and readdir. The shared
  `resource_gauges_track_usage_and_warn_on_approach` proof checks the five
  stable names plus structured `{limitName, limit, observed}` error details.
- [x] **Fixed semantic string/list caps.**
  `raw_abi_fixed_tables_lists_and_strings_prove_boundary_plus_one_and_warning`
  covers supplementary groups, account names, xattr names, and xattr values at
  the adapter boundary. `xattr_value_and_name_list_limits_accept_boundary_and_rollback_plus_one`
  and `xattr_value_limit_accepts_linux_boundary_and_rejects_plus_one_transactionally`
  prove the semantic owner accepts 64 KiB, rejects limit plus one with Linux
  errno, and preserves the previous path/fd value and encoded name list.
- [x] **Deadlines.** `BlockingReadDeadline` and
  `OperationDeadlineTracker` are the common one-shot 80% warning state machines
  required by every `maxBlockingReadMs` and `operationDeadlineMs` path,
  including synchronous poll and readiness re-park paths. The focused
  `operation_deadline_warns_at_eighty_percent_before_success_or_typed_expiry`
  proof covers near-limit success, typed expiry, synchronous socket writes, and
  reconstruction without clock reset or duplicate warning. Kernel
  `blocking_pipe_and_pty_reads_time_out_instead_of_hanging_forever`, raw
  `raw_abi_blocking_read_warns_at_eighty_percent_before_typed_expiry`, and
  `wasm_parent_child_write_deadline_wakes_after_parent_stops_polling` cover
  guest-visible warning/expiry and teardown behavior.

### Phase 0 artifact evidence

The Phase 1 tree completed `just tools-rebuild` after removing the obsolete
`wasi-libc-overrides/ownership.c` definitions and retaining the canonical
patched-libc ownership implementation. The rebuild produced 119 standalone
Rust commands, compiled 98 C programs, installed the selected C commands, and
confirmed 166 entries in the default command corpus. Because a focused Vim
build was already present in `toolchain/target`, that completed rebuild/copy
invocation staged and audited 167 command entries. The generated ABI remained
current at 169 functions.

Those 169 manifest rows have 29 distinct core function signatures. The 40
Preview1 rows also link through the `wasi_unstable` module alias, yielding 209
effective linker names without duplicating implementations. The inventory
tables contain 111 semantic rows: 110 supported binding groups plus the one
intentional unsupported-import group. The generated registry must preserve
those counts and map every import to explicit handler, decoder, encoder,
execution-class, restartability, return-convention, permission, and
transaction/prevalidation metadata.

The automated Binaryen audit inspected all 167 staged command entries (136 distinct
modules) and found 145 unique module/function imports. Every observed import
has the exact signature declared in Sections 3–7; no undeclared import or
conflicting signature remains. The artifact set specifically confirms that
both `path_chown`/`fd_chown` legacy aliases and `proc_spawn_v3`/`proc_spawn_v4`
plus `proc_waitpid_v2`/`proc_waitpid_v3` version pairs remain live. Generated
command and package outputs are ignored build evidence and are not committed.
A subsequent focused DuckDB build expanded the optional staged corpus to 168
commands and 137 distinct modules without changing the 145-import inventory or
the 169-function ABI manifest; it is not part of the 166-command default corpus.

## 10. V8-WASM performance and memory baseline

Two current measurements serve different purposes.

The committed warm resource matrix at
`packages/runtime-benchmarks/results/baseline-local.json` was captured on a
12th-generation Intel i7-12700KF, 20 logical cores, 62.6 GiB RAM, Node 24.17.0,
Linux 6.1 x86-64, with 20 iterations after five warmups. Across ordinary
V8-WASM lanes the incremental sidecar VmHWM range is 11.2-20.0 MiB, the median
is 14.6 MiB, and the large stream-copy case reaches 56.3 MiB. This is a
sidecar-level high-water delta, not isolated engine RSS/PSS.

The focused command-floor capture from revision `38b6a84b` on the same machine
used one fresh-cache iteration, no benchmark warmup, three serial executions,
and warmup diagnostics proving the first execution performed V8 prewarm while
the next two used the cache:

| Command | Module bytes | Fresh-cache first execution | Cached warm p50 |
| --- | ---: | ---: | ---: |
| `true` | 259,407 | 59.04 ms | 35.31 ms |
| `printf` (0 bytes) | 501,725 | 69.95 ms | 42.45 ms |
| `pwd` | 350,044 | 64.53 ms | 41.59 ms |
| `ls` (empty directory) | 1,054,728 | 83.24 ms | 56.11 ms |
| `date --version` | 2,468,903 | 94.44 ms | 58.31 ms |

This is a captured baseline, not a statistically strong acceptance run. Phase
3 reruns at least five independent fresh-cache processes with five measured
iterations each, records p50/p95 and RSS/PSS/VIRT separately, and compares the
two engines using identical module bytes, cache state, host-service path, output
capture, and concurrency.

The reproducible command is:

```bash
BENCH_ONLY=wasm-command-floor \
BENCH_WASM_COMMAND_FLOOR_ITERATIONS=5 \
BENCH_WASM_COMMAND_FLOOR_WARMUP=0 \
BENCH_WASM_COMMAND_FLOOR_SERIAL_RUNS=3 \
BENCH_WASM_COMMAND_FLOOR_WARMUP_DEBUG=1 \
pnpm --dir packages/runtime-benchmarks bench
```

Run it with a fresh sidecar cache root for each measured cold sample. Generated
tool binaries remain uncommitted; use `pnpm install --frozen-lockfile` followed
by `just tools-rebuild` before a release-quality capture.

## 11. Locked implementation decisions

1. **Feature profile.** Both standalone backends prevalidate with one
   `wasmparser` profile. Enable MVP, mutable globals, sign extension,
   nontrapping float-to-int, bulk memory, reference types, multivalue, and
   SIMD128 and finalized exnref exception instructions. The owned toolchain
   translates the legacy encoding emitted by LLVM 19 for DuckDB with pinned,
   checksum-verified Binaryen 128.
   Disable threads/shared memory, memory64, multi-memory, relaxed SIMD, tail
   calls, function references, components, custom page
   sizes, and other proposals until an explicit profile revision. Engine
   defaults never silently expand the accepted language.
2. **Code placement.** Kernel semantic APIs stay in `agentos-kernel`. Shared
   request/reply types and capability-sized host traits live under
   `crates/execution/src/host/`; native-sidecar implements them using kernel and
   process-lifecycle context. Wasmtime remains under
   `crates/execution/src/wasm/wasmtime/`. No new crate is created unless the
   actual dependency graph produces a cycle that cannot be removed by moving
   transport-neutral types to the existing bridge crate.
3. **ABI generation.** Generate Preview1 layouts/types from a checked-in pinned
   Preview1 WITX description; generated glue implements AgentOS host traits and
   does not construct a `wasmtime-wasi` context. Generate custom-import
   signatures from one checked-in AgentOS ABI manifest shared by linker tests
   and import-audit tooling. Handwritten code remains only for bounded memory
   copying and version-to-canonical request conversion.
4. **CPU fields.** Remove the misleading lockstep `maxWasmFuel` field. Replace
   it with `activeCpuTimeLimitMs` (default runaway safeguard), optional
   `wallClockLimitMs`, and optional `deterministicFuel`. No compatibility alias
   is needed because protocol/client/sidecar ship together.
5. **Engine profiles.** The process caches Engines by exact feature profile and
   exact stack cap. Unspecified stack uses 512 KiB. Admit at most eight distinct
   profiles process-wide by default, warn at 80%, and reject the ninth with a
   typed limit naming the engine-profile setting. Set the async stack to the
   WASM stack cap plus 1.5 MiB of host-call headroom (2 MiB for the default
   profile), reject arithmetic/platform-size overflow as typed configuration
   errors, and charge the complete async-stack reservation per active Store.
   This preserves Wasmtime 46's default host-call headroom while making custom
   stack profiles explicit and accounted. Modules never cross Engines.
6. **Module cache.** Per Engine, use a 32-entry LRU and a 256 MiB conservative
   admission budget. Charge `max(module_bytes * 8, 1 MiB)` per compiled Module,
   warn at 80%, never deserialize native artifacts, and expose hits, misses,
   evictions, source bytes, charged bytes, compile latency, and retained RSS in
   metrics. Phase 3 may tune defaults from measured evidence without changing
   ownership.
7. **Platforms.** Linux x86-64, Linux arm64, macOS x86-64, and macOS arm64 are
   initial release blockers because all four native sidecars are published.
   Full conformance and performance gates run on canonical Linux x86-64;
   cross-compile plus smoke/conformance subsets run on the other three. No
   browser build or browser compile repair is in scope.
8. **Preferred-backend gate.** Correctness and safety permit no regression. On
   the canonical machine, Wasmtime may become preferred only when the command
   corpus geometric-mean p50 regresses no more than 10%, no individual p95
   regresses more than 20%, concurrency throughput regresses no more than 10%,
   and per-execution retained RSS/PSS regresses no more than the greater of 10%
   or 4 MiB. VIRT is reported separately. A threshold miss keeps Wasmtime
   selectable but does not make it preferred.
9. **Backend selection.** The sidecar protocol carries an optional sealed
   standalone-WASM backend enum: `wasmtime` or `v8`. Omission means the
   sidecar-owned default. Phase 2 initially defaults to V8; Phase 3 may switch
   omission to Wasmtime after gates pass. Both clients mirror the explicit
   override and neither owns the default.
10. **Threads and snapshots.** Threads/shared memory remain Phase 4 and require
    a new per-thread signal/libc audit. Process isolation is decided in that
    phase from teardown and containment evidence. Live snapshots/fork, Wizer,
    serialized AOT artifacts, pooling, and components remain outside initial
    completion.

These decisions close the implementation questions in the parent Wasmtime
specification. A later change requires an explicit spec revision, not an
adapter-local exception.
