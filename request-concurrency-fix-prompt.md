# Native-sidecar P0 request concurrency and protocol progress

Status: implemented; final P0 validation passed on 2026-08-05

Owner: agentOS native sidecar

Scope: P0 protocol correctness and guaranteed progress
Working revision: `zid-sleeper-agent`

Checkbox legend: `[x]` means the implementation exists and its mapped active
test passed in this working copy. `[ ]` means incomplete; parenthetical text
calls out partial implementation or coverage that must not be mistaken for P0
acceptance.

## Current audited status

- [x] A sleeping ACP prompt no longer retains the native protocol coordinator.
- [x] ACP cancel, permission response, machine callback response, and bounded
      unload/delete teardown can progress while a prompt is active.
- [x] Different ACP routes can have prompts in flight concurrently.
- [x] The public-client regression for two prompts plus concurrent file
      write/read is checked in and active.
- [x] Output producers no longer synchronously wait for stdout/fd-3 capacity.
- [x] The public-client two-prompt/filesystem regression passes against a
      freshly built shared sidecar.
- [x] Ordinary extension and generic requests are synchronously admitted and
      prepared, then started under independently tracked `JoinSet` tasks.
- [x] Production stdio ingress never calls `dispatch_wire(...).await`; that
      API remains only for direct in-process compatibility.
- [x] Connection/session/VM ownership, cloneable `VmHandle`s,
      `RoutedExtensionServices`, internal-event admission, and bounded
      shutdown drain/flush satisfy the complete P0 contract.

## Completion contract

This document is both the design contract and the implementation checklist. A
checkbox may be marked complete only when the code exists, the corresponding
test is active, and the listed acceptance command passes.

The completed implementation must maintain this invariant:

> The native stdio ingress router may decode, validate, reserve bounded
> admission, register a request, and route a frame. It must never await business
> execution, output capacity, guest/runtime activity, a host callback, or event
> drainage.

The reader, router, operation execution, state coordination, output broker, and
physical writers are separate progress domains. Backpressure in one domain must
not stop cancellation, registered responses, shutdown, or unrelated admitted
work in another.

## Why this is required

Before this change, the protocol loop awaited
`NativeSidecar::dispatch_wire(&mut self, ...)` through completion. An ACP
`session/prompt` could remain pending indefinitely, so one prompt held
exclusive mutable access to the complete sidecar and prevented unrelated
request dispatch. `dispatch_wire` remains as a direct in-process compatibility
API, but production stdio ingress cannot reach it.

The retired ACP-specific interrupt workaround examined one additional ordinary
frame. If that frame was unrelated, it stored the frame in `pending_frame` and
resumed awaiting the prompt. A later cancel remained behind that frame:

```text
prompt A starts
-> unrelated request B arrives
-> B occupies pending_frame
-> cancel A arrives behind B
-> prompt A, request B, and cancel A cannot complete
```

Output had an independent progress defect: producer-side publication could
wait synchronously for writer capacity. If the host did not drain a lane, a
producer could park indefinitely and stop protocol progress. The physical
writer may still block on its dedicated thread; producers now use reserved,
nonblocking or async broker publication and never wait on that thread's
condition variable.

These are generic routing and ownership defects. ACP must remain an opaque
extension over the generic extension contract; native-sidecar code must not
decode ACP payloads.

## P0 scope

P0 consists of five coupled changes:

- [x] P0.1: Decouple every ordinary extension and generic request from protocol
      ingress through register/prepare/start; route shutdown directly with a
      bounded drain.
- [x] P0.2: Partition connection/session/VM ownership, VM state handles,
      extension services, lifecycle ordering, and internal-event work.
- [x] P0.3: Route progress-critical messages directly with reserved admission,
      including bounded shutdown/disconnect drain and cleanup.
- [x] P0.4: Replace synchronous producer-side output waits with an output broker.
- [x] P0.5: Delete the ACP prompt interrupt workaround; the production-used
      real-loop harness passes without prompt-specific stdio routing.

P1/P2 fairness, blocking-I/O migration, observability, architecture cleanup, and
expanded load/fuzz work are recorded in the existing
`~/.agents/friction/agentos.md` occurrence. They are not allowed to weaken the
P0 invariants, but they are not P0 completion gates unless a P1 defect prevents
a P0 progress test from passing.

## Non-negotiable invariants

- [x] There is no global FIFO that serializes ordinary business requests.
- [x] There is no global `Arc<Mutex<NativeSidecar>>` or equivalent lock held
      across request execution.
- [x] Different connections, VMs, and extension sessions can have operations in
      flight concurrently.
- [x] A long operation never holds a mutable connection/session/VM critical
      section while it waits for external activity. Its lightweight operation
      registration remains live so teardown can cancel and drain it; that
      registration is not an exclusive state lease and does not serialize
      ordinary operations.
- [x] Same-entity conflicts have an explicit ordering key and bounded admission.
- [x] Every admitted request has exactly one terminal response.
- [x] Terminal responses preserve request ID and ownership and may complete out
      of order.
- [x] Active and queued operation counts and retained bytes are bounded by
      configuration.
- [x] A limit rejection names the observed use, limit, and configuration path.
- [x] Cancellation, permission responses, registered sidecar responses,
      shutdown, and terminal transport failure cannot wait for ordinary
      operation admission.
- [x] No output producer synchronously waits on a condition variable.
- [x] Ordinary output saturation cannot consume reserved control progress.
- [x] No fire-and-forget task may lose a panic, error, cancellation, or terminal
      response.
- [x] Shutdown stops new ordinary admission, signals active work, and drains or
      cancels it within a configured deadline.
- [x] Core native-sidecar routing remains extension-agnostic.

## Target architecture

```text
fd 0 reader                         fd 3 control reader
    |                                      |
    | bounded decoded frames               | direct registered responses
    v                                      v
+--------------------+             +------------------------+
| ProtocolIngress    |<------------| ProgressControlRouter  |
| - validate         |             | - callback response    |
| - reserve          |             | - cancel/permission    |
| - register         |             | - shutdown/error       |
| - route only       |             +------------------------+
+---------+----------+
          |
          | admitted operation
          v
+--------------------+       short commands       +----------------------+
| RequestSupervisor  |---------------------------->| Sidecar coordinators |
| - bounded permits  |                             | - connection/session |
| - operation table  |                             | - per-VM state       |
| - cancellation     |                             | - extension services |
| - terminal guard   |                             +----------------------+
+---------+----------+
          |
          +---------------------------> +----------------------+
          | event subscriptions         | EventBroker          |
          |                              | - durable demux       |
          |                              | - ownership waiters   |
          |                              | - no VM lock on wait  |
          |                              +----------------------+
          |
          | terminal response / events
          v
+----------------------+
| ProtocolFrameWriter  |
| - reserved control   |
| - ordinary events    |
| - async backpressure |
+-----+------------+---+
      |          |
      v          v
   stdout       fd 3
   writer       writer
```

Physical readers and writers may remain dedicated threads/tasks. The defect is
producer-side waiting in a progress-critical domain, not the use of a blocking
stdout writer on its own dedicated thread.

## P0.1 — Concurrent request supervision

### Admission model

There is no general ordinary-work FIFO after routing. The existing bounded
transport ingress channel absorbs decoder/router scheduling jitter only. Once
the router receives a valid `RequestFrame`, it either:

1. reserves an in-flight operation permit, retained request bytes, and one
   terminal-response reservation; registers the operation; and starts it, or
2. emits a typed overload response through reserved rejection capacity.

Add explicit runtime protocol configuration:

- `runtime.protocol.maxInFlightRequests`
- `runtime.protocol.maxInFlightRequestBytes`
- `runtime.protocol.maxTerminalFrames`
- `runtime.protocol.maxTerminalBytes`
- `runtime.protocol.terminalFallbackBytes`
- `runtime.protocol.maxProgressFrames`
- `runtime.protocol.maxProgressBytes`
- `runtime.protocol.maxRejectionFrames`
- `runtime.protocol.maxRejectionBytes`
- `runtime.protocol.shutdownGraceMs`

The values must be positive, included in runtime validation, exposed in queue
metrics, and covered by the limits inventory/architecture guards applicable to
runtime protocol configuration. `shutdownGraceMs` bounds only the draining
phase after shutdown closes ordinary admission; it does not extend an
operation's own timeout or permit new work.

A separate general request backlog is intentionally omitted. The already
bounded ingress channel is the only pre-admission ordinary backlog. This avoids
admitting work that cannot run and prevents cancellation from being hidden
behind an application FIFO.

### Operation record

Each admitted request has one logical tracked record split across the registry,
its operation handle, and the task supervisor:

```rust
struct OperationRecord {
    generation: u64,
    metadata: RequestOperationMetadata, // ownership + ordering
    request_bytes: usize,
    state: RequestOperationState,
    cancellation: CancellationToken,
    terminal: TerminalResponseGuard,
}

// RequestOperation carries the registry key/generation and admission
// accounting. JoinSet supervision, detached completion, and the terminal
// output reservation remain outside the registry record.
```

Required state transitions:

```text
Admitted -> Running -> Completing -> Terminal
                    -> Cancelling -> Completing -> Terminal
Admitted/Running -> Failed -> Terminal
Admitted/Running -> Shutdown -> Completing -> Terminal
```

The terminal guard atomically permits exactly one terminal response. A late
completion after cancellation is recorded and discarded without producing a
second response. Dropping an unfinished record is a hard logged invariant
failure and must synthesize a typed terminal error when transport is still
available.

Request IDs are unique within a connection, so the registry key is
`(connection_id, request_id)`. A duplicate in-flight ID receives a typed
conflict response and does not replace the original operation. The same numeric
request ID on two authenticated connections is valid and must not collide.

### Task execution

Operations run on the process-owned Tokio runtime using its tracked task
facility. They do not create per-request OS threads or runtimes. The supervisor
owns every task handle and observes:

- normal completion,
- returned error,
- panic/join failure,
- explicit cancellation,
- connection disposal,
- process shutdown.

Long operations may await external activity in their own task. They acquire
mutable coordinator/state access only for short commands and release it before
an external await. Their nonexclusive ownership registration remains until the
terminal response so disposal cannot remove state underneath an active task.

### P0.1 checklist and tests

- [x] Add in-flight request count and byte configuration with validation.
- [x] Implement count/byte admission reservations.
- [x] Implement the operation registry and duplicate-ID protection for ordinary
      admitted work. Progress requests use the separate P0.3 registry.
- [x] Implement tracked task completion and terminal-response guard.
- [x] Convert ingress request handling from inline await to register-and-start.
- [x] Preserve request ID and ownership on out-of-order completion.
- [x] Propagate task errors and panics to one typed terminal response.
- [x] Release every admission reservation on all terminal paths.

Tests:

- [x] Rust real-loop test: blocking request 10 starts; independent request 11
      completes before request 10 is released.
- [x] Rust real-loop test: two blocking operations in different sessions both
      reach their start gates.
- [x] Rust real-loop test: request 11 responds before request 10 and both retain
      correct IDs and ownership.
- [x] Rust real-loop test: duplicate in-flight request ID is rejected without
      affecting the original.
- [x] Rust real-loop test: the same numeric request ID on two connections does
      not collide.
- [x] Rust real-loop test: operation panic produces one terminal error and frees
      admission.
- [x] Rust real-loop test: count saturation returns a typed error naming
      `runtime.protocol.maxInFlightRequests`.
- [x] Rust real-loop test: byte saturation returns a typed error naming
      `runtime.protocol.maxInFlightRequestBytes`.

All eight scenarios above run through the production-used
`run_protocol_engine` harness in `stdio/request_concurrency_tests.rs`. The
harness supplies bounded ingress/control channels, the real operation and
output brokers, and deterministic gates; it does not call `dispatch_wire`
directly as its concurrency proof.

## P0.2 — Ownership partitioning and ordering

### Why ownership changed

The old `dispatch(&mut self)` and borrowed extension host allowed an async
future to retain exclusive access to all sidecar state. Spawning that future
did not create concurrency; wrapping the sidecar in one async mutex would have
recreated the same serialization. Production `ExtensionContext` now owns an
`Arc<dyn ExtensionServices>` and has no lifetime-bound mutable host borrow.

The target separates cloneable service handles from entity-owned mutable state.

### Process and connection state

A small connection/session coordinator owns:

- connection authentication and version state,
- session membership,
- VM membership indexes,
- request-operation ownership indexes,
- disposal state.

Its commands must not perform guest execution, filesystem/network I/O, adapter
I/O, output writes, or unbounded waits. It may mutate indexes and return
cloneable entity handles.

Connection/session disposal changes the entity state to `Closing` before
signalling owned operations. New requests for a closing entity are rejected.

### VM state

Each live VM has two complementary objects. Cloneable, thread-affine
`VmHandle(Rc<RefCell<VmState>>)` values provide short `try_read` and
`try_command` state sections. A `VmCoordinator` owns bounded operation and
lifecycle admission. Different handles and coordinators make progress
independently, and no state borrow crosses an external await.

P0 ordering classes:

```rust
enum RequestOrderingKey {
    Connection(String),
    Session { connection_id: String, session_id: String },
    VmLifecycle { connection_id: String, session_id: String, vm_id: String },
    VmOperation { connection_id: String, session_id: String, vm_id: String },
    Extension {
        namespace: String,
        connection_id: String,
        key: Vec<u8>,
        policy: ExtensionOrderingPolicy,
    },
    Unordered,
}
```

Ordering semantics:

- Authentication and connection/session membership changes use the relevant
  connection/session coordinator.
- VM create is ordered with session disposal and VM membership insertion.
- Configure, dispose, layer topology changes, root snapshot/import/export, and
  package linking use `VmLifecycle`.
- Filesystem, kernel, execution, stdin, process inspection/control, and VM fetch
  use `VmOperation`.
- A VM lifecycle-exclusive command excludes VM operations for that VM.
- Ordinary VM operations are admitted independently but enter the VM
  coordinator only for their state critical sections.
- Extension operations use an extension-provided opaque ordering key. Core does
  not decode the extension payload. Core-exclusive keys reject an overlapping
  operation with a typed bounded conflict; extensions may explicitly retain
  conflict enforcement when their protocol requires a richer typed response.
- ACP uses its durable session ID as an extension-managed ordering key. Its
  bounded route guard returns the existing typed `session_busy` policy for a
  second active prompt in the same ACP session; prompts in different ACP
  sessions run concurrently.

A request-level operation may issue multiple VM commands over its lifetime. It
must not retain a mutable VM-state borrow or exclusive command guard between
commands. It does retain its nonexclusive ownership registration until terminal
completion. This lets teardown find the operation without preventing
`readFile` from completing while an ACP prompt waits on adapter output in the
same VM.

### Event broker

The production event topology has two parts. The central protocol pump is the
sole consumer of runtime producer wakes and claims root, attached-child, and
detached-child internal work. `ProcessEventBroker` owns bounded durable public
event state and demultiplexes it by connection/session/VM/process ownership.

An extension or request registers an ownership-scoped waiter, then awaits that
waiter without retaining the process registry, VM coordinator, or extension
resource coordinator. Event handling that mutates a VM is submitted as a short
VM command; the waiter itself never makes a VM actor sleep until its timeout.

The broker preserves the one-consumer rule where a protocol requires it. For
ACP, at most one adapter JSON-RPC response loop consumes stdout for one adapter
process, while different routes have independent consumers. Targeted public
waiters use a separate post-pump notification, so they cannot consume the
pump's producer wake. After each bounded turn, durable-source probes re-arm the
pump when one coalesced wake represented multiple executor events.

### Cloneable extension services

Production extensions use a transport-agnostic cloneable trait:

```rust
pub trait ExtensionServices: Send + Sync {
    fn guest_filesystem_call(...) -> ExtensionFuture<'static, ...>;
    fn poll_process_event(...) -> ExtensionFuture<'static, ...>;
    fn invoke_callback_async(...) -> ExtensionFuture<'static, ...>;
    // Other VM/process/resource operations use the same owned form.
}
```

`RoutedExtensionServices` implements this trait through a bounded command
channel and the process-event broker. Service calls return owned `'static`
futures or immediate results, communicate with coordinators, and never expose
internal maps or stdio/fd details.

The generic `Extension` contract remains namespace-based and opaque. Its
implemented hooks are `request_class() -> ExtensionRequestClass::{Ordinary,
Progress}`, `request_ordering_key()`, and `request_ordering_policy()`.
Core supplies reserved progress admission; ACP alone decodes its opaque payload
and signals its keyed route state.

Native-sidecar must not import or decode `agentos-protocol`.

`impl ExtensionHost for NativeSidecar` remains only for the direct in-process
compatibility API and is unreachable from the production protocol engine. An
owned production extension request cannot regain whole-sidecar access through
a trait object. Retiring the compatibility API is P1 cleanup, not a P0 protocol
progress blocker.

### P0.2 checklist and tests

- [x] Introduce cloneable connection/session and VM service handles.
- [x] Ensure different VM coordinators can execute independently.
- [x] Move long waits outside coordinator critical sections.
- [x] Replace `ExtensionContext`'s mutable host borrow with cloneable
      transport-agnostic services.
- [x] Split long event waits into the ownership-aware event broker.
- [x] Claim root, attached-child, and detached-child internal JavaScript/Python
      runtime events exactly once and service them as bounded owned VM work.
- [x] Retain claimed internal event work durably when service admission is full;
      do not drop, duplicate, or hot-requeue it.
- [x] Make extension request futures independently tracked and spawnable.
- [x] Add extension-owned opaque ordering/progress classification.
- [x] Preserve all ownership validation at the coordinator boundary.
- [x] Preserve explicit same-session ACP `session_busy` behavior.
- [x] Prevent lifecycle operations from racing conflicting same-VM operations.

Tests:

- [x] Rust test: a gated VM-A operation does not delay a VM-B operation.
- [x] Rust test: prompt wait releases VM coordination so same-VM filesystem
      access completes.
- [x] Rust test: a long event waiter does not retain VM coordination and wakes
      only for its matching ownership/process event.
- [x] Rust test: root Python VFS work and attached/detached child RPC/output/exit
      work progress while an independent ordinary request is gated.
- [x] Rust test: repeated process-event notifications and service-capacity
      saturation preserve exact-one-consumer delivery without loss or spin.
- [x] Rust test: internal event-service failure is observable, releases
      admission, and VM disposal cancels/drains blocked VM-bound service work.
- [x] Rust test: configure/dispose are ordered against same-VM operations.
- [x] Rust test: session disposal prevents new owned operations and cancels
      existing ones.
- [x] ACP test: same-session second prompt receives typed `session_busy`.
- [x] ACP test: different-session prompts both start and complete independently.
- [x] Ownership test: a connection cannot use another connection's operation or
      extension-session key.

Production stdio uses cloneable owned services and independently supervises
generic and extension futures. VM state is partitioned per handle, ordering is
enforced by `OwnershipCoordinator`, and claimed internal work uses independent
active/deferred bounds while remaining cancellation- and disposal-tracked.

## P0.3 — Direct progress routing

### Progress-critical classes

The following traffic must have reserved admission independent of ordinary
request saturation:

- shutdown control,
- transport termination/error,
- registered `SidecarResponseFrame`,
- cancellation of an active operation,
- ACP permission response for an active prompt,
- terminal response/rejection emission.

A progress message is still bounded. It uses an existing reserved control
capacity or a new explicitly configured reserved capacity; it is never admitted
to an unbounded collection.

### Generic extension progress routing

ACP cancellation and permission responses remain encoded as opaque extension
requests. The extension classifies these frames through a generic hook. The
router uses the returned namespace plus opaque target key to find or signal the
active extension operation.

The implemented generic hooks are:

```rust
fn request_class(...) -> ExtensionRequestClass; // Ordinary | Progress
fn request_ordering_key(...) -> Option<Vec<u8>>;
fn request_ordering_policy(...) -> ExtensionOrderingPolicy;
```

Core gives `Progress` independent reserved admission and invokes the opaque
extension request. Only the ACP extension decodes the payload, locates its
route key, and interprets cancel or permission semantics.

### ACP route state machine

ACP keys live routes by the existing full-identity durable route key. Each
route uses short atomic state transitions:

```text
Idle
  -> StartingOrRestoring
  -> Idle
  -> PromptRunning { prompt_id, cancellation, completion }
  -> Stopping
  -> removed
```

Rules:

- Open/restore is single-flight per route; concurrent callers never launch two
  adapter processes.
- Prompt installs its cancellation/completion state before durable acceptance
  or adapter write, closing the cancel-before-registration race.
- A second prompt on the same route receives typed `session_busy`; prompts on
  different routes run concurrently.
- Read/list/history operations may run during a prompt.
- Configuration operations that would start a competing adapter response loop
  receive typed busy while a prompt runs.
- Cancel signals the installed token directly and also cancels a permission
  waiter. It never starts a competing adapter response loop.
- Permission response validates the option before consuming the pending waiter;
  an invalid option leaves the waiter live for a later valid response.
- Unload/delete enters `Stopping`, signals cancellation, awaits prompt durable
  terminal commit outside the route lock, and then tears down the adapter.
- KillProcess executes independently. The prompt observes ProcessExited and
  commits its real terminal result; no router may drop the prompt future and
  substitute a synthetic terminal response.
- Cancel, permission, process exit, disconnect, and shutdown races commit
  exactly one durable prompt outcome and empty every live waiter/route entry.

A progress request receives its own exactly-once acknowledgement. Signalling
the target does not consume the target's terminal response reservation.

### Direct registered sidecar responses

A matching `SidecarResponseFrame` routes directly from the control reader to
its registered waiter. It does not enter ordinary ingress, acquire ordinary
operation admission, or scan unrelated events. Production ACP uses
`invoke_callback_async`; its matching response settles the registered waiter
directly. The synchronous direct in-process compatibility callback API remains
P1 cleanup.

### Shutdown

Shutdown behavior:

1. atomically enter `Draining`,
2. reject new ordinary requests,
3. continue accepting progress messages and registered responses,
4. signal cancellation to every active operation,
5. wait up to `runtime.protocol.shutdownGraceMs` for tracked operations,
6. synthesize terminal shutdown responses for unfinished operations when the
   transport remains writable,
7. close output only after terminal/control drainage,
8. report every forced cancellation or failed terminal delivery.

No active task, waiter, admission reservation, or response reservation may
remain after shutdown completion.

### P0.3 checklist and tests

- [x] Implement reserved direct routing for shutdown and transport failure.
- [x] Preserve direct registered sidecar-response delivery.
- [x] Implement generic extension-owned progress classification.
- [x] Route ACP cancel directly to the active prompt token.
- [x] Route ACP permission response directly to its active waiter.
- [x] Implement the keyed ACP route state machine and single-consumer adapter
      response-loop guard.
- [x] Make open/restore single-flight and unload/delete cancellation-aware.
- [x] Remove synthetic KillProcess interruption in favor of cooperative
      ProcessExited observation.
- [x] Give every progress request an exactly-once acknowledgement.
- [x] Implement supervisor drain/cancel shutdown sequencing.
- [x] Add and validate `runtime.protocol.shutdownGraceMs` and include it in the
      runtime limits inventory.
- [x] Release all operation and waiter state on connection loss.

Tests:

- [x] Rust real-loop test: prompt A, unrelated B, cancel A; B completes and A
      cancels without either frame trapping the other.
- [x] Rust real-loop test: cancel works when ordinary operation admission is
      saturated.
- [x] ACP test: permission response resumes its target prompt while unrelated
      requests are active.
- [x] Callback test: matching sidecar response reaches its waiter under ordinary
      saturation.
- [x] Progress test: a duplicate live `(connection_id, request_id)` is rejected
      without consuming the original request's exactly-once acknowledgement.
- [x] Shutdown test: active gated operations are cancelled/drained and no task
      or reservation leaks.
- [x] Disconnect test: all connection-owned operations and waiters terminate.
- [x] Race test: cancel versus natural completion yields exactly one target
      terminal response and one cancel acknowledgement.
- [x] ACP race tests: cancel before reservation, before durable acceptance,
      before adapter write, during output wait, during permission wait, and
      after terminal commit.
- [x] ACP test: invalid permission option leaves the waiter live; a subsequent
      valid option succeeds.
- [x] ACP test: unload/delete during a prompt reaches durable terminal state
      before teardown and rejects new prompt/config work while stopping.
- [x] ACP test: adapter kill progresses independently and the prompt observes
      ProcessExited without dropping its future.
- [x] ACP test: concurrent open/restore launches exactly one adapter.
- [x] ACP test: one adapter has at most one response-loop consumer while
      different routes can have concurrent consumers.

Progress work uses its own reserved-lane-bounded, connection-scoped registry.
Its handle is retained until broker publication, claims exactly one
acknowledgement, and rejects a duplicate live request ID without affecting the
original. Shutdown closes ordinary admission, signals tracked work, continues
progress/control routing through the grace period, force-terminalizes any
unfinished operation exactly once, aborts only after takeover, drains control
output, and reports failed delivery.

## P0.4 — Nonblocking output broker

### Required lanes and logical classes

Maintain the physical lane contract:

- fd 0: host `RequestFrame` ingress,
- stdout: non-heartbeat ordinary `EventFrame` egress,
- fd 3: responses, sidecar requests, heartbeats, registered callback traffic,
  and typed shutdown/control.

Combined stdio compatibility may multiplex physical writes, but logical
ordinary and control admission remain independent.

Within fd 3/control output, use independent logical queues and budgets:

1. **Progress** — shutdown, cancel/permission acknowledgements, sidecar callback
   requests, terminal transport errors, and other frames needed to unblock
   active work.
2. **Rejection** — typed responses for requests rejected before ordinary
   operation admission.
3. **Terminal** — exactly one response for each admitted request.
4. **Observability** — heartbeat and limit-warning delivery; best-effort or
   coalesced and never permitted to consume required progress capacity.

Ordinary non-heartbeat events keep their independent stdout queue/budget. A
shared physical fd does not imply a shared admission budget. The sum of the
configured logical control capacities must be validated against any physical
control bound retained by the implementation.

The control writer drains progress before rejection, rejection before terminal,
and terminal before best-effort observability, preserving FIFO within each
class. Combined stdio drains all logical control classes before ordinary
events.

### Producer API

`ProtocolFrameWriter` exposes the implemented broker operations:

```rust
fn try_reserve_terminal(...);
fn try_reserve_progress(...);
fn publish_reserved_terminal_for_operation(...);
fn publish_reserved_progress_for_request(...);
async fn publish(...); // ordinary producers
fn try_publish_rejection(...);
fn try_publish_observability(...);
```

Rules:

- The ingress router only calls nonblocking methods backed by already-reserved
  capacity.
- A request terminal response consumes the reservation acquired at request
  admission.
- A progress rejection/acknowledgement consumes reserved control/rejection
  capacity.
- An ordinary producer task may asynchronously await ordinary capacity.
- No producer calls `Condvar::wait`, blocking channel `send`, or blocking
  stdout/fd writes.
- The encoded frame owns its count/byte reservation until the physical writer
  has completed or failed the write.
- Writer failure closes the broker and wakes every waiter with a terminal error.
- Closing the broker atomically records the terminal error, drains and drops
  queued encoded frames so their reservations release, and wakes all async
  budget waiters. Closure is idempotent.
- `SidecarRequestFrame` uses timed progress publication; its registered waiter
  is cancelled if publication fails or its deadline expires.
- Limit warnings remain in their existing bounded warning source and are
  retried/coalesced or explicitly logged to stderr when observability admission
  is unavailable. They never await from ingress.
- Heartbeats are coalesced best-effort observability and cannot consume
  terminal/progress/rejection reservations.

### Ordinary event backpressure

P0 requires ingress independence, bounded memory, and no silent event loss.
When ordinary event capacity is unavailable:

- event state remains in its bounded durable producer queue,
- at most one coalesced output-ready wake remains queued/in flight,
- the event pump stops draining that producer,
- the output broker wakes the producer after capacity is released.

A request task returning a finite event batch may await broker capacity in that
request task. It may not hold a coordinator while waiting.

### Response reservation and overload

Admission reserves one terminal frame and
`runtime.protocol.terminalFallbackBytes`, not the maximum wire-frame size. The
configuration must satisfy:

```text
maxTerminalFrames >= maxInFlightRequests
maxTerminalBytes >= maxInFlightRequests * terminalFallbackBytes
```

This guarantees a small typed terminal fallback for every admitted operation
without reducing default concurrency to `maxControlBytes / maxFrameBytes`.

When the real terminal response exceeds its fallback reservation, the
completion task asynchronously acquires the additional bytes from the terminal
budget. It holds no coordinator while waiting. If the response exceeds the
wire-frame maximum, or cannot be retained before bounded shutdown, it uses the
already-reserved fallback to emit a typed frame/egress-limit terminal response.

Terminal capacity is independent of progress capacity. Admitting many prompts
therefore cannot consume the `SidecarRequestFrame` capacity those prompts need
for host callbacks.

Keep a fixed rejection capacity so failure to admit an ordinary request can
still return a typed overload response. Ordinary events and admitted terminal
responses cannot consume this reserve.

If neither ordinary operation admission nor rejection reservation is
available, the router must stop dequeuing ordinary ingress while continuing
progress/control handling. If the reader itself reaches bounded ingress and no
rejection can be retained, transition to a typed terminal transport failure and
close rather than silently dropping a request and pretending it was answered.

### P0.4 checklist and tests

- [x] Implement logical ordinary/control output broker lanes.
- [x] Split control output into terminal, progress, rejection, and
      observability classes with validated configuration math.
- [x] Replace synchronous producer-side condition-variable waits.
- [x] Reserve terminal response capacity during request admission.
- [x] Add reserved rejection/progress capacity.
- [x] Route response, event, sidecar request, warning, cancel acknowledgement,
      and terminal error emission through the broker.
- [x] Make broker close drain queued frames, release reservations, and wake all
      publishers with the same terminal error.
- [x] Define and implement rejection-reserve exhaustion as pause-or-typed-close,
      never silent loss.
- [x] Ensure live extension events use async/nonblocking broker semantics.
- [x] Stop and re-arm ordinary event drainage on output backpressure.
- [x] Wake all producers and waiters when a writer fails.
- [x] Keep actual physical writes off the ingress router.

Tests:

- [x] Output test: saturating ordinary events does not prevent a response.
- [x] Output test: saturating ordinary events does not prevent cancel/shutdown.
- [x] Output test: a deliberately non-reading host does not park ingress.
- [x] Output test: reservation remains charged through physical write
      completion.
- [x] Output test: writer failure releases reservations and fails waiters.
- [x] Output test: closing a full broker wakes a publisher already waiting for
      budget and returns all usage to zero.
- [x] Output test: admitted request always emits one terminal response even when
      ordinary output is full.
- [x] Event test: backpressured durable events resume without loss or
      duplication.
- [x] Combined-stdio test: logical control priority survives physical
      multiplexing.
- [x] Output classification test covers response/error, progress ack, sidecar
      request, live/batch event, warning, and heartbeat classes.
- [x] Output test: terminal reservations for all admitted requests do not
      consume callback/progress capacity.
- [x] Overload test: exhausted rejection capacity follows the specified
      pause-or-typed-close policy and never silently drops a response.

The aggregate suite covers real cancel/shutdown routing while ordinary output
is full, an admitted request's terminal response under the same saturation,
exact-256 executor-source continuation after a coalesced wake, separate public
and pump wake ownership, and per-child relay gating that prevents exit from
overtaking retained stdout. Durable-event stop/re-arm has no loss, duplication,
or empty hot-spin.

## P0.5 — ACP workaround removal

After P0.1–P0.4 are active:

- remove `pending_frame` from the protocol loop,
- remove `dispatch_with_prompt_interrupt`,
- remove `BlockingExtensionRequest`,
- remove the old blocking-request single-frame interruption plumbing when no
  longer used,
- remove tests that validate the workaround's mechanics,
- retain ACP cancellation/permission behavior through P0.3 generic extension
  progress routing.

No production native-sidecar code may recognize ACP methods or payload types.

### P0.5 checklist and tests

- [x] Delete the single `pending_frame` slot.
- [x] Delete `dispatch_with_prompt_interrupt`.
- [x] Delete obsolete blocking extension interruption types/hooks.
- [x] Delete or rewrite workaround-specific unit tests.
- [x] Confirm native-sidecar remains independent of `agentos-protocol`.
- [x] Confirm all retained cancellation tests use generic direct routing.

Tests:

- [x] Architecture guard: no `pending_frame` or
      `dispatch_with_prompt_interrupt` remains.
- [x] Architecture guard: native-sidecar does not import ACP protocol types.
- [x] Full real-loop interleaving suite passes without prompt-specific stdio
      routing.

## Real protocol-loop test harness

The production `run_async` wiring delegates to `run_protocol_engine`.
`stdio/request_concurrency_tests.rs` drives that exact engine with:

- bounded ordinary ingress,
- bounded progress/control ingress,
- a deterministic output broker/sink,
- a configured native sidecar plus fake extensions,
- shutdown and writer-failure signals.

The harness does not call `dispatch_wire` as its concurrency proof.

Its fake extension uses oneshot/notify gates:

- start gate confirms an operation is actually running,
- release gate controls completion,
- cancellation token confirms direct cancellation,
- no sleep determines ordering,
- short timeouts are failure bounds only,
- every failing test releases or aborts all gates before cleanup.

The extracted harness lives outside the inline production test module; focused
private state-machine and source-wake tests remain inline where they need
private access.

## Required integration coverage

Rust is authoritative. The non-nightly TypeScript public-client coverage runs
against a freshly built shared sidecar and proves both of these flows:

1. A delayed host-tool response crosses exactly 256 ACP updates, the terminal
   response arrives, every update remains ordered/exactly once, and the same
   adapter session is reused.
2. Two prompts remain simultaneously in flight while public file write/read
   calls complete; both prompts then cancel and unload cleanly.

The migration-parity suite additionally covers filesystem/process/snapshot,
registered bindings, host-loopback fetch, and ACP lifecycle behavior.

## Implementation order

- [x] 1. Land the extracted real-loop harness and initial red regression.
- [x] 2. Add runtime request-admission configuration and reservations.
- [x] 3. Add request supervisor, tracked terminal guard, and shutdown registry.
- [x] 4. Introduce cloneable state/VM/extension service handles.
- [x] 5. Move request dispatch to tracked independently executing operations.
- [x] 6. Add extension-owned ordering and progress classification.
- [x] 7. Route cancel, permission, callback response, shutdown, and errors
      directly.
- [x] 8. Add output broker with terminal/control reservations.
- [x] 9. Move every output producer to the broker.
- [x] 10. Remove ACP stdio interruption and `pending_frame`.
- [x] 11. Add and pass public-client integration smoke coverage.
- [x] 12. Run final validation and complete an architecture review against every
      invariant and checkbox.

## Validation commands

Required focused commands:

```bash
cargo test -p agentos-native-sidecar --lib request_concurrency -- --test-threads=1
cargo test -p agentos-native-sidecar --lib protocol_output -- --test-threads=1
cargo test -p agentos-native-sidecar --lib deferred_tcp_connect -- --test-threads=1
cargo test -p agentos-native-sidecar --test architecture_guards -- --test-threads=1
cargo test -p agentos-sidecar --lib -- --test-threads=1
```

Required crate/workspace gates:

```bash
cargo test -p agentos-native-sidecar --lib
cargo test -p agentos-native-sidecar --test service --no-run
cargo check --workspace
```

The explicit `--lib` filters are intentional: an unqualified package filter
also compiles every integration target, which duplicates the expensive native
link and can exhaust the workspace disk before the focused unit tests start.
The separate `service --no-run` command keeps the public integration harness
compile-checked without weakening that coverage.

Do not run an unfiltered `cargo test -p agentos-sidecar`; its integration
binaries can spawn real processes and hang. The `--lib` command above runs the
bounded ACP and durable-session unit suite without executing those integration
binaries.

If TypeScript public-client coverage changes:

```bash
pnpm --dir packages/core check-types
pnpm --dir packages/core build
pnpm --dir packages/core exec vitest run tests/migration-parity.test.ts \
  tests/acp-reactor-regression.test.ts --fileParallelism=false --reporter=verbose
```

## Final acceptance checklist

- [x] All P0.1 checkboxes and tests are complete.
- [x] All P0.2 checkboxes and tests are complete.
- [x] All P0.3 checkboxes and tests are complete.
- [x] All P0.4 checkboxes and tests are complete.
- [x] All P0.5 checkboxes and tests are complete.
- [x] No global ordinary request FIFO or global sidecar mutex exists.
- [x] No ingress path awaits business execution or output capacity.
- [x] Slow prompts, slow output, cancellation, and shutdown interleavings pass.
- [x] Different VMs and extension sessions demonstrate concurrent progress.
- [x] Same-entity ordering and ownership isolation are explicit and tested.
- [x] Admission, output, cancellation, panic, disconnect, and shutdown release
      all tracked reservations.
- [x] The friction log contains every deferred non-P0 audit item.
- [x] The final report lists any unchecked item as a blocker; P0 must not be
      reported complete while an item remains unchecked.

## Validation record — 2026-08-05

Passed in this working copy:

- `cargo fmt --all -- --check`
- `cargo check --workspace`
- `cargo build -p agentos-sidecar --bin agentos-sidecar`
- `cargo test -p agentos-runtime --lib`: 62 passed, 1 intentionally ignored
- `cargo test -p agentos-native-sidecar-core --lib`: 84 passed
- `cargo test -p agentos-native-sidecar --lib -- --test-threads=1`: 324
  passed, 1 intentionally ignored
- focused native-sidecar suites: request concurrency 20/20, protocol output
  5/5, deferred TCP connect 3/3, plus the deferred-event ownership,
  source-rearm, exact-256-update, child-relay ordering, Python service, and
  binding-rollback regressions
- `cargo test -p agentos-native-sidecar --test architecture_guards
  -- --test-threads=1`: 40/40
- `cargo test -p agentos-native-sidecar --test service --no-run`
- `cargo test -p agentos-sidecar --lib`: 74/74
- `pnpm --dir packages/core check-types`
- `pnpm --dir packages/core build`
- public TypeScript integration coverage: ACP reactor 2/2 and migration parity
  4/4, including the real host-loopback fetch and ACP lifecycle flow

Additional non-P0 workspace baseline:

- `pnpm --dir packages/core test:pr` reaches 98/99 unit tests before an
  unchanged public-export test expects `AgentOs.prototype.pread`, which is
  absent from both this revision and its parent. The two required integration
  files were therefore run directly and pass 6/6.
- Repository-wide `pnpm check-types` stops at
  `examples/js-filesystem`: its package-local `node_modules` and local
  `@rivet-dev/agentos` link are absent. The changed public package's scoped
  typecheck and build pass. Both workspace baselines are recorded in the
  agentOS friction log.
