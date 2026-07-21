# Standalone WebAssembly executors

AgentOS has two maintained native standalone-WebAssembly engines: V8-WASM and
Wasmtime. Wasmtime has separate single-threaded and threaded execution
profiles. JavaScript always remains in V8; the selector described here only
chooses the engine/profile for a standalone WASM command. The engines do not
share guest memory and there is no V8-to-Wasmtime bridge.

## Production decision

Omitting the selector currently chooses **V8**. Wasmtime is production-ready
and explicitly selectable, but the July 2026 post-threading canonical
comparison did not pass the cold-start p95 or retained RSS/PSS gates.
Warm Wasmtime module-cache hits were generally much faster than V8-WASM, so an
explicit Wasmtime selection is appropriate for a process expected to reuse a
small module set. It is not yet the safe fleet-wide default for cold or
module-diverse traffic.

Select a backend per execution:

```ts
await vm.execCommand("ls", ["-la"], { wasmBackend: "wasmtime" });
await vm.execCommand("threaded-tool", [], {
  wasmBackend: "wasmtime-threads",
});
await vm.execCommand("ls", ["-la"], { wasmBackend: "v8" });
```

The selector is sealed to `"wasmtime" | "wasmtime-threads" | "v8"`.
`"wasmtime"` remains the exact single-threaded profile and rejects shared
memory, atomics, and the AgentOS thread-spawn import. Omission is the
sidecar-owned default; clients must not invent another default. The immediate
rollback for a Wasmtime workload is an explicit `wasmBackend: "v8"`. A fleet
rollback is to omit Wasmtime overrides; no code, cache, or data migration is
required.

## Shared host behavior and safety

Both executors use the same sidecar-owned kernel, VFS, process table, fd and
socket tables, signal broker, permissions, and resource ledger. Wasmtime links
the AgentOS-owned Preview1/POSIX ABI directly. It does not construct a
`wasmtime-wasi` context and receives no ambient host filesystem, network,
process, environment, clock, random, or stdio authority.

All filesystem and network waits use owned request/result buffers. Wasmtime
validates and copies guest input before awaiting, holds no guest-memory borrow
across the wait, then reacquires and revalidates every output range before
commit. Guest execution runs on the bounded non-Tokio VM executor; host I/O
continues on the process's one Tokio runtime.

The threaded profile puts each guest thread group in a dedicated killable
worker process. Wasmtime Engines, Stores, Instances, shared linear memory, and
native guest threads live in that child; the kernel, VFS, descriptors, sockets,
permissions, process table, and signal dispositions remain authoritative in
the parent. A bounded typed protocol carries owned host-operation values over
stdio. The child receives no ambient host capability, and guest memory is
never mapped into the parent as an IPC shortcut.

V8-WASM remains on the same parity and safety suite. AgentOS never shadow-runs
a side-effecting command through both engines.

## Metrics and warnings

`getResourceSnapshot()` exposes these process/VM diagnostics:

| Field | Meaning |
| --- | --- |
| `wasmReservedMemoryBytes` | Live ledger charge for WASM linear memory, tables, and Wasmtime async stacks. It must return to zero after execution teardown. |
| `wasmtimeEngineProfiles` | Process-wide exact Engine profiles retained for distinct stack/feature configurations. |
| `wasmtimeModuleEntries` | Compiled modules currently retained across Engine-profile caches. |
| `wasmtimeModuleCacheHits`, `wasmtimeModuleCacheMisses`, `wasmtimeModuleCacheEvictions` | Cumulative cache behavior. A rising miss or eviction rate predicts cold-start latency. |
| `wasmtimeCompiledSourceBytes` | Cumulative source bytes compiled; this is a counter, not current resident memory. |
| `wasmtimeChargedModuleBytes` | Conservative current compiled-module cache charge used for bounded admission/eviction. |
| `wasmtimeCompileTimeMicros` | Cumulative synchronous Wasmtime compilation time. |
| `wasmtimeProcessRetainedRssBytes` | Whole-sidecar process RSS sampled on Linux. It is intentionally not presented as Wasmtime-only RSS. |

Operators should alert on sustained module-cache misses/evictions, nonzero
`wasmReservedMemoryBytes` after all executions drain, or profile counts near
the configured bound. The runtime emits host-visible warnings before the
bounded Engine-profile and module-cache limits and typed errors at the limit:

- `WARN_AGENTOS_WASMTIME_ENGINE_PROFILES_NEAR_LIMIT` / `ERR_AGENTOS_WASMTIME_ENGINE_PROFILE_LIMIT`
- `WARN_AGENTOS_WASMTIME_MODULE_CACHE_NEAR_LIMIT` / `ERR_AGENTOS_WASMTIME_MODULE_CACHE_LIMIT`
- `WARN_AGENTOS_WASMTIME_LIMIT_WARNING` for aggregate Store reservations
- `WARN_AGENTOS_RESOURCE_NEAR_LIMIT` / `ERR_AGENTOS_WASM_THREAD_LIMIT` for
  per-VM and process-wide threaded-WASM admission

Limit errors include `limitName`, `limit`, and `observed` details where those
values apply. Guest traps use `ERR_AGENTOS_WASM_TRAP` plus a stable `trapKind`;
memory, table, stack, active-CPU, fuel, wall-clock, cancellation, invalid-module,
and instantiation outcomes have stable AgentOS codes. Raw Wasmtime validation
or trap strings are private diagnostics and are not an API contract.

## Compilation, cold start, and memory

The Wasmtime backend performs ordinary in-process compilation and keeps a
bounded, SHA-256-keyed `Module` LRU per exact Engine profile. Cache input is the
original trusted module bytes and the configured feature/stack profile; there
is no serialized or externally supplied native artifact.

The production configuration uses on-demand linear-memory allocation and
Wasmtime's eligible copy-on-write module-memory initialization. It does **not**
use pooling allocation, AOT deserialization, Wizer, or a live Store/Instance
snapshot. Wasmtime does not provide a general live-process snapshot/fork for
AgentOS: copying linear memory alone would omit kernel process, fd, socket,
signal, waiter, and thread state. V8's JavaScript heap snapshot remains a
JavaScript startup optimization and is not a cross-engine WASM snapshot.

Memory figures must be kept separate:

- RSS/PSS measure committed process memory; the benchmark records baseline,
  peak, end, and retained-after-teardown values from `/proc`.
- VIRT includes Wasmtime guard/address-space reservations and is not committed
  memory. It can rise sharply at concurrency without an equivalent RSS rise.
- `guestLinearMemoryBytes`, `asyncStackBytes`, and `reservedStoreBytes` are
  recorded per Wasmtime execution in opt-in phase diagnostics.
- compiled-module charge and kernel buffered bytes are reported separately;
  neither is guest linear memory.
- an active `wasmtime-threads` group also has a dedicated child-process image,
  two fixed IPC support threads, and one Store/Instance/native stack for each
  admitted guest thread. This overhead is intentionally not conflated with the
  ordinary single-thread Wasmtime RSS numbers below.

## Canonical benchmark and rollback criteria

The latest raw release result is
[`packages/runtime-benchmarks/results/wasm-backend-comparison-phase4.json`](../../packages/runtime-benchmarks/results/wasm-backend-comparison-phase4.json).
It uses identical hashed source modules and host-service paths on one machine,
five independent sidecar processes per engine, five samples per workload, and
warm cache hits. V8 adds its existing two-byte memory-maximum rewrite before
compilation; both the identical source byte count and the transformed V8 byte
count are retained in phase diagnostics.

The matrix covers trivial, coreutils, shell pipeline, loopback curl, sqlite,
Vim, large-module git, compute-heavy SHA-256, and host-call-heavy filesystem
work, plus repeated/diverse concurrency at 1/10/50/100/200 and permission
denial, cancellation, and CPU-limit paths.

The canonical result completed with this decision table:

| Gate | Result | Evidence |
| --- | --- | --- |
| Correctness and safety | Pass | Zero V8 or Wasmtime workload validation failures; denial, cancellation, and CPU-limit paths passed. |
| Geometric-mean p50 | Pass | Wasmtime/V8 ratio `0.2741` (about 73% lower latency across the mixed sample set). |
| Individual p95 | **Fail** | Cold Wasmtime compilation dominates substantive-module p95; several workload ratios exceed the `1.20` ceiling. |
| Throughput | Pass | Wasmtime exceeded V8 on every comparable repeated/diverse row through concurrency 100; at 200, Wasmtime completed admitted work while V8 produced its typed admission outcome. |
| Retained RSS | **Fail** | V8 `161,894,400` bytes; Wasmtime `256,995,328` bytes. |
| Retained PSS | **Fail** | V8 `162,531,328` bytes; Wasmtime `257,593,344` bytes. |

Across workload medians, Wasmtime cold p50 ranged from slightly faster than V8
for the trivial module to about 13× slower for Vim; warm p50 was about
2.8–5.2× faster. These results support explicit warm-cache use, but the failed
p95 and retained-memory gates require the omission default to remain V8.

Run it from the repository root with a release sidecar and rebuilt canonical
commands:

```bash
AGENTOS_SIDECAR_BIN=/absolute/path/to/release/agentos-native-sidecar \
AGENTOS_WASM_COMMANDS_DIR=/absolute/path/to/packages/runtime-core/commands \
pnpm --dir packages/runtime-benchmarks bench:wasm-backends
```

Wasmtime may become the omission default only when the same canonical run has
zero correctness/safety regressions and passes every locked threshold:

- geometric-mean p50 no more than 10% slower;
- no individual p95 more than 20% slower;
- throughput no more than 10% lower;
- retained RSS and PSS no more than the greater of 10% or 4 MiB above V8.

Keep or restore V8 as the default when any threshold fails, a stable typed
outcome diverges, cache misses become the dominant traffic shape, Store/kernel
resources do not drain, or Wasmtime causes a production safety regression.
An individual workload can still opt into Wasmtime when its own warm-cache and
memory evidence supports that choice.

## Threads

Shared WebAssembly memory and pthreads are enabled only by the explicit
`"wasmtime-threads"` profile. AgentOS does not rely on shared memory between V8
isolates, and no memory is shared between V8 and Wasmtime. Threaded programs use
the owned AgentOS sysroot/libc and import `wasi.thread-spawn`; each pthread gets
its own Wasmtime Store and Instance over one group-owned `SharedMemory`.

Before any guest code runs, admission transactionally reserves the configured
`limits.wasm.maxThreads` (including the initial thread), maximum shared-memory
envelope, table capacity, async stacks, and native thread capacity. The
per-group default is 16 threads, `limits.wasm.maxConcurrentThreads` bounds the
aggregate reservations of concurrent groups in one VM at 64 by default, and
the process-wide default is 256. Capacity failure is typed and starts no
partial group. Shared-memory growth stays within the pre-reserved maximum, and
atomics, wait/notify, and cross-Store mutation operate on the group-owned
memory.

Signal dispositions and process-pending signals remain process-wide in the
kernel. Masks, temporary `ppoll` masks, and in-progress handler state are
per-thread; process-directed delivery deterministically selects an unblocked
thread. Trap, process exit, cancellation, timeout, `SIGKILL`, and VM disposal
terminate and reap the entire child process, including threads indefinitely
parked in `memory.atomic.wait`, without terminating the sidecar.

The threaded libc covers mutexes, condition variables, TLS, join/detach,
thread exit, and cooperative cancellation. Live Store/Instance snapshots,
cross-engine memory, AOT artifacts, Wizer, pooling, components, and browser
execution remain unsupported. Main-thread exit tears down the group; detached
guest threads do not outlive the command.
