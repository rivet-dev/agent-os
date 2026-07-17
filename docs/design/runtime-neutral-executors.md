# Runtime-Neutral Executors and Kernel Host Services

Status: ready for implementation; prerequisite for the Wasmtime executor

Audience: AgentOS kernel, runtime, execution, native-sidecar, VFS, and language
executor owners

## 1. Executive decision

AgentOS will refactor the boundary between the kernel, sidecar, and guest
executors before adding Wasmtime as a second standalone-WASM backend.

- The kernel owns process identity, descriptors, VFS state, permissions,
  signals, process groups, wait status, virtual sockets, PTYs, and resource
  accounting.
- The sidecar owns runtime selection, external asynchronous I/O, the one Tokio
  runtime, package resolution, lifecycle coordination, and host-visible events.
- Guest execution never runs on Tokio. Work that can wait uses async readiness;
  unavoidable blocking work requires admission to the fixed bounded blocking
  executor. No subsystem creates another runtime or blocks a Tokio worker.
- An executor owns only engine state, guest-memory/ABI adaptation, guest
  scheduling, and engine-specific interruption mechanics.
- The kernel defines a runtime-neutral process-control contract. V8, Wasmtime,
  Python, and binding executions expose control endpoints implementing that
  contract; the kernel never imports concrete executor types.
- Executors consume capability-sized filesystem, network, process, terminal,
  signal, clock, entropy, and identity services. They do not carry parallel
  policy or resource tables.
- Every Linux/POSIX operation supported by AgentOS has one semantic
  implementation in the kernel or its kernel-owned resource layer. Executors
  adapt guest ABIs to that implementation; they do not provide per-engine
  versions.
- The first implementation stays in the existing crates. A new shared crate is
  not required unless dependency pressure proves that one is necessary.

This is prerequisite architecture, not Wasmtime adapter work. The Wasmtime
executor specification depends on the exit gates in this document. All work in
this specification lands as one independent prerequisite JJ revision after the
specification/baseline revision and before any Wasmtime executor code. The
capability sequence in Section 12 is an implementation workstream order inside
that one revision, not permission to interleave Wasmtime or create one landing
revision per capability.

### 1.1 Non-goals

This refactor does not:

- force JavaScript, WASM, and Python to expose the same guest API;
- move Node streams, WASI layouts, Python objects, or engine event loops into
  the kernel;
- make the kernel own or poll executor instances;
- move external Tokio networking into the kernel;
- replace working kernel VFS/socket/PTY/process implementations;
- require every capability family to migrate in one atomic change; or
- compile, repair, or reactivate the browser runtime.

The shared layer standardizes authority, state, typed operations, completion,
and control. Adapters remain free to expose language-appropriate APIs.

### 1.2 Scope and parity target

The scope is **feature parity or better with the complete currently supported
V8-hosted standalone-WASM environment**, not an abstract project to implement
every syscall ever shipped by Linux.

The baseline includes:

- every active `wasi_snapshot_preview1` and AgentOS `host_*` import;
- the owned patched Rust/libc/sysroot surface used by current software;
- commands and interactive programs that already work, including `ls`, `vim`,
  `grep`, `curl`, shell/process pipelines, sqlite, and the registry command
  suite;
- current filesystem, descriptor, networking, process, signal, TTY, identity,
  clock, permission, and resource-limit behavior; and
- hostile raw modules that call imports directly without libc.

If the current implementation has multiple engine-specific versions of one
operation, the refactor selects or builds one Linux-correct kernel
implementation and migrates all current executors to it. Correctness fixes are
in scope and may intentionally change existing V8 behavior.

Future supported Linux APIs follow the same rule: add the semantic operation to
the kernel/shared resource owner first, then expose it through adapters. The
project is complete when the existing V8-WASM feature and software corpus runs
through this single implementation and Wasmtime can consume it without adding
semantic host code.

### 1.3 Wasmtime research constraints on this refactor

No Wasmtime spike is required, but the published embedding API imposes concrete
requirements that the shared boundary must satisfy:

- Wasmtime's `Linker` can provide the existing AgentOS Preview1 and `host_*`
  imports directly; the refactor must not assume `wasmtime-wasi` resource
  ownership.
- Async host imports suspend a Wasmtime call as a future. The host service must
  accept bounded owned values and return through an awaitable direct reply;
  guest-memory borrows cannot cross the wait.
- Wasmtime does not provide the embedding's execution pool. The sidecar must
  poll guest execution on the existing bounded non-Tokio VM executor while
  Tokio owns external async I/O.
- A Wasmtime `Store` needs cloneable generation-bound handles to host services,
  cancellation, readiness, limits, and the kernel process reporter. It cannot
  own or mutably borrow the sidecar's `KernelVm`.
- Epoch interruption and Store cancellation need a thread-safe control handle
  that maps naturally to the same kernel process endpoint used by V8.
- Compiled `Module` sharing and caching are engine concerns and must not affect
  kernel process, fd, filesystem, or signal ownership.
- Shared-memory threads are not part of the initial parity target; the current
  V8-WASM software surface is the first Wasmtime admission gate.

These constraints are sufficient to design the kernel interface without
building a provisional Wasmtime implementation.

## 2. Why this refactor is needed

The current code already contains the beginning of the correct abstraction,
but it is not connected to the real executors.

`agentos-kernel` defines `DriverProcess`, stores an
`Arc<dyn DriverProcess>` in every process-table entry, and owns blocked and
pending signal sets. However, `KernelVm::register_process` always registers a
`StubDriverProcess`. The stub records signals and synthesizes exits; it is not
the `ActiveExecution` actually running the guest.

The native sidecar separately stores:

- `ActiveExecution::{Javascript, Python, Wasm, Binding}`;
- an additional signal-disposition map;
- an additional pending-WASM-signal set;
- V8-specific signal/session delivery;
- runtime pause, resume, termination, and OS-process signal logic; and
- readiness targets containing `V8SessionHandle`.

This produces two process-control planes:

```text
kernel ProcessTable
  -> DriverProcess
  -> StubDriverProcess

sidecar ActiveProcess
  -> ActiveExecution enum
  -> V8 / Python / V8-WASM / binding-specific control
```

Signals generated by kernel operations such as `EPIPE`, child exit, or PTY
resize can therefore take a different path from signals delivered through the
sidecar. Wasmtime would add another path unless this boundary is fixed first.

The same pattern exists in less concentrated form for fd aliases, filesystem
permissions, mutable rlimits, clocks, identity, terminal metadata, and socket
readiness. The semantic implementation is often already in Rust, but the
effective state or transport remains executor-specific.

## 3. Required dependency direction

The kernel must not own or depend on V8, Wasmtime, Pyodide, JavaScript bridge
types, Tokio tasks, or concrete sidecar process records.

```text
                         kernel-owned durable state
                     process / fd / VFS / signal / PTY
                                  ^       |
                                  |       | control wake
                        typed host services
                                  |       v
sidecar lifecycle + I/O ---- runtime-neutral execution contract
                                  ^
                                  |
                 +----------------+----------------+
                 |                |                |
              V8 adapter     Wasmtime adapter   Python adapter
```

There are three distinct interfaces:

1. **Kernel to executor:** nonblocking, coalesced process-control requests.
2. **Executor to host:** typed, bounded operations over kernel/sidecar
   capabilities.
3. **Sidecar to executor:** start, event, exec-replacement, cancellation, and
   teardown lifecycle.

Combining these into one enormous `Executor` or `GuestHost` trait would couple
unrelated capabilities and recreate the current switchboard under a new name.

## 4. Kernel-facing process runtime endpoint

### 4.1 Replace the stub-only driver connection

Evolve `DriverProcess` into a narrow runtime endpoint registered with the
process-table entry:

```rust
pub trait ProcessRuntimeEndpoint: Send + Sync {
    fn request_control(
        &self,
        request: ProcessControlRequest,
    ) -> Result<(), ProcessRuntimeEndpointError>;
}

pub enum ProcessControlRequest {
    Checkpoint,
    Stop,
    Continue,
    Terminate(ProcessTermination),
    Cancel(CancellationReason),
}
```

The exact names are not normative. The behavioral requirements are:

- calls never execute guest code inline;
- calls never block a kernel lock or Tokio worker;
- standard-signal notifications are coalesced into durable kernel state;
- each execution has at most one queued wake;
- stop, terminate, and cancellation cannot be dropped because an ordinary
  event queue is full;
- endpoint failure is typed and observable; and
- the endpoint contains no authority beyond its registered VM generation and
  kernel PID.

The endpoint implementation should be a cloneable control handle separate from
the owned engine instance. It may set atomic control bits, request a Wasmtime
epoch interruption, interrupt a V8 session, notify an admitted VM-executor
thread, or signal a native child process. Those mechanics stay inside the
adapter.

Process allocation and backend construction currently have a PID dependency in
both directions. Resolve it with a two-part control cell:

1. the sidecar creates a bounded `RuntimeControlCell` and registers its producer
   endpoint while the kernel allocates the PID;
2. the sidecar constructs the backend with that PID and attaches the one
   consumer to the cell before starting guest instructions.

Control requested during construction remains durable in the cell. A
termination requested before attachment prevents the guest from starting; no
temporary production stub and no lost-signal window are allowed.

`StubDriverProcess` remains only for kernel unit tests and deliberately virtual
processes. Production guest processes register their real runtime endpoint.

### 4.2 Executor-to-kernel exit reporting

Do not retain `DriverProcess::wait` as a second source of process status. The
kernel process table is authoritative for wait and zombie state.

At registration, the sidecar receives a generation-bound reporter capability:

```text
report_exit(Exited(code))
report_exit(Signaled { signal, core_dumped })
report_runtime_fault(typed_error)
```

Reporting is idempotent and first-terminal-result wins. The kernel records the
exit, closes or releases process resources through the existing lifecycle,
creates `SIGCHLD`, wakes waiters, and exposes exact signal metadata. An exit
code of `128 + signal` is never used to infer that a signal occurred.

Kernel-wide termination requests control through every endpoint and then waits
on process-table terminal state with bounded grace and kill phases. It does not
call an endpoint-specific `wait` method, because that would restore a second
source of process status.

## 5. Kernel-owned signal model

Signals should be handled at the kernel level. The current kernel owns only
part of the state; the refactor completes that ownership.

### 5.1 Authoritative state

Each kernel process owns bounded signal state:

- disposition for signals 1 through 64: default, ignore, or user;
- disposition flags and handler mask, but not a guest function pointer;
- blocked signal set;
- coalesced pending standard-signal set;
- running, stopped, and exited state;
- bounded in-progress delivery tokens needed for nested handlers; and
- exact terminating signal and core-dump metadata.

Guest handler pointers remain inside V8 or WASM linear memory and are never
kernel capabilities. A `user` disposition means the adapter must deliver the
signal at a guest safe point.

`sigaction`, `sigprocmask`, `sigpending`, signal generation, exec disposition
reset, and wait-state changes all operate on this one record. The sidecar
`signal_states` map and `ActiveProcess.pending_wasm_signals` are deleted.

### 5.2 Delivery decision

`signal_process` performs Linux-compatible target validation and then makes the
delivery decision while holding the process-table state:

- signal 0 validates only;
- a blocked catchable signal becomes pending;
- an ignored signal is discarded, except for required `SIGCONT` resume
  behavior;
- a caught signal becomes pending and requests `Checkpoint`;
- a default stop signal changes kernel status and requests `Stop`;
- `SIGCONT` changes kernel status and requests `Continue` before any caught
  handler runs;
- a default terminating signal requests `Terminate`; and
- `SIGKILL` and `SIGSTOP` cannot be blocked, ignored, or caught.

Kernel-generated `SIGPIPE`, `SIGCHLD`, and PTY foreground-group `SIGWINCH` use
this exact path. The sidecar does not generate duplicates.

### 5.3 Guest handler checkpoint

At a safe point, an adapter asks the kernel to begin one pending delivery. The
kernel atomically selects an unblocked signal, applies `sa_mask`,
`SA_NODEFER`, and `SA_RESETHAND`, and returns a bounded delivery token plus the
signal number and flags. The adapter invokes its guest handler and then closes
the token so the previous mask is restored.

For WASM, the owned libc calls `__wasi_signal_trampoline`. For Node/V8, the V8
adapter schedules the matching process signal event. The kernel does not call
either engine.

An interruptible host operation registers its waiter and temporary `ppoll`
mask atomically with the signal state. A caught signal wakes the operation and
returns `EINTR` or a restart checkpoint. Ignored and still-blocked signals do
not spuriously interrupt it.

`SA_RESTART` requires one documented, shared set of restartable operations.
Before real threads, the mask is process-scoped. The later threads project
moves masks and in-progress delivery stacks to kernel thread records without
changing executor control.

## 6. Sidecar-facing execution backend contract

`ActiveExecution` currently implements common behavior through a growing enum
match and also exposes V8-specific session and sync-RPC methods. Replace that
surface with a small common backend contract and adapter-owned extensions.

The common lifecycle is:

```rust
pub trait ExecutionBackend {
    fn runtime_kind(&self) -> GuestRuntimeKind;
    fn control_endpoint(&self) -> Arc<dyn ProcessRuntimeEndpoint>;
    fn start_prepared(&mut self) -> Result<(), ExecutionError>;
    fn poll_event(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Option<ExecutionEvent>, ExecutionError>>;
    fn begin_shutdown(&mut self, reason: ShutdownReason)
        -> Result<(), ExecutionError>;
}
```

This is an architectural shape, not a requirement to use `async_trait` or
dynamic dispatch. An enum may remain as storage if all common callers use the
contract and engine-specific matches are confined to construction/adapters.
The owned backend is deliberately not required to be `Send`: V8 remains
thread-affine on its admitted VM-executor thread. Only its control and wake
handles cross threads.

Common events are bounded and runtime-neutral:

```text
stdout(bytes + reservation)
stderr(bytes + reservation)
host_call(typed request + direct reply handle)
warning(typed warning)
exited(process termination)
```

A host-call event carries its response capability. Shared code must not call
methods such as `respond_javascript_sync_rpc_*` on the execution enum. Python
VFS requests, V8 synchronous bridge calls, and Wasmtime async imports normalize
to the same typed host operations where their semantics are shared.

V8-only stream events and Node-specific callbacks remain adapter extensions;
their types do not appear in filesystem, process, signal, or readiness owners.

## 7. Executor-facing host services

The executor-facing API is split by capability rather than engine:

```text
GuestFileHost       paths, descriptors, metadata, directory operations
GuestNetworkHost    sockets, DNS, connect/listen/data/options/readiness
GuestProcessHost    spawn, exec, wait, groups, rlimits, descriptor actions
GuestTerminalHost   PTYs, termios, window size, foreground group, stdio
GuestSignalHost     sigaction, masks, pending delivery checkpoints
GuestIdentityHost   uid/gid/groups and passwd/group lookup
GuestClockHost      realtime, monotonic time, timers
GuestEntropyHost    bounded random bytes
```

These are typed operation families, not necessarily Rust async traits. The
initial implementation can use bounded request messages with direct reply
handles because the sidecar owns mutable `KernelVm` and the process-wide Tokio
reactor. The requirements are:

- request inputs are owned, bounded values;
- the caller supplies only VM generation, kernel PID, and registered
  capability identity—not authority chosen by guest bytes;
- replies carry typed `{ code, message, details }` errors;
- every async request has one registered waiter and cancellation path;
- no synchronous waiter scans or consumes unrelated execution events;
- overload is a typed limit error, never an infinite retry loop;
- kernel operations remain the semantic and permission authority; and
- external OS I/O remains in sidecar runtime services, not the kernel crate.

### 7.1 Async and blocking execution contract

Synchronous guest semantics do not authorize blocking a Tokio worker. Every
operation is classified by where work executes and how it waits:

| Work | Owner | Waiting rule |
| --- | --- | --- |
| V8, Wasmtime, Python, or binding guest instructions | Bounded non-Tokio VM executor | May occupy only its admitted VM-executor capacity; never polled or entered synchronously by Tokio |
| Bounded in-memory kernel operation | Kernel called by sidecar host service | May execute inline only when it cannot wait and has a bounded work quantum |
| Kernel fd read/write/poll that would block | Kernel readiness plus sidecar waiter | Return readiness/`EAGAIN` state and suspend the guest operation; never wait on a condvar or blocking channel on Tokio |
| Native TCP/UDP/Unix/TLS/DNS and timers | One process-wide Tokio runtime | Use async I/O, bounded commands, direct completion waiters, and cancellation |
| Unavoidably blocking host filesystem/library work | Fixed bounded blocking executor | Acquire admission before submission; bounded queue and deadline; never use Tokio's elastic blocking pool as unbounded admission |
| Guest-visible sleep, child wait, terminal input, and record locks | Durable kernel/sidecar wait registration | Suspend until readiness, signal, timeout, cancellation, or teardown; no polling timer and no executor-specific child pumping |

The process contains exactly one Tokio runtime. No VM, executor, socket,
filesystem adapter, or child process creates another runtime. No code running
on a Tokio worker may use `block_on`, `Atomics.wait`, a condition-variable wait,
a blocking channel receive/send, synchronous guest entry, or an unadmitted
blocking syscall.

An asynchronous host call follows this sequence:

1. The guest adapter validates and copies bounded owned input.
2. It submits a typed request with a registered direct reply handle and
   cancellation identity.
3. The Wasmtime/V8/Python execution yields or parks only its admitted
   non-Tokio execution context.
4. The sidecar performs bounded kernel work, starts async Tokio I/O, or admits
   unavoidable blocking work to the fixed blocking executor.
5. Completion settles only the registered waiter, updates durable readiness,
   and enqueues at most one coalesced execution wake.
6. The guest adapter resumes, revalidates guest memory if applicable, and
   encodes the typed result.

Every path has explicit limits for request count, request bytes, retained
buffers, outstanding waiters, blocking jobs, and completion bytes. Cancellation
and VM teardown settle or fail every waiter; fire-and-forget work that can lose
an error is prohibited.

The V8 RPC decoder and Wasmtime linker are two transports into these same
operations. A transport may decode a different ABI, but it may not implement a
second fd table, signal state, network policy, or filesystem permission model.

## 8. Filesystem and descriptor requirements

The kernel fd table is the only authoritative guest descriptor namespace.
Kernel state owns open descriptions, offsets, flags, rights, cwd/path-at
resolution, preopen metadata, filesystem permission tier, rlimits, and errno.

The current native sidecar contains a bidirectional mutable shadow filesystem:
some embedded-Node operations write host paths, sidecar calls copy those paths
into the kernel, kernel mutations are mirrored back to host paths, and exit-time
walks reconcile them again. This exists because parts of the V8/Node filesystem
and module loader still access a materialized host tree. It can resurrect stale
files, requires a second inventory, duplicates permissions/metadata, and makes
the source of truth timing-dependent.

That mechanism is migration debt. It is not part of the shared host service and
is not inherited by Wasmtime.

Current evidence is concentrated in
`crates/native-sidecar/src/filesystem.rs`: `guest_filesystem_call` invokes
`sync_guest_filesystem_shadow_before_call` and
`mirror_guest_filesystem_shadow_after_call`; `ProcessModuleFsReader` reads the
process shadow before the kernel; and launch/exit paths call
`sync_process_host_roots_to_kernel`. The managed WebAssembly path already
prefers kernel filesystem RPCs when its execution root is configured, so this
mirror is primarily embedded-runtime compatibility debt rather than a Wasmtime
requirement.

The target has one mutable source of truth:

- all guest filesystem operations, including embedded V8 `fs`, module
  resolution, WASI, Python, and wire filesystem calls, use `GuestFileHost` over
  the kernel VFS and fd table;
- host-directory access occurs through explicit confined kernel mount/plugin
  resources, not by copying a directory tree into and out of the VFS;
- `/opt/agentos` package projection means kernel/VFS mounts of immutable package
  resources, not a mutable shadow copy;
- a module loader may cache immutable bytes under ordinary bounded cache rules,
  but it cannot maintain writable filesystem state outside the kernel; and
- V8 may temporarily retain an ABI fd-alias map while Node-WASI is migrated,
  but aliases resolve to kernel descriptors and cannot own offsets, rights,
  contents, or lifecycle.

Cutover requirements include:

- remove mutable host-shadow reconciliation, including shadow inventories,
  pre-call host-to-kernel sync, post-call kernel-to-host mirroring, and exit-time
  tree walks;
- route embedded V8 filesystem builtins and module reads to the kernel-backed
  service before declaring the prerequisite complete;
- raw `host_fs` calls cannot bypass the configured filesystem tier;
- absolute paths and dirfd-relative paths share kernel resolution;
- preopens and descriptor rights come from kernel metadata;
- mutable `RLIMIT_NOFILE` moves into kernel process state;
- descriptor allocation has one limit and warning path; and
- kernel errors cross the sidecar as typed values instead of strings.

## 9. Network and readiness requirements

The kernel remains the owner of virtual socket state and the sidecar remains
the owner of external TCP/UDP/Unix/TLS transports.

Replace `V8SessionHandle` readiness targets with a generation-bound
`ExecutionWakeHandle`. Readiness is durable level state in the resource owner;
the wake is only a coalesced hint. Each execution has at most one queued wake,
and each in-flight operation has one direct completion waiter.

The wake handle cannot:

- clear readiness merely because an adapter consumed a hint;
- select another VM generation;
- enqueue unbounded packet/chunk events;
- run guest code on a Tokio thread; or
- own a second socket registry.

V8 translates the wake into its event-loop checkpoint. Wasmtime wakes the
future polled by the bounded VM executor. Python uses the same operation and
readiness state rather than a polling timer.

## 10. Process, terminal, identity, clock, and entropy requirements

- Spawn and exec are sidecar lifecycle operations over kernel process/fd state.
  Runtime selection is not an executor responsibility.
- Process file actions use kernel descriptors directly and preserve atomic
  commit/rollback semantics.
- PTY state, line discipline, termios, foreground pgid, and window size stay in
  the kernel. Host stdout ordering, raw-mode cleanup leases, and tracked-runtime
  `SIGWINCH` wakes stay in the sidecar service.
- UID/GID/effective IDs, supplementary groups, umask, and mutable rlimits are
  kernel process state. Executors do not reconstruct them from environment
  variables.
- Realtime policy and monotonic process time come from a shared clock service;
  Wasmtime and V8 do not use ambient engine clocks.
- Random reads validate the guest range first and use a bounded/chunked shared
  entropy service backed by the same source as virtual `/dev/urandom`.
- Intentional Linux-compatibility stubs such as fixed identity mutation,
  loopback-only interface enumeration, or unsupported `mlock` stay in the
  owned sysroot and behave identically under both engines.

## 11. Proposed code organization

No new crate is required initially:

```text
crates/kernel/src/
  process_table.rs          authoritative process/signal/wait state
  process_runtime.rs        runtime endpoint, control requests, exit reporter
  signal.rs                 dispositions and delivery checkpoints

crates/execution/src/
  backend/
    mod.rs                  common lifecycle/event contract
    control.rs              engine-side endpoint helpers
    event.rs                runtime-neutral bounded events/reply handles
  host/
    mod.rs
    error.rs
    filesystem.rs
    network.rs
    process.rs
    terminal.rs
    signal.rs
    identity.rs
    clock.rs

crates/native-sidecar/src/execution/
  registry.rs               PID/generation to backend + wake/control handles
  host/
    filesystem.rs           kernel/mount-backed filesystem operations
    network.rs              kernel/native transport capability operations
    process.rs
    terminal.rs
    signal.rs
    identity.rs
    clock.rs
```

The exact file split can follow the implementation, but these ownership
boundaries are normative. Do not put every operation in one `executor.rs`, one
`host.rs`, or one mega-trait.

A later `agentos-executor-host` crate is justified only if the existing crate
graph creates a real dependency cycle or if independent fuzzing/linkage needs
it. Creating a crate merely to hold traits adds packaging and versioning cost
without improving ownership.

## 12. Prerequisite-revision workstream sequence

The entire sequence below is **one delivery phase and one JJ revision**. These
workstreams provide implementation and review checkpoints while the revision is
being developed. The revision is ready to land only when every workstream and
every Section 13 exit gate is complete. Local intermediate revisions may be
used while developing it, but they are folded before handoff.

### Workstream 0: Freeze contracts and parity tests

- Inventory every active Preview1 and `host_*` import and every owned-sysroot
  extension used by the current software suite.
- Record signal, fd, process, terminal, network, errno, permission, async-wait,
  and resource-limit behavior.
- Freeze the working V8-WASM command corpus, including interactive and
  process/network-heavy programs, as the minimum parity suite.
- Add hostile raw-import tests for permission and ambient-host bypasses.
- Add exact exit-code-versus-signal assertions.

### Workstream 1: Connect kernel processes to real runtime endpoints

- Add `ProcessRuntimeEndpoint` and the generation-bound exit reporter.
- Register real control handles for V8, Python, binding, and compatibility
  WASM executions.
- Remove production dependence on `StubDriverProcess`.
- Preserve current sidecar signal behavior temporarily behind the endpoint.

### Workstream 2: Make signals kernel-authoritative

- Move dispositions, masks, pending state, and exec reset into the process
  table.
- Route `SIGPIPE`, `SIGCHLD`, PTY signals, kill, and process-group delivery
  through one path.
- Add begin/end handler-delivery checkpoints and atomic `ppoll` masks.
- Delete sidecar and runner duplicate signal state.

### Workstream 3: Introduce typed backend events and host calls

- Add direct reply handles and typed errors.
- Stop routing shared operations through unrelated session-event scanning.
- Confine V8 sync-RPC details to the V8 adapter.

### Workstream 4: Consolidate filesystem and descriptor authority

- Move permission tier, preopens, fd rights, rlimits, and path-at resolution to
  kernel process state.
- Implement the shared filesystem host service.
- Route embedded V8 filesystem and module loading through that service.
- Delete mutable host-shadow inventories and bidirectional reconciliation.
- Preserve host access only through explicit confined mounts/plugins.

### Workstream 5: Generalize readiness and networking

- Replace V8 session readiness targets with execution wake handles.
- Route V8, Python, and later Wasmtime through the same bounded operations and
  direct waiters.
- Remove adapter-owned socket state that duplicates kernel/capability state.

### Workstream 6: Finish process, terminal, identity, clock, and entropy services

- Remove runner-local rlimits, identity, TTY caches, and clock/random providers.
- Complete live kernel termios, pgid, and supplementary-group operations.
- Close typed-error and Linux-conformance gaps.

### Workstream 7: Close the prerequisite revision

- Require new executors to implement only the common lifecycle/control
  contract and the ABI adapter over shared host services.
- Start the Wasmtime implementation only after the complete current V8-WASM
  ABI and software parity surface, including signal/readiness foundations, has
  closed its exit gates.
- Do not build a separate Wasmtime spike or provisional semantic host layer.
- Verify that the final tree contains no Wasmtime executor implementation and
  passes the complete current-executor parity suite before the next JJ revision
  begins.

## 13. Exit gates

The prerequisite refactor is complete when:

- every production kernel process has a real runtime endpoint;
- `StubDriverProcess` is test/virtual-process-only;
- the kernel process table is the only owner of signal dispositions, masks,
  pending sets, process status, and wait events;
- kernel-generated and externally requested signals use one delivery path;
- filesystem permissions, fd rights, preopens, rlimits, identity, and umask are
  authoritative kernel process state;
- no mutable guest filesystem state is synchronized between a kernel VFS and a
  host shadow tree;
- embedded V8 filesystem calls and module resolution observe kernel state
  directly;
- shared readiness targets contain no `V8SessionHandle`;
- common host services contain no `Javascript*`, `V8*`, `Wasmtime*`, or
  `Python*` types;
- common sidecar code does not match an executor variant to perform signal,
  filesystem, network, process, or terminal semantics;
- every request, reply, queue, waiter, and retained buffer is bounded and
  accounted;
- kernel error codes cross the host boundary without string parsing; and
- V8, Python, and compatibility WASM pass the complete current V8-WASM ABI and
  working-software parity suite through the new services before Wasmtime work
  begins.

## 14. Current extraction inventory

| Capability | Existing shared owner | Executor/sidecar-specific debt | Required target |
| --- | --- | --- | --- |
| Backend lifecycle | `ActiveExecution` normalizes some start/poll/control operations | Large enum switchboard exposes V8 session and JavaScript sync-RPC methods | Common lifecycle, control handle, bounded events, and adapter-owned extensions |
| Signals | Kernel process table owns target selection, masks, pending bits, groups, and wait status | Production kernel process uses a stub; sidecar owns dispositions; WASM owns another pending set | Real runtime endpoint plus fully kernel-owned dispositions/delivery state |
| Filesystem | Kernel VFS and fd tables implement most operations | Node-WASI fd aliases, bidirectional shadow-tree reconciliation, raw-import permission gaps, duplicated limits | `GuestFileHost` over the sole mutable kernel state; explicit mounts for host resources; delete shadow synchronization |
| Networking | Kernel owns virtual sockets/policy; sidecar owns external Tokio transports | Readiness and some socket state contain `V8SessionHandle` or JavaScript naming | Shared capability operations, direct waiters, and runtime-neutral coalesced wakes |
| Processes | Kernel owns PID/fd/group/wait/exec state; sidecar owns runtime selection | Spawn/event paths and descendant pumping are JavaScript-shaped; mutable rlimits live in runner | Shared process host service and kernel process limits with adapter-neutral lifecycle |
| TTY/PTY | Kernel owns PTYs, buffers, line discipline, termios core, pgid, and window size | Runner `isatty` cache, libc termios shadow, stubbed `pty_open`, adapter wait loops | Live kernel terminal operations plus sidecar lifecycle/output hooks |
| Identity/Linux | Kernel owns process identity, groups, `/proc`, `/dev`, and umask | Environment reconstruction, primary-GID-only groups, hardcoded hostname, clock quirks | Kernel identity/rlimits plus shared clock, entropy, and system-identity providers |
| Errors | Kernel errors contain stable errno-like codes | Sidecar converts them to strings and adapters reconstruct errno | Typed code/message/details through every shared operation |

The complete import-by-import mapping is normative in
[`wasmtime-phase-0.md`](./wasmtime-phase-0.md). It identifies every import's host
service operation, authority checks, limits, async wait, guest-memory direction,
compatibility status, and parity tests. Phase 1 keeps the generated ABI manifest
and rebuilt-module import audit synchronized with that mapping.

## 15. Principal risks

| Risk | Severity | Mitigation |
| --- | --- | --- |
| A generic interface becomes a mega-trait mirroring `ActiveExecution` | High | Split lifecycle, control, events, and capability-sized host services. |
| Kernel calls guest code or blocks on an executor | Critical | Runtime endpoint only sets bounded/coalesced control state and wakes the admitted executor. |
| Signal state moves but remains duplicated during migration | Critical | Declare kernel state authoritative phase by phase; adapters become subscribers, not mirrors. |
| Endpoint queue saturation drops `SIGKILL`, stop, or cancellation | Critical | Durable atomic control state with at most one wake; no ordinary event queue for control. |
| Mutable V8 host shadows survive and remain a second filesystem truth | Critical | Migrate embedded V8 `fs` and module reads to `GuestFileHost`; delete shadow inventories and bidirectional sync before Wasmtime. |
| Host-operation traits hide unbounded allocation or waiting | Critical | Owned bounded request types, direct waiters, resource reservations, typed overload. |
| Kernel grows Tokio or engine dependencies | High | Keep external I/O and executor mechanics in sidecar/adapters; kernel contracts remain engine-neutral. |
| Large refactor blocks all feature work | High | Migrate one capability family at a time inside the prerequisite revision, keeping V8 parity at every workstream checkpoint. |

## 16. Resolved owner decisions

1. The kernel owns every executor-independent Linux/POSIX semantic operation,
   including complete signal state and delivery decisions.
2. Production kernel processes register real runtime control endpoints;
   `StubDriverProcess` remains test/explicit-virtual-process-only.
3. The parity target is the entire currently supported V8-WASM ABI and working
   software surface, not a hand-picked Wasmtime subset and not every
   theoretical Linux syscall.
4. Correctness, security, errno, and Linux-behavior fixes may ship during the
   refactor even when they intentionally change current V8 behavior.
5. The browser runtime is entirely out of scope, including compile fixes and
   migration gates.
6. `ActiveExecution` may remain a sealed enum behind the common contracts;
   dynamic dispatch is not a project goal.
7. Mutable filesystem state has one source of truth in the kernel. Existing
   host-shadow synchronization is removed rather than generalized.
8. No Wasmtime spike is built. Wasmtime implementation starts after the
   prerequisite parity gates close and uses only the resulting shared services.
9. The complete runtime-neutral refactor is one independent JJ revision. The
   following Wasmtime implementation is a different revision; engine code is
   never used to paper over an incomplete prerequisite.
