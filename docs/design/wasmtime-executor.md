# Wasmtime Executor and Shared WASM Host ABI

Status: ready for implementation; depends on the runtime-neutral executor
refactor

Audience: AgentOS kernel, sidecar runtime, execution, VFS, toolchain, and
registry-software owners

## 1. Executive summary and decision

- **Keep V8 permanently for JavaScript.** JavaScript's `WebAssembly.*` APIs also
  remain inside V8; there is no V8-to-Wasmtime memory bridge.
- **Add Wasmtime as a permanent standalone-WASM executor alongside V8-WASM.**
  Wasmtime becomes the preferred backend after its parity, safety, and
  performance gates close, but the V8-WASM executor remains a maintained,
  selectable compatibility backend.
- **Do not create a crate or one giant Rust file initially.** Use a module tree
  under `crates/execution/src/wasm/`; extract a crate only for a measured
  dependency/build/composition benefit.
- **Do not rewrite filesystem, network, process, TTY, signal, or identity
  semantics.** Most already live in the kernel/sidecar. Consolidate the pieces
  still duplicated in the JavaScript runner through the prerequisite
  [runtime-neutral executor refactor](./runtime-neutral-executors.md).
- **Keep the AgentOS-owned WASI/POSIX ABI.** Wasmtime does not require
  `wasmtime-wasi`; link the existing Preview1 plus `host_*` functions to
  AgentOS resources and install no ambient host capabilities.
- **Async imports require bounded copies, not a new architecture.** Decode and
  copy guest input, release memory before awaiting shared I/O, then reacquire
  and revalidate memory before writing results.
- **Current native V8-WASM does not materially depend on shared memory between
  isolates.** Its `SharedArrayBuffer` use is local blocking coordination.
  Wasmtime threads are a separate later project because the sysroot still needs
  real pthread semantics and bounded thread-group lifecycle.
- **Snapshotting is limited initially to compiled in-memory Module reuse and
  Wasmtime's eligible copy-on-write memory initialization.** Live instance
  snapshot/fork and serialized AOT artifacts are out of scope.
- **No engine performance claim is justified yet.** The current ordinary warm
  V8-WASM baseline is 11.2-20.0 MiB incremental sidecar high-water memory, but
  cold compile, RSS/PSS, address-space reservation, async-stack cost, and
  concurrency must be measured directly against the completed initial
  implementation.

AgentOS will add Wasmtime as a native executor for standalone WebAssembly
commands. V8 remains the permanent JavaScript executor, including JavaScript
code that uses `WebAssembly.Module`, `WebAssembly.Instance`, or the asynchronous
JavaScript WebAssembly APIs. The existing V8-hosted standalone-WASM runner also
remains available as a compatibility executor. There is no direct bridge
between the two engines: a standalone-WASM process is created under exactly one
selected backend, and both backends reach the same kernel-owned resources
through their adapters.

Wasmtime becomes the preferred standalone-WASM backend only after conformance,
safety, and performance exit gates close. V8-WASM must remain selectable for
compatibility and diagnosis, must run the shared parity suite, and must not
retain private implementations of Linux semantics. The lockstep client and
protocol surface carries an optional sealed `wasmtime`/`v8` override; omission
uses the sidecar-owned default, so clients do not independently choose or drift
that default.

The first implementation lives under `crates/execution/src/wasm/`. A separate
crate is not required for the initial implementation. Extraction is allowed
later only if it produces a measured build, dependency, fuzzing, or binary
composition benefit.

AgentOS will continue to use its owned patched `wasm32-wasip1` sysroot and its
existing Preview1 plus `host_*` imports. Wasmtime is the core WebAssembly
engine; it does not become the owner of filesystem, network, process, terminal,
identity, permission, or resource semantics.

Ahead-of-time compilation, serialized Wasmtime artifacts, components, and live
process snapshots are outside the first implementation. The initial executor
uses ordinary Wasmtime compilation and a bounded in-process compiled `Module`
cache.

The kernel/executor boundary, signal ownership, shared host-operation services,
and runtime-neutral readiness contract are specified normatively in
[`runtime-neutral-executors.md`](./runtime-neutral-executors.md). This document
owns the Wasmtime engine, linker, guest-memory, limits, feature-profile,
performance, and preferred-backend decisions. Wasmtime-specific code must not
work around an unfinished prerequisite by adding another process-control or
host-service implementation.

## 2. Outcomes

The completed migration has these outcomes:

1. Standalone WASM can execute without a V8 isolate or JavaScript WASI runner
   when the Wasmtime backend is selected.
2. JavaScript execution and JavaScript's WebAssembly API remain on V8.
3. V8, Wasmtime, and Python adapters use the same kernel-owned filesystem,
   descriptor, process, signal, terminal, identity, permission, and accounting
   semantics.
4. External asynchronous I/O remains owned by the process-wide sidecar Tokio
   runtime and its bounded capability/readiness machinery.
5. A Wasmtime host import performs ABI decoding and result encoding only. It
   does not implement a second filesystem, socket table, process table, or
   permission model.
6. Guest execution never runs on a Tokio runtime worker.
7. The initial Wasmtime executor does not depend on pthreads, shared WebAssembly
   memory, AOT artifacts, Wizer, pooling allocation, or live snapshots.
8. V8-WASM remains a maintained compatibility executor over the same shared
   services; neither backend is implemented in terms of the other.
9. Every limit is bounded by default and fails with a typed error naming the
   limit and configuration field.

## 3. Non-goals

The initial Wasmtime executor does not:

- replace V8 for JavaScript;
- route JavaScript `WebAssembly.*` calls into Wasmtime;
- create a V8-to-Wasmtime memory or function bridge;
- adopt ambient host filesystem, network, clock, or process access from
  `wasmtime-wasi`;
- promise pthread, OpenMP, or general threaded-software compatibility;
- deserialize `.cwasm` or another native-code cache format;
- implement a general live `Store`/`Instance` snapshot or OS-style `fork()`;
- enable every proposal supported by the selected Wasmtime release;
- build a provisional Wasmtime spike before the shared executor/kernel
  prerequisite is complete;
- move the process-wide Tokio reactor into `crates/kernel` or create another
  Tokio runtime.

## 4. Current architecture

Standalone WASM is currently implemented as a JavaScript execution:

```text
standalone WASM request
  -> native sidecar process lifecycle
  -> WasmExecutionEngine
  -> JavascriptExecutionEngine
  -> V8 isolate
  -> wasm-runner.mjs
  -> WebAssembly.Module / WebAssembly.Instance
  -> JavaScript Preview1 and host_* adapters
  -> sidecar RPC
  -> AgentOS kernel, VFS, and native I/O owners
```

`WasmExecution` contains a `JavascriptExecution`, and `WasmExecutionEngine`
owns a `JavascriptExecutionEngine`. The JavaScript runner currently owns four
different kinds of code that must be distinguished during migration:

1. **ABI marshalling**: reading pointers, iovecs, strings, arrays, and structures
   from guest linear memory and writing results back.
2. **Transport adaptation**: translating imports into sidecar bridge calls and
   translating sidecar errors into Preview1 errno values.
3. **Node-WASI compensation**: descriptor shadow maps, synthetic descriptors,
   synthetic pipes, preopen collision handling, child polling, and local
   `Atomics.wait` loops required by the V8/JavaScript host topology.
4. **Actual semantics**: any behavior that still exists only in the runner and
   has not yet moved into the kernel or shared sidecar services.

The first three categories do not justify a second kernel. Category four must
be inventoried operation by operation and either moved to the shared kernel or
explicitly retained as a narrow runtime adapter behavior.

### 4.1 Current memory evidence

The current V8 runner has a 2 GiB JavaScript heap ceiling because large WASM
module compilation exceeded the ordinary 128 MiB runner heap. This is a lazy
ceiling rather than immediate resident memory, but guest-driven compilation can
approach it.

The committed local warm benchmark in
`packages/runtime-benchmarks/results/baseline-local.json` reports incremental
sidecar high-water memory above a prewarmed baseline for 19 current WASM lanes:

- 11.2 MiB minimum;
- 14.6 MiB median across all measured lanes;
- 11.2-20.0 MiB for lanes that do not intentionally move large buffers;
- up to 56.3 MiB for the measured large stream-copy lane.

These values are useful as the current warm V8-WASM acceptance baseline. They
are not a V8-versus-Wasmtime result: they include sidecar and adapter behavior,
exclude cold compilation through prewarming, and do not separate V8 isolate,
compiled code, linear memory, and kernel buffers.

## 5. Target architecture

This design extends the guest-adapter contract in
[`unified-sidecar-runtime.md`](./unified-sidecar-runtime.md): one sidecar
capability registry, one process-wide Tokio runtime, and no executor-owned
descriptor, poller, resource policy, or permission decision.

```text
clients / ACP
      |
native sidecar
      |
      +-- process lifecycle and runtime selection
      +-- process-wide Tokio runtime and native I/O owners
      +-- capability, readiness, and cancellation brokers
      +-- AgentOS kernel
      |     +-- VFS and mounts
      |     +-- fd/open-description tables
      |     +-- pipes and PTYs
      |     +-- process table and signals
      |     +-- virtual sockets and DNS policy
      |     +-- identity, permissions, and resource accounting
      |
      +-- V8 JavaScript adapter
      |     +-- JavaScript and JavaScript WebAssembly API
      |
      +-- Wasmtime standalone-WASM adapter
            +-- Engine and bounded Module cache
            +-- Store<WasmStoreState> per execution
            +-- Preview1 and host_* Linker functions
            +-- guest-memory ABI codec
```

There is no direct V8-to-Wasmtime bridge. A JavaScript process that spawns a
standalone WASM child uses the existing kernel process API. The sidecar runtime
selector starts the child under the requested standalone-WASM backend, and
stdio, signals, wait status, and exit events use the same cross-runtime process
model as every other child.

## 6. Code organization

The implemented initial organization is:

```text
crates/execution/src/wasm.rs
                            retained V8-WASM implementation and dual-backend
                            facade; keeping this working compatibility backend
                            intact avoids coupling Wasmtime to a wholesale move

crates/execution/src/wasm/
  profile.rs                shared wasmparser proposal profile

  wasmtime/
    mod.rs                  execution and engine facade
    engine.rs               Config and bounded Engine profiles
    store.rs                per-execution host state
    module.rs               validation and module loading
    cache.rs                bounded in-memory Module cache
    limits.rs               memory, stack, CPU, and cancellation
    lifecycle.rs            start, exit, traps, signals, teardown
    memory.rs               checked guest-memory ABI primitives
    error.rs                stable AgentOS outcome normalization

    linker/
      mod.rs                generated-registry trampoline and signal checkpoints
      preview1.rs
      filesystem.rs
      network.rs
      process.rs
      terminal.rs
      user.rs

    threads/                Phase 4 only; absent from initial parity
      mod.rs
      group.rs
      admission.rs
```

Rust permits `wasm.rs` and the `wasm/` submodule directory to coexist. Retaining
the established V8-WASM implementation in `wasm.rs` is deliberate: it keeps the
compatibility backend independently reviewable while new Wasmtime code is
split by responsibility. A later mechanical move under `v8_compat/` would not
change ownership or behavior and is not a prerequisite for this project.

Runtime-neutral host operations do not belong exclusively under `wasm/`.
Existing kernel and sidecar operations should be exposed through small
capability-oriented services used by both the V8 RPC adapter and Wasmtime
linker. Avoid one enormous `GuestHost` trait and avoid types named
`Javascript*` when Python and Wasmtime use the same operation.

## 7. WASI and the owned AgentOS ABI

### 7.1 Wasmtime does not force its WASI implementation

The `wasmtime` engine and `wasmtime-wasi` are separate crates. A core Wasm
module imports functions by module and function name, and a Wasmtime `Linker`
supplies whichever definitions the embedder chooses. AgentOS can therefore
provide its existing `wasi_snapshot_preview1`, `host_process`, `host_net`,
`host_user`, `host_fs`, and `host_tty` modules without installing ambient
`wasmtime-wasi` host resources.

There is no conflict between using Wasmtime as the engine and using the
AgentOS-owned WASI/POSIX ABI. The danger is only in accidentally linking a
second ambient implementation that opens host files or sockets outside the
AgentOS kernel.

### 7.2 Initial integration decision

The first implementation will:

- treat the patched sysroot and `toolchain/crates/wasi-ext` imports as the
  guest ABI source of truth;
- link exactly the Preview1 and custom-import surface generated in
  `crates/execution/assets/agentos-wasm-abi.json` and required by built
  software;
- route every resource-bearing operation to an AgentOS kernel or sidecar
  service;
- avoid constructing a default ambient `WasiCtx` with host preopens, host
  sockets, or inherited host stdio;
- generate Preview1 signatures and value layouts from the pinned checked-in
  WITX description, but do not adopt upstream resource ownership or policy;
- generate custom-import signatures and repetitive bindings from the one
  checked-in AgentOS ABI manifest instead of manually duplicating signatures in
  Rust and JavaScript.

If an upstream helper cannot be backed by AgentOS descriptors without creating
parallel state or ambient authority, the Wasmtime linker will implement that
thin ABI function directly.

## 8. Dependency on shared host services

The required refactor lives in
[`runtime-neutral-executors.md`](./runtime-neutral-executors.md). The completed
function-level inventory, baseline, and locked Phase 0 decisions live in
[`wasmtime-phase-0.md`](./wasmtime-phase-0.md). Wasmtime is admitted only after
the runtime-neutral document's exit gates close for the entire currently
supported V8-WASM ABI and working-software surface.

The Wasmtime adapter receives:

- a generation-bound execution control cell registered with the kernel process;
- a kernel PID and exit-reporter capability;
- capability-sized filesystem, network, process, terminal, signal, identity,
  clock, and entropy host services;
- a runtime-neutral coalesced wake handle and direct operation waiters; and
- typed limits, permissions, cancellation, and error mapping.

The linker does not know about `ActiveProcess`, `V8SessionHandle`, Node-WASI
fd aliases, sidecar signal maps, native Tokio handles, or mutable `KernelVm`
ownership. It decodes the owned AgentOS ABI into bounded values and calls the
shared services.

Engine-specific responsibilities remaining in this document are safe linear
memory access, async suspension, Wasmtime interruption, module validation and
caching, feature configuration, trap normalization, and execution teardown.
## 9. Async guest-memory contract

Wasmtime async host imports cannot retain a borrowed guest-memory slice or a
`Caller`-derived view across an `.await`. This is not an architectural blocker;
it defines the adapter boundary.

Every async import uses three phases:

1. **Decode and prevalidate:** validate all pointers, lengths, iovec counts, and
   output ranges; enforce byte/count limits; copy input strings, structures,
   address data, and write payloads into bounded owned Rust values.
2. **Await shared operation:** call the kernel/sidecar service using owned values
   and opaque process/capability identity. Retain no raw guest pointer, slice,
   or store borrow.
3. **Reacquire and encode:** reacquire the Wasmtime memory through the Store,
   validate output ranges again, and copy the bounded result back.

Examples:

- `fd_write` snapshots iovec metadata and payload bytes before awaiting the
  kernel write.
- `fd_read` snapshots destination iovecs, awaits an owned result buffer, then
  reacquires memory and scatters the bytes.
- `net_connect` copies the address before awaiting readiness.
- `recv`, `accept`, and DNS calls await owned results and only then write guest
  memory.
- `proc_spawn` copies command, argv, env, and actions and prevalidates the pid
  result pointer before performing the side effect.
- `waitpid` prevalidates status outputs before reaping a child.

For the initial single-threaded executor, the suspended Store cannot execute
guest code concurrently and linear memory cannot shrink. Reacquisition is still
required because memory growth can relocate backing storage. With future shared
memory, another guest thread may mutate memory while an import is suspended;
input structures and destination addresses therefore remain snapshotted once
rather than reread after the await.

This introduces an owned-buffer copy for asynchronous I/O. The current V8 path
already performs JavaScript and bridge copies, so the Wasmtime path may still
reduce total copying, but the benchmark must measure this rather than assume it.

Signal handlers follow the same memory rule and the existing cooperative
AgentOS ABI. A caught signal may run at an import, `sched_yield`, top-level
call boundary, or another declared safe point; neither V8-WASM nor Wasmtime can
inject `__wasi_signal_trampoline(i32)` into arbitrary pure guest computation
and then resume that computation. Epochs provide bounded STOP scheduling and
terminal interruption, not caught-handler injection. The adapter must:

- claim exactly one kernel delivery token, invoke the trampoline, and close or
  explicitly disarm that token before claiming the next signal; it must never
  preclaim a FIFO batch against the kernel's LIFO delivery scopes;
- validate the trampoline's exact type before accepting a user disposition and
  initialize the inherited mask through `__agentos_set_initial_sigmask` before
  `_start` and after successful exec replacement;
- keep a restartable operation's same durable waiter alive across a handler,
  so retry cannot duplicate an accept, read, write, lock, or other side effect;
- let an atomically committed or partial operation result win a simultaneous
  signal race; otherwise return `EINTR` unless every delivered handler carried
  `SA_RESTART`; and
- reacquire and revalidate memory after the handler, because the handler may
  grow or mutate linear memory. Handler trap, exit, exec, nested delivery, and
  terminal interruption each have an explicit token-cleanup path.

The shared host-operation API owns the authoritative restartability enum and
completion-versus-signal arbitration. Engine adapters do not scatter their own
restart booleans or cancel and reissue side-effecting operations.

## 10. Runtime placement and scheduling

The normative async/blocking ownership and waiter sequence are defined in
[`runtime-neutral-executors.md`](./runtime-neutral-executors.md#71-async-and-blocking-execution-contract).
Wasmtime does not introduce a scheduler exception to that contract.

Wasmtime does not provide an execution thread pool. Guest execution occurs
synchronously while a Wasmtime future is polled. Therefore:

- Wasmtime execution futures run on the bounded non-Tokio VM executor;
- async host operations use the one process-wide sidecar Tokio runtime;
- no Tokio worker synchronously enters Wasmtime guest code;
- no executor or VM creates another Tokio runtime;
- epoch/fuel yields bound uninterrupted guest work and provide cancellation
  points;
- blocking host work uses the existing fixed, bounded blocking executor with
  admission.

The Wasmtime Store carries VM id, generation, process id, permission profile,
limit ledger, cancellation state, readiness sink, and access to the shared
host-operation services. Guest-controlled payloads do not supply authority.

## 11. Safety and limits

The initial executor must preserve or improve the existing controls:

| Control | Initial Wasmtime behavior |
| --- | --- |
| Module bytes and parser work | Preserve the 256 MiB file cap and bounded import/memory/varuint parsing before compilation. |
| Linear memory | Preserve the 128 MiB default, validate declarations, enforce growth through Store limits, and account aggregate guest memory outside per-memory limits. |
| Stack | Use Wasmtime's stack cap through a bounded set of Engine profiles because stack configuration is Engine-wide while AgentOS configuration is per VM. |
| CPU and cancellation | Use epoch checks as the unbypassable interruption mechanism, but preserve the current active-CPU rather than wall-time policy: executor accounting tracks only guest-running intervals and refreshes the Store deadline after async waits. Use fuel only for an explicitly deterministic budget. |
| Wall time | Remain opt-in for interactive commands; use an outer cancellable deadline when configured. |
| Files, fds, pipes, PTYs, sockets, processes | Continue to enforce in the kernel and shared sidecar ledgers. |
| Output and queues | Preserve current bounded output, reactor, bridge, readiness, and completion limits. |
| Permissions | Omit prohibited imports at link time and repeat authorization at the kernel operation. |
| Errors | Return stable AgentOS/POSIX typed errors; do not expose engine error strings as API contracts. |

The current `maxWasmFuel` field means milliseconds in the V8 runner, not fuel.
It must not silently change meaning. Phase 1 removes it lockstep and adds
`activeCpuTimeLimitMs`, optional `wallClockLimitMs`, and optional
`deterministicFuel` as three distinct fields.
The default runaway-guest safeguard is currently 30 seconds of active V8 CPU,
while an explicitly configured `maxWasmFuel` is an opt-in wall-clock timeout.
Wasmtime must preserve that distinction: time spent blocked on terminal,
network, filesystem, child, or timer waits cannot exhaust the default active
execution budget.

## 12. Shared memory and threads

Three mechanisms must remain distinct:

1. The current runner creates small local `SharedArrayBuffer` objects so
   `Atomics.wait` can block its own V8 execution thread without busy-spinning.
2. Legacy synchronous bridges can use shared buffers to coordinate a
   JavaScript worker with a host thread.
3. WebAssembly threads use shared linear memory and atomic WASM instructions
   across multiple executing agents.

The native standalone runner does not rely on shared memory between V8 isolates
for filesystem, networking, process, or kernel state. Guest `worker_threads`
is an inert compatibility surface, not a source of real V8 worker isolates.
Removing the V8-hosted standalone runner therefore does not require sharing
memory between V8 and Wasmtime.

Wasmtime shared memory is a later milestone. Runtime support alone is
insufficient because the current AgentOS sysroot links emulated single-thread
pthreads. Real pthread support requires a threaded sysroot, a bounded
thread-spawn ABI, real mutex/condvar/TLS behavior, group cancellation, and
per-VM plus process-wide thread admission.

Wasmtime 46 does not expose a supported host hook that can replace or cancel a
guest blocked in `memory.atomic.wait`. Epoch interruption, fuel, dropping an
async call future, and dropping ordinary Store handles do not provide a hard
reap guarantee for that parked native thread. The threaded profile must
therefore run each WASM thread group in a killable worker process unless a
reviewed later Wasmtime API supplies an equivalent bounded interruption
primitive. The parent sidecar remains the sole kernel and policy authority;
the worker receives typed host operations over a bounded control lane, never
ambient filesystem or network resources. Teardown must first request an orderly
group stop, then terminate and reap the worker at a fixed deadline.

The first executor rejects modules that define or import shared memories and
does not expose a thread-spawn import, regardless of Wasmtime's compile-time
threads feature default. It must not use the experimental upstream
WASI-threads integration that can terminate the entire host process when one
guest thread traps.

## 13. Compilation cache and snapshots

The first implementation does not persist native compiled artifacts and does
not deserialize AOT files. The unsafe-artifact concern is therefore deferred,
not an initial blocker.

The allowed initial cache is a bounded, process-memory cache of trusted
`Module` values keyed by module contents and Engine profile. It improves
repeat execution within one sidecar process but does not survive restart.
Wasmtime `Module` compilation is synchronous and complete at construction;
there is no later optimizing tier. `Module` clones are shallow and the compiled
code is shareable across threads, so this cache avoids recompilation without
copying the native code image. The implementation should also benchmark caching
`InstancePre` values, which can reuse import resolution and type checking when
all closed-over imports are Store-independent.

Snapshot support is classified as follows:

- **V8 JavaScript heap snapshot:** remains available to the JavaScript
  executor; it is unrelated to a running standalone WASM process.
- **In-memory compiled Module reuse:** in scope for the first Wasmtime
  executor.
- **Wasmtime serialized/AOT module:** explicitly deferred.
- **Copy-on-write module memory initialization:** may be benchmarked after the
  basic executor works. Wasmtime's `memory_init_cow` is enabled by default and
  can use Linux memory mappings or `memfd_create` for eligible modules whose
  initial data has static, in-bounds offsets. This speeds memory initialization;
  it is not a live guest snapshot and does not require serialized AOT input.
- **Wizer build-time preinitialization:** deferred and opt-in if later useful to
  individual software packages.
- **Live Store/Instance snapshot or fork:** unsupported in the first design.

Open files, sockets, processes, timers, permissions, host capabilities, and
threads are sidecar/kernel state and cannot be captured by merely copying WASM
linear memory and globals.

## 14. Performance and memory validation

No external benchmark is accepted as the AgentOS answer because host imports,
module sizes, V8 topology, kernel bridges, and cache configuration dominate the
comparison. The repository must measure both backends under the same sidecar,
kernel, command modules, release build, and hardware.

There is no defensible numeric V8-versus-Wasmtime result yet. The existing
11.2-20.0 MiB ordinary warm-command range is the V8-WASM baseline, not an
engine comparison. The initial Wasmtime implementation and direct benchmark
are required before claiming a cold-start or resident-memory win.

The benchmark records these phases independently:

```text
sidecar and VM baseline
runtime/package projection
Engine lookup or creation
module read and validation
module compilation or in-memory cache lookup
Linker/import resolution
Store and async-stack allocation
instantiation and memory initialization
_start to first host call
first stdout byte
completion and teardown
```

The matrix includes:

- current V8-WASM cold compile and warm compile-cache paths;
- Wasmtime cold compile and warm in-memory Module-cache paths;
- trivial, coreutils, shell, curl, sqlite, vim, and large-module commands;
- compute-heavy and host-call-heavy workloads;
- concurrency 1, 10, 50, 100, and 200;
- repeated-module and diverse-module workloads;
- success, denied permission, cancellation, and resource-limit paths.

Memory measurements include process baseline, incremental RSS/PSS, peak RSS,
virtual-address reservation, committed linear-memory bytes, compiled-code
cache bytes, async stack bytes, kernel buffer bytes, page faults, and memory
retained after teardown. Large virtual reservations must not be reported as
resident memory.

On a 64-bit host, current Wasmtime defaults reserve 4 GiB of virtual address
space plus guard regions for each 32-bit linear memory so generated code can
elide many explicit bounds checks. Reservation is not committed RSS. AgentOS's
128 MiB accessible-memory policy still requires a Store resource limiter and
aggregate accounting; it does not by itself reduce Wasmtime's virtual
reservation. The initial implementation must benchmark default reservation
against a smaller reservation because reducing it can add bounds checks or
memory-growth relocation.

Wasmtime async execution also allocates a separate native stack used for stack
switching when an async host function suspends. Measure this per active Store at
the target concurrency. Configure `async_stack_size` as the selected WASM stack
cap plus 1.5 MiB of host-call headroom (2 MiB for the default 512 KiB profile),
charge the whole reservation before Store admission, and reject overflow rather
than assuming the engine default is negligible.

The pooling allocator is deferred. It can improve reuse and high-concurrency
instantiation, but requires fixed process-wide slot counts, can reserve roughly
one multi-gigabyte virtual-memory slot per admitted linear memory, and may
retain resident pages in warm slots. Start with on-demand allocation, establish
the workload and memory baseline, then evaluate pooling as an independent
optimization.

The hypotheses to test are:

- Wasmtime removes the per-execution V8 isolate and JavaScript runner heap;
- process-global V8 memory remains because JavaScript still uses V8;
- compiled Wasmtime Modules can be shared across executions in one process;
- Wasmtime may reserve more virtual address space for linear memories while
  consuming less resident adapter memory;
- direct typed host calls reduce JavaScript/bridge overhead, but owned buffers
  required across async waits still impose copying;
- threaded execution, when added, materially increases memory through one
  Store/instance/native stack per admitted thread.

Cutover requires no regression against the current warm V8-WASM memory and
latency baselines for representative commands, or an explicitly approved
tradeoff supported by measurements.

## 15. Behavioral parity and errors

AgentOS does not promise V8 error strings, Wasmtime error strings, or identical
compiler diagnostics. It does promise stable sidecar error categories and
Linux-compatible guest-visible behavior.

The adapter normalizes:

- malformed/unsupported module;
- missing or incompatible import;
- memory, table, and stack limit;
- CPU/fuel exhaustion;
- explicit termination;
- guest trap;
- host-operation errno;
- process exit and terminating signal;
- internal executor fault.

Differential tests assert stdout, stderr, exit status, errno, signal behavior,
fd inheritance, permissions, and side effects. Floating-point and proposal
behavior follow the selected published AgentOS WASM feature profile rather than
whichever features an engine happens to enable by default.

## 16. Delivery phases and JJ revision contract

Each phase below lands as one distinct JJ revision, in order. A phase may be
developed through temporary local revisions, but those revisions are folded
before handoff so the review and landing stack has one revision per phase. Do
not mix Wasmtime implementation into the prerequisite refactor revision. Do
not collapse the prerequisite refactor and Wasmtime executor into one revision.

This intentionally produces a small semantic stack instead of one revision per
capability family:

1. specification and frozen baseline;
2. complete runtime-neutral kernel/executor refactor;
3. complete initial Wasmtime executor at current V8-WASM feature parity; and
4. performance validation and preferred-backend enablement.

The later threaded-WASM project is not part of initial parity and begins in its
own revision after these four phases.

The initial Wasmtime project is complete at the end of Phase 3: Wasmtime passes
the entire current V8-WASM ABI and working-software corpus, is production-ready,
and can be the preferred backend while V8-WASM remains a supported selection.
Phase 4 expands that completed executor with threads; it is not required to
claim parity with the current single-threaded V8-WASM surface.

This section is the canonical implementation tracker. Check an item only in
the JJ revision that supplies its implementation and evidence. A phase summary
is checked only when every required item under that phase is checked and the
revision has been sealed:

- [x] Phase 0: specification, inventory, baseline, and locked decisions.
- [x] Phase 1: complete runtime-neutral kernel/executor prerequisite.
- [x] Phase 2: production Wasmtime executor at current V8-WASM parity.
- [x] Phase 3: performance decision, preferred-backend rollout, and initial
      project completion.
- [x] Phase 4: separately gated threaded-WASM roadmap completion.

### Phase 0 revision: Specification, inventory, and baseline

- [x] Land the architecture specifications without production runtime behavior
      changes.
- [x] Inventory every Preview1 and `host_*` import, including aliases, versions,
      permission tiers, memory direction, async behavior, limits, and parity
      tests.
- [x] Identify the kernel/sidecar semantic owner and JavaScript-only behavior
      for every import.
- [x] Record the current V8-WASM cold/warm latency and memory evidence on the
      canonical machine.
- [x] Freeze the differential command, raw-ABI, and hostile-module corpus.
- [x] Lock the feature profile, code placement, ABI generation, CPU fields,
      Engine/profile limits, Module-cache limits, release platforms,
      performance thresholds, selector, and deferred features in
      [`wasmtime-phase-0.md`](./wasmtime-phase-0.md).
- [x] Audit every module produced before the current canonical toolchain build
      failure and record all live legacy imports.
- [x] Seal Phase 0 as one independently reviewable JJ revision.

### Phase 1 revision: Complete the runtime-neutral executor prerequisite

- [x] Resolve the duplicate ownership symbols in the owned wasi-libc and make
      `just tools-rebuild` produce the complete canonical command set.
- [x] Add the narrow, generation-bound `ProcessRuntimeEndpoint`, durable
      bounded/coalesced control state, and executor-to-kernel exit reporter.
- [x] Register real runtime endpoints for every production V8, Python,
      binding, and compatibility-WASM process; restrict `StubDriverProcess` to
      tests and explicitly virtual processes.
- [x] Introduce the runtime-neutral execution lifecycle, control handle,
      bounded event types, and direct host-call reply handles without requiring
      V8's owned backend object to be `Send`.
- [x] Split executor-facing operations into typed filesystem, fd, network,
      process, terminal, signal, identity, clock, and entropy capabilities;
      reject a single mega-trait or executor switchboard.
- [x] Preserve typed `{ code, message, details }` errors from the kernel through
      the sidecar and adapters without string-to-errno reconstruction.
- [x] Make the kernel process table the only owner of signal dispositions,
      masks, pending state, stop/continue state, terminating state, and wait
      events.
- [x] Route external signals, `SIGPIPE`, `SIGCHLD`, PTY control signals,
      process-group signals, cancellation, and termination through one bounded
      delivery path.
- [x] Add handler begin/end checkpoints, exec disposition reset, restart
      behavior, and atomic temporary signal masks for `ppoll`.
- [x] Replace every `V8SessionHandle` readiness target with a runtime-neutral,
      bounded, coalesced execution wake handle.
- [x] Make kernel VFS, fd tables, permission tier, preopens, descriptor rights,
      rlimits, identity, umask, and mount policy the sole mutable filesystem and
      descriptor authority.
- [x] Route embedded V8 filesystem calls and module resolution through the
      shared kernel-backed filesystem service.
- [x] Delete mutable host-shadow inventories, bidirectional reconciliation,
      and adapter-owned descriptor/socket state that duplicates kernel state;
      retain host access only through explicit confined mounts and plugins.
- [x] Move shared DNS, TCP, UDP, Unix socket, TLS, options, polling, and
      readiness behavior behind the same sidecar reactor capabilities used by
      all executors.
- [x] Move spawn, exec, wait, fd actions, rlimits, locks, terminal/PTY,
      credentials, account lookup, clocks, timers, entropy, and system identity
      behind the shared operations.
- [x] Enforce the async/blocking contract: one process Tokio runtime, guest
      execution on the bounded non-Tokio executor, direct async waiters, and
      bounded admission to fixed workers for unavoidable blocking work.
- [x] Bound and account every request, reply, queue, waiter, decoded array,
      retained buffer, blocking job, deadline, fd, socket, process, PTY, and
      guest-visible output path; warn near limits and return named typed limit
      errors.
- [x] Fix process-aware DAC/sticky/read-only checks for link, remove, rename,
      symlink, and unlink operations.
- [x] Reject oversized writes, iovecs, subscriptions, pollfds, groups, argv,
      env, paths, records, and result encodings before allocation, copying, or
      side effects.
- [x] Return `ERANGE` with required lengths for short account buffers and cap
      supplementary groups before reading guest memory.
- [x] Correct the `socketpair(kind, nonblock, cloexec)` ABI and implement the
      existing bounded kernel `pty_open` path.
- [x] Make fd xattrs and metadata operate on canonical open descriptions after
      rename/unlink; remove ambient Node filesystem fallbacks and sentinel
      errors.
- [x] Replace terminal fd caches and libc shadow state with live kernel
      terminal identity, termios, foreground-group, resize, and raw-mode state.
- [x] Generate and check in the pinned Preview1 types/layouts and AgentOS custom
      ABI manifest used by both adapters and import-audit tooling. The generated
      runtime-neutral registry must cover all 169 manifest imports, 29 core
      signatures, 40 `wasi_unstable` aliases, and 110 supported semantic binding
      groups, including handler/codec identity, execution class,
      restartability, return convention, permissions, and transactional
      prevalidation metadata.
- [x] Rebuild and inspect every owned-sysroot command; require zero undeclared
      imports and explain or remove every compatibility alias/version.
- [x] Pass the raw ABI, software, filesystem, process/signal, network, terminal,
      identity/system, hostile-import, and resource-attack suites listed in
      [`wasmtime-phase-0.md`](./wasmtime-phase-0.md#9-required-differential-proof-suites)
      through V8-WASM.
- [x] Pass all exit gates in
      [`runtime-neutral-executors.md`](./runtime-neutral-executors.md#13-exit-gates)
      for V8, Python, and compatibility WASM.
- [x] Verify common host services contain no V8, JavaScript, Python, or
      Wasmtime types and that the Phase 1 tree contains no Wasmtime executor.
- [x] Seal all Phase 1 work as one independently reviewable JJ revision on top
      of Phase 0.

### Phase 2 revision: Add Wasmtime at full current feature parity

- [x] Pin one reviewed Wasmtime version and revalidate the referenced API
      defaults, safety contracts, supported platforms, and Cargo feature set.
- [x] Add the multi-file `crates/execution/src/wasm/wasmtime/` Engine, Store,
      Module cache, Linker, ABI, memory, error, interruption, and execution
      modules without creating a new crate or giant source file.
- [x] Configure a bounded process-wide Engine registry keyed by the exact
      AgentOS feature profile and stack cap; enforce the eight-profile default
      limit and 80% warning.
- [x] Prevalidate every module with the shared `wasmparser` profile so V8-WASM
      and Wasmtime accept and reject the same features independently of engine
      defaults.
- [x] Add the bounded per-Engine 32-entry/256 MiB charged in-memory Module LRU,
      exact cache keys, metrics, and eviction behavior; never deserialize
      native artifacts.
- [x] Build Store context from trusted VM generation, kernel PID, permission
      profile, limit ledger, cancellation state, and shared host-service
      handles only.
- [x] Enforce linear-memory, table, instance, stack, aggregate memory, active
      CPU, optional wall-clock, deterministic-fuel, and interruption limits
      with typed errors.
- [x] Implement epoch-based termination and active-CPU accounting that pauses
      while an import is asynchronously waiting.
- [x] Implement cooperative caught-signal delivery at import/safe-point
      boundaries with exact trampoline validation, inherited-mask setup,
      one-at-a-time LIFO token settlement, nested delivery, and shared
      completion/partial-result versus `SA_RESTART` arbitration; use epochs
      only for STOP scheduling and terminal interruption.
- [x] Generate and link the owned Preview1 ABI, `wasi_unstable` alias, and every
      `host_fs`, `host_net`, `host_process`, `host_tty`, and `host_user`
      function/version over the Phase 1 shared operations using the generated
      registry and one dynamic `func_new_async` trampoline; no handwritten
      import-name switchboard is permitted.
- [x] Do not create a `wasmtime-wasi` context or install ambient filesystem,
      network, process, environment, clock, random, or stdio capabilities.
- [x] Apply the three-phase async guest-memory contract to every waiting import:
      validate/copy bounded input, await with no guest borrow, then reacquire
      and revalidate output before commit.
- [x] Prevalidate all output ranges before side effects and make fd/resource
      allocation transactional when result encoding can fail.
- [x] Run guest code only on the bounded non-Tokio VM executor while async host
      work continues to use the one sidecar Tokio runtime and its direct waiters.
- [x] Normalize validation failures, traps, stack exhaustion, cancellation,
      timeout, fuel exhaustion, exit, terminating signal, errno, and internal
      faults into stable AgentOS typed outcomes.
- [x] Add the optional sealed `wasmtime`/`v8` protocol and client selector;
      omission remains the sidecar-owned V8 default during Phase 2.
- [x] Keep V8 permanently for JavaScript and keep V8-WASM as an independent,
      maintained compatibility backend; add no V8-to-Wasmtime bridge.
- [x] Keep shared memory, threads, memory64, multi-memory, relaxed SIMD, tail
      calls, GC/function references, components, custom page sizes,
      AOT, pooling, Wizer, and live snapshots disabled for initial parity.
      Enable finalized core exception tags/instructions and translate LLVM
      19's legacy DuckDB encoding with checksum-verified Binaryen 128 during
      the owned toolchain build; Wasmtime's compiler intentionally does not
      accept the legacy encoding.
- [x] Pass the complete differential ABI and working-software corpus—including
      `ls`, `vim`, `grep`, `curl`, shell pipelines, sqlite, git, tar/gzip, and
      metadata tools—against both standalone-WASM backends.
- [x] Pass permission-tier, errno, malformed-module, hostile-import,
      cancellation, signal, fd/process/TTY/network, and every limit-at/over-bound
      test against Wasmtime with no ambient-host escape.
- [x] Pass full Linux x86-64 conformance plus Linux arm64 and macOS x86-64/arm64
      build and smoke/conformance release gates; keep browser builds out of
      scope.
- [x] Verify teardown releases Store, waiter, fd, socket, process, memory,
      compiled-code, and kernel reservations without cross-VM state retention.
- [x] Seal the complete executor and parity/safety proof as one independently
      reviewable Phase 2 JJ revision on top of Phase 1; do not land a partial
      linker or spike.

Phase 2 evidence (Rust 1.94.0, Linux x86-64 canonical workspace):

- `just tools-rebuild` rebuilt and audited all 166 default commands; the
  required focused Vim build raised the corpus to 167 commands/136 modules,
  with all 145 live imports declared in the 169-function/29-signature ABI.
- Native workspace check, all-target strict clippy, formatting, protocol tests,
  client tests, fixed-version checks, protocol-inventory checks, TypeScript
  request mapping, and workflow YAML parsing passed. Root `pnpm check-types`
  passed all 146 tasks.
- Wasmtime units passed 15/15; architecture guards 61/61; safety/limits/ambient
  denial 7/7; raw differential ABI 9/9; and the serial real-software corpus
  5/5 in 220.82 seconds (`ls`, real HTTP `curl`, `grep`, sqlite, git, tar/gzip,
  metadata, shell/child affinity, and focused Vim).
- Release gates pin the reviewed Wasmtime MSRV and require Linux x86-64/arm64
  builds plus native macOS x86-64/arm64 smoke tests before assets can publish;
  browser entrypoints remain excluded.

### Phase 3 revision: Measure and enable the preferred backend

- [x] Run release builds of both backends on the same canonical machine with
      identical module bytes, host-service path, output capture, permissions,
      limits, and cache state.
- [x] Measure at least five independent fresh-cache processes with five samples
      each, plus warm cache-hit runs, for trivial, coreutils, shell, curl,
      sqlite, vim, large-module, compute-heavy, and host-call-heavy workloads.
- [x] Run concurrency 1/10/50/100/200 with repeated-module and diverse-module
      workloads, success, denial, cancellation, and resource-limit paths.
- [x] Record phase timing for VM/package setup, Engine lookup, module read,
      validation, compilation/cache, linking, Store/async stack, instantiation,
      memory initialization, first host call, first output byte, completion,
      and teardown.
- [x] Record baseline/incremental/peak RSS and PSS, VIRT separately, committed
      linear memory, compiled-code/cache bytes, async-stack bytes, kernel
      buffers, page faults, and retained memory after teardown.
- [x] Benchmark on-demand memory allocation and eligible copy-on-write module
      memory initialization; do not enable pooling, AOT, Wizer, or live
      snapshots in this phase.
- [x] Tune only evidence-supported Engine, Module-cache, memory-reservation,
      async-stack, and concurrency defaults while retaining all named bounds.
- [x] Require zero correctness/safety regression, geometric-mean p50 regression
      no worse than 10%, no individual p95 regression worse than 20%, throughput
      regression no worse than 10%, and retained RSS/PSS regression no worse
      than the greater of 10% or 4 MiB.
- [x] If every preferred-backend threshold passes, make omission select
      Wasmtime; otherwise keep omission on V8 while leaving Wasmtime explicitly
      selectable and record the failed thresholds.
- [x] Add operator metrics, warnings, explicit backend override, rollback
      control, cache/profile-limit visibility, and stable error attribution.
- [x] Keep V8-WASM selectable, supported, and on the shared parity suite; never
      shadow-run side-effecting executions through both engines.
- [x] Commit the raw benchmark results, selected defaults, threshold decision,
      rollback criteria, and operator documentation with the implementation.
- [x] Seal Phase 3 as one independently reviewable JJ revision on top of Phase
      2. Completion of this checkbox means the initial production Wasmtime
      project is complete.

Phase 3 evidence (Rust 1.94.0, release profile, Linux x86-64 canonical
workspace, 2026-07-20 Pacific):

- The canonical matrix used the same release sidecar, host-service route,
  permissions, limits, output capture, and SHA-256-inventoried source modules
  for both engines. V8's existing safety transform adds a maximum to an
  uncapped memory section (two bytes for this corpus); diagnostics record both
  the identical source size and the transformed executable size.
- Five fresh sidecar processes per engine ran five samples for each of nine
  distinct workload modules, including a real recursive `find` host-call case.
  Per-execution diagnostics record setup, Engine, read, profile validation,
  compile/cache, Linker, Store/async stack, Instance (including memory
  initialization), first host call, first guest host call, first output,
  completion, teardown, guest linear memory, and Store reservations.
- Repeated and diverse 1/10/50/100/200 concurrency rows completed. The 20-way
  executor bound and 128-frame ingress bound produced their documented typed
  admission failures above capacity; every comparable successful Wasmtime row
  exceeded V8 throughput. Permission denial, abort cancellation, and active-CPU
  limit paths passed for both engines.
- Correctness, geometric-mean p50 (`0.2972` Wasmtime/V8), and throughput gates
  passed. Individual p95 failed because cold compilation dominates substantial
  modules. Retained RSS failed (`127,930,368` V8 versus `264,069,120` Wasmtime)
  and retained PSS failed (`129,094,656` versus `264,836,096`). Omission
  therefore remains V8; explicit Wasmtime and V8 overrides remain available.
- No Engine, module-cache, memory, async-stack, or concurrency default was tuned:
  the evidence supports Wasmtime for repeated warm modules but did not identify
  a bounded default change that clears the cold-p95 and retained-memory gates.
  Pooling, AOT, Wizer, serialized artifacts, and live snapshots remain off;
  on-demand allocation and eligible copy-on-write initialization remain on.
- Resource snapshots expose live WASM reservations, Engine profiles, cache
  entries/hits/misses/evictions, source and charged cache bytes, compile time,
  and whole-process Linux RSS. Near-limit warnings and stable typed failures
  name their configuration bounds. Rollback is the explicit V8 selector or
  removal of a Wasmtime override.
- The readiness correction found during measurement passed 200 V8 curl samples
  across 20 fresh sidecars (plus 200 matching Wasmtime samples) without the
  former 30-second lost-wake failure. The canonical nine-workload result then
  completed with zero validation failures.
- Raw samples and environment/module provenance are committed in
  `packages/runtime-benchmarks/results/wasm-backend-comparison.json`; operator
  interpretation, override, rollback, metrics, cold-start, memory, and snapshot
  guidance is in `docs/wasmvm/executors.md`.

### Phase 4 revision: Threading as a separate later project

- [x] Confirm Phase 3 is complete before enabling shared memory or threads.
- [x] If threading cannot remain reviewable as one revision, approve a
      replacement multi-revision threading specification before implementation;
      do not silently fragment this phase.
- [x] Rebuild the owned sysroot and libc for real pthread semantics instead of
      the current emulated single-thread implementation.
- [x] Add an explicit AgentOS thread-spawn ABI and enable the exact shared-memory
      and atomic WASM feature profile only for configured threaded executions.
- [x] Implement bounded per-VM and process-wide thread admission, one accounted
      Store/instance/native stack per admitted guest thread where required, and
      transactional failure when capacity is unavailable.
- [x] Isolate each threaded WASM thread group in a killable worker process,
      keep AgentOS kernel state in the parent, use bounded typed host-operation
      IPC, and prove fixed-deadline termination/reaping for a guest parked in
      `memory.atomic.wait`.
- [x] Implement pthread mutex, condition variable, TLS, join/detach, exit,
      cancellation, robust teardown, and required libc behavior.
- [x] Move masks and in-progress signal delivery to per-thread kernel records
      while retaining process-wide dispositions and correct process/thread
      signal selection.
- [x] Define shared-memory ownership, growth, atomic wait/notify, limits,
      retained-memory accounting, and cross-thread guest-memory mutation rules.
- [x] Make trap, exit, cancellation, timeout, and VM teardown terminate and reap
      the complete thread group without terminating or corrupting the sidecar.
- [x] Pass pthread/libc, signal, shared-memory, race, resource-exhaustion,
      teardown, isolation, and high-concurrency memory tests for hostile VMs.
- [x] Re-run the full single-thread parity and performance gates to prove the
      threaded profile does not regress ordinary V8-WASM or Wasmtime execution.
- [x] Keep browser support, AOT artifacts, Wizer, components, pooling, and live
      process snapshot/fork outside the threading milestone unless separately
      specified and approved.
- [x] Seal the approved threading implementation and conformance evidence in
      its own JJ revision or approved replacement revision stack. Completion of
      this checkbox means the full roadmap in this document is complete.

Phase 4 evidence (Rust 1.94.0, release performance profile, Linux x86-64
canonical workspace, 2026-07-21 Pacific):

- The explicit `wasmtime-threads` selector uses the sealed
  `AgentOsOwnedWasiV1Threads` profile. Plain `wasmtime` and V8-WASM remain
  single-threaded and continue rejecting shared memory, atomics, and
  `wasi.thread-spawn`. JavaScript remains on V8 and there is no V8/Wasmtime
  memory bridge.
- Each configured threaded group starts in its own native-sidecar worker
  process. The child owns only Wasmtime Engine/Store/Instance/shared-memory and
  guest-thread state; the parent retains the kernel, VFS, descriptors, sockets,
  permissions, processes, and signal dispositions. Bounded CBOR frames carry
  typed owned host operations, results, signals, stderr, group failures, and a
  final completion acknowledgement. The child has two fixed IPC support
  threads and one process-level Tokio runtime rather than per-operation threads
  or per-thread/subsystem runtimes.
- Admission reserves the complete configured group before guest entry:
  per-VM and process-wide thread capacity, one Store/Instance/native stack per
  guest thread, table space, and the maximum shared-memory envelope. The
  group-owned `SharedMemory` supplies growth, atomic wait/notify, and
  cross-Store mutation; every reservation is released on all teardown paths.
- The owned pthread sysroot/libc passes the generated mutex, condition
  variable, TLS, join, detach, exit, and cooperative-cancellation conformance
  program. Kernel signal records now keep masks, temporary `ppoll` masks, and
  in-progress handler state per thread while retaining process-wide
  dispositions and deterministic process-directed thread selection.
- The serial threaded safety suite passed 20 default tests plus its generated
  pthread/libc test. It covers shared-memory growth/visibility, a four-thread
  atomic race, transactional resource exhaustion, per-thread signals, eight
  concurrent isolated groups, secondary-thread traps, process exit, timeout,
  `SIGKILL`, VM disposal, and threads parked indefinitely in
  `memory.atomic.wait`; fixed-deadline group reaping leaves the sidecar usable.
- The ordinary single-thread raw-ABI suite passed 9/9 and the real-software
  parity suite passed 6/6 (`ls`, loopback `curl`, the direct corpus,
  shell/children, Vim, and the release command set). Wasmtime units and
  architecture guards passed. Strict all-target native Clippy, Rust formatting, the native
  workspace check, fixed-version/package/protocol inventory checks, all 54
  JavaScript build tasks, and all 146 type-check tasks passed.
- The post-threading canonical single-thread performance matrix completed with
  zero correctness failures. Geometric-mean p50 (`0.2741` Wasmtime/V8) and
  throughput passed; individual cold p95, retained RSS (`161,894,400` V8 versus
  `256,995,328` Wasmtime), and retained PSS (`162,531,328` versus `257,593,344`)
  failed. V8 therefore remains the omission/default and rollback backend. Raw
  evidence is committed in
  `packages/runtime-benchmarks/results/wasm-backend-comparison-phase4.json`.
- Browser entrypoints remain dormant and excluded. AOT/serialized artifacts,
  Wizer, components, pooling, and live process snapshots/fork remain disabled.
  The repository-wide package-layout and fixed-version checks pass.

## 17. Principal risks

| Risk | Severity | Required mitigation |
| --- | --- | --- |
| Porting JavaScript compatibility state as a second kernel | Critical | Inventory each operation; move semantics to the kernel/shared service; keep linker code to ABI marshalling. |
| Accidentally installing ambient Wasmtime WASI resources | Critical | Link only AgentOS-owned imports and test host-escape denial. |
| Raw imports bypass the filesystem permission tier | Critical | Put the effective tier and descriptor rights in kernel process state; test malicious direct imports. |
| Retaining guest-memory borrows across await | Critical | Enforce the three-phase owned-value memory contract and focused tests. |
| Side effect succeeds before an invalid result pointer is detected | High | Prevalidate all result ranges before spawn/reap/write-like side effects and specify commit ordering. |
| Readiness remains tied to `V8SessionHandle` | High | Add runtime-neutral bounded execution readiness sinks. |
| Separate Wasmtime and kernel descriptor namespaces | High | Use the kernel fd table directly and delete Node-WASI shadow descriptor machinery. |
| Signal masks, dispositions, and pending state remain split across three layers | Critical | Consolidate a bounded runtime-neutral signal broker before Wasmtime admission; test `SIGPIPE`, `SIGCHLD`, `ppoll`, and stop/continue. |
| An adapter preclaims multiple caught signals against the kernel's LIFO delivery scopes | Critical | Claim, invoke, and settle exactly one token at a time; test two-signal ordering, nested `SA_NODEFER`, `SA_RESETHAND`, exec, exit, and trap cleanup. |
| Ambient clocks, randomness, hostname, procfs, or devfs leak host state | Critical | Link owned providers only and add hostile raw-import escape tests. |
| Runner-local identity and rlimits become Wasmtime Store state | High | Move mutable process state into the kernel and keep only process identity handles in the Store. |
| Kernel errno is converted to text and reparsed by adapters | High | Carry typed error code/message/details across the shared boundary. |
| Engine-specific behavior leaks into public errors | High | Normalize typed executor errors and test guest-visible errno/status. |
| `maxWasmFuel` silently changes meaning | High | Phase 0 replaces it lockstep with active CPU time, optional wall-clock time, and optional deterministic fuel fields. |
| Claimed memory win is based on heap ceilings or VIRT | High | Measure RSS/PSS, peaks, cache retention, and virtual reservations separately. |
| Default 4 GiB-per-memory virtual reservations exhaust address space under high concurrency | High | Account and cap memories process-wide; benchmark reservation settings; defer pooling. |
| Async Wasmtime stacks create unaccounted per-execution RSS | High | Bound `async_stack_size`, include it in admission, and measure at concurrency gates. |
| Wasmtime compile/binary cost affects all builds | Medium | Measure; use Cargo feature composition or later crate extraction only if justified. |
| Permanent dual backends drift | High | Run the same owned-ABI and software parity corpus against both in CI; keep all Linux semantics below the adapters; version one explicit engine feature profile. |
| Thread support is mistaken for pthread compatibility | Critical | Keep threads out of initial scope and require a separate sysroot/runtime milestone. |
| A threaded guest parks forever in `memory.atomic.wait` | Critical | Put each threaded thread group in a killable worker process and prove deadline-bounded whole-group reaping. |
| Fake libc terminal state diverges from kernel PTYs | High | Replace process-global termios/pgid/winsize stubs with live typed kernel operations. |

## 18. Resolved implementation decisions

Phase 0 resolved the feature profile, code placement, ABI generation, CPU limit
fields, Engine profile bound, Module cache limits, release platforms,
performance thresholds, backend selector, and deferred threading/snapshot
scope. The decisions in
[`wasmtime-phase-0.md`](./wasmtime-phase-0.md#11-locked-implementation-decisions)
are normative. The initial implementation has no unresolved architectural
question; new evidence changes a decision through a specification revision.

## Appendix A: Research and inventory sources

The subsystem inventory was performed against these current implementation
owners:

- standalone WASM adapter and runner:
  `crates/execution/src/wasm.rs`,
  `crates/execution/assets/runners/wasm-runner.mjs`, and
  `crates/execution/assets/runners/wasi-module.js`;
- ABI declarations and libc behavior:
  `toolchain/crates/wasi-ext/src/lib.rs`, `toolchain/std-patches/`, and
  `toolchain/std-patches/wasi-libc-overrides/`;
- kernel semantics: `crates/kernel/src/kernel.rs`, `process_table.rs`,
  `pty.rs`, `user.rs`, `device_layer.rs`, and socket/VFS modules;
- runtime lifecycle and external I/O:
  `crates/native-sidecar/src/execution/`, `state.rs`, `filesystem.rs`, and
  `service.rs`; and
- current performance evidence:
  `packages/runtime-benchmarks/results/baseline-local.json`.

The Wasmtime API conclusions were checked against the current upstream API:

- [`wasmtime::Module`](https://docs.wasmtime.dev/api/wasmtime/struct.Module.html)
  for synchronous compilation, cheap cloning, thread-safe sharing, and unsafe
  serialized-artifact loading;
- [`wasmtime::Config`](https://docs.wasmtime.dev/api/wasmtime/struct.Config.html)
  for proposal flags, epochs/fuel, async stack configuration, memory
  reservation, and copy-on-write initialization;
- [`wasmtime::Memory`](https://docs.wasmtime.dev/api/wasmtime/struct.Memory.html)
  for borrow, relocation, and shared-memory safety rules;
- [`wasmtime::Linker`](https://docs.wasmtime.dev/api/wasmtime/struct.Linker.html)
  and [`InstancePre`](https://docs.wasmtime.dev/api/wasmtime/struct.InstancePre.html)
  for Store-independent host functions and reusable import resolution;
- [`StoreLimitsBuilder`](https://docs.wasmtime.dev/api/wasmtime/struct.StoreLimitsBuilder.html)
  for per-memory and per-Store limits;
- [Wasmtime async execution](https://docs.wasmtime.dev/api/wasmtime/#asynchronous-wasm)
  for embedder-owned scheduling and native stack switching;
- [`wasmtime-wasi` Preview1 linker integration](https://docs.wasmtime.dev/api/wasmtime_wasi/p1/fn.add_to_linker_async.html)
  for the optional `WasiP1Ctx` ownership model that AgentOS is not adopting;
- [`PoolingAllocationConfig`](https://docs.wasmtime.dev/api/wasmtime/struct.PoolingAllocationConfig.html)
  for address-space reservation and resident warm-slot tradeoffs; and
- the upstream
  [`wasmtime-wasi-threads` implementation](https://docs.wasmtime.dev/api/src/wasmtime_wasi_threads/lib.rs.html)
  for the process-exit behavior that is unsuitable for an in-process
  multi-tenant embedding.

These links describe the current upstream release at the time of writing. The
implementation must pin an exact Wasmtime version and revalidate all referenced
defaults and safety contracts during dependency review.
