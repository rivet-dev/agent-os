# Native-sidecar ownership and VM lifecycle gate simplification

Status: implemented and validated; unrelated repository-baseline gate failures
are recorded below

Owner: agentOS native sidecar

Baseline: the request-concurrency stack through
`refactor(native-sidecar): separate ownership from conflict policy`

This document is the implementation contract and completion checklist. A box
may be checked only after the implementation exists and its corresponding test
passes. The implementing agent must record the commands and results in the
final validation section before declaring the work complete.

## Objective

Keep the concurrency behavior fixed by `request-concurrency-fix-prompt.md`, but
replace the overlapping request, ownership, conflict, and VM lifecycle
coordination machinery with the smallest explicit model that provides the same
safety.

The completed design has:

1. one bounded operation table for every inbound request on a connection,
   including ordinary and progress requests;
2. one short-held ownership/membership state lock, never held across `.await`;
3. one small lifecycle gate per VM;
4. ACP-owned per-route exclusion, without generic core extension-ordering
   machinery; and
5. no duplicate per-connection, per-session, and per-VM operation maps.

The implementation must be a simplification. Production code in the ownership,
request-admission, and lifecycle subsystem should be materially smaller after
the change. Adding behavioral tests is expected and does not count against that
goal.

## Why this change is needed

The current implementation correctly prevents a long ACP prompt from holding
the protocol ingress loop, but represents one request in several overlapping
places:

- `RequestOperationRegistry` or `ProgressRequestRegistry`;
- connection ownership operations;
- session ownership operations;
- VM ownership operations;
- extension conflict registrations;
- task/completion supervision; and
- terminal/progress publication guards.

The ownership coordinator also combines four different concerns:

- entity membership;
- disposal state;
- operation tracking;
- VM lifecycle exclusion.

That makes cancellation and disposal hard to audit and caused ordinary and
progress request IDs to be checked in separate registries. For example, an
ordinary request `42` and progress request `42` can currently be admitted on
the same connection even though request IDs are connection-scoped.

The lifecycle exclusion itself is necessary. The hierarchy around it is not.

## Non-negotiable invariants

- [x] Production ingress only decodes, validates, reserves, registers, and
      starts work. It never awaits business execution, VM activity, output
      capacity, adapter I/O, or event drainage.
- [x] Ordinary work is concurrent by default.
- [x] Different VMs never share a lifecycle gate.
- [x] A lifecycle mutation runs only after conflicting work already admitted
      for that VM has drained.
- [x] Once lifecycle admission becomes pending, new ordinary work for that VM
      is rejected with a typed, retryable conflict; it is not placed in a
      hidden waiter queue.
- [x] Progress requests, registered sidecar responses, cancellation, permission
      responses, shutdown, and transport failure do not require ordinary
      request admission.
- [x] Progress-critical internal events needed to settle already-admitted work
      can continue during lifecycle drain, remain bounded, and are included in
      the drain count.
- [x] No internal event mutates a VM while its lifecycle gate is active.
- [x] A long ACP prompt does not hold the VM lifecycle gate. Only its short VM
      service operations enter the gate.
- [x] Each `(connection_id, request_id)` identifies at most one live inbound
      request, regardless of whether it is ordinary or progress-classified.
- [x] Every admitted request retains exactly one response-publication right and
      releases all count/byte/gate accounting on every terminal path.
- [x] Entity disposal closes admission before signalling work, waits without
      holding a state lock, and cannot remove state underneath an admitted
      operation.
- [x] Every queue, table, retained byte collection, and waiter set remains
      bounded with a typed error naming the limit and configuration path.
- [x] No mutex or `RefCell` borrow crosses `.await`.
- [x] No global `Arc<Mutex<NativeSidecar>>` or equivalent is introduced.
- [x] Native-sidecar remains opaque to ACP payloads.

## Target architecture

```text
fd 0 / fd 3 readers
        |
        v
+------------------------+
| Protocol router        |
| - classify             |
| - reserve output       |
| - admit operation      |
| - start task           |
+-----------+------------+
            |
            v
+------------------------+
| Operation table        |  one short metadata mutex
| - global request IDs   |
| - ordinary/progress    |
| - count/byte budgets   |
| - ownership + phase    |
| - cancellation         |
| - drain notification   |
+-----------+------------+
            |
            | VM-scoped work only
            v
+------------------------+
| VmLifecycleGate        |  one independent gate per VM
| - ordinary permits     |
| - internal permits     |
| - lifecycle permit     |
+------------------------+
```

ACP route state remains inside the ACP extension. Output reservations and the
physical output broker remain separate from the operation table.

## 1. One operation table

Replace the separate ordinary and progress registries with one operation table
keyed by:

```rust
struct OperationKey {
    connection_id: String,
    request_id: RequestId,
}
```

The table may retain separate configured budgets for ordinary and progress
classes, but duplicate-ID detection is global across both classes.

The conceptual record is:

```rust
struct OperationRecord {
    generation: u64,
    class: OperationClass, // Ordinary | Progress
    ownership: OwnershipScope,
    operation: String,
    request_bytes: usize,
    cancellation: OperationCancellation,
    publication: ResponsePublicationGuard,
}
```

Requirements:

- [x] Replace `RequestOperationRegistry` and `ProgressRequestRegistry` with one
      authoritative map, or make one a thin class-specific view over a single
      authoritative map.
- [x] Ordinary and progress budgets remain independently bounded.
- [x] Duplicate detection happens before either class reserves or starts work.
- [x] The same numeric request ID remains valid on different connections.
- [x] Replace `TerminalResponseGuard` and `ProgressAcknowledgementGuard` with
      one response-publication primitive parameterized only by response class.
- [x] Keep exactly-once takeover for bounded shutdown and transport failure.
- [x] Remove operation lifecycle states that are used only to mirror task
      execution. Retain only state that affects admission, cancellation,
      publication correctness, or externally useful diagnostics.
- [x] Scope cancellation and drain scan the single bounded table. Do not add
      secondary per-entity operation maps merely to avoid scanning at most the
      configured in-flight operation bound.
- [x] Ordinary shutdown may close ordinary admission while progress admission
      remains open for the bounded drain.
- [x] Table closure and entity closure use generations so an old disposal
      cannot cancel a later entity reusing the same textual ID.

## 2. Ownership and membership state

Use one short-held metadata lock for connection, session, and VM membership.
This lock is allowed because it protects only bounded in-memory transitions and
is always released before external work or waiting.

The simplest recommended representation places entity phases and the bounded
operation map under the same metadata mutex. If the implementation keeps
membership and operation storage behind separate mutexes, it must document one
lock order, make admission/disposal one linearizable transaction through a
generation-bearing lease, and add a deterministic test for the race. Merely
checking `Open`, releasing the membership lock, and registering an operation
later is not safe.

The ownership state must not contain a second copy of every active operation.
The operation table is authoritative for active work and cancellation.

Requirements:

- [x] Entity records have an explicit generation and `Open | Closing` phase.
- [x] Request admission validates the complete connection/session/VM ownership
      path against one coherent snapshot.
- [x] Entity creation and disposal are linearized against admission.
- [x] Disposal marks the entity `Closing` before signalling matching operation
      records.
- [x] Disposal waits on operation-table and lifecycle-gate drain notification
      without holding the membership lock.
- [x] Entity state is removed only after matching operations and gate permits
      have drained.
- [x] A stale permit or completion from an earlier entity generation cannot
      mutate a newly created entity with the same textual ID.
- [x] Delete `ConnectionOperationRegistration`,
      `SessionOperationRegistration`, `VmOperationRegistration`, and their
      independent operation-ID counters.
- [x] Delete nested lock sequences used solely to register the same operation
      at connection, session, and VM scopes.
- [x] Do not reuse `runtime.protocol.maxInFlightRequests` as a session
      membership limit. Reuse an existing semantically correct bound, remove a
      redundant retained collection, or add a dedicated documented limit.

## 3. Per-VM lifecycle gate

Each live VM owns one independent `VmLifecycleGate`. The gate contains only the
state required to exclude lifecycle mutations from conflicting VM work.

Conceptual API:

```rust
impl VmLifecycleGate {
    fn try_enter_ordinary(
        &self,
        cancellation: OperationCancellation,
    ) -> Result<VmOrdinaryPermit, VmGateError>;

    fn try_enter_internal(
        &self,
        cancellation: OperationCancellation,
    ) -> Result<InternalAdmission, VmGateError>;

    async fn begin_lifecycle(
        &self,
        cancellation: OperationCancellation,
    ) -> Result<VmLifecyclePermit, VmGateError>;
}
```

Conceptual state:

```rust
struct VmGateState {
    phase: VmGatePhase,
    closing: bool,
    ordinary_active: usize,
    internal_active: usize,
    next_generation: u64,
}

enum VmGatePhase {
    Idle,
    Pending { generation: u64 },
    Active { generation: u64 },
}
```

Exact state semantics:

```text
Idle
  ordinary -> increment ordinary_active
  internal -> increment internal_active
  lifecycle -> Pending and close new ordinary admission

Pending
  new ordinary -> typed retryable conflict
  bounded internal settlement work -> admitted or durably deferred
  ordinary_active == 0 && internal_active == 0 -> Active

Active
  ordinary -> typed retryable conflict
  internal -> remains in its durable source; never mutates the VM
  matching lifecycle permit drops -> Idle

closing == true (orthogonal to Idle/Pending/Active)
  all new gate admission -> typed shutdown/disposal error
  existing permits drain and notify disposal
  cancellation/permit drop must never reopen admission
```

Requirements:

- [x] `Idle -> Pending` is the linearization point that closes ordinary VM
      admission.
- [x] Only one lifecycle request may be pending or active. A second receives a
      typed conflict rather than waiting.
- [x] The lifecycle waiter registers notification before checking drain state,
      preventing lost wakeups.
- [x] Lifecycle cancellation while pending restores `Idle` only when the
      cancellation belongs to the current gate generation and the gate is not
      closing. A closing gate remains unavailable.
- [x] Dropping an active lifecycle permit restores `Idle` exactly once when the
      gate is open. It never clears the orthogonal closing state.
- [x] Dropping an ordinary or internal permit decrements exactly one counter
      and wakes a pending lifecycle waiter when the gate may advance.
- [x] Counter underflow, stale generation, and poisoned-state recovery are hard
      logged invariant failures.
- [x] Ordinary and internal admission remains bounded independently.
- [x] Internal work admitted during `Pending` is limited to the existing
      settlement/event path. Public requests cannot label themselves internal.
- [x] Durable internal events are not removed from their source unless the
      gate or a tracked deferred permit owns them.
- [x] A pending lifecycle operation cannot spin while internal capacity is
      unavailable.
- [x] A lifecycle gate for VM A never reads, writes, or notifies VM B's gate.

### Why this is not an `RwLock`

The production doc comment on `VmLifecycleGate` must explain all of the
following:

- a standard-library lock cannot cross `.await` without blocking a runtime
  worker;
- ordinary admission must reject after lifecycle becomes pending rather than
  join an implicit waiter queue;
- progress-critical internal settlement work must remain admissible during
  `Pending` and must be counted in the lifecycle drain;
- disposal needs explicit cancellation, generations, and active counts; and
- the mutex inside the gate protects only non-suspending state transitions and
  is released before waiting.

The comment must also state that a short global metadata mutex is acceptable:
the forbidden design is a lock held across request execution, not a lock around
bounded map/counter transitions.

## 4. Request classification

Replace generalized conflict policy with one narrow VM concurrency class:

```rust
enum VmConcurrencyClass {
    None,
    Ordinary,
    ExclusiveLifecycle,
}
```

The names may differ, but the model must have no extension-specific conflict
variant.

Classification requirements:

- [x] Connection- and session-owned operations use `None`.
- [x] Extension envelope requests use `None`; ACP enforces its own route-level
      response-loop exclusion.
- [x] Short VM service calls made by extensions use `Ordinary`.
- [x] Internal VM settlement/event work uses the dedicated internal gate API,
      not `Ordinary` and not a public request class.
- [x] Ordinary core VM work uses `Ordinary`.
- [x] The following remain `ExclusiveLifecycle` unless an operation owner
      provides a narrower, tested safety argument:
      - dispose VM;
      - bootstrap root filesystem;
      - configure VM;
      - create or seal layer;
      - import or export snapshot;
      - create overlay;
      - snapshot root filesystem;
      - link package.
- [x] Classification is implemented in one auditable location or on the
      request type itself. Do not introduce a pairwise conflict matrix.
- [x] A newly added lifecycle request must require an explicit classification
      in a compile-time exhaustive match or architecture test.

## 5. Delete dead extension ordering machinery

ACP is the only production extension currently returning an ordering key, and
it returns `ExtensionManaged`; the core coordinator therefore performs no
exclusion for that key. Retain ACP's route state and remove the unused generic
layer around it.

- [x] Delete `ExtensionOrderingPolicy`.
- [x] Delete `Extension::request_ordering_key`.
- [x] Delete `Extension::request_ordering_policy`.
- [x] Delete `ConflictPolicy::Extension` or its replacement equivalent.
- [x] Delete connection-level `extension_conflicts` state and registration
      guards.
- [x] Delete tests and architecture guards that require the removed hooks.
- [x] Preserve `Extension::request_class` so an opaque extension can identify
      progress requests without native-sidecar decoding its payload.
- [x] Preserve ACP's per-route state machine and typed `session_busy` response.
- [x] Preserve concurrent prompts on different ACP routes.

The following source audit must return no production matches:

```bash
rg -n \
  'ExtensionOrderingPolicy|request_ordering_key|request_ordering_policy|ConflictPolicy::Extension|extension_conflicts' \
  crates/native-sidecar/src crates/agentos-sidecar/src
```

## 6. Progress must remain end-to-end independent

This refactor must not simplify away the reserved progress path. It must also
verify that progress is not reserved only at ingress/output while silently
joining an ordinary internal service queue.

- [x] ACP cancel and permission response bypass ordinary request count/byte
      admission and the VM ordinary gate.
- [x] Registered `SidecarResponseFrame` routing remains direct to its waiter.
- [x] Shutdown and terminal transport failure remain directly routable.
- [x] If ACP cancellation must issue `WriteStdin`, the service scheduler has
      bounded progress-reserved admission or another direct bounded path.
- [x] Saturating ordinary extension-service work cannot prevent an admitted
      cancellation from reaching the adapter and receiving its acknowledgement.
- [x] Progress saturation returns a typed limit error without consuming the
      target operation's terminal response right.
- [x] Continuous progress traffic cannot starve already-retained terminal
      responses. Use bounded fair scheduling rather than unlimited strict
      priority if the existing output test exposes starvation.

## 7. Required tests

### Pure gate/state tests

- [x] Multiple ordinary permits coexist.
- [x] Lifecycle becomes pending and waits for all earlier ordinary permits.
- [x] New ordinary admission is rejected while lifecycle is pending.
- [x] Lifecycle becomes active only after ordinary and internal counts reach
      zero.
- [x] A second lifecycle request is rejected in both pending and active phases.
- [x] Cancelling a pending lifecycle operation reopens ordinary admission.
- [x] Dropping the active lifecycle permit reopens ordinary admission.
- [x] A stale lifecycle permit cannot reopen or mutate a later generation.
- [x] Internal settlement work can run during pending and is included in drain.
- [x] Internal work is not admitted while lifecycle is active.
- [x] Gate closure cancels/rejects admission and drains all permits.
- [x] Closing during pending and closing during active lifecycle work cannot
      reopen admission when the lifecycle future or permit later drops.
- [x] Near-limit warnings and typed count-limit errors name the configuration
      path.

### Operation-table tests

- [x] Ordinary request `42` followed by progress request `42` on the same
      connection rejects the second request.
- [x] Progress request `42` followed by ordinary request `42` rejects the
      second request.
- [x] Request `42` on two different connections remains valid.
- [x] Ordinary and progress class budgets are independent despite sharing the
      ID table.
- [x] Cancellation versus natural completion publishes exactly one response.
- [x] Forced shutdown takeover publishes at most one response and releases all
      accounting.
- [x] Closing a connection rejects later admission and cancels only matching
      operations.
- [x] Scope drain reaches zero count and zero retained request bytes.

### Real protocol-loop tests

- [x] Same-VM read and write requests execute concurrently.
- [x] VM-A lifecycle drain does not delay VM-B ordinary work.
- [x] Configure/dispose waits for already-admitted same-VM work and rejects new
      same-VM work while pending.
- [x] A sleeping ACP prompt does not retain the VM gate; same-VM file read/write
      completes during the prompt.
- [x] Two ACP prompts on different routes run concurrently.
- [x] A second prompt on the same route receives ACP's typed `session_busy`.
- [x] ACP cancellation progresses while ordinary request admission is full.
- [x] ACP cancellation reaches adapter stdin while ordinary extension-service
      capacity is saturated.
- [x] Disposal during an active prompt cancels, drains, and removes all route,
      operation, gate, permission, and event-waiter state.
- [x] Repeated lifecycle/cancel/process-exit races produce no duplicate terminal
      response, leaked permit, lost event, or hot spin.

### Model/fuzz/load coverage

- [x] Add a deterministic model test that compares randomized gate operations
      against a small reference state machine. Cover admit/drop/cancel/close
      and stale-generation sequences.
- [x] Extend protocol frame fuzzing with mixed ordinary/progress duplicate IDs,
      lifecycle transitions, cancellation, and shutdown.
- [x] Add a bounded load test with multiple VMs, concurrent ordinary work,
      periodic lifecycle requests, ACP progress, and deliberately delayed
      completions.
- [x] The load test asserts a finite completion deadline, per-VM independence,
      exactly-once responses, and zero final accounting rather than relying on
      sleeps or log inspection.
- [x] Safeguard-firing saturation tests remain cheap and active in PR CI; tests
      attempting to prove absence of a resource bound remain explicitly
      ignored.

## 8. Cleanup requirements

- [x] Remove obsolete types, aliases, compatibility wrappers, error variants,
      metrics, comments, and tests instead of leaving deprecated paths.
- [x] Do not retain both the old coordinator and new gate behind a feature flag.
- [x] Do not introduce a second request scheduler, runtime, or worker pool.
- [x] Do not add a general ordinary request FIFO.
- [x] Remove duplicated admission preflight where one atomic admission method
      can reserve all required state safely.
- [x] Keep output reservations outside the VM gate; no gate permit may be held
      while waiting for output capacity.
- [x] Keep public and Rust client behavior identical if any wire/config surface
      changes.
- [x] Update `request-concurrency-fix-prompt.md` to describe the final simplified
      ownership/gate model and remove the obsolete ordering-key description.
- [x] Update native-sidecar architecture comments and guards to enforce behavior
      and forbidden dependencies, not incidental private symbol names.
- [x] `cargo fmt` and Clippy complete without new allows for dead code, complex
      types, or too many arguments in the new subsystem.
- [x] The production ownership/request/gate implementation is net smaller than
      the baseline, or the final validation record explains every net-new
      production abstraction.

## 9. Suggested implementation sequence

1. [x] Add failing tests for cross-class duplicate IDs and the standalone VM
       gate state machine.
2. [x] Introduce the single operation table and shared publication guard.
3. [x] Introduce `VmLifecycleGate` with ordinary, internal, and lifecycle RAII
       permits.
4. [x] Rewire core VM requests and extension service commands to the gate.
5. [x] Move disposal cancellation/drain to the authoritative operation table.
6. [x] Remove per-entity operation maps and registration guards.
7. [x] Remove generic extension ordering hooks and conflict state.
8. [x] Run the real-loop ACP/filesystem/lifecycle regressions.
9. [x] Saturate the extension service path and close any progress-reservation
       gap exposed by the test.
10. [x] Run model/fuzz/load coverage and the final source/architecture audits.
11. [x] Update the original concurrency design document and record final
       validation below.

The implementing agent may reorder mechanical steps, but must not temporarily
weaken the production invariants on the final revision.

## 10. Validation commands

Run focused checks first:

```bash
cargo fmt --check
cargo test -p agentos-native-sidecar request_operations
cargo test -p agentos-native-sidecar ownership_coordinator
cargo test -p agentos-native-sidecar request_concurrency
cargo test -p agentos-native-sidecar --test architecture_guards
cargo test -p agentos-sidecar acp
pnpm --dir packages/core test:pr
```

Then run repository gates proportional to the final diff:

```bash
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
pnpm check-types
pnpm build
```

If exact test filters change as obsolete modules are removed, replace them with
the new target names and record the actual commands below. Do not silently skip
a behavioral category because its old filter no longer matches tests.

Required source audits:

```bash
rg -n \
  'ExtensionOrderingPolicy|request_ordering_key|request_ordering_policy|ConflictPolicy::Extension|extension_conflicts|ProgressRequestRegistry' \
  crates/native-sidecar/src crates/agentos-sidecar/src

rg -n \
  'ConnectionOperationRegistration|SessionOperationRegistration|VmOperationRegistration' \
  crates/native-sidecar/src
```

Both commands must return no production matches. Test-only reference-model
names should also be renamed so the audit remains unambiguous.

## 11. Completion definition

This work is complete only when:

- [x] Every checkbox in sections 1 through 8 is complete.
- [x] The original prompt/filesystem/cancel reproduction passes.
- [x] All required focused validation passes; repository-wide gates either pass
      or have a reproduced, unrelated baseline failure recorded below.
- [x] Source audits show no obsolete ownership or ordering implementation.
- [x] A reviewer can identify one authoritative operation table and one
      lifecycle gate per VM without tracing duplicate shadow state.
- [x] The final diff contains no unrelated refactor or generated artifacts.
- [x] The working copy contains no accidental empty jj revision in the stack.
- [x] The implementation revision has a plain conventional-commit description
      with no coding-agent attribution.

## Final validation record

Implementing agent fills this section in before handoff.

- Implementation revision: `xulmxrrl` —
  `refactor(native-sidecar): simplify VM lifecycle coordination`
- Final reviewer revision: `xulmxrrl`
- Focused Rust tests: `cargo fmt --check`; operation-table tests 20/20;
  lifecycle-coordinator tests 11/11; request-concurrency tests 25/25;
  architecture guards 41/41; ACP tests 52/52.
- ACP/public-client regression: the delayed response after 256 updates and the
  two-prompts-plus-filesystem regression pass; all four native-sidecar migration
  parity scenarios pass (6/6 tests across the two files).
- Model/fuzz/load tests: the deterministic randomized gate model, mixed
  ordinary/progress protocol framing, duplicate-ID cases, lifecycle/cancel
  races, bounded progress saturation, and multi-VM load deadline all pass in
  the focused Rust suites.
- Workspace checks: `cargo check --workspace`, package-scoped Core and
  runtime-core builds/typechecks, fixed-version verification, and publish
  helper typechecks/tests pass. The repository-wide Clippy gate still reaches
  unchanged `large_enum_variant` and disabled browser-target failures; root
  `pnpm check-types` still reaches an unmaterialized example dependency; root
  `pnpm build` still requires the generated Codex WASI release artifact; and
  `packages/core test:pr` still has the unchanged top-level `pread` export
  assertion (98/99 unit tests). These baseline failures are recorded in the
  agentOS friction log; the focused changed-package and real-loop gates pass.
- Source audits: both required `rg` commands return no production matches.
  `OperationTable` is the sole inbound request table and each `VmRecord` owns
  one independent `VmLifecycleGate`.
- Production lines removed/added in the simplified subsystem: before test
  modules, ownership coordination changed from 1,843 to 1,686 lines (-157) and
  request operations from 1,484 to 1,524 (+40), for a net reduction of 117
  production lines. The request-operation increase is the shared progress
  projection and cross-class identity/publication coverage replacing the
  deleted second registry.
- Remaining known risks: no known change-specific correctness gap. The
  unrelated repository-baseline gates above remain cleanup debt; the release
  workflow remains the authoritative generated-artifact build.
