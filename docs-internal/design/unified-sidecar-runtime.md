# Unified Sidecar Runtime and Node-Compatible I/O Pump

Status: implemented; Appendix B exit gates closed by the validation record below

Audience: AgentOS runtime, kernel, execution, bridge, and guest-adapter owners

Reference implementation studied: Node.js at
fbf82766d623fd9855fdb2fde32aeb6794af84e9

## 1. Decision

AgentOS will use one process-owned, multi-thread Tokio runtime for all trusted
asynchronous work in a sidecar process. Trusted per-VM orchestration, native
networking backends, the kernel readiness adapter, protocol tasks, timers,
signal paths, TLS connections, and HTTP/2 connections share that runtime. The
VMs themselves do not execute on it.

Guest execution does not run on Tokio workers. V8 is synchronous,
thread-affine, and untrusted, so each active V8 execution runs on a bounded
non-Tokio executor. Blocking host work uses a second bounded executor with
fixed workers and bounded admission. Neither executor may create another Tokio
runtime.

V8 platform initialization has one approved constant process-lifetime owner
thread and a fixed four-worker V8 platform pool. Before V8 or its platform creates threads, the embedder initializes V8
sandbox hardware support; before any executor or maintenance thread first
enters V8, it restores V8's default per-thread protection-key permissions.
This is required because Linux protection-key state is inherited across thread
creation and host workers may predate V8 initialization.

The JavaScript networking contract copies Node.js's behavioral invariants, not
its trust topology:

- readiness-driven nonblocking I/O;
- bounded work on every scheduling turn;
- a real Duplex stream;
- Readable.push(false) stopping application reads;
- _read() resuming application reads;
- asynchronous write completion and genuine drain;
- referenced handles controlling VM liveness;
- Node-compatible event and error ordering.

AgentOS inserts a security boundary that Node.js does not have. Descriptors
remain in the trusted sidecar. VMs use opaque, generation-checked
capabilities. Readiness crosses the boundary as durable coalesced state, not
one message per packet or chunk.

This migration includes TCP, Unix sockets, listeners, UDP, TLS, HTTP/2,
signals, embedded V8, standalone WASM, and Python networking adapters. The generic
one-millisecond V8 platform-work poll is separately tracked because it is not
part of the socket, bridge, or signal pump.

Browser runtime support is intentionally outside this contract. Its source is
retained for a future design, but browser crates and packages are disabled from
default builds, CI, and publication so they cannot block the native reactor
migration. Standalone WASM/WASI support is not browser support and remains in
scope.

The implementation contains work from phases 1 through 8: direct bridge
waiters, one process runtime, bounded executors, the coalesced readiness broker,
native transports and guest adapters, signal/reaper consolidation, architecture
guards, and disabled browser entrypoints. Native UDP uses one descriptor-owning
task with bounded commands and datagram batches; connected sockets, multicast,
source-specific membership, and Node socket options use that same owner.
Asynchronous native connect admission and generation-checked capability aliases
are closed by focused regressions.
Decision 9 is resolved in favor of a required inherited full-duplex fd 3
response/control stream, in addition to ordinary stdin/stdout.

Validation includes workspace Rust formatting and clippy, the non-browser Rust
workspace test graph, JavaScript build and type-check, the 154-task JavaScript
test graph, focused Node networking conformance, native JavaScript/Python/WASM
identity tests, architecture guards, churn/soak tests, and generated protocol
parity. Regression coverage includes VM retirement, response-lane reservation,
bridge telemetry fallback, async connect, TCP/Unix fairness, single-owner UDP
readiness and options, Node close ordering, loopback ownership, accept-wake
coalescing, and fd 3 client liveness.

## 2. Outcomes

The completed target architecture has these externally meaningful outcomes:

1. A successful host call response cannot be trapped behind unrelated session
   events.
2. A hot socket cannot create an unbounded queue or one cross-boundary wake per
   chunk.
3. Guest backpressure reaches the actual transport instead of pausing only a
   JavaScript facade.
4. A blocked or hostile VM cannot occupy a Tokio worker.
5. A hot VM or HTTP/2 connection has explicit work and memory budgets rather
   than relying on Tokio scheduler fairness.
6. Creating more VMs or connections creates bounded tasks and state, not
   runtimes or OS threads per socket, signal, or HTTP/2 session.
7. Kernel-backed and native sockets share capability, readiness, accounting,
   lifecycle, and guest-facing contracts.
8. Every overload is either source backpressure or a typed, observable limit
   error that identifies the configuration field.

## 3. Terminology

- **Sidecar runtime**: the one process-wide Tokio runtime.
- **Runtime worker**: one fixed Tokio scheduler worker thread.
- **VM executor**: the bounded non-Tokio execution facility. Its initial V8
  implementation admits at most the configured number of session-affine
  threads and enters one isolate on each admitted thread.
- **Blocking executor**: fixed non-Tokio workers for admitted blocking host
  work such as resolver, filesystem, or process operations that cannot be made
  asynchronous.
- **V8 platform owner**: one named constant process-lifetime thread that owns
  V8's process-global initialization state and remains parked after startup.
- **V8 platform worker**: one of exactly four process-global background workers
  used by V8 for admitted compilation and engine work. The host CPU count does
  not change this bound.
- **Session**: an execution session associated with one VM generation.
- **Capability**: an opaque handle owned by the sidecar and exposed to a guest
  as an ID plus generation.
- **Handle task**: a Tokio task that owns one native transport or listener and
  serializes its commands and readiness.
- **Ready state**: bounded durable sidecar state saying that work is available.
- **Wake**: a coalesced notification asking a VM dispatcher to inspect ready
  state. A wake carries no socket data. At most one wake is queued or being
  processed for a VM generation at a time.
- **Application read interest**: permission to deliver application bytes to a
  guest stream. It is distinct from transport work required to advance TLS.
- **Bridge call**: a guest-to-host request identified by session generation and
  call ID.

## 4. Trust and ownership

The client is trusted except for guest code and payloads it submits. The
sidecar runtime and kernel are trusted enforcement points. V8, Python, WASM,
third-party packages, and guest-created input are untrusted.

Therefore:

- the sidecar owns descriptors, sockets, TLS state, HTTP/2 state, queues,
  quotas, timers, and cancellation;
- guests receive only opaque capability IDs and operation results;
- every operation validates VM ID, session generation, capability generation,
  type, permission, and lifecycle state;
- a completion from an old generation is discarded with an observable stale
  completion record and can never mutate a new VM;
- guest code never executes while a sidecar mutex is held;
- a Tokio task never synchronously enters a guest isolate;
- no registry or ready-state critical section performs I/O, awaits, blocks,
  allocates from an unbounded source, or invokes guest-controlled code;
- untrusted work never chooses an unbounded queue, task count, or allocation.

### 4.1 Authority derivation and network policy

The authenticated sidecar transport and its live execution record supply the
VM ID and session generation for every guest operation. Those fields are not
accepted as authority from a guest payload. A guest may name only a capability
ID and operation-specific arguments; lookup is scoped to the authenticated VM
generation before type, capability-generation, lifecycle, and permission
checks. Capability IDs are opaque, are never descriptors or registry indexes,
and cannot be delegated to another VM by copying their serialized value.

The ready-batch, read-batch, and complete-wake protocol is an internal native
adapter binding, not a user-callable guest syscall surface. Wake epochs and
observed revisions are issued and retained by the trusted dispatcher. Guest
JavaScript, Python, or WASM cannot invent an acknowledgement, clear another
handle's readiness, or select a different session generation by constructing a
bridge payload.

Network permission checks happen at the operation that creates or expands
authority, not only when an adapter object is allocated:

- native name resolution uses the trusted resolver, and every returned address
  is checked against the effective VM policy immediately before its connect
  attempt; a permitted hostname or an earlier DNS answer does not implicitly
  authorize a later address;
- UDP bind/connect checks the local or peer address, and every unconnected
  `sendto` destination is checked before the datagram is admitted;
- listener bind checks the concrete local address or Unix path, and an accepted
  socket inherits the listener's VM, policy version, and capability scope only
  after socket/connection admission succeeds;
- a backend transition such as raw TCP to TLS transfers the existing authority
  and descriptor ownership atomically; it cannot broaden destinations or leave
  two usable capabilities for one descriptor.

Permission policy is immutable for a VM generation unless live policy update
is implemented as an explicit versioned operation. A tightening update must
revoke or close every now-invalid capability before the new version becomes
effective. If that atomic behavior is unavailable, the sidecar rejects the
live update and requires a new VM generation; it never leaves grandfathered
network authority by accident.

## 5. The incident this design must fix

The observed failure was:

~~~text
[zid-ext] callHost response {"tool":"snapchat_ads","ok":true,"error":""}
Error: sync bridge deferred message queue exceeded limit of 256
  at Function.applySyncPromise (...)
...
ACP adapter auto-restart unsupported ... session evicted
~~~

The host call itself succeeded. The crash happened after that success.

Today, BridgeResponse and StreamEvent messages share the same per-session
bounded command channel. While applySyncPromise blocks the V8 execution thread
waiting for one BridgeResponse, ChannelResponseReceiver reads that mixed
channel. Every unrelated message it encounters is moved to a second deferred
queue. The command-channel capacity and deferred-queue limit are both 256. The
producer can send more than 256 messages over time because the receiver drains
the first queue into the second. If it defers a 257th stream event before
observing the expected response, the deferred queue fails the execution. The
ACP layer then sees a dead adapter and cannot resume it.

The callHost success line means the host-side tool operation produced a
successful result. It does not mean ChannelResponseReceiver had received the
corresponding BridgeResponse before the event flood exhausted its deferred
queue.

The reproduction in crates/v8-runtime/tests/embedded_runtime_session.rs proves
the queue failure without ACP or a real network: block a guest in a
synchronous bridge call, deliberately withhold its response, send 257
net_socket StreamEvents, and observe the same applySyncPromise error. This
isolates the failing runtime mechanism; it does not claim to reproduce the
production ordering of the successful host response.

Raising 256 only delays the failure and increases memory. A process-wide Tokio
runtime alone also does not fix it. The root problem is a response waiter
consuming unrelated event traffic, amplified by a socket pump that emits one
session event per read.

The required fix has two independent parts:

1. Route each bridge response directly to its registered call waiter. A
   synchronous call must never scan, consume, or defer session events.
2. Replace per-chunk session events with bounded durable ready state and at
   most one outstanding wake, queued or in flight, per execution session.

The deferred synchronous-message queue is deleted after direct routing lands.
There is no replacement knob for its limit because the queue is not part of
the target design.

## 6. Baseline architecture and migration debt

This table records the production architecture that motivated this migration,
not the post-migration target or test-only helper threads. The completion
checklist in section 23 is the source of truth for landed migration work.

| Area | Current shape | Failure mode or mismatch |
| --- | --- | --- |
| Sidecar entrypoint | stdio.rs builds one current-thread Tokio runtime | It is not the process-wide multi-thread owner required by this design |
| Protocol I/O | stdin and stdout OS threads plus bounded frame queues; timer-based event pump | Responses, events, control, and periodic pumping have coupled progress |
| V8 sessions | bounded session threads, each with a 256-entry command channel shared by that session's responses, events, and control | Sync response waits scan unrelated events; auxiliary forwarding threads add topology |
| TCP/Unix/TLS | OS reader thread per socket, std MPSC event queue, StreamEvent per state change | Unbounded data queue, wake amplification, fake backpressure |
| JavaScript socket reads | synchronous one-event polling bridge operations | Repeated bridge crossings and timer/read polling |
| Listeners | blocking accept loops with readiness waits and polling delays | Thread growth and timer-driven progress |
| HTTP/2 | OS thread and current-thread Tokio runtime per session; unbounded Tokio commands | Runtime/thread amplification and unbounded admission |
| Signals | an OS thread, sleep, and StreamEvent for delivered V8 signals | Thread per signal and shared-event-channel pressure |
| Python | independent polling and a current-thread Tokio runtime in part of the implementation | Parallel networking semantics and runtime ownership |
| Plugins and dispatch | additional local runtimes, including blocking dispatch and S3 setup | Violates one-runtime ownership and hides blocking admission |
| Kernel sockets | SocketTable has empty-to-nonempty readiness callbacks | Useful primitive, but currently converted into lossy per-event VM messages |
| Resource accounting | kernel SocketTable bytes and counts are bounded | Native, TLS, HTTP/2, bridge, and adapter buffers are not one accounting domain |

The migration must classify every runtime builder and every production
thread::spawn call. A source audit is an exit gate, not a documentation aid.
Test fixtures may create runtimes and threads when the test owns their complete
lifecycle.

## 7. Node.js reference model

Node.js uses one libuv loop per Node process. Network descriptors are
registered with epoll, kqueue, or IOCP. Readiness causes bounded native reads.
The native read callback calls the JavaScript stream's push method. If push
returns false, Node calls readStop. A later _read call invokes readStart.
Writes settle through native completion callbacks, and referenced handles keep
the loop alive.

The important local reference points are:

- lib/net.js: Socket is a Duplex, _read starts the native handle, and ref/unref
  delegate to the handle;
- lib/internal/stream_base_commons.js: onStreamRead calls push and calls
  readStop when push returns false;
- src/stream_wrap.cc: native readStart and readStop bindings;
- deps/uv/docs/src/design.rst: platform readiness loop;
- deps/uv/src/unix/stream.c: bounded nonblocking read loop.

### 7.1 What is the same

| Concern | Node.js invariant | AgentOS invariant |
| --- | --- | --- |
| Readiness | Platform readiness, no socket polling timer | Tokio platform readiness, no socket polling timer |
| Read turn | Bounded native reads | Bounded handle/VM batch |
| Stream | Real Duplex | Exact guest node:stream Duplex |
| Pause | push(false) calls readStop | push(false) disables application read interest |
| Resume | _read calls readStart | _read sends resume demand |
| Writes | Completion callback after native completion | Completion after sidecar write acceptance/completion |
| Drain | Writable HWM reflects queued bytes | Writable HWM and sidecar byte reservations drive drain |
| Liveness | Referenced libuv handles keep process alive | Referenced capabilities keep VM execution alive |
| Ordering | Defined data/end/error/close order | Node-compatible order |
| Turn bounds | libuv/native work is bounded | Per-handle, per-VM, and per-protocol quanta are bounded |

### 7.2 What is different

| Concern | Node.js | AgentOS |
| --- | --- | --- |
| Runtime scope | One loop for one application process | One runtime for a multi-VM sidecar process |
| JavaScript placement | Callbacks run on the event-loop thread | V8 executes on a separate bounded executor |
| Descriptor ownership | Node process owns descriptors | Trusted sidecar owns descriptors |
| Guest identity | Native handle is in the same process | Opaque capability ID plus generation |
| Wake path | Direct native callback into JS | Coalesced wake, bounded bridge batch, then JS |
| Fairness | Primarily one application | Hostile multi-tenant VMs with explicit quotas |
| Cancellation | Process/handle lifecycle | VM generation cancellation plus stale completion checks |
| Errors | Node/POSIX errors | Node/POSIX errors plus typed AgentOS limit metadata |

The additional boundary is:

~~~text
Node.js

platform readiness
    -> libuv handle
    -> bounded native read
    -> onStreamRead
    -> Readable.push

AgentOS

platform readiness
    -> process-wide Tokio runtime
    -> capability/handle task
    -> bounded per-VM ready state
    -> one coalesced VM wake
    -> bounded ready/read bridge operations
    -> exact guest Duplex
~~~

## 8. Mandatory invariants

The implementation is incomplete until all of these are true:

1. Exactly one production Tokio runtime is constructed in the sidecar process.
2. That runtime is multi-threaded, has a fixed worker count, enables I/O and
   time once, and is owned by the process entrypoint.
3. All subsystems receive a cloned Tokio Handle or a narrower runtime-owned
   service; none constructs a runtime.
4. V8, Python guest execution, WASM guest execution, and synchronous bridge
   waits never run on a Tokio worker.
5. Blocking host work requires an acquired bounded-executor permit before it
   is submitted.
6. No network descriptor has an OS reader thread, polling timer, or unbounded
   data queue.
7. No signal delivery creates an OS thread.
8. No HTTP/2 session or connection creates a runtime or an unbounded channel.
9. Bridge responses use direct call-specific routing and never enter the
   session event broker.
10. Response ingress settles the call registry directly; it does not wait for
    an ordinary router, session, event, or request queue.
11. Each execution session has at most one outstanding readiness wake, either
    queued or in flight.
12. Socket data is retained in the OS/kernel transport or explicitly charged
    buffers; it is not stored in a wake queue.
13. Every stream application read is gated by guest demand and available byte
    budget. UDP receive is instead gated by admitted datagram count/byte
    capacity because Node dgram sockets are not Readable streams.
14. Every queue, map, set, task class, and buffer is bounded by count, bytes, or
    both.
15. Near-limit warnings and terminal limit errors are observable outside the
    guest.
16. Every overload path either pauses its source or returns a typed error. It
    does not block a progress-critical producer indefinitely.
17. VM teardown cancels only that VM generation and does not stop the shared
    runtime or other VMs.
18. Kernel-backed and native capabilities expose the same operation and
    lifecycle contract.
19. JavaScript NetSocket extends the exact Duplex constructor returned by the
    guest's node:stream module.
20. Apart from admitted VM-executor threads, the fixed blocking executor, and
    explicitly approved constant process workers (including the V8 platform
    owner and unavoidable transport integration), no production path creates
    an auxiliary OS thread per VM, output stream, connection, request, or
    event.
21. V8 sandbox hardware support is initialized before V8-owned thread
    creation, and every non-V8-owned thread restores V8's default per-thread
    protection-key permissions before entering V8.

## 9. Process topology

~~~text
Sidecar process
|
+-- process entrypoint
|   +-- build SidecarRuntime exactly once
|   +-- build bounded VM executor
|   +-- build bounded blocking executor
|   +-- initialize one process-lifetime V8 platform owner
|   +-- install shutdown coordinator
|
+-- SidecarRuntime: one multi-thread Tokio runtime
|   |
|   +-- protocol ingress task
|   +-- protocol egress task
|   +-- ready-VM scheduling task
|   +-- signal ingress/broker
|   +-- timer and shutdown tasks
|   |
|   +-- process-owned services [bounded, no mailbox required]
|   |   +-- BridgeCallRegistry direct response fast path
|   |   +-- SessionBroker durable ready/completion state
|   |   +-- aggregate resource accountant
|   |
|   +-- VM supervisor task [bounded by active VM limit]
|   |   +-- capability registry
|   |   +-- ready state and wake gate
|   |   +-- async completion lane
|   |   +-- generation cancellation token
|   |
|   +-- native handle tasks [bounded by socket/handle limits]
|   |   +-- TCP/Unix stream task
|   |   +-- listener task
|   |   +-- UDP task
|   |   +-- TLS transport task
|   |   +-- HTTP/2 connection task
|   |
|   +-- kernel readiness adapter
|       +-- maps SocketId to capability
|       +-- records the same VM ready state
|
+-- VM executor: non-Tokio, bounded
|   +-- V8 isolate execution and wake dispatch [same thread, thread-affine]
|   +-- guest stream callbacks/microtasks [same thread]
|
+-- V8 platform owner: non-Tokio, constant
|   +-- initialize sandbox hardware before V8 platform threads
|   +-- preserve process-global V8 owner state for process lifetime
|
+-- blocking executor: non-Tokio, fixed and bounded
    +-- admitted blocking resolver/filesystem/process/plugin work
~~~

There is not one Tokio task per Node.js process in the sense of running guest
JavaScript. A VM supervisor is lightweight trusted orchestration. The actual
V8 isolate remains on its executor thread. Native handles are ordinary Tokio
tasks on the shared runtime.

BridgeCallRegistry is deliberately a directly callable bounded service, not a
router task with another response mailbox. Protocol ingress performs a bounded
lookup and nonblocking settlement before it admits ordinary requests or
events. SessionBroker similarly owns durable state; only fairness scheduling
needs a task. These distinctions prevent the target topology from recreating
the response-behind-events dependency under different names.

### 9.1 Runtime construction

The sidecar entrypoint constructs SidecarRuntime with:

- a fixed worker count selected from sidecar configuration;
- I/O and time drivers enabled;
- deterministic thread names;
- panic reporting;
- runtime metrics hooks;
- one shutdown token rooted at process lifetime.

The runtime is not stored in a lazily initialized subsystem singleton. Tests
build a RuntimeContext explicitly. Production constructors require a
RuntimeContext or narrower service dependency, making a hidden runtime builder
unrepresentable.

RuntimeContext is created at every AgentOS-owned sidecar process entrypoint and
passed into the NativeSidecar service, execution engines, plugins, and protocol
backends. Blocking compatibility methods must dispatch onto that context; they
must not fall back to a static runtime. A Tokio worker must never call
Runtime::block_on or synchronously wait for a future scheduled on the same
runtime.

Migration of the currently central mutable NativeSidecar must not replace task
ownership with one process-wide mutex. State that participates in async work is
partitioned into Send owner tasks or short-lived bounded registries. No await
occurs while a registry lock is held.

### 9.2 VM executor

The initial safe topology keeps the current V8 thread-affinity property: one
executor thread per active session, admitted under the global concurrency cap.
Admission happens before an OS thread is created; a queued session is bounded
state, not a thread waiting for a slot. A parked warm worker may be reused only
after the previous isolate and generation are fully detached, and parked
workers have a separate explicit count and memory limit.

A fixed thread pool is not automatically better. It is valid only if isolate
affinity, thread-local V8 state, termination, snapshot initialization, and
session reuse are proven. The spec does not require that extra change for the
I/O migration.

The executor blocks only itself during guest CPU work or a synchronous bridge
wait. It communicates with the runtime through bounded commands, direct
response waiters, and coalesced wake state.

Wake dispatch, runtime-event conversion, and output forwarding do not get
additional per-session OS threads. They execute on the admitted VM thread when
they touch V8, or as bounded Tokio work when they do not. A synchronous bridge
wait observes both its call-specific result and an out-of-band blocking
cancellation primitive; a Tokio-only cancellation future is insufficient for
a parked VM thread.

### 9.3 Blocking executor

The blocking executor has a fixed worker count, a bounded job queue, per-VM
admission, deadlines, cancellation, and byte reservations for job payloads.
Submitting a job without a permit is a bug. Tokio's elastic blocking pool is
not the admission policy.

Code that cannot yet migrate off blocking APIs must use this executor. Creating
a local current-thread runtime around an async SDK is prohibited; the SDK
future must be spawned on SidecarRuntime and bridged back through a typed
completion.

## 10. Task ownership and cardinality

| Task/state | Owner | Cardinality bound | May block? |
| --- | --- | --- | --- |
| Protocol ingress/egress | Process | Constant | No |
| Bridge call registry | Process | Per-VM and process call/byte limits; no task | No |
| Session broker state and ready scheduler | Process | Constant scheduler plus bounded per-VM state | No |
| VM supervisor | VM generation | max active VMs | No |
| V8 executor | Active execution | global active execution limit | Yes, off Tokio |
| V8 platform owner | Process | Exactly one | Parked after initialization |
| V8 platform workers | Process | Exactly four | V8-owned background work only |
| Native stream/listener/UDP/TLS task | Capability | per-VM and process handle limits | No |
| HTTP/2 connection task | Connection capability | connection limit | No |
| HTTP/2 stream state | Parent connection task | stream count and byte limits | No separate task by default |
| Kernel readiness adapter | Process or VM | Constant or max VMs | No |
| Blocking job | Blocking executor | worker plus queue limits | Yes, off Tokio |
| Signal broker state | VM/process | bounded bitsets/control state | No |

Per-handle Tokio tasks are acceptable because handle creation is already a
permissioned, quota-controlled operation. Per-packet, per-chunk, per-signal,
and unconstrained per-HTTP/2-stream tasks are not acceptable.

The protocol frontend uses SidecarRuntime when the host transport can be
registered for asynchronous readiness. If blocking stdio requires the
constant process transport-worker exception discussed in section 27, those
workers only perform bounded transport I/O and handoff; decoding, routing, and
service work still runs on SidecarRuntime. A permanently occupied transport
worker must not consume the general blocking executor's finite job capacity.
This possible exception is constant process topology, not permission for a
reader thread per session or handle.

## 11. Channels and durable state

Channels carry commands and completions. They do not represent level
readiness, buffered socket data, or liveness.

| Path | Primitive | Bound | Full behavior |
| --- | --- | --- | --- |
| Protocol ingress | bounded decoded-request admission plus direct control/response fast paths | frames and bytes | continue parsing multiplexed response/control frames; reject or defer ordinary requests without starving progress |
| Protocol responses | bounded egress with pre-reserved response bytes and reserved control capacity | frames and bytes | apply host transport backpressure; termination and already-admitted responses remain deliverable |
| Bridge call requests | bounded request admission plus call/request/response-byte reservations | calls and bytes | reject before host-visible dispatch when no reservation |
| Sync bridge result | call-specific capacity-one blocking waiter plus cancellation receiver | one result and reserved bytes | nonblocking direct settlement or typed duplicate/late response |
| Async bridge result | call registry plus bounded durable VM completion state | calls and result bytes | reject before dispatch when no reservation; settle without a session-event send |
| VM control | dedicated bounded lane plus cancellation token | small fixed count | cancellation/termination cannot wait behind data |
| VM wake | capacity-one mailbox plus an Idle/Outstanding wake state | one queued or in flight | coalesce; full while state is Idle is an invariant failure |
| Ready handles | bounded revisioned map keyed by capability | max capabilities | merge flags; never clear a newer publication with an older acknowledgement |
| Handle commands | bounded Tokio MPSC plus byte reservations | messages and bytes | source waits only where safe or receives typed EAGAIN/limit error |
| Handle completions | direct waiter or bounded completion state | operations and bytes | no shared event scan |
| Blocking jobs | bounded work queue plus permits | jobs and bytes | reject before starting |
| HTTP/2 commands | parent connection mailbox | commands and bytes | flow-control wait or typed AgentOS limit |
| Signals | bounded/coalesced signal state plus reserved termination | supported signal set | POSIX coalescing or explicit occurrence bound |

Every channel has:

- an owning component;
- a producer and consumer list;
- a message and byte bound;
- a documented full action;
- an ordering guarantee;
- a shutdown rule;
- a near-capacity metric and warning;
- a typed error with the canonical configuration field.

Using an MPSC capacity as accidental backpressure is not sufficient. If the
producer blocks a thread needed to receive the response that would drain the
queue, the design has a dependency cycle. Response and termination paths are
therefore separate from ordinary event admission.

No Tokio worker performs a blocking channel send. Awaiting bounded capacity is
allowed only when the producer holds no lock or scarce permit needed by the
consumer, the consumer does not depend on work behind that producer, and
cancellation/deadline progress is independent. Otherwise the operation uses
source backpressure or fails before it becomes externally visible.

For a multiplexed host transport, raw ingress cannot simply stop when the
ordinary request queue is full: doing so could leave the response needed to
free that queue unread in the same byte stream. The decoder classifies frames
first. Bridge responses, cancellations, and shutdown use bounded direct paths;
ordinary requests are rejected with a typed overload or admitted only after
their eventual response capacity is reserved.

Logical priority starts only after a complete frame is decoded, so the transport
uses physical separation. Fd 0 carries host `RequestFrame` ingress and stdout
carries non-heartbeat `EventFrame` egress. Required inherited full-duplex fd 3
carries host `SidecarResponseFrame` and typed `ControlFrame` ingress, plus
sidecar `ResponseFrame`, `SidecarRequestFrame`, and heartbeat egress. A partial
ordinary frame therefore cannot delay a registered response or shutdown.
Wrong-lane frames are terminal protocol errors.

Every multi-stage operation has one end-to-end reservation, transferred rather
than reacquired at each queue. In particular, accepting a bridge call reserves
its registry entry, request bytes, and declared maximum response storage before
the request can reach the host. A response that is within that declared maximum
must not fail merely because an intermediate completion queue filled later.

## 12. Direct bridge response routing

This section governs guest-to-host BridgeCall responses, including the
applySyncPromise failure path. Sidecar-local pump operations such as
ready_batch, read_batch, and complete_wake may use a narrower per-execution RPC
registry, but they obey the same rule: each response settles a registered
call-specific waiter or completion target directly and never traverses the
ordinary session-event lane.

### 12.1 Registry

The process owns a BridgeCallRegistry keyed by:

~~~text
(vm_id, session_generation, call_id)
~~~

An entry contains:

- expected response kind and maximum bytes;
- an atomic Waiting, Delivered, Cancelled, or TimedOut lifecycle;
- a sync result waiter or async session-completion target;
- deadline and cancellation token;
- reserved response-byte budget;
- tracing metadata.

Call IDs are unique within a live session generation. Generations are
monotonic and never reused for a new live VM identity.

The target response envelope carries enough identity to validate the VM,
session generation, and call ID before settlement. The existing
call_id-to-session CallIdRouter is not the target registry: it only selects a
session and then places BridgeResponse on the mixed session command channel.
During protocol migration, a globally unique call ID may locate an entry, but
the entry's VM and generation must still match the response envelope before
delivery.

The registry is bounded per VM and per process. Protocol ingress invokes its
settle operation directly; there is no router-task mailbox between frame
decoding and the waiter. The implementation may use a bounded slab, sharding,
or short critical sections, but performs no await, blocking send, or
guest-controlled work while registry state is locked.

### 12.2 Call lifecycle

1. Validate the call and acquire call-count, request-byte, and declared
   response-byte reservations.
2. Allocate the call ID, create its completion target, and insert a Waiting
   entry in BridgeCallRegistry.
3. For an async V8 call, install its resolver in V8-local pending state.
4. Emit the host request only after both forms of registration succeed.
5. Route the host response directly from protocol ingress to the registry.
6. Validate VM, generation, kind, size, and status.
7. Atomically transition Waiting to Delivered and transfer the result to
   exactly one completion target.
8. Release reservations after ownership of the result transfers or the caller
   drops it.

Registration happens before dispatch. If dispatch fails, remove the entry and
settle it with that failure. Timeout, cancellation, VM teardown, and process
shutdown race delivery through the same atomic terminal transition. Exactly
one transition wins; a losing payload is released immediately and recorded as
late or duplicate.

The bridge request must not become host-visible before registration. An actor
command that says “register later” is insufficient because a fast host could
respond before the actor processes it. Likewise, the async V8 API must be split
into allocate/register/dispatch or provide an equivalent atomic helper; the
current dispatch-then-insert shape is not safe once response routing becomes
concurrent with the VM executor.

### 12.3 Sync calls

A synchronous V8 call waits on a capacity-one blocking waiter while running on
the VM executor. It never reads SessionMessage, StreamEvent, the ready set, or
another call's response.

Settlement uses a nonblocking one-result operation; a full waiter is a
duplicate/invariant violation, not a reason to park protocol ingress. The VM
thread waits on both that result and a blocking cancellation receiver. VM
generation cancellation wins the same lifecycle race, interrupts the waiter,
and requests V8 termination without enqueueing behind data.

### 12.4 Async calls

An async response atomically transfers its reserved payload from the call
entry into the session's bounded durable completion registry and coalesces the
VM wake. It does not send a BridgeResponse or StreamEvent to the VM. The guest
dispatcher drains completions in a bounded batch and resolves promises on the
V8 executor. Promise resolution and microtask checkpoints preserve
Node-compatible ordering.

The V8-local resolver exists before the request is externally visible. If the
request cannot be dispatched, the implementation removes both registrations
and rejects that resolver. This avoids relying on the current incidental fact
that response processing and resolver insertion occur on the same session
thread.

### 12.5 Response edge cases

- Unknown call ID: protocol error and structured log; never reinterpret as an
  event. A bounded generation/tombstone record distinguishes a stale response
  from a never-issued ID where the wire protocol cannot do so directly.
- Duplicate response: typed protocol error; the original result remains
  unchanged.
- Late response after cancellation: observable stale response; release payload
  immediately.
- Oversize response: settle the registered call with a typed response-size
  limit error.
- Wrong generation: stale completion record; never deliver to the current VM.
- Registry full: reject the call before it reaches the host.

## 13. Coalesced readiness and wake protocol

### 13.1 State

Each VM generation owns ReadyState:

~~~text
ReadyState {
    handles: bounded map CapabilityId -> ReadyEntry {
        flags: ReadyFlags
        revision: u64
    }
    completions_ready: bool
    signals_ready: bool
    control_ready: bool
    wake: Idle | Outstanding { epoch: u64 }
    next_wake_epoch: u64
}
~~~

The three non-handle ready fields are derived from their own bounded durable
completion, signal, and control stores. complete_wake recomputes them under the
same lock; it never clears a Boolean based only on an older guest batch.

ReadyFlags are level state such as readable, writable, accept, datagram, end,
error, and close. Correlated operations such as connect and write settle their
registered completion targets directly; readiness is not a substitute for an
operation completion. Terminal state is sticky until
observed and acknowledged. Repeated readiness for the same handle sets the
same bit and advances that entry's revision; it does not append another
message. Revisions and wake epochs never wrap or reuse within a live
generation; exhaustion is a typed terminal runtime error.

The flags represent guest-deliverable work, not raw platform readiness. For
example, a socket that is normally writable is not continuously ready unless
a pending write can make progress or a write completion must be delivered.
Readable application work is not published while application read interest is
disabled. The handle task also stops awaiting that application-read path so it
does not spin on a level-readable descriptor. If source readiness is already
known when _read() restores demand, the handle republishes it without waiting
for a new platform edge.

Application read interest and the readable entry are updated under the same
capability/ReadyState synchronization. SetReadInterest(false) makes readable
non-deliverable and suppresses or clears that flag even if an older batch sees
a revision mismatch; a later SetReadInterest(true) or source publication
advances the revision and re-arms it. Therefore complete_wake never requeues a
readable-only entry while demand is disabled. The revision rule protects a
newer deliverable publication, not raw readiness that current interest makes
ineligible.

After publishing a readiness class, a native handle task disarms that class in
its own select loop until a bounded sidecar operation consumes, clears, or
rearms it. It must not repeatedly await a Tokio readiness future that remains
immediately ready and continuously advance revisions. Kernel, TLS, and HTTP/2
backends provide the equivalent source-state gate.

control_ready covers bounded non-urgent lifecycle work. VM cancellation and
forced termination remain on the out-of-band control primitive and never
depend on the VM consuming a wake.

ReadyState cardinality is bounded by the VM capability limit. HTTP/2 stream
readiness is bounded inside its parent connection by the negotiated and
AgentOS stream limits.

### 13.2 Producer algorithm

When a handle becomes ready:

1. Under the ReadyState lock, record or merge its deliverable flags and advance
   the entry revision. A new publication advances the revision even if the same
   flag was already set.
2. If deliverable work exists and wake is Idle, allocate the next epoch, set
   wake to Outstanding, and try-send that epoch to the capacity-one wake
   mailbox before releasing the lock.
3. If wake is already Outstanding, do nothing else after updating durable
   state.

Adding more data does not add more wakeups. For a native socket the bytes
usually remain in the OS receive buffer. For a kernel socket they remain in
SocketTable's bounded buffer. For TLS or HTTP/2, any bytes already pulled into
trusted protocol buffers are charged to the VM exactly once. The wake says
only “inspect durable state.”

The wake send never waits. Full while ReadyState says Idle violates the
single-wake invariant and terminates that VM generation with a structured
internal error; a disconnected mailbox follows normal VM teardown. State
change and try-send occur under the same short lock so another producer or
complete_wake call cannot observe an Idle state between them.

### 13.3 Consumer algorithm

On a wake, the trusted native guest dispatcher:

1. validates VM generation and epoch, then requests a ready batch capped by
   handle count, operation count, and bytes; each returned entry includes its
   observed revision;
2. processes completions and terminal/control state before ordinary data;
3. asks for bounded data/accept/datagram batches only for streams with demand;
4. pushes bytes into the exact guest stream;
5. disables application read interest immediately when push returns false;
6. reports the completed wake epoch.

The broker's complete_wake operation is atomic with respect to producers:

- reject a stale, duplicate, wrong-generation, or non-Outstanding epoch without
  mutating current state;
- clear an acknowledged flag only when its current revision equals the
  observed revision and a sidecar-owned capability operation established that
  it is no longer deliverable;
- retain sticky terminal flags until their defined delivery transition;
- set wake to Idle after applying valid acknowledgements;
- if deliverable state remains, allocate the next epoch, set Outstanding, and
  try-send one replacement wake before releasing the lock.

The guest cannot clear readiness merely by claiming to have consumed it.
Acknowledgements refer to results observed by bounded sidecar operations. If a
producer republishes the same flag while an older batch is in flight, its
revision changes and the older acknowledgement cannot clear it. This detail is
required for kernel empty-to-nonempty notifications as well as native
readiness; a global wake epoch alone does not prevent that lost-wake race.

The native dispatcher owns completion of the handshake. If a stream callback
throws or the dispatcher cannot complete the epoch, it either retries within
its bound or terminates the VM generation and clears its broker state; it never
leaves an immortal Outstanding wake. The direct call-specific completion path
guarantees that ready_batch and complete_wake responses cannot be trapped
behind event traffic.

### 13.4 Fairness

A turn is bounded by all of:

- maximum ready handles per VM turn;
- maximum operations per handle;
- maximum bytes per handle;
- maximum total bytes per VM turn;
- maximum accept/datagram count;
- maximum HTTP/2 frames and streams;
- maximum async completions and signals.

If more work remains, it stays level-ready and a subsequent wake is scheduled.
The scheduler rotates VMs and handles rather than draining one hot source.

## 14. JavaScript stream pump

### 14.1 Stream identity

`node:net.Socket` must inherit from the exact `Duplex` constructor exported by
that VM's `node:stream` singleton. In particular, the CommonJS, `node:`-prefixed,
and ESM views must agree on constructor and prototype identity. The bridge must
not bundle a private `stream-browserify` or `readable-stream` constructor, and a
second socket shim must not bypass the runtime's canonical stream module.

Conformance tests must at least prove:

- `socket instanceof require("node:stream").Duplex`;
- `Object.getPrototypeOf(net.Socket.prototype) === Duplex.prototype`;
- `require("stream").Duplex === require("node:stream").Duplex`; and
- accepted, connected, and TLS sockets preserve those identities.

Identity alone is insufficient. The current embedded V8 stream shim is a
minimal emitter whose `Readable.push()` and `Writable.write()` return true
unconditionally; it has no usable high-water-mark state. The migration must
replace or upgrade that canonical module with a bounded, Node-compatible stream
implementation before the socket pump can claim backpressure or `drain`
correctness. The separate built-in `NetSocket` class and execution-shim socket
must then converge on that module instead of preserving two behaviors.

The initial migration preserves the currently exposed 16 KiB high-water mark
unless a compatibility change is separately approved. Current Node.js defaults
are not silently imported as part of the pump rewrite.

### 14.2 Read path

~~~text
Tokio readiness
  -> native handle task marks capability readable
  -> VM ReadyState merges readable flag
  -> capacity-one wake
  -> guest dispatcher grants one bounded read demand
  -> sidecar try_read up to the grant and current quanta
       EAGAIN -> clear stale readiness and await the next readiness edge
       bytes  -> return one bounded raw buffer
       EOF    -> return sticky terminal state
  -> guest Socket.push(buffer)
       true  -> Readable may invoke _read again
       false -> do not grant another application read
~~~

`Socket._read(n)` is the demand edge. It enables application read interest and
submits a bounded grant; `n` is a stream demand hint, not permission to bypass
the VM, capability, or bridge byte limits. There is at most one guest-bound read
operation in flight per capability generation. If readiness was recorded while
interest was disabled, re-enabling interest immediately attempts `try_read`;
it does not wait for a second OS edge.

The native handle task calls `try_read` only when:

- the capability is live and generation matches;
- application read interest is enabled;
- a read grant is outstanding and no prior read is in flight;
- the VM and handle have buffer budget;
- the current fairness quantum permits it.

For TCP and Unix streams, withholding the next grant stops application reads
and leaves subsequent bytes in the OS receive buffer. It does not close or
unregister the descriptor. The amount read ahead across the trust boundary is
therefore bounded by one admitted read result. If the result itself contains a
bounded vector of buffers, the adapter stops calling `push` after the first
false result and retains the remainder under its guest-bound reservation; it
does not request another batch.

This reproduces the important Node path: libuv calls `onStreamRead`,
`stream.push(buffer)` returning false calls `readStop()`, and a later `_read()`
calls `readStart()`. AgentOS performs the equivalent state changes across an
opaque capability boundary rather than giving the guest the libuv handle.

An EOF result calls `push(null)`. A non-EOF read failure destroys the stream
with its Node/POSIX-compatible error. EOF, error, and close are sticky terminal
state, not zero-length data or ordinary readiness messages. The implementation
must match Node's `end` before `close` behavior, default `allowHalfOpen: false`
write shutdown, explicit half-open behavior, and `close(hadError)` ordering.

### 14.3 Write path

`Socket._write` and `_writev` submit a generation-scoped operation only after
reserving command and user-space write bytes. Writes issued while connecting
remain in the guest `Writable` queue and are dispatched after `connect`, or fail
with the Node-compatible closed-before-connect error.

The operation completes when every byte has been accepted by the transport
backend and is no longer retained in an AgentOS user-space write buffer. For a
native TCP or Unix socket this means successful completion of all required
nonblocking writes; it does not mean that the peer acknowledged the bytes. TLS
and HTTP/2 define the corresponding backend completion point below.

The write callback runs on the VM executor after the sidecar either:

- completes that transport write contract; or
- returns a Node/POSIX-compatible error.

`Writable.write()` returns false based on the real guest `Writable` high-water
mark, not on a fabricated socket flag. Sidecar admission may subsequently fail
`_write` with a typed resource error, but it cannot claim completion merely
because it copied bytes into a bridge request or actor mailbox.

The canonical `Writable` implementation, not the sidecar, owns `drain`. Match
the pinned Node fixture: a synchronous native completion is deferred out of the
current `_write` call; when `needDrain` is set, current Node emits `drain` after
the writable length reaches zero and before invoking that write callback in the
same `afterWrite` turn. Error, destroy, `_final`, `finish`, and pending callback
ordering remain stream concerns driven by correlated operation results.

AgentOS may reject an admitted `_write` with a typed resource-limit error where
Node would continue buffering after returning false. That security-policy
divergence is explicit and has a conformance fixture; it must not be mistaken
for Node high-water-mark behavior.

### 14.4 Connect and accept

Connect is an asynchronous operation with deadline and cancellation. DNS,
candidate addresses, permission checks, socket creation, and completion are
bounded. Failed multi-address attempts release reservations before the next
attempt.

Each native listener has one admitted Tokio accept task. Accept readiness is
level/coalesced. Before calling `accept`, the task reserves a VM socket and
connection slot. It releases those reservations on `EAGAIN` or failure and
converts them to capability ownership on success. When no reservation is
available it stops accepting and leaves connections in the OS backlog; if an
unavoidable platform race returns a descriptor after admission is lost, the
descriptor is closed immediately and the typed overload is surfaced. It never
creates an untracked socket or thread.

### 14.5 Ref, unref, and liveness

Each capability has a referenced bit owned by the VM supervisor. `ref()` and
`unref()` return the guest object and update that bit even while connect is
pending, so the final handle inherits the latest state. Active referenced
handles, pending operations whose handles are referenced, and explicitly
referenced timers contribute to VM liveness. `unref()` changes liveness only;
it does not pause I/O, suppress events, cancel a connect, or destroy a handle.

The VM executor may become idle while sidecar handle tasks continue to exist.
The supervisor may allow natural VM/session completion only when no referenced
work remains. It never interrupts a currently executing callback merely because
the last handle was unreferenced.

## 15. Protocol-specific contracts

### 15.1 TCP and Unix streams

- At most one native Tokio handle task owns each admitted descriptor; kernel
  sockets need no Tokio task merely to exist.
- Nonblocking connect/read/write/shutdown.
- One correlated result for each connect, read, write, shutdown, and close
  operation; readiness is not used as a substitute for operation completion.
- Same read-interest, write, half-close, timeout, ref, and error contracts.
- Unix path permission and namespace validation remain sidecar-owned.
- No reader thread, std MPSC data queue, or recurring poll timeout.

### 15.2 UDP

- At most one native Tokio task owns each admitted datagram descriptor.
- Datagram boundaries, source address, truncation behavior, and connected UDP
  semantics are preserved.
- Queue limits apply by datagram count and bytes.
- One readable flag represents one or more queued/OS-buffered datagrams.
- Each VM turn receives a bounded datagram batch.
- Node `dgram.Socket` is not a `Readable` and has no `push(false)` demand signal.
  Receive interest stays enabled while the socket is active and admitted
  datagram queue space exists. When count or byte capacity is exhausted, the
  task stops calling `recv_from` until the broker drains capacity; packets the
  OS drops in that interval are counted where the platform exposes a drop
  counter. Otherwise AgentOS reports the paused duration and documents that the
  platform loss count is unknowable.
- Before a receive syscall, the task acquires a datagram-count slot and a byte
  reservation for its bounded receive buffer. It shrinks the byte reservation
  to the actual retained length or releases both on `EAGAIN` or error. If the
  platform reports truncation, the payload and `rinfo` match the selected
  Node/platform compatibility behavior; datagrams are never silently split or
  concatenated.
- `send` completion means the complete datagram was accepted by the native or
  kernel backend, not delivered to its peer. Callback, synchronous validation,
  asynchronous ICMP error, and `close` ordering follow pinned Node fixtures.

### 15.3 TLS

TLS has two separate notions of demand:

1. transport progress needed for handshake, alerts, shutdown, and record
   framing;
2. application plaintext demand controlled by Readable.push and _read.

`push(false)` disables further plaintext grants. It must not deadlock a required
handshake, alert, or `close_notify` exchange. The TLS owner may continue the
minimum transport work needed for protocol progress only while it has explicit
ciphertext and plaintext reservations. Once its bounded plaintext staging area
is full, it stops consuming ciphertext that could produce more plaintext.

Wrapping an existing socket transfers exclusive descriptor ownership to the
TLS capability; the raw and TLS capability may not both read it. Ciphertext,
decrypted plaintext, pending application writes, encoded records, and handshake
state all have explicit byte budgets. TLS write completion means the plaintext
has been encoded and all resulting records satisfy the underlying transport
write contract, not merely that encryption finished. `secureConnect`, TLS
error, EOF, shutdown, and `close` ordering are compared with Node fixtures.

TLS is a capability backend on SidecarRuntime, not a TLS reader thread.

### 15.4 HTTP/2

Each HTTP/2 connection has one bounded connection-driver task or one
equivalently serialized owner. Optional stream helper tasks are admitted under
the connection and VM stream-task limits; there is never a task per frame,
header, or data chunk. Stream state normally remains in the connection owner.

The implementation combines:

- HTTP/2 flow-control windows;
- per-connection and per-stream AgentOS byte budgets;
- bounded command and completion queues;
- bounded ready stream sets;
- fair stream rotation;
- GOAWAY, reset, close, and cancellation ordering;
- VM generation validation.

Protocol flow control is not an AgentOS memory limit. A peer-advertised window
can exceed local policy. Reservations are acquired before buffering headers,
body data, pending writes, or events.

Inbound DATA capacity is released to the peer only as application consumption
and local reservations allow. Pausing an `Http2Stream` stops application
delivery and corresponding window release without preventing bounded
connection-level control progress. Outbound stream writes await HTTP/2 send
capacity without spinning; their callbacks run only after the h2 backend no
longer retains the submitted bytes under AgentOS ownership. Header list size,
continuation state, stream count, and queued control frames are bounded before
guest event materialization.

The current per-session OS thread, current-thread Tokio runtime, unbounded
command channel, polling accept loop, and event VecDeque paths are removed.

### 15.5 Signals

Signal delivery uses one process signal ingress and bounded per-VM signal
state. It never creates an OS thread per delivered signal.

`ProcessTable` remains authoritative for masks, pending state, default actions,
and process status. The current `SignalSet` is a bitset, so signals represented
there coalesce while pending and do not preserve multiplicity. A signal class
that requires queued occurrences must use a separate bounded counter/queue with
specified overflow behavior; it cannot be silently routed through `SignalSet`.

`SIGKILL`, `SIGSTOP`, and other non-catchable/default state transitions never
become JavaScript handler events. Termination and VM shutdown use reserved
control state, independent of the ordinary signal budget.

For a live catchable handler, the broker maps kernel/process delivery to the
target VM generation, marks `signals_ready`, and coalesces a wake. The guest
dispatcher drains a bounded batch and runs handlers on the VM executor. Handler
dispatch acknowledges the broker's delivery bit through the same
generation-checked path; kernel mask, pending-set, and disposition changes
remain explicit `ProcessTable` operations.

The initial supported guest-visible signal set uses standard-signal
coalescing; no signal class preserves multiplicity. Adding realtime or another
occurrence-preserving signal requires a separate protocol and bounded counter
decision rather than changing the meaning of the existing SignalSet.

### 15.6 Timers

Networking timeouts use SidecarRuntime's time driver. A timeout changes durable
operation state and wakes the VM once. It does not run guest code on Tokio.
Timer callbacks that are part of guest JavaScript execute on the VM executor
and participate in active-handle liveness.

## 16. Shared kernel and native capability model

The kernel already provides useful backend-independent primitives:

- SocketId and stream/datagram socket types;
- SocketReadiness and SocketReadinessKind;
- empty-to-nonempty readiness callbacks for accept, stream data, and datagrams;
- socket, connection, buffered-byte, and datagram limits through
  `ResourceAccountant` and `SocketTable` snapshots;
- ProcessTable signal masks, pending signals, and delivery rules.

Those primitives are backend foundations, not a complete reactor contract.
Today `SocketReadinessKind` contains only `Data` and `Accept`; peer shutdown,
remove, error, connect completion, and writable transitions are observable by
polling but do not all emit readiness callbacks. The migration must extend the
kernel mutation/readiness API for those transitions before deleting its polling
fallback. The kernel remains independent of Tokio, bridge types, and V8.

### 16.1 Capability registry

The sidecar owns a registry shaped conceptually as:

~~~text
CapabilityEntry {
    vm_id
    session_generation
    capability_id
    capability_generation
    kind
    referenced
    lifecycle
    backend:
        Native(handle_mailbox)
        Kernel(SocketId)
}
~~~

Guest operations target opaque `CapabilityId` plus the current generation;
native descriptors and kernel `SocketId` values never cross the trust boundary.
The registry dispatches to either a native owner or `SocketTable` while
preserving one result, error, readiness, and lifecycle contract. Each operation
that can complete later also has an operation ID and a registered completion;
connect/write completion is never inferred by consuming an unrelated readiness
flag.

Kernel calls currently use synchronous mutex-protected operations and may copy
data. A Tokio task may call them directly only when the critical section and
copy are demonstrably bounded by the current turn's byte quantum. Any operation
that can block or perform larger host work goes through the bounded blocking
executor. This rule avoids adding Tokio to the kernel while also avoiding a
blocking kernel call on a shared runtime worker.

### 16.2 What is shared

- capability IDs and generations;
- permissions and ownership checks;
- ReadyState and coalesced wake protocol;
- operation request/result schemas;
- resource reservations and accounting snapshots;
- cancellation and shutdown;
- Node/POSIX error mapping;
- liveness and ref/unref;
- tracing, metrics, and conformance tests.

### 16.3 What stays backend-specific

Kernel:

- virtual/loopback socket storage and routing;
- virtual process and descriptor ownership;
- POSIX signal masks and pending state;
- PTY and virtual filesystem relationships.

Sidecar/native:

- OS descriptors and Tokio registration;
- DNS and native address selection;
- TLS and HTTP/2 protocol engines;
- host protocol and bridge routing;
- guest adapter transport.

### 16.4 Readiness integration

Kernel `SocketTable` readiness callbacks do not send `StreamEvent` directly.
The callback runs after the `SocketTable` state lock is released, does bounded
nonblocking work only, resolves `SocketId` to a live capability generation,
merges a ready flag into the same `ReadyState` as native sockets, and calls the
same wake gate. It must never await, perform bridge serialization, or acquire
locks in the reverse of the capability-registry order.

Existing empty-to-nonempty data/accept/datagram notifications are compatible
with level coalescing: after every wake, the backend is re-queried and remains
ready until drained. Section 16's missing terminal and state-transition
notifications must be added with the same level behavior. The migration removes
payload serialization with `unwrap_or_default` and ignored
`send_stream_event` failures from this path.

### 16.5 Unified accounting

Resource admission moves from a kernel-only snapshot check to an atomic per-VM
reservation ledger used by both backends. `maxSockets`, `maxConnections`,
`maxSocketBufferedBytes`, and datagram count limits include native and kernel
capabilities. The existing resource snapshot remains useful for reconciliation
and metrics, but a check-then-mutate snapshot is not the concurrency control.

`maxSocketBufferedBytes` becomes a VM-wide socket/protocol memory budget.
Reservations cover:

- kernel stream and datagram buffers;
- native bytes retained outside the OS;
- TLS ciphertext and plaintext;
- HTTP/2 headers, body, and pending writes;
- bridge batches holding socket data;
- guest-bound data until ownership transfers into memory covered by the guest
  heap or external-memory limit.

OS receive and send buffers are not AgentOS allocations; bytes become charged
when AgentOS retains them in userspace and stop being charged after the OS
accepts them or ownership moves to another charged domain. A zero-copy move
transfers its reservation atomically. A real copy temporarily owns two
allocations, so it acquires a second reservation before copying and releases
the source reservation afterward. There is no uncharged handoff window and no
assumption of zero-copy where the bridge actually copies.

Kernel buffer growth consumes a reservation before mutating `SocketTable` and
releases it when the peer reads, shuts down, or closes. Native, TLS, HTTP/2,
bridge, and adapter owners follow the same acquire/transfer/release discipline.
Separate sub-limits may protect protocol queues, but they are nested inside the
aggregate VM budget rather than creating unaccounted memory pools. Teardown
reconciles every capability's outstanding reservations to zero.

## 17. Guest adapters

All guest environments use the same sidecar capability registry and backend
operations. The common contract includes allocate/connect/bind/listen/accept,
bounded read or datagram receive, write or send, readiness subscription,
shutdown/close, cancellation, and generation-checked results. It does not
standardize guest object shapes or pretend every guest exposes Node streams.

No adapter owns a descriptor, native socket poller, Tokio runtime, resource
policy, or permission decision. Adapter-local queues and retained buffers are
included in VM accounting, and adapters cannot convert a typed sidecar overload
into an infinite retry loop.

### 17.1 Embedded V8/Node

Both the bundled built-in path and the execution shim path use the exact
`node:stream` `Duplex`, the Node-compatible pump, active-handle liveness, and
Node event/error ordering. They are thin views over the same capability and may
not retain parallel socket registries with independent lifecycle truth.

### 17.2 Standalone WASM/WASI

Maps sidecar capabilities to WASI-facing interfaces where those APIs require
sidecar networking. WASI poll/read demand maps to the same bounded read-interest
operation, while UDP datagram boundaries remain distinct from byte-stream
reads. A cooperative WASM guest yields and is resumed from a registered waiter
rather than polling with a recurring timer.

### 17.3 Python

Maps Python socket and asynchronous APIs to the same capability operations.
It does not own a Tokio runtime or poll native sockets on a Python-specific
timer. Blocking Python socket calls block only an admitted guest executor, not
`SidecarRuntime`; they wait on a registered per-operation waiter and remain
cancellable by timeout or VM teardown. `asyncio` integrations yield and resume
from the same readiness/completion state instead of using a second descriptor
watcher.

Adapters may translate API shape and errors, but permissions, descriptors,
readiness, resource policy, and lifecycle remain one sidecar implementation.
Cross-adapter conformance runs the same backend operation fixtures first, then
adapter-specific tests for Node stream ordering, WASI demand, and Python
blocking/nonblocking behavior.

## 18. Lifecycle and cancellation

Every capability follows:

~~~text
Allocating -> Open -> Closing -> Closed
                 \-> Failed -> Closed
~~~

Transitions are monotonic. Close is idempotent. A capability generation is
never resurrected. Terminal stream events are emitted at most once and in the
ordering defined by the guest adapter; closing the sidecar object and emitting
the guest's `close` event are related transitions, not two independent races.

Every asynchronous operation has a second, shorter lifecycle:

~~~text
Registered -> InFlight -> Settling -> Settled
                  \--------cancel------/
~~~

Registration happens before dispatch and records the VM generation,
capability generation, cancellation token, response destination, deadline,
and all acquired count/byte reservations. The registry removes an operation
exactly once before settling it. A task owns its reservations through RAII and
transfers that ownership explicitly when it transfers buffered data; teardown
must not separately decrement counters that a cancelled task will release.

No completion is delivered while a registry or capability lock is held. Task
panic, channel closure, serialization failure, and cancellation all run the
same exactly-once settlement path. A completion that loses the settle race
releases its reservations and increments the stale or duplicate completion
counter; it does not silently return.

### 18.1 VM teardown

1. Atomically mark the VM generation closing.
2. Close admission for new calls, operations, capabilities, and executor work.
3. Trigger its cancellation token and reserved control state.
4. Atomically detach and settle bridge waiters and pending operations with
   typed cancellation.
5. Remove the VM from the process ready queue and invalidate its wake epoch.
6. Ask native handle tasks to close, unregister kernel readiness targets, and
   await the VM task set through a bounded teardown barrier.
7. Let task/buffer owners release their reservations; verify that the VM's
   accounting ledger returns to zero except for explicitly retained executor
   state. A mismatch is logged as a resource-integrity failure and keeps the
   generation quarantined rather than silently correcting counters.
8. Request V8 termination and join the VM executor without holding a sidecar,
   registry, or protocol lock.
9. Detach the generation only after all routes and owned state are gone.

Late completions validate generation, record a stale-completion metric, and
release resources. They never enter a successor VM.

Teardown has a configured deadline, but a deadline does not authorize reuse of
the generation or its permits. If a V8 executor cannot be joined, the VM enters
an observable `Quarantined` terminal state, retains the executor permit and
minimal ownership record, and rejects all work until the thread exits. This
contains one failed VM without lying about resource release. Process shutdown
may escalate an executor that survives its final deadline to a process-fatal
error; it must not detach an untrusted live thread.

### 18.2 Process shutdown

The process shutdown coordinator stops new request admission, cancels VMs,
closes capabilities, settles registered operations, drains bounded
control/response egress within a deadline, joins bounded executors, and finally
drops SidecarRuntime. Protocol ingress continues only long enough to route
already-admitted responses and shutdown control; ordinary work is rejected.
Shutdown reports which VM, task class, or executor missed the deadline and
exits nonzero rather than presenting a partial drain as success. No subsystem
independently shuts down the shared runtime.

## 19. Backpressure and overload

Backpressure has three different meanings and the implementation must not
collapse them:

1. **Transport backpressure**: stop reading or await write readiness.
2. **Guest stream backpressure**: push(false) and Writable high-water marks.
3. **Resource admission**: reject work that cannot acquire count/byte budget.

Examples:

- TCP read HWM reached: disable application read interest; bytes remain in the
  OS buffer.
- Kernel socket HWM reached: stop dequeueing from SocketTable; its configured
  buffer limit remains authoritative.
- TLS plaintext HWM reached: pause plaintext delivery, continue only bounded
  protocol-required transport work.
- HTTP/2 stream HWM reached: withhold application consumption/window updates
  while respecting connection progress and local memory limits.
- Bridge call registry full: reject before sending a host request.
- Handle mailbox full: await only from a non-progress-critical async producer;
  otherwise return typed overload/EAGAIN.
- Protocol event egress full: retain durable state and keep the wake coalesced;
  do not append another event.

Increasing a queue bound is an operational tuning action, not a correctness
mechanism.

### 19.1 Admission rules

Count and byte permits are acquired before reading, accepting, decoding, or
allocating the bounded object. A producer may not read 64 KiB and then discover
that no 64 KiB reservation exists. If the underlying API requires a temporary
read buffer, that reusable buffer is itself charged to the owning capability.

Permit ownership follows bytes through this path:

~~~text
transport task -> protocol decoder -> VM batch -> guest copy/transfer -> release
~~~

At every arrow, exactly one owner is charged. Borrowed views do not create a
second charge, and a copy must acquire a second reservation until the old copy
is released. OS-owned TCP receive buffers are not sidecar allocations; once
bytes enter sidecar memory they are charged. Kernel SocketTable storage is
charged by the same aggregate ledger as native and protocol buffers.

Every producer is statically classified as one of:

- **wait-capable**: may asynchronously wait for admission because it owns no
  resource needed by the consumer;
- **reject-capable**: returns a typed overload result immediately;
- **coalescing**: records bounded durable state and suppresses another wake;
- **source-pausable**: disables read/accept/window interest until budget is
  restored.

A progress-critical producer may not await a queue drained by work that the
producer itself blocks. In particular, protocol ingress must route registered
responses and termination before applying backpressure to ordinary events.
Because stdio is one ordered byte stream, pausing that stream on an ordinary
event can trap a later bridge response behind unread bytes. The protocol must
therefore prevent ordinary senders from exceeding advertised credits, or make
ordinary admission nonblocking after decoding a size-bounded frame. Separate
logical priority lanes do not create physical priority on a single pipe.

| Full resource | Required action |
| --- | --- |
| TCP/Unix application read budget | Stop application reads; leave bytes in the transport |
| Listener admission | Stop accept interest until a capability slot is available |
| UDP datagram budget | Stop receive interest or record a bounded, observable drop according to the configured UDP policy |
| TLS protocol budget | Preserve only the reserved handshake/shutdown allowance; time out rather than grow it |
| HTTP/2 data budget | Withhold consumption and window credit before allocating DATA bytes |
| Handle write mailbox | Return backpressure/EAGAIN or await from a non-critical async caller with a permit already reserved |
| VM ready set | Reserve one possible entry when the capability is admitted; reject capability creation, never readiness for an already-admitted capability |
| Ordinary wake lane | Leave wake state Outstanding; merge durable revisions and do not enqueue another wake |
| Registered bridge response | Settle its reserved waiter directly, including a typed oversize error; never enqueue it as an event |
| Ordinary multiplexed protocol ingress | Use sender credits or nonblocking bounded admission; after a frame is read, coalesce it or return/fail it explicitly without blocking later registered responses |
| Termination/control | Set reserved state and wake through the reserved lane |

The reserved TLS allowance is bounded by bytes and time and is unavailable to
application plaintext. HTTP/2 connection and stream windows are never treated
as AgentOS memory reservations: window credit is granted only after AgentOS
budget is available.

## 20. Fair scheduling

Tokio's cooperative scheduler prevents some task monopolization but does not
provide tenant fairness. AgentOS owns fairness.

Required scheduling levels:

- process: rotate VMs with ready work;
- VM: rotate capabilities;
- capability: limit operations and bytes per turn;
- listener/UDP: limit accepts or datagrams per turn;
- HTTP/2: rotate streams within a connection;
- bridge: limit completions per VM turn;
- internal pump RPC: limit ready/read/complete service work per VM turn so a
  hot VM cannot monopolize the shared runtime while other VM executor threads
  wait for correlated pump results;
- signals: limit delivered handlers per VM turn.

Budgets use both counts and bytes. A workload with tiny messages cannot evade
count limits; a workload with few huge messages cannot evade byte limits.
Control, cancellation, terminal errors, and registered responses have reserved
progress independent of ordinary data.

The initial scheduler uses hierarchical deficit round robin:

1. The process broker rotates nonempty VM ready queues.
2. Each selected VM rotates ready capabilities.
3. A capability consumes at most its operation and byte quantum.
4. HTTP/2 connections apply another round-robin/deficit level to streams.
5. A task that exhausts a quantum requeues level state and yields to Tokio.

Queue membership is coalesced, so requeueing a still-ready VM or capability
does not add a second entry. Deficits are capped to prevent an idle tenant from
banking an unbounded burst. Weights, if exposed later, are bounded policy input;
the default weight is one.

With `N` continuously ready equal-weight VMs, each conforming VM receives a
turn within at most `N` process selections, excluding time spent in guest code.
The same bound applies to `M` ready capabilities within a VM. Wall-clock
service is not promised while that VM's own executor is running synchronous
guest code, but the VM cannot delay another VM or direct bridge response.

Every drain loop has an explicit count and byte budget in code and a test that
forces exhaustion. Relying only on Tokio's cooperative budget, `yield_now`, or
channel FIFO order is insufficient.

## 21. Errors and observability

### 21.1 Error contract

Guest-visible transport failures match Node/POSIX behavior where applicable:
ECONNRESET, EPIPE, ETIMEDOUT, EADDRINUSE, EMFILE, ENOBUFS, EAGAIN, and related
codes.

AgentOS policy/resource errors include structured fields:

~~~text
code: ERR_AGENTOS_RESOURCE_LIMIT | ERR_AGENTOS_OVERLOADED |
      ERR_AGENTOS_CANCELLED | ERR_AGENTOS_STALE_GENERATION
message
limit_name
configured_limit
current_usage
requested
unit: items | bytes | tasks | workers | connections | streams
scope: process | vm | capability | connection | stream
vm_id
session_generation
capability_id, when applicable
operation, when applicable
configuration_path
retryable
errno, when a Node/POSIX surface requires one
~~~

The message names how to raise the limit. Errors are returned to the caller and
also visible through host logs or structured traces when they terminate a
session or indicate sustained pressure.

Guest-visible errors expose usage only for that guest's VM or capability. A
process-wide limit may be named as the cause, but aggregate usage and other-VM
identities remain host-only telemetry; typed overload reporting must not become
a cross-tenant occupancy oracle.

Linux/Node errno and AgentOS policy identity are not conflated. For example,
descriptor exhaustion caused by an AgentOS capability limit may be exposed as
`EMFILE` to a Node API while retaining `ERR_AGENTOS_RESOURCE_LIMIT`, scope, and
configuration fields in the structured cause. A transient full nonblocking
mailbox may map to `EAGAIN` and `retryable: true`; a configured hard byte limit
is not described as transient.

No fallible send, serialization, task join, response settlement, or event
delivery is ignored. A deliberate stale or coalesced outcome is recorded
through an explicit branch, not let _ = or unwrap_or_default.

Near-limit warnings are edge-triggered per limit and scope, rate-limited, and
rearmed only after usage falls below a lower hysteresis threshold. If structured
host egress is the resource under pressure, a compact warning or fatal error
falls back to sidecar stderr. Warning delivery may be coalesced, but warning
state itself is bounded and observable.

### 21.2 Metrics

At minimum:

- Tokio worker utilization and long poll durations;
- VM and blocking executor active/queued work;
- active VMs, capabilities, tasks, and bridge calls;
- ready-set size and age per VM;
- wake attempts, coalesced wakes, delivered wakes, and re-arms;
- read/write bytes, paused duration, and fairness yields;
- every channel's count and byte high-water mark;
- kernel/native/TLS/HTTP2/bridge buffer reservations;
- response latency split by host, router, VM dispatch, and guest settlement;
- stale/duplicate/late responses and completions;
- signal coalescing and overflow;
- HTTP/2 connection/stream flow-control stalls;
- all near-limit warnings and terminal limit errors.

Tracing links VM ID, generation, capability ID, call ID, operation ID, and
wake epoch without logging guest secrets or payload contents by default.
Metrics labels remain low-cardinality: VM, capability, call, and operation IDs
are trace fields, not metric labels. Metrics for a retired VM are removed or
aggregated when teardown finishes.

Every supervised task reports one terminal reason: completed, cancelled,
failed, or panicked. An unexpected task exit transitions the capability or VM
to failed and settles its operations; it is not merely a metric increment.

### 21.3 Watchdogs

A Tokio task that fails to yield within the configured diagnostic threshold is
logged with task class and scope. VM executor CPU and wall-clock limits remain
separate. A guest CPU loop is expected to block its executor until termination;
it must not appear as a stuck Tokio worker.

Watchdog records are bounded and rate-limited. A runtime-worker stall triggers
stderr fallback because the normal telemetry task may be unable to run.

## 22. Configuration

All limits have safe nonzero defaults in sidecar-owned configuration. Clients
send overrides only when callers explicitly provide them.

The implementation must expose canonical fields for:

- Tokio runtime worker count;
- V8 platform worker count (fixed at four in this contract);
- maximum active VM executor threads;
- blocking executor worker, job, and queued-byte limits;
- process and per-VM capability counts;
- ready handles and work/byte quanta per turn;
- handle command count and bytes;
- pending bridge call count and response bytes;
- async completion count and bytes;
- process and per-VM aggregate socket/protocol buffered bytes;
- UDP datagram count and bytes;
- TLS buffer bytes;
- HTTP/2 connections, streams, headers, and data bytes;
- protocol ingress/egress frame count and bytes;
- shutdown and operation deadlines.

Exact public field spelling must be reconciled with existing
NativeSidecarConfig and ResourceLimits during the configuration migration.
There will not be undocumented environment-only escape hatches. Every typed
limit error reports the final canonical field path.

Runtime worker count and blocking-worker count are immutable after process
startup. Per-VM overrides may only reduce process ceilings. Aggregate limits
must be no smaller than mandatory reserved control/response capacity, and
protocol sub-limits must fit within their aggregate parent. Invalid
relationships fail sidecar startup with the canonical field paths; they are
not silently clamped. Ready-set capacity is reserved by admitted capabilities,
so the per-VM capability ceiling cannot exceed its ready-entry capacity. Zero
never means unbounded.

## 23. Migration program

These phases are implementation order and review gates within one migration
program. TCP, Unix, UDP, TLS, HTTP/2, signals, Python, standalone WASM, and V8
adapters are not optional follow-ups. The old and new pumps must not remain
long-term selectable runtime modes.

### Phase 0: Lock the failure and inventory

- Keep the isolated applySyncPromise plus 257 StreamEvent reproduction.
- Add metrics to the current mixed session channel, deferred queue, and every
  current socket event producer.
- Generate a production-path manifest of runtime builders, thread spawns,
  unbounded channels, recurring timers/polls, ignored sends, payload fallbacks,
  and unsupervised tasks. Classify test-only sites separately rather than
  hiding production sites in a broad source allowlist.
- Include kernel DNS and zombie reaping, V8 event bridges and timeout helpers,
  deferred kernel waits, tool invocations, node-import materialization,
  snapshot pre-warm, plugin setup, heartbeat/stdio, Python, and HTTP/2 in that
  manifest; the inventory is not limited to socket readers.
- Record current event ordering and compatibility fixtures.
- Record thread/task/channel/buffer counts at idle and under increasing VM,
  socket, signal, bridge-call, and HTTP/2 load.

Exit gate:

- the incident reproduces deterministically without ACP;
- every production runtime/thread/unbounded-channel/timer/task site is
  classified with an owner, bound, cancellation path, and destination phase;
- CI can distinguish the reviewed production allowlist from test fixtures.

### Phase 1: Separate bridge responses from events

- Introduce BridgeCallRegistry and generation-aware direct waiters.
- Register before host dispatch.
- Route sync and async responses directly.
- Move termination to cancellation/control state.
- Remove ChannelResponseReceiver's mixed-channel scan and the deferred sync
  queue.
- Replace blocking ordinary session-command admission with bounded,
  source-aware admission.
- Implement generation-aware logical routing and the decision-9 dedicated fd 3
  response/control transport. Continue to use nonblocking post-decode
  admission so a full ordinary logical lane cannot stop response/control
  classification.
- Until Phase 3 removes per-socket StreamEvent amplification, bound the legacy
  ordinary-event backlog and reject or coalesce explicitly. Phase 1 fixes
  response progress; it does not claim the old event producer is memory-safe.

Exit gate:

- with every ordinary event/session queue at capacity, a blocked sync call
  still receives its registered response and the session remains usable;
- response and termination progress do not depend on event queue capacity;
- an ordinary producer either consumes a credit, coalesces durable state, or
  receives a typed rejection; no protocol or VM thread blocks indefinitely;
- duplicate, late, wrong-generation, oversize, and cancellation tests pass.

This phase fixes the immediate 256-message crash. Later phases remove the
producer amplification and make the fix robust under real socket load.

### Phase 2: Establish process runtime ownership

- Build one fixed-worker multi-thread SidecarRuntime at process entry.
- Pass RuntimeContext/Handle to all trusted async subsystems.
- Add the bounded blocking executor.
- Remove the static blocking-dispatch runtime and the S3/plugin setup runtimes;
  move async SDK/plugin setup to SidecarRuntime and blocking work to the
  bounded executor.
- Replace the kernel DNS thread plus owned runtime with an injected
  sidecar-owned async resolver service. Any unavoidable blocking resolver call
  uses bounded blocking admission.
- Remove the Python-owned runtime even though Python API migration completes in
  Phase 7.
- Route node-import materialization, tool host calls, and other finite blocking
  setup jobs through the bounded executor instead of spawning per request.
- Move heartbeat and ordinary timeout scheduling to SidecarRuntime. Treat
  V8-thread-sensitive snapshot construction as the accepted bounded
  maintenance-thread exception in section 27.
- Add a source/build audit rejecting production Tokio runtime construction
  outside the entrypoint.

Exit gate:

- exactly one Tokio runtime builder is reachable in the production sidecar
  binary, including kernel and plugin code;
- creating VMs, DNS resolvers, HTTP/2 sessions, plugin clients, and
  Python/WASM adapters creates zero additional Tokio runtimes;
- every blocking submission acquires count and byte permits before spawn and
  has cancellation, deadline, and join ownership;
- runtime and blocking-worker census metrics match configured fixed counts.

### Phase 3: Land the session broker and guest dispatcher

- Add per-VM ReadyState, revisioned entries, Idle/Outstanding wake epochs, and
  a capacity-one wake lane.
- Implement bounded ready_batch, read/accept/datagram batches, and
  complete_wake.
- Add per-VM fairness quanta and async completion batches.
- Replace the per-execution V8 event-bridge thread, per-sync-RPC timeout thread,
  and per-deferred-kernel-wait thread with the VM dispatcher, SidecarRuntime
  timers/readiness, and reserved cancellation state.
- Supervise every dispatcher and broker task in its VM task set.
- Unify NetSocket with the guest node:stream singleton.
- Implement real Duplex read, write, drain, ref, unref, destroy, and ordering
  semantics.

Exit gate:

- one million repeated ready marks before a guest turn produce at most one
  queued wake;
- no wake loses a concurrent readiness transition;
- push(false) and _read drive sidecar read interest in conformance tests;
- the number of event, wait, and timeout OS threads does not increase with
  calls or readiness transitions.

### Phase 4: Migrate TCP, Unix, and listeners

- Replace reader and accept threads with shared-runtime handle tasks.
- Remove std MPSC socket data queues and polling timeouts.
- Use the unified capability registry for native and kernel sockets.
- Implement bounded connect, accept, read, write, shutdown, close, and timeout.
- Derive VM/generation authority from execution context and enforce permission
  checks after DNS resolution for every candidate, at bind/listen, and before
  every authority-expanding backend transition.

Exit gate:

- socket/thread count is independent;
- task count grows by no more than the documented bounded cardinality per
  admitted handle and returns to baseline after close;
- idle sockets consume no polling CPU;
- paused streams do not grow sidecar application buffers;
- forged cross-VM capability IDs, stale generations, DNS answers that resolve
  outside policy, and duplicate raw/TLS ownership are rejected;
- Node ordering, half-close, error, and liveness tests pass.

### Phase 5: Migrate UDP and TLS

- Move UDP to datagram handle tasks and batched/coalesced readiness.
- Move TLS to shared-runtime transport tasks.
- Separate TLS transport progress from plaintext demand.
- Charge all protocol buffers to aggregate resource accounting.

Exit gate:

- datagram boundaries and bounded drops are tested;
- TLS handshake, pause, resume, renegotiation policy, shutdown, and failure
  cases cannot grow uncharged buffers or deadlock;
- read, decrypt, encode, and delivery copies obey the reservation-transfer
  rules and accounting returns to baseline on every failure path.

### Phase 6: Migrate HTTP/2 and signals

- Replace per-session HTTP/2 runtimes/threads and unbounded channels.
- Implement bounded connection ownership and stream fairness.
- Replace thread-per-signal delivery with the signal broker.
- Integrate ProcessTable pending-signal semantics and reserved termination.
- Replace the per-ProcessTable zombie-reaper thread with sidecar-driven bounded
  timer work while keeping the kernel API independent of Tokio.

Exit gate:

- connection/session count adds no runtimes or OS signal/socket threads;
- VM/process count adds no zombie-reaper threads;
- HTTP/2 and signal floods preserve response and termination progress;
- all HTTP/2 buffers and stream states are bounded and accounted;
- cancellation at each HTTP/2 handshake/stream state and each signal delivery
  boundary releases permits and cannot target a successor generation.

### Phase 7: Migrate all guest adapters

- Route Python and standalone WASM networking through shared capabilities.
- Remove adapter-owned runtimes, native socket pollers, and parallel resource
  policy.
- Preserve adapter-specific API semantics over common operations.
- Ensure adapters cannot supply VM/generation authority or expose internal wake
  acknowledgement operations to guest code.

Exit gate:

- all guest environments exercise the same permission, readiness, accounting,
  cancellation, and stale-generation tests;
- adapter creation adds no Tokio runtime or network polling thread;
- adapter-local queues and copies are included in the same aggregate memory
  ledger and expose the same typed limit metadata.

### Phase 8: Delete debt and enforce architecture

- Delete old per-event StreamEvent socket/signal delivery.
- Delete old socket reader threads, poll constants, and unbounded queues.
- Delete all subsystem runtime builders.
- Remove compatibility branches and obsolete configuration.
- Remove or migrate remaining per-operation production threads, including tool,
  import-cache, timeout, deferred-wait, DNS, and reaper workers. Keep only the
  explicitly approved fixed executors and constant stdio exception.
- Replace every ignored production send/settlement and payload fallback with an
  explicit stale/coalesced branch, typed propagation, or host-visible log.
- Turn source audits and architecture tests into CI gates.

Exit gate:

- all mandatory invariants in section 8 are mechanically or behaviorally
  verified;
- the only production Tokio runtime builder is the process entrypoint;
- every production `thread::spawn` call is inside the reviewed bounded-executor
  implementation, the approved admitted V8-maintenance path, or an approved
  constant-process exception;
- no production path uses an unbounded channel;
- no production networking, signal, protocol, or guest-adapter path uses a
  recurring polling timer. The separately tracked generic V8 platform-work
  pump is the sole explicit temporary exception and has its own removal issue.

## 24. Validation

### 24.1 Deterministic correctness tests

- Original 257-event sync bridge reproduction now succeeds once its direct
  response arrives, does not evict the session, and leaves no deferred events.
- The same test fills the ordinary logical lane on one stdio stream before the
  BridgeResponse frame and proves ingress still reaches the registered waiter.
- The selected decision-9 transport is tested with a partially transmitted
  maximum-size ordinary frame: either the independent response path still
  settles promptly, or the single-stream profile fails within its documented
  bounded head-of-line deadline without leaking a waiter.
- Sync call plus readiness, signal, async completion, termination, and module
  reader interleavings.
- Wake producer/consumer race at every complete_wake boundary.
- push(false) pauses actual native and kernel reads; _read resumes immediately.
- Data/end/error/close and connect/error/close ordering against Node fixtures.
- Write callback, writev, drain, destroy, and half-close behavior.
- Node high-water-mark backpressure is distinguished from AgentOS's explicit
  write-memory limit error in a pinned compatibility fixture.
- ref/unref liveness with only sockets, listeners, timers, and pending calls.
- VM generation teardown with late connect/read/write/bridge/TLS/H2 completion.
- Close/cancel/timeout/task-panic races at every operation state transition,
  using barriers or hooks rather than timing sleeps; each waiter settles once
  and each reservation releases once.
- A quarantined executor retains its permit and generation record until join;
  it cannot be replaced by a VM reusing identifiers.
- V8 initializes after fixed host workers already exist, then repeatedly
  creates, destroys, and refills isolates from descendant executor threads;
  protection-key defaults remain readable and no process-global code-pointer
  table access faults.
- Kernel and native backend conformance using the same operation fixture.
- TCP, Unix, UDP, TLS, and HTTP/2 permissions and POSIX errors.
- Cross-VM capability forgery, wrong-type IDs, stale capability/session
  generations, and guest-authored wake acknowledgements cannot affect live
  state.
- DNS permission tests validate every resolved candidate immediately before
  connect, including mixed allowed/denied answers and an answer change between
  resolutions; unconnected UDP validates every destination.
- Live policy tightening is either atomic with capability revocation or
  rejected without changing the generation's effective policy.
- Signal mask, pending/coalescing, handler, default, and termination behavior.

### 24.2 Bound tests

Fast tests configure small limits and prove the safeguard fires:

- waiter registry, ready set, handle commands, completion lane;
- aggregate and per-protocol buffer bytes;
- UDP datagram count;
- HTTP/2 connection/stream/header/body state;
- blocking jobs and VM executor admission;
- protocol ingress/egress.

Each bound test asserts the error code, scope, configured field path, requested
count/bytes, retryability, near-limit warning, stderr fallback where relevant,
and final zero/baseline accounting. Tests measure peak allocated/charged bytes
to prove admission occurs before allocation, not merely that a later queue
rejects the object.

Tests that try to prove absence of a bound by exhausting machine resources stay
ignored and are labeled expensive.

### 24.3 Fairness tests

- A hot TCP socket does not starve a second socket in the same VM.
- A hot VM does not starve another VM.
- A listener accept flood does not starve bridge responses.
- One HTTP/2 stream cannot monopolize a connection.
- One HTTP/2 connection cannot monopolize the sidecar.
- Signal/readiness floods cannot delay registered responses or termination
  beyond the explicit scheduling bound.
- Deficit carry is capped, and an idle VM cannot accumulate a burst that
  violates another VM's service bound.

Tests assert selection counts and byte quanta under a deterministic test
scheduler. Wall-clock stress tests supplement but do not replace these checks.

### 24.4 Topology tests

- Runtime builder audit.
- Production thread-spawn audit with a narrow allowlist for entrypoint,
  bounded executors, approved V8 maintenance work, and unavoidable constant
  stdio integration.
- Unbounded channel audit.
- Polling timer audit for networking, signals, protocol delivery, and guest
  adapters, with the generic V8 platform-work pump named as the only temporary
  exception.
- Task and thread counts under increasing VM/socket/session load.
- Runtime census proving kernel DNS, plugins, Python, and HTTP/2 reuse the
  process runtime.
- Supervision audit proving every spawned Tokio task belongs to a process, VM,
  capability, connection, or bounded background task set.

The source audit checks production build reachability and a reviewed manifest;
a regex allowlist that accidentally exempts a `#[cfg(not(test))]` site is not
sufficient. Runtime census tests catch constructors hidden behind wrappers or
dependencies.

### 24.5 Performance tests

Compare old baseline and target for:

- idle CPU with many sockets;
- throughput and tail latency for small and large TCP writes;
- wake-to-byte ratio;
- bridge crossings per MiB;
- paused-stream memory;
- multi-VM fairness;
- TLS and HTTP/2 throughput;
- VM create/destroy churn.

Phase 0 records platform-specific acceptance budgets for idle CPU, throughput,
tail latency, wake amplification, and memory. A regression outside an approved
budget blocks that backend's exit gate and requires an explained measurement,
not removal of a bound. Performance cannot waive a bound or direct-response
invariant.

### 24.6 Fault injection and long-running validation

- Close each command, wake, completion, response, and protocol channel while a
  producer is active.
- Panic each supervised task class and verify typed settlement and cleanup.
- Cancel DNS, connect, accept, TLS handshake, HTTP/2 handshake/stream, and
  plugin setup at every await boundary.
- Repeatedly create/destroy VMs while old-generation completions arrive.
- Run multi-VM TCP/UDP/TLS/HTTP/2/signal/bridge soak tests and assert bounded
  resident memory, stable task/thread counts, and no accounting drift.
- Run loom-style model tests where practical and deterministic adversarial
  schedules elsewhere; sleeps are not evidence that a lifecycle race is safe.

Machine-exhaustion variants are explicitly expensive/ignored. Small configured
limits that deterministically fire remain in the default suite.

## 25. Rollout and rollback

Land the migration behind internal implementation checkpoints, not a public
old/new architecture option. Each phase must preserve protocol lockstep across
sidecar and clients. Once a backend passes its exit gate, remove its old pump
before completing the program.

Rollback is revision rollback of the migration, not a runtime flag that
silently restores unbounded queues or per-socket threads. Because the wire
protocol ships in lockstep and live VM state is not migration-compatible,
rollback drains/restarts sidecars with the matching clients; it does not move a
live capability or bridge waiter between implementations.

Roll out each migrated backend through tests, internal canaries, and then the
default path. Before advancing, compare typed limit errors, queue/buffer high
water marks, stale completions, task/thread census, wake amplification,
response tail latency, and Node conformance against the Phase 0 baseline.
Unexpected session eviction, accounting drift, runtime/thread growth, a lost
registered response, or a teardown leak stops the rollout.

During development, temporary dual-path code must have:

- one owner;
- a removal issue/checkpoint;
- no shared capability ID ambiguity;
- tests proving only one path owns a descriptor;
- separate accounting ledgers so reservations cannot be charged or released by
  both paths;
- no production configuration that lets users select the legacy path after its
  backend exit gate passes.

## 26. Rejected alternatives

### One Tokio runtime per VM

Rejected. It multiplies I/O drivers and worker pools, weakens global admission,
complicates shutdown, and still cannot safely run synchronous V8 on a Tokio
worker. VM isolation comes from capabilities, generations, quotas, and
executor boundaries, not a runtime per VM.

### One current-thread Tokio runtime per HTTP/2 session or subsystem

Rejected. This is current migration debt. It creates threads/runtimes with
connection count and fragments policy and observability.

### Run each VM as a Tokio task

Rejected. Synchronous hostile JavaScript can block a runtime worker, and V8 is
thread-affine. A VM supervisor may be a Tokio task; guest execution is not.

### Keep per-socket threads and only coalesce wakes

Rejected. It reduces bridge amplification but retains thread growth, polling,
unbounded data queues, and fake transport backpressure.

### Raise the deferred queue above 256

Rejected. It postpones the deterministic failure and consumes more memory
without correcting response routing.

### One unbounded channel with consumer fairness

Rejected. Fair draining does not bound producer memory or preserve response
and termination progress.

### Logical priority lanes without protocol admission

Rejected. A priority response queue helps only after a frame has been read and
routed. On one ordered stdio stream, an unread ordinary frame still sits before
a later BridgeResponse. Sender credits, nonblocking bounded admission, or a
physically separate response transport is required in addition to logical
lanes.

### Unrestricted tokio::spawn_blocking

Rejected. Tokio's blocking pool is not per-VM admission control and can grow
threads or queued work independently of AgentOS limits. Blocking work first
acquires AgentOS count and byte permits and runs through the fixed bounded
executor.

### Put Tokio inside the kernel

Rejected. The kernel's virtual socket, process, signal, and accounting
semantics are reusable without coupling them to one async runtime or guest
engine. The sidecar adapter owns Tokio integration.

## 27. Owner decisions

Decisions 1 through 9 are accepted implementation choices:

1. **Runtime worker default.** Should the fixed default be available
   parallelism with a small cap, or a fixed product value? Recommendation:
   available parallelism capped at 4 initially, with an explicit sidecar
   override and metrics.
2. **V8 maintenance threads.** Snapshot pre-warm currently requires a fresh,
   fully joined thread because reusing the building thread has corrupted later
   isolate creation. Should this be an explicit admitted exception?
   Recommendation: allow at most one serialized, ephemeral V8-maintenance
   thread, charged against the V8 executor limit, until a safe in-executor
   design is proven. It remains in the production thread manifest.
3. **Canonical configuration names and process memory parent.** Reuse and
   extend existing
   NativeSidecarConfig/ResourceLimits fields or introduce a nested runtime
   section in one lockstep protocol change, and what should name the aggregate
   process parent above per-VM socket and bridge limits? Recommendation: one
   nested sidecar-owned schema, with a required process buffered-memory limit
   and compatibility aliases removed before completion.
4. **stdio threads.** Are the process's blocking stdin/stdout integration
   threads accepted as a narrow architecture exception, or should Unix
   AsyncFd/Windows equivalents join SidecarRuntime? Recommendation: allow a
   two-thread, constant process exception initially. Heartbeat, event pumping,
   warnings, and routing still move to SidecarRuntime and bounded lanes.
5. **Standalone kernel workers.** May the reusable kernel retain its default
   DNS runtime/thread and per-ProcessTable reaper outside the sidecar build, or
   should all native callers inject and drive those services? Recommendation:
   remove owned workers from the kernel API; use an async-neutral resolver and
   reaper interface, with explicit adapters for tests or standalone callers.
6. **Migration landing policy.** May intermediate phases merge when their exit
   gates pass, or must the entire multi-backend migration land atomically?
   Recommendation: merge phases behind internal code structure, but do not
   declare the architecture complete or retain permanent runtime fallbacks
   until Phase 8.
7. **Bridge response identity.** Should the response wire envelope carry VM ID
   and session generation, or should globally unique call IDs plus bounded
   tombstones provide equivalent validation? Recommendation: put generation
   identity on the lockstep protocol response. It is simpler to audit and does
   not make stale-response safety depend on tombstone retention.
8. **Response-byte reservation.** Must every bridge call pessimistically
   reserve its declared maximum response before dispatch, or may large
   responses use an explicitly bounded streaming/chunk-credit protocol?
   Recommendation: reserve ordinary responses end to end, set a practical
   per-call maximum, and require chunk credits for intentionally large
   results. Never admit a call whose successful response has no guaranteed
   completion capacity.
9. **Ordered ingress transport.** Registered bridge responses and termination
   use the required inherited full-duplex fd 3 response/control stream. Fd 0
   and stdout remain the ordinary request and event streams. Shutdown is a
   typed `ControlFrame`, not a priority reinterpretation of an ordinary request.

The one-thread-per-admitted-session initial V8 topology, VM-wide aggregate
socket memory semantics, UDP receive-pause policy, wake epoch handshake,
hierarchical scheduler, direct response registry, and quarantine behavior are
decisions in this specification, not open product options. They may change
implementation shape without weakening their stated invariants.

## Appendix A: Initial source audit anchors

The implementation inventory must begin with this reviewed production map. Line
numbers will move; CI tracks the owning symbol and destination, not only text
matches.

| Source area | Current production debt or reusable primitive | Destination |
| --- | --- | --- |
| crates/native-sidecar/src/stdio.rs | Current-thread sidecar runtime; blocking stdin/stdout and heartbeat threads; event poll timer; unbounded warning/error lanes | Phase 2 runtime/heartbeat/warning migration; Phase 3 broker; only approved stdin/stdout threads remain |
| crates/native-sidecar/src/service.rs | Static current-thread blocking-dispatch runtime; thread per deferred kernel wait; timer polling loops | Phase 2 runtime/executor; Phase 3 readiness/timer broker |
| crates/native-sidecar/src/execution/ | Tool invocation workers; TCP/Unix/TLS reader threads; listener/UDP polling; HTTP/2 runtime/thread per session and unbounded commands; thread-per-signal | Phases 2 through 6 by subsystem |
| crates/native-sidecar/src/state.rs | Socket and HTTP/2 event queues and maps | Phases 3 through 6 bounded capability/broker state |
| crates/native-sidecar/src/vm.rs | Kernel SocketReadiness converted to per-event StreamEvent with ignored send/fallback paths | Phases 3 and 4 unified ready state and explicit errors |
| crates/native-sidecar/src/plugins/s3_common.rs and other plugins | S3 creates a thread and Tokio runtime for setup; blocking plugin work has local ownership | Phase 2 shared runtime/bounded executor |
| crates/v8-runtime/src/session.rs | Bounded VM/warm-worker threads; mixed 256-entry command channel; deferred sync queues; blocking sends and joins | Phase 1 direct waiters; Phase 3 bounded VM executor/dispatcher |
| crates/v8-runtime/src/embedded_runtime.rs | Constant dispatch thread; bounded runtime-event/output channels that mix event classes | Phases 1 and 3 direct router/session broker |
| crates/execution/src/javascript.rs | Per-sync-RPC timeout thread; pipe reader/writer threads; per-execution V8 event bridge; polling and guest stream implementation | Phases 2 and 3 executor, timers, dispatcher, and exact Duplex; pipe exceptions must be explicit |
| crates/execution/src/python.rs | Current-thread runtime in wait; per-VFS-RPC timeout thread; adapter polling | Phase 2 runtime/timer removal; Phase 7 shared capability adapter |
| crates/execution/src/node_import_cache.rs | Unbounded channel and a newly spawned materialization thread per attempt | Phase 2 deduplicated bounded blocking job |
| crates/execution/src/v8_host.rs | Fresh joined thread for snapshot pre-warm because of V8 thread-state sensitivity | Phase 2 admitted maintenance path; accepted decision 2 |
| crates/kernel/src/dns.rs | Per-resolver OS thread, unbounded std MPSC, and owned multi-thread Tokio runtime | Phase 2 injected sidecar resolver and bounded admission |
| crates/kernel/src/process_table.rs | Per-ProcessTable zombie-reaper thread; reusable signal mask/pending semantics | Phase 6 sidecar-driven timer plus kernel-neutral API |
| crates/kernel/src/socket_table.rs | Bounded virtual socket data and empty-to-nonempty readiness callbacks | Phases 3 and 4 unified capability readiness |
| crates/kernel/src/resource_accounting.rs | Kernel socket counts/bytes only | Phases 3 through 7 aggregate process/VM/backend accounting |

Test-only mock servers, race tests, and fixture runtimes remain permitted when
their lifecycle is local and joined. `#[cfg(test)]` is not a reason to omit a
shared helper that is also called by production. The audit must also inspect
dependencies and wrapper functions so an aliased runtime constructor or
channel factory cannot bypass the manifest.

## Appendix B: Completion checklist

The current revision closes the phase gates with the following retained
regression requirements:

- JavaScript UDP sends and receives through both VM-local kernel sockets and,
  for policy-authorized external destinations, a lazily activated native
  socket. One native handle task exclusively owns each descriptor, its bounded
  command lane, readiness, send completion, and receive batches. The OS retains
  unread datagrams, one `READABLE` wake remains pending, and polling rearms only
  after `EAGAIN`. Connected UDP, disconnect, multicast/source membership, TTL,
  broadcast, multicast loopback/interface, and buffer-size options all route
  through that owner.

- JavaScript native TCP/Unix and Python native TCP connection establishment
  run as process-runtime Tokio operations. Each commits its socket/capability
  only when its generation-checked, bounded dispatcher completion is handled;
  VM-local kernel loopback remains an immediate nonblocking operation.
- `maxAsyncCompletions` is one VM-wide aggregate reservation across native
  socket/listener completion lanes and in-process V8 session output lanes.
  Per-lane capacities remain local safety bounds; each queued envelope owns one
  `ResourceClass::AsyncCompletions` reservation and releases it on dequeue,
  failed insertion, or receiver teardown. VM admission close wakes blocked
  senders without acquiring or leaking another reservation.
- Root and descendant Python/WASM `child_process` startup uses the asynchronous
  runtime adapters. Import-cache materialization and prewarm yield through the
  async process pump and preserve the existing deferred sync-RPC completion
  ordering instead of parking a trusted Tokio worker.
- JavaScript `dgram.poll` awaits the shared fairness broker and readiness path
  directly. It never calls `Handle::block_on` from a Tokio worker, so external
  UDP receive cannot panic by trying to start a nested runtime. Regression
  drivers drain the same bounded sidecar completion dispatcher as production
  before re-polling V8, preserving async connect response ordering.
- Plain TCP and Unix reader/writer tasks acquire the process fairness broker's
  committed VM-generation/capability identity after OS readiness and before
  each bounded nonblocking I/O turn. They do not retain a fairness grant while
  waiting for readiness or while publishing guest-visible events.
- Capability release retires that same fairness identity. Retirement removes
  queued or granted work immediately, lets an already-issued bounded turn
  settle before removing its scheduler entry, and rejects every later acquire
  for the retired identity. Monotonic capability IDs are retained as merged
  retirement ranges, so repeated socket churn cannot consume the bounded
  scheduler membership limit or add one tombstone per closed handle.
- VM-generation retirement records its compact tombstone before scheduler
  cleanup, revokes queued or unissued work, and lets an already-issued turn
  settle before removing the VM. Retired generations cannot reacquire service,
  including when retirement races an active turn.
- Backend-local socket strings are compatibility aliases only. Every
  JavaScript network operation resolves an alias through its live capability
  lease and validates VM session generation and capability kind before backend
  access; Python's trusted adapter aliases perform the same validation on
  send, receive, and close.
- Outbound TCP/UDP evaluates the requested hostname with ordinary policy
  semantics and then requires every resolved candidate with an applicable
  address rule to allow the operation. A rule-set default is not applied a
  second time to an address with no matching address rule, preserving hostname
  allowlists; literal IP requests retain ordinary IP-only semantics. Mixed
  safe/blocked DNS answers fail closed as a unit.
- Process-ledger rejections omit exact aggregate `current_usage` and `used`
  values from guest-visible diagnostics, with cross-tenant regression tests.
- Every admitted asynchronous bridge call reserves space in its physical
  response lane, including already queued responses. Registration rejects
  before host dispatch when the lane has no completion slot, and a route stays
  registered until delivery succeeds.
- Graceful Node sockets emit buffered data and canonical `end`, then wait for
  writable `finish` before `close` and exactly-once capability release.
  Destroyed loopback sockets cannot emit stale `connect`/`ready`, and listener
  accept wakes use one queued pump latch. `net.Server.close()` stops accepting
  immediately through the asynchronous response lane; its pending resolver
  keeps the VM pump live without blocking JavaScript. It does not emit `close`
  or invoke close callbacks until the sidecar has acknowledged transport
  teardown and all accepted sockets have emitted `close`, matching Node's drain
  gate.
- Structured security and DNS telemetry failures fall back directly to stderr
  without recursively depending on the failed bridge telemetry path.

- [x] One production Tokio runtime builder.
- [x] Fixed runtime workers and bounded blocking executor.
- [x] V8 guest execution never runs on Tokio.
- [x] Every production thread is a bounded executor member or approved constant/admitted exception.
- [x] Direct generation-aware bridge response routing.
- [x] No deferred sync event queue.
- [x] Full ordinary post-decode admission cannot block a registered response.
- [x] Decision-9 physical ingress guarantee prevents an incomplete ordinary frame from blocking a registered response or typed shutdown indefinitely.
- [x] Capacity-one wake and durable ready state per session.
- [x] Real Node-compatible Duplex backpressure and liveness.
- [x] Unified native/kernel capability registry with generation-checked
  compatibility aliases.
- [x] TCP, Unix, listener, UDP, TLS, HTTP/2, and signal migration.
- [x] Python and standalone WASM use shared capabilities.
- [x] Browser sources are retained but excluded from default builds, CI, and publication.
- [x] Kernel DNS and zombie reaping use injected sidecar services without owned runtimes/threads.
- [x] No socket/session/signal/wait/timeout/tool OS thread amplification.
- [x] No unbounded production channel in the native reactor dependency closure,
  and no recurring network/signal/adapter poll.
- [x] Count and byte limits, warnings, typed errors, and configuration paths.
- [x] Admission precedes allocation and reservation ownership transfers exactly once.
- [x] Generation-safe teardown and stale completion tests.
- [x] Quarantined executors retain permits and cannot leak into a successor VM.
- [x] Every task is supervised and every terminal reason is handled.
- [x] Metrics are low-cardinality and fatal telemetry has stderr fallback.
- [x] Fairness, topology, conformance, and incident regression gates.
- [x] Rollout census and accounting show no drift under VM churn and soak load.

### B.1 Completion validation record

The completion revision was validated on the non-browser surface with:

- `node scripts/check-rustfmt.mjs` (Cargo default members only) and non-browser
  workspace clippy with warnings denied;
- the non-browser Rust workspace test graph, including the production topology
  manifest, single-owner UDP guard, task supervision, resource accounting, and
  executor quarantine, followed by explicit ignored-test invocations for the
  50,000-generation runtime churn and the 100-VM protocol/reactor soak;
- `pnpm build`, `pnpm check-types`, and `pnpm test` (156 of 156 tasks);
- generated Rust/TypeScript protocol byte parity and an idempotent V8 bridge
  bundle regeneration;
- the local Node ecosystem matrix against the exact rebuilt sidecar: Express,
  Fastify, WebSocket, Axios, node-fetch, and a real Hono Node server/client flow
  (7 of 7 runnable cases);
- the packaged AgentOS core adapters against local Anthropic-compatible model
  fixtures: Pi tool execution and live updates (2 of 2), OpenCode session,
  provider, tool, permission, cancellation, and resume flows (9 of 9 runnable
  cases), and Claude shell, nested child process, session, cancellation, mode,
  and raw ACP flows (8 of 8); plus the incident regression delivering 256
  ordinary updates during a delayed tool response and then reusing the same ACP
  session;
- npm and Rust publish discovery, fixed-version, frozen-lockfile, and
  idempotent generated compatibility-mirror checks;
- the release-mode runtime benchmark drift and latency gates.

The repository-wide Biome command remains an advisory CI step and reports the
pre-existing formatting backlog outside this migration. Changed bridge/client
files pass targeted Biome checks. Browser runtime sources remain present and
remain workspace members, but are excluded from default workspace commands,
build, test, CI, and publish discovery with comments recording the intentional
hold.

### B.2 Audited follow-ups outside the completion gates

- The host-side actor plugin still serializes `ActorJob` values through a Tokio
  unbounded channel. It is not in the native sidecar/reactor dependency closure
  and cannot amplify reactor wakes or bridge responses, but it violates the
  repository-wide bounded-queue rule and must be migrated separately.
