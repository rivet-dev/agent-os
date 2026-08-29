# DESIGN-43s — request concurrency for the stdio protocol loop

Scope: `crates/native-sidecar/src/stdio.rs`, beads `odw-43s`.

## What shipped

The `biased` `tokio::select!` in `run_async` (`stdio.rs`, the `'protocol` loop)
polls arms in declaration order and services the first ready one. With
`stdin_rx` **above** both event-pump arms (`event_ready_rx`,
`process_event_notify`), a host that pipelines requests keeps `stdin_rx`
permanently ready and no VM's queued output is ever flushed — one tenant's
request stream starves every other tenant's events indefinitely.

Reordering the arms does not fix that; it inverts which lane starves, and the
inverted form is the worse of the two. Neither event arm is self-limiting under
load:

- `ActiveProcess::queue_pending_execution_event` calls
  `process_event_notify.notify_one()` for *every* queued execution event
  (`execution/process.rs:243`, `:374`), and `pump_process_events` re-arms the
  notify itself whenever a VM burns its `runtime.fairness.vm_quantum_operations`
  quantum (`execution/process_events.rs:531`). A guest emitting sustained stdout
  keeps a `Notify` permit stored at essentially all times.
- The drain is not a pure drain either: `poll_event` with a zero timeout still
  pulls `process_event_receiver` into `pending_process_events`
  (`service.rs:1694-1717`), so the guest refills it as fast as the loop empties
  it.

And `stdin_rx` is the cancel lane, not merely the new-work lane:
`route_decoded_combined_frame` (`stdio.rs`) sends every
`ProtocolFrame::RequestFrame` — `cancel_execution` included
(`language_execution.rs:1637`) — to `stdin_tx`, while
`route_decoded_control_frame` admits only `SidecarResponseFrame` and shutdown
`ControlFrame`s to `stdin_control_tx`. Put above stdin, the event arms let one
untrusted guest stall the whole multi-tenant process's request lane *and* make
its own flood uncancellable.

The fix is therefore a round-robin, not a priority swap:

- `service_process_events` (`stdio.rs`) performs one **bounded** round — pump
  every active session, then emit at most
  `runtime.fairness.vm_quantum_operations` event frames. Work left over by the
  cap re-arms `event_ready_tx` so the next turn resumes it.
- The `'protocol` loop calls it once per turn, at the top, before taking on more
  work. Every path that handles a frame (the `pending_frame` park, the stdin
  arm, the control lane) comes back through there, so a pipelining host can no
  longer keep queued output unflushed.
- Arm order is unchanged from before this work — both event-pump arms stay below
  `stdin_rx` — and their bodies now only consume the edge that woke the loop.

Ordering is: shutdown → control lane (sidecar responses, permission replies) →
stdin requests (cancels included) → event-pump wakes → limit warnings → write
errors, with one bounded event flush per turn regardless of which arm fired.

## What did NOT ship, and why

A long dispatch is still awaited **inline** with `&mut sidecar` held:

```
'protocol loop → handle_protocol_frame(&mut sidecar, …).await
                 → dispatch_with_prompt_interrupt(&mut sidecar, …)
                   → sidecar.dispatch_wire(request).await
```

`NativeSidecar::dispatch_wire`, `NativeSidecar::poll_event_wire`
(`service.rs:1601`, `service.rs:1610`) and
`NativeSidecar::pump_process_events` (`execution/process_events.rs:476`) are all
`&mut self`. While the dispatch future is alive it holds the only mutable
borrow, so nothing else in the loop — no pump, no unrelated VM's request — can
touch the sidecar. `dispatch_with_prompt_interrupt` (`stdio.rs:1286`) already
shows the ceiling of what is reachable without restructuring: it can select on
`stdin_rx` beside the pinned dispatch, but every frame it reads is either a
same-request ACP interrupt or gets parked in the single-slot `pending_frame` and
waits for the dispatch to resolve.

So an ACP prompt (minutes long) still blocks every other VM's requests *and*
their process-event pumping. Removing that needs the restructure below; it is
not a select-ordering change.

## Design for full request concurrency

### 1. Split the sidecar into shared state + per-VM ownership

`NativeSidecar` (`crates/native-sidecar/src/state.rs`, `service.rs`) is one
struct holding `vms: BTreeMap<String, VmState>` plus process-global pieces
(config, metrics, extension registry, `process_event_receiver`,
`SharedSidecarRequestClient`, `SharedEventSink`). Today every method takes
`&mut self`, so VM-local work and process-global work are indistinguishable to
the borrow checker.

Change:

- Move `VmState` behind `Arc<Mutex<VmState>>` (tokio `Mutex`; these paths await)
  and keep `vms: BTreeMap<String, Arc<Mutex<VmState>>>` behind an `RwLock` so
  creating/removing a VM does not block dispatches on other VMs.
- Take `&self` on `NativeSidecar` for anything that resolves a VM and then
  operates under that VM's lock. `dispatch`, `dispatch_wire`,
  `pump_process_events`, `poll_event`, `poll_event_wire` become `&self`.
- Keep genuinely process-global mutable pieces (`process_event_receiver`, the
  disposed-session signal) in their own small mutexes rather than the outer one,
  so the pump does not serialize behind a dispatch.

Files: `state.rs` (the `VmState`/`NativeSidecar` field layout and every
`self.vms.get_mut(..)` caller), `service.rs`, `vm.rs`,
`execution/process_events.rs`, `language_execution.rs`, `filesystem.rs`,
`extension.rs` (`ExtensionHost::poll_event` and friends take `&mut self` today
and would follow).

### 2. Actor per VM in the transport

With `&self` dispatch, `stdio.rs` stops awaiting inline:

- `run_async` holds `sidecar: Arc<NativeSidecar<LocalBridge>>`.
- Route each inbound `RequestFrame` by `request.ownership` to a per-VM actor:
  `BTreeMap<VmKey, mpsc::Sender<AccountedProtocolFrame>>`, one bounded channel
  each (backpressure per VM, not per process). Connection- and session-scoped
  frames keep a single shared lane, since they mutate the connection/session
  registries.
- Each actor is a `tokio::spawn`ed task owning that VM's serial ordering: recv a
  frame, `sidecar.dispatch_wire(frame).await`, write the response through the
  existing `ProtocolFrameWriter` (already `Clone` + `Send`, already the
  synchronization point for egress ordering).
- The `'protocol` loop keeps only: shutdown, control lane, the two event-pump
  arms, routing, limit warnings, write errors. No arm awaits a dispatch, so no
  arm can starve another.

`pending_frame` disappears: the single-slot park exists only because one loop
owns both the dispatch and the stdin reader. `dispatch_with_prompt_interrupt`
shrinks to "the VM actor also selects on its own interrupt channel"; the control
lane feeds interrupts into the owning VM's actor instead of into a shared slot.

### 3. Ordering and shutdown invariants to preserve

- Per-VM request ordering must stay FIFO — one actor task per VM, never a task
  per request.
- Events for a VM must not overtake the response to the request that produced
  them; both go through `ProtocolFrameWriter`, so keep the existing
  ordinary/control lane split and emit the response before returning from the
  actor turn.
- `cleanup_connections` / `untrack_disposed_sessions` must join or cancel a VM's
  actor before `remove_connection`, otherwise a dispatch races VM teardown.
- `active_sessions` is read by the event pump and written by
  `track_session_state`; it moves into the shared lane or behind its own lock.

### 4. Test plan for the restructure

`crates/native-sidecar/tests/stdio_binary.rs` already spawns the real sidecar
binary over stdio and creates VMs; it is the right harness. The regression test
is: create VM A and VM B, start a blocking extension request (an ACP prompt, or
the existing `TEST_EXTENSION_NAMESPACE` blocking request) on A, then assert B's
`execute` gets an `execution_accepted` response and B's `execution_output` event
frames arrive while A's request is still outstanding. That test cannot be
written today — it deadlocks on the inline `.await` — which is precisely the
gap this design closes.

The shipped fairness round is covered indirectly. Reproducing either starvation
deterministically needs a real guest flooding stdout racing a pipelining host —
`stdio_binary.rs` can host that test, but only once the restructure above makes
the outcome deterministic rather than timing-dependent. What *is* pinned today
is the fact the arm order rests on:
`cancel_execution_is_admitted_on_the_stdin_lane_not_the_control_lane`
(`stdio.rs` unit tests) fails if cancels ever move off `stdin_tx`, which is the
only condition under which demoting stdin below the event arms would become
defensible.
