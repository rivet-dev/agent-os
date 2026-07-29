// Session management: create/destroy sessions with V8 isolates on dedicated threads

#[cfg(not(test))]
use std::collections::BTreeMap;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(not(test))]
use agentos_bridge::queue_tracker::warn_limit_exhausted;
use agentos_bridge::queue_tracker::{register_queue, QueueGauge, TrackedLimit};
use agentos_bridge::{bridge_contract, BridgeCallConvention};
use agentos_runtime::accounting::{Reservation, ResourceClass, ResourceLedger};
use agentos_runtime::metrics::{ExecutorMetricClass, RuntimeMetrics};
use agentos_runtime::readiness::{
    ReadyAcknowledgement, ReadyBatch as RuntimeReadyBatch, ReadyFlags, ReadyObservation, ReadyWake,
    SessionReadyBroker as RuntimeSessionReadyBroker,
};
use agentos_runtime::RuntimeContext;
use crossbeam_channel::{Receiver, Select, Sender};

use crate::execution;
#[cfg(test)]
use crate::host_call::BridgeCallRegistry;
#[cfg(not(test))]
use crate::host_call::{BridgeCallContext, ChannelRuntimeEventSender};
use crate::host_call::{CallIdRouter, SharedCallIdCounter};
use crate::ipc::ExecutionError;
#[cfg(not(test))]
use crate::ipc_binary::ExecutionErrorBin;
use crate::runtime_protocol::{
    BridgeResponse, RuntimeEvent, SessionMessage, StreamEvent, WarmSessionHint,
};
use crate::snapshot::{snapshot_cache_key, SnapshotCache, SnapshotCacheKey};
#[cfg(not(test))]
use crate::{bridge, isolate, snapshot};

/// Commands sent to a session thread
pub enum SessionCommand {
    /// Shut down the session and destroy the isolate
    Shutdown,
    /// Forward a typed session message to the session thread for processing
    Message(SessionMessage),
    /// Install a direct module-source reader on the session thread. Carried as a
    /// live object over the in-process command channel (NOT a serialized frame),
    /// so subsequent module loads on this thread read source directly instead of
    /// round-tripping the bridge. Sent just before an Execute message.
    SetModuleReader(Box<dyn crate::execution::GuestModuleReader>),
    /// A bounded capability-identity batch drained from the session's
    /// dedicated readiness lane. Durable data remains in the owning subsystem.
    ReadyBatch(RuntimeReadyBatch),
}

#[derive(Debug)]
struct SessionReadyWakeState {
    runtime_wake_rx: tokio::sync::mpsc::Receiver<ReadyWake>,
}

/// VM-scoped adapter from the Tokio broker's capacity-one wake lane to the
/// thread-affine V8 executor's capacity-one crossbeam lane.
#[derive(Debug)]
struct SessionReadiness {
    generation: u64,
    max_batch_handles: usize,
    broker: RuntimeSessionReadyBroker,
    wakes: Mutex<SessionReadyWakeState>,
    executor_wake_tx: Sender<ReadyWake>,
}

impl SessionReadiness {
    fn new(
        generation: u64,
        runtime: &RuntimeContext,
        max_batch_handles: usize,
    ) -> Result<(Arc<Self>, Receiver<ReadyWake>), String> {
        if max_batch_handles == 0 {
            return Err(String::from(
                "ERR_AGENTOS_READY_BATCH_LIMIT: limits.reactor.workQuantum must be greater than zero",
            ));
        }
        let (broker, runtime_wake_rx) = RuntimeSessionReadyBroker::new_with_resources(
            generation,
            Arc::clone(runtime.resources()),
            runtime.metrics().clone(),
        )
        .map_err(|error| error.to_string())?;
        let (executor_wake_tx, executor_wake_rx) = crossbeam_channel::bounded(1);
        Ok((
            Arc::new(Self {
                generation,
                max_batch_handles,
                broker,
                wakes: Mutex::new(SessionReadyWakeState { runtime_wake_rx }),
                executor_wake_tx,
            }),
            executor_wake_rx,
        ))
    }

    /// Readiness-disabled adapter for the standalone event-loop test seam. It
    /// has no publication handle; production sessions always use `new` with
    /// their VM-scoped runtime and configured capability bound.
    fn disabled(generation: u64) -> Result<(Arc<Self>, Receiver<ReadyWake>), String> {
        let (broker, runtime_wake_rx) =
            RuntimeSessionReadyBroker::new(generation, 1).map_err(|error| error.to_string())?;
        let (executor_wake_tx, executor_wake_rx) = crossbeam_channel::bounded(1);
        Ok((
            Arc::new(Self {
                generation,
                max_batch_handles: 1,
                broker,
                wakes: Mutex::new(SessionReadyWakeState { runtime_wake_rx }),
                executor_wake_tx,
            }),
            executor_wake_rx,
        ))
    }

    fn publish(
        &self,
        capability_id: u64,
        capability_generation: u64,
        flags: ReadyFlags,
    ) -> Result<(), String> {
        self.broker
            .mark_ready(self.generation, capability_id, capability_generation, flags)
            .map_err(|error| error.to_string())?;
        let mut state = self.wakes.lock().map_err(|_| {
            String::from("ERR_AGENTOS_READY_STATE_POISONED: session readiness lock poisoned")
        })?;
        self.forward_runtime_wake_locked(&mut state)
    }

    fn publish_signal(&self, signal: i32) -> Result<(), String> {
        self.broker
            .mark_signal_ready(self.generation, signal)
            .map_err(|error| error.to_string())?;
        let mut state = self.wakes.lock().map_err(|_| {
            String::from("ERR_AGENTOS_READY_STATE_POISONED: session readiness lock poisoned")
        })?;
        self.forward_runtime_wake_locked(&mut state)
    }

    fn remove(&self, capability_id: u64, capability_generation: u64) -> Result<(), String> {
        self.broker
            .remove_capability(self.generation, capability_id, capability_generation)
            .map_err(|error| error.to_string())
    }

    fn set_application_read_interest(
        &self,
        capability_id: u64,
        capability_generation: u64,
        enabled: bool,
    ) -> Result<(), String> {
        self.broker
            .set_application_read_interest(
                self.generation,
                capability_id,
                capability_generation,
                enabled,
            )
            .map_err(|error| error.to_string())
    }

    fn publish_timer(&self, timer_id: u64) -> Result<(), String> {
        self.broker
            .mark_timer_ready(self.generation, timer_id)
            .map_err(|error| error.to_string())?;
        let mut state = self.wakes.lock().map_err(|_| {
            String::from("ERR_AGENTOS_READY_STATE_POISONED: session readiness lock poisoned")
        })?;
        self.forward_runtime_wake_locked(&mut state)
    }

    fn forward_runtime_wake_locked(&self, state: &mut SessionReadyWakeState) -> Result<(), String> {
        let wake = match state.runtime_wake_rx.try_recv() {
            Ok(wake) => wake,
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => return Ok(()),
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                return Err(String::from(
                    "ERR_AGENTOS_READY_WAKE_DISCONNECTED: shared readiness wake source disconnected",
                ));
            }
        };
        match self.executor_wake_tx.try_send(wake) {
            Ok(()) => Ok(()),
            Err(crossbeam_channel::TrySendError::Full(_)) => Err(String::from(
                "ERR_AGENTOS_READY_WAKE_INVARIANT: executor readiness lane was full for a shared wake",
            )),
            Err(crossbeam_channel::TrySendError::Disconnected(_)) => Err(String::from(
                "ERR_AGENTOS_READY_WAKE_DISCONNECTED: executor readiness consumer disconnected",
            )),
        }
    }

    fn take_batch(&self, wake: ReadyWake) -> Result<RuntimeReadyBatch, String> {
        self.broker
            .ready_batch(wake.generation, wake.epoch, self.max_batch_handles)
            .map_err(|error| error.to_string())
    }

    fn drain_signals(&self, batch: &RuntimeReadyBatch) -> Result<Vec<i32>, String> {
        self.broker
            .drain_signals(batch.generation, batch.epoch, self.max_batch_handles)
            .map_err(|error| error.to_string())
    }

    fn drain_timers(&self, batch: &RuntimeReadyBatch) -> Result<Vec<u64>, String> {
        self.broker
            .drain_timers(batch.generation, batch.epoch, self.max_batch_handles)
            .map_err(|error| error.to_string())
    }

    fn complete_batch(
        &self,
        batch: &RuntimeReadyBatch,
        delivered: &[ReadyObservation],
    ) -> Result<(), String> {
        let acknowledgements = delivered
            .iter()
            .map(|entry| ReadyAcknowledgement {
                capability_id: entry.capability_id,
                capability_generation: entry.capability_generation,
                observed_revision: entry.revision,
                clear: entry.flags,
            })
            .collect::<Vec<_>>();
        self.broker
            .complete_wake(batch.generation, batch.epoch, &acknowledgements)
            .map_err(|error| error.to_string())?;
        let mut state = self.wakes.lock().map_err(|_| {
            String::from("ERR_AGENTOS_READY_STATE_POISONED: session readiness lock poisoned")
        })?;
        self.forward_runtime_wake_locked(&mut state)
    }
}

#[derive(Default)]
struct SessionPauseState {
    paused: bool,
    shutdown: bool,
}

#[derive(Default)]
#[doc(hidden)]
pub struct SessionPauseControl {
    state: Mutex<SessionPauseState>,
    resumed: Condvar,
}

impl SessionPauseControl {
    pub(crate) fn pause(&self) {
        self.state
            .lock()
            .expect("session pause lock poisoned")
            .paused = true;
    }

    pub(crate) fn resume(&self) {
        let mut state = self.state.lock().expect("session pause lock poisoned");
        state.paused = false;
        self.resumed.notify_all();
    }

    fn shutdown(&self) {
        let mut state = self.state.lock().expect("session pause lock poisoned");
        state.shutdown = true;
        state.paused = false;
        self.resumed.notify_all();
    }

    pub(crate) fn wait_while_paused(&self) {
        let mut state = self.state.lock().expect("session pause lock poisoned");
        while state.paused && !state.shutdown {
            state = self
                .resumed
                .wait(state)
                .expect("session pause lock poisoned while waiting");
        }
    }
}

#[cfg(not(test))]
extern "C" fn pause_isolate_interrupt(_isolate: &mut v8::Isolate, data: *mut std::ffi::c_void) {
    // SAFETY: pause_session passes one Arc strong reference with into_raw for
    // this callback. V8 invokes each accepted interrupt exactly once.
    let control = unsafe { Arc::from_raw(data.cast::<SessionPauseControl>()) };
    control.wait_while_paused();
}

#[cfg(not(test))]
type SharedIsolateHandle = Arc<Mutex<Option<v8::IsolateHandle>>>;
#[cfg(test)]
type SharedIsolateHandle = Arc<Mutex<Option<()>>>;

/// Sender for typed runtime events produced by session threads. IPC sessions
/// share a bounded connection writer lane; in-process sessions write directly
/// to their own bounded output lane so one backpressured VM cannot stall or
/// destroy unrelated VMs in a global dispatch thread.
#[derive(Clone)]
pub enum RuntimeEventSender {
    Channel(crossbeam_channel::Sender<RuntimeEventEnvelope>),
    Closed,
    Direct {
        generation: u64,
        sender: RuntimeEventOutputSender,
    },
}

impl RuntimeEventSender {
    pub fn closed() -> Self {
        Self::Closed
    }

    pub fn direct(generation: u64, sender: RuntimeEventOutputSender) -> Self {
        Self::Direct { generation, sender }
    }

    pub fn send(&self, envelope: RuntimeEventEnvelope) -> Result<(), String> {
        match self {
            Self::Channel(sender) => sender.try_send(envelope).map_err(|error| match error {
                crossbeam_channel::TrySendError::Full(_) => String::from(
                    "ERR_AGENTOS_V8_OUTPUT_LIMIT: runtime output lane is full; raise runtime.resources.maxAsyncCompletions",
                ),
                crossbeam_channel::TrySendError::Disconnected(_) => String::from(
                    "ERR_AGENTOS_V8_OUTPUT_DISCONNECTED: runtime output consumer disconnected",
                ),
            }),
            Self::Closed => Err(String::from(
                "ERR_AGENTOS_V8_OUTPUT_UNREGISTERED: session has no registered output lane",
            )),
            Self::Direct { generation, sender } => {
                if envelope.output_generation != Some(*generation) {
                    return Err(format!(
                        "ERR_AGENTOS_STALE_V8_OUTPUT: event generation {:?} does not match registered generation {generation}",
                        envelope.output_generation
                    ));
                }
                sender.try_send(envelope.event)
            }
        }
    }
}

impl From<crossbeam_channel::Sender<RuntimeEventEnvelope>> for RuntimeEventSender {
    fn from(sender: crossbeam_channel::Sender<RuntimeEventEnvelope>) -> Self {
        Self::Channel(sender)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeEventEnvelope {
    pub output_generation: Option<u64>,
    pub event: RuntimeEvent,
}

#[derive(Clone)]
pub struct RuntimeEventOutputSender {
    inner: flume::Sender<QueuedRuntimeEvent>,
    resources: Arc<ResourceLedger>,
    gauge: Arc<QueueGauge>,
}

pub struct RuntimeEventOutputReceiver {
    inner: flume::Receiver<QueuedRuntimeEvent>,
    gauge: Arc<QueueGauge>,
}

struct QueuedRuntimeEvent {
    event: RuntimeEvent,
    _reservation: Reservation,
}

pub fn runtime_event_output_channel(
    capacity: usize,
    resources: Arc<ResourceLedger>,
) -> (RuntimeEventOutputSender, RuntimeEventOutputReceiver) {
    let (sender, receiver) = flume::bounded(capacity);
    let gauge = register_queue(TrackedLimit::V8SessionFrames, capacity);
    (
        RuntimeEventOutputSender {
            inner: sender,
            resources,
            gauge: Arc::clone(&gauge),
        },
        RuntimeEventOutputReceiver {
            inner: receiver,
            gauge,
        },
    )
}

impl RuntimeEventOutputSender {
    #[cfg(test)]
    pub(crate) fn capacity(&self) -> Option<usize> {
        self.inner.capacity()
    }

    pub fn send(&self, event: RuntimeEvent) -> Result<(), String> {
        let reservation = self
            .resources
            .reserve(ResourceClass::AsyncCompletions, 1)
            .map_err(|error| error.to_string())?;
        let result = self
            .inner
            .send(QueuedRuntimeEvent {
                event,
                _reservation: reservation,
            })
            .map_err(|_| {
                String::from(
                    "ERR_AGENTOS_V8_OUTPUT_DISCONNECTED: session output consumer disconnected",
                )
            });
        self.gauge.observe_depth(self.inner.len());
        result
    }

    pub fn try_send(&self, event: RuntimeEvent) -> Result<(), String> {
        let reservation = self
            .resources
            .reserve(ResourceClass::AsyncCompletions, 1)
            .map_err(|error| error.to_string())?;
        let result = self
            .inner
            .try_send(QueuedRuntimeEvent {
                event,
                _reservation: reservation,
            })
            .map_err(|error| match error {
                flume::TrySendError::Full(_) => String::from(
                    "ERR_AGENTOS_V8_OUTPUT_LIMIT: session output lane is full; raise limits.reactor.maxAsyncCompletions",
                ),
                flume::TrySendError::Disconnected(_) => String::from(
                    "ERR_AGENTOS_V8_OUTPUT_DISCONNECTED: session output consumer disconnected",
                ),
            });
        self.gauge.observe_depth(self.inner.len());
        result
    }
}

impl RuntimeEventOutputReceiver {
    pub fn recv(&self) -> Result<RuntimeEvent, flume::RecvError> {
        let result = self.inner.recv().map(|queued| queued.event);
        self.gauge.observe_depth(self.inner.len());
        result
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<RuntimeEvent, flume::RecvTimeoutError> {
        let result = self.inner.recv_timeout(timeout).map(|queued| queued.event);
        self.gauge.observe_depth(self.inner.len());
        result
    }

    pub fn try_recv(&self) -> Result<RuntimeEvent, flume::TryRecvError> {
        let result = self.inner.try_recv().map(|queued| queued.event);
        self.gauge.observe_depth(self.inner.len());
        result
    }

    pub async fn recv_async(&self) -> Result<RuntimeEvent, flume::RecvError> {
        let result = self.inner.recv_async().await.map(|queued| queued.event);
        self.gauge.observe_depth(self.inner.len());
        result
    }
}

impl Drop for RuntimeEventOutputReceiver {
    fn drop(&mut self) {
        while self.inner.try_recv().is_ok() {}
        self.gauge.observe_depth(self.inner.len());
    }
}

const LATE_TERMINATE_EXECUTION_ERROR_CODE: &str = "ERR_LATE_TERMINATE_EXECUTION";
const LATE_STREAM_EVENT_ERROR_CODE: &str = "ERR_LATE_STREAM_EVENT";
const LATE_BRIDGE_RESPONSE_ERROR_CODE: &str = "ERR_LATE_BRIDGE_RESPONSE";
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct WarmPoolKey {
    snapshot_key_digest: SnapshotCacheKey,
    heap_limit_mb: u32,
}

struct ParkedWorker {
    assignment_tx: Sender<SessionAssignment>,
    join_handle: thread::JoinHandle<()>,
}

#[derive(Default)]
struct WarmWorkerPoolState {
    workers: HashMap<WarmPoolKey, Vec<ParkedWorker>>,
    refilling: HashSet<WarmPoolKey>,
    reserved_workers: usize,
}

#[derive(Default)]
struct WarmWorkerPool {
    state: Mutex<WarmWorkerPoolState>,
}

struct SessionAssignment {
    heap_limit_mb: Option<u32>,
    cpu_time_limit_ms: Option<u32>,
    wall_clock_limit_ms: Option<u32>,
    rx: Receiver<SessionCommand>,
    shutdown_rx: Receiver<()>,
    ready_rx: Receiver<ReadyWake>,
    ready_broker: Arc<SessionReadiness>,
    slot_permit: SessionSlotPermit,
    event_tx: RuntimeEventSender,
    call_id_router: CallIdRouter,
    shared_call_id: SharedCallIdCounter,
    snapshot_cache: Arc<SnapshotCache>,
    isolate_handle: SharedIsolateHandle,
    execution_abort: SharedExecutionAbort,
    execution_active: Arc<AtomicBool>,
    pause_control: Arc<SessionPauseControl>,
    session_id: String,
    output_generation: Option<u64>,
    runtime: RuntimeContext,
    bridge_call_timeout: Duration,
}

#[cfg(not(test))]
struct PrecreatedIsolate {
    // Keep both V8 owners optional so `Drop` can enforce the required order:
    // every Global must be released before its isolate, and isolate destruction
    // must share the process-wide lifecycle lock with isolate creation.
    isolate: Option<v8::OwnedIsolate>,
    context: Option<v8::Global<v8::Context>>,
    bridge_code: String,
    userland_code: String,
}

#[cfg(not(test))]
impl Drop for PrecreatedIsolate {
    fn drop(&mut self) {
        drop(self.context.take());
        isolate::drop_isolate(self.isolate.take());
    }
}

#[cfg(test)]
struct PrecreatedIsolate;

#[cfg(not(test))]
#[derive(Default)]
struct V8SessionPhaseStats {
    calls: u64,
    total_ns: u128,
    max_ns: u128,
}

#[cfg(not(test))]
static V8_SESSION_PHASES: OnceLock<Mutex<BTreeMap<String, V8SessionPhaseStats>>> = OnceLock::new();

#[cfg(not(test))]
fn v8_session_phases_enabled() -> bool {
    std::env::var("AGENTOS_V8_SESSION_PHASES").as_deref() == Ok("1")
}

#[cfg(not(test))]
fn record_v8_session_phase(stage: &str, elapsed: Duration) {
    if !v8_session_phases_enabled() {
        return;
    }
    let phases = V8_SESSION_PHASES.get_or_init(|| Mutex::new(BTreeMap::new()));
    let Ok(mut phases) = phases.lock() else {
        return;
    };
    let stats = phases.entry(stage.to_string()).or_default();
    stats.calls += 1;
    let elapsed_ns = elapsed.as_nanos();
    stats.total_ns += elapsed_ns;
    stats.max_ns = stats.max_ns.max(elapsed_ns);

    let Some(path) = std::env::var_os("AGENTOS_V8_SESSION_PHASES_FILE") else {
        return;
    };
    let mut output = String::new();
    for (stage, stats) in phases.iter() {
        let total_us = stats.total_ns / 1_000;
        let avg_us = if stats.calls == 0 {
            0
        } else {
            total_us / u128::from(stats.calls)
        };
        let max_us = stats.max_ns / 1_000;
        output.push_str(&format!(
            "stage={stage} calls={} total_us={total_us} avg_us={avg_us} max_us={max_us}\n",
            stats.calls
        ));
    }
    let _ = std::fs::write(path, output);
}

#[cfg(not(test))]
fn record_warm_worker_hit() {
    record_v8_session_phase("warm_worker_hit", Duration::ZERO);
}

#[cfg(test)]
fn record_warm_worker_hit() {}

#[cfg(not(test))]
fn record_warm_worker_miss() {
    record_v8_session_phase("warm_worker_miss", Duration::ZERO);
}

#[cfg(test)]
fn record_warm_worker_miss() {}

fn warm_worker_capacity_per_key() -> usize {
    std::env::var("AGENTOS_V8_WARM_ISOLATES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(MAX_PROCESS_WARM_WORKERS)
        .min(MAX_PROCESS_WARM_WORKERS)
}

const MAX_PROCESS_WARM_WORKERS: usize = 4;

fn effective_heap_limit_mb(heap_limit_mb: Option<u32>) -> u32 {
    heap_limit_mb.unwrap_or(crate::isolate::DEFAULT_HEAP_LIMIT_MB)
}

fn warm_pool_key(
    bridge_code: &str,
    userland_code: &str,
    heap_limit_mb: Option<u32>,
) -> WarmPoolKey {
    WarmPoolKey {
        snapshot_key_digest: snapshot_cache_key(
            bridge_code,
            (!userland_code.is_empty()).then_some(userland_code),
        ),
        heap_limit_mb: effective_heap_limit_mb(heap_limit_mb),
    }
}

fn warm_key_prefix(key: &WarmPoolKey) -> String {
    key.snapshot_key_digest[..4]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

impl WarmWorkerPool {
    fn claim(&self, key: &WarmPoolKey) -> Option<ParkedWorker> {
        let mut state = self.state.lock().expect("warm worker pool lock poisoned");
        state.workers.get_mut(key).and_then(Vec::pop)
    }

    fn shutdown_handles(&self) -> Vec<thread::JoinHandle<()>> {
        let mut state = self.state.lock().expect("warm worker pool lock poisoned");
        state.refilling.clear();
        state.reserved_workers = 0;
        state
            .workers
            .drain()
            .flat_map(|(_, workers)| workers)
            .map(|worker| {
                drop(worker.assignment_tx);
                worker.join_handle
            })
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    fn ensure_count(
        self: &Arc<Self>,
        runtime: RuntimeContext,
        snapshot_cache: Arc<SnapshotCache>,
        slot_control: SlotControl,
        bridge_code: String,
        userland_code: String,
        heap_limit_mb: Option<u32>,
        requested_count: usize,
    ) {
        let capacity = warm_worker_capacity_per_key();
        if capacity == 0 || requested_count == 0 {
            return;
        }

        let target_count = requested_count.min(capacity);
        let key = warm_pool_key(&bridge_code, &userland_code, heap_limit_mb);
        {
            let mut state = self.state.lock().expect("warm worker pool lock poisoned");
            let current = state.workers.get(&key).map_or(0, Vec::len);
            if current >= target_count || state.refilling.contains(&key) {
                return;
            }
            state.refilling.insert(key.clone());
        }

        let pool = Arc::clone(self);
        let spawn_key = key.clone();
        let requested_bytes = bridge_code.len().saturating_add(userland_code.len());
        if let Err(error) = runtime.blocking().submit(requested_bytes, move || {
            pool.refill_until(
                snapshot_cache,
                slot_control,
                spawn_key,
                bridge_code,
                userland_code,
                heap_limit_mb,
                target_count,
            );
        }) {
            eprintln!("ERR_AGENTOS_V8_WARM_REFILL: bounded executor rejected refill: {error}");
            self.state
                .lock()
                .expect("warm worker pool lock poisoned")
                .refilling
                .remove(&key);
        }
    }

    // Internal pool-refill plumbing; args mirror the parked-worker construction.
    #[allow(clippy::too_many_arguments)]
    fn refill_until(
        &self,
        snapshot_cache: Arc<SnapshotCache>,
        slot_control: SlotControl,
        key: WarmPoolKey,
        bridge_code: String,
        userland_code: String,
        heap_limit_mb: Option<u32>,
        target_count: usize,
    ) {
        loop {
            let capacity = warm_worker_capacity_per_key();
            if capacity == 0 {
                break;
            }
            let desired = target_count.min(capacity);
            let refill_slot = {
                let mut state = self.state.lock().expect("warm worker pool lock poisoned");
                let current = state.workers.get(&key).map_or(0, Vec::len);
                let total = state.workers.values().map(Vec::len).sum::<usize>();
                if current >= desired {
                    break;
                }
                if total.saturating_add(state.reserved_workers) < MAX_PROCESS_WARM_WORKERS {
                    state.reserved_workers += 1;
                    Some(None)
                } else {
                    let evict_key = state
                        .workers
                        .iter()
                        .find(|(candidate, workers)| *candidate != &key && !workers.is_empty())
                        .map(|(candidate, _)| candidate.clone());
                    evict_key.and_then(|evict_key| {
                        let workers = state
                            .workers
                            .get_mut(&evict_key)
                            .expect("selected warm worker key exists");
                        let worker = workers.pop();
                        if workers.is_empty() {
                            state.workers.remove(&evict_key);
                        }
                        worker.map(|worker| Some((evict_key, worker)))
                    })
                }
            };
            let Some(evicted) = refill_slot else {
                break;
            };
            if let Some((evicted_key, worker)) = evicted {
                drop(worker.assignment_tx);
                let _ = worker.join_handle.join();
                eprintln!(
                    "agentos-v8-runtime: warm worker evicted key={} heap={}",
                    warm_key_prefix(&evicted_key),
                    evicted_key.heap_limit_mb
                );
                continue;
            }

            let worker = spawn_warm_worker(
                Arc::clone(&snapshot_cache),
                Arc::clone(&slot_control),
                key.clone(),
                bridge_code.clone(),
                userland_code.clone(),
                heap_limit_mb,
            );
            let mut state = self.state.lock().expect("warm worker pool lock poisoned");
            state.reserved_workers = state.reserved_workers.saturating_sub(1);
            let Some(worker) = worker else {
                break;
            };

            let workers = state.workers.entry(key.clone()).or_default();
            if workers.len() >= desired {
                drop(worker.assignment_tx);
                let _ = worker.join_handle.join();
                break;
            }
            workers.push(worker);
            eprintln!(
                "agentos-v8-runtime: warm worker refilled key={} heap={} pool_size={}",
                warm_key_prefix(&key),
                key.heap_limit_mb,
                workers.len()
            );
        }

        self.state
            .lock()
            .expect("warm worker pool lock poisoned")
            .refilling
            .remove(&key);
    }
}

#[cfg(not(test))]
fn spawn_warm_worker(
    snapshot_cache: Arc<SnapshotCache>,
    slot_control: SlotControl,
    key: WarmPoolKey,
    bridge_code: String,
    userland_code: String,
    heap_limit_mb: Option<u32>,
) -> Option<ParkedWorker> {
    let (assignment_tx, assignment_rx) = crossbeam_channel::bounded::<SessionAssignment>(1);
    let (ready_tx, ready_rx) = crossbeam_channel::bounded::<Result<(), String>>(1);
    let worker_bridge_code = bridge_code.clone();
    let worker_userland_code = userland_code.clone();
    // AGENTOS_THREAD_SITE: bounded-v8-warm-worker
    let join_handle = match thread::Builder::new()
        .name(String::from("agentos-v8-warm-worker"))
        .spawn(move || {
            let precreated = precreate_warm_isolate(
                snapshot_cache,
                slot_control,
                worker_bridge_code,
                worker_userland_code,
                heap_limit_mb,
            );
            match precreated {
                Ok(precreated) => {
                    if ready_tx.send(Ok(())).is_err() {
                        eprintln!("INFO_AGENTOS_STALE_WARM_WORKER: warm worker requester disconnected before startup completed");
                    }
                    if let Ok(assignment) = assignment_rx.recv() {
                        session_thread(assignment, Some(precreated));
                    }
                }
                Err(error) => {
                    if ready_tx.send(Err(error)).is_err() {
                        eprintln!("INFO_AGENTOS_STALE_WARM_WORKER: warm worker requester disconnected before startup failure was delivered");
                    }
                }
            }
        }) {
        Ok(handle) => handle,
        Err(error) => {
            eprintln!("agentos-v8-runtime: warm worker spawn failed: {error}");
            return None;
        }
    };

    match ready_rx.recv() {
        Ok(Ok(())) => Some(ParkedWorker {
            assignment_tx,
            join_handle,
        }),
        Ok(Err(error)) => {
            eprintln!(
                "agentos-v8-runtime: warm worker refill failed key={} heap={}: {error}",
                warm_key_prefix(&key),
                key.heap_limit_mb
            );
            let _ = join_handle.join();
            None
        }
        Err(error) => {
            eprintln!(
                "agentos-v8-runtime: warm worker refill failed key={} heap={}: {error}",
                warm_key_prefix(&key),
                key.heap_limit_mb
            );
            let _ = join_handle.join();
            None
        }
    }
}

#[cfg(test)]
fn spawn_warm_worker(
    _snapshot_cache: Arc<SnapshotCache>,
    _slot_control: SlotControl,
    _key: WarmPoolKey,
    _bridge_code: String,
    _userland_code: String,
    _heap_limit_mb: Option<u32>,
) -> Option<ParkedWorker> {
    None
}

#[cfg(not(test))]
fn precreate_warm_isolate(
    snapshot_cache: Arc<SnapshotCache>,
    _slot_control: SlotControl,
    bridge_code: String,
    userland_code: String,
    heap_limit_mb: Option<u32>,
) -> Result<PrecreatedIsolate, String> {
    isolate::init_v8_platform();
    let snapshot_blob = snapshot_cache.get_or_create_with_userland(
        &bridge_code,
        (!userland_code.is_empty()).then_some(userland_code.as_str()),
    )?;
    let snapshot_blob = (*snapshot_blob).clone();
    // Parked workers are bounded by MAX_PROCESS_WARM_WORKERS and do not execute
    // guest code until they receive a slot-owning SessionAssignment. Building
    // one must not wait for every active session to exit: long-lived parents
    // need the pool to replenish while short-lived child commands come and go.
    let mut isolate = snapshot::create_isolate_from_snapshot(snapshot_blob, heap_limit_mb);
    isolate.set_host_import_module_dynamically_callback(execution::dynamic_import_callback);
    isolate.set_host_initialize_import_meta_object_callback(execution::import_meta_object_callback);
    let context = isolate::create_context(&mut isolate);
    Ok(PrecreatedIsolate {
        isolate: Some(isolate),
        context: Some(context),
        bridge_code,
        userland_code,
    })
}

/// Normalize an opt-in CPU-time budget: `Some(0)` means "disabled" and folds to
/// `None` so the CPU-budget watchdog is NOT armed. The runtime layer does not
/// invent a default here: agentos sidecar VM executions pass the typed
/// `limits.jsRuntime.cpuTimeLimitMs` default, while lower-level callers can pass
/// `None`/`0` deliberately.
fn normalize_cpu_time_limit_ms(cpu_time_limit_ms: Option<u32>) -> Option<u32> {
    cpu_time_limit_ms.filter(|budget_ms| *budget_ms > 0)
}

/// Normalize an opt-in WALL-CLOCK backstop: `Some(0)` means "disabled" and folds
/// to `None` so the wall-clock `TimeoutGuard` is NOT armed. There is no default —
/// when the caller passes `None`/`0`, the guest runs with no wall-clock limit
/// (opt-in by design, so long-lived ACP adapters are never killed by a default).
/// This is INDEPENDENT of the CPU-time budget: setting one does not arm the other.
fn normalize_wall_clock_limit_ms(wall_clock_limit_ms: Option<u32>) -> Option<u32> {
    wall_clock_limit_ms.filter(|limit_ms| *limit_ms > 0)
}

fn signal_session_shutdown(sender: &Sender<()>, session_id: &str) {
    match sender.try_send(()) {
        Ok(()) => {}
        Err(crossbeam_channel::TrySendError::Full(())) => eprintln!(
            "INFO_AGENTOS_VM_SHUTDOWN_COALESCED: session={session_id} already has a pending shutdown signal"
        ),
        Err(crossbeam_channel::TrySendError::Disconnected(())) => eprintln!(
            "INFO_AGENTOS_STALE_VM_SHUTDOWN: session={session_id} executor already disconnected"
        ),
    }
}

pub(crate) fn configured_resource_capacity(
    runtime: &RuntimeContext,
    resource: ResourceClass,
    vm_config_path: &'static str,
    process_config_path: &'static str,
) -> Result<usize, String> {
    let config_path = if runtime.vm_generation().is_some() {
        vm_config_path
    } else {
        process_config_path
    };
    match runtime.resources().usage(resource).limit {
        Some(capacity) if capacity > 0 => Ok(capacity),
        Some(_) => Err(format!(
            "ERR_AGENTOS_RUNTIME_CONFIG: {config_path} must be greater than zero"
        )),
        None => Err(format!(
            "ERR_AGENTOS_RUNTIME_CONFIG: {config_path} must configure a bounded {} limit",
            resource.name()
        )),
    }
}

/// Internal entry for a running session
struct SessionEntry {
    /// Output receiver generation current when this session was created.
    output_generation: Option<u64>,
    /// Channel to send commands to the session thread
    tx: Sender<SessionCommand>,
    /// Configured bound for this generation's ordinary command lane.
    command_capacity: usize,
    /// Dedicated capacity-one control lane. Shutdown must never contend with
    /// ordinary session commands, because that lane may be full precisely when
    /// an overloaded session needs to be terminated.
    shutdown_tx: Sender<()>,
    /// Thread join handle
    join_handle: Option<thread::JoinHandle<()>>,
    /// Thread-safe V8 isolate handle for out-of-band termination.
    #[cfg_attr(test, allow(dead_code))]
    isolate_handle: SharedIsolateHandle,
    /// Current execution abort handle used to wake sync bridge waits.
    execution_abort: SharedExecutionAbort,
    /// Set from Execute admission until that operation leaves the session
    /// thread, including the startup window before an isolate handle exists.
    execution_active: Arc<AtomicBool>,
    pause_control: Arc<SessionPauseControl>,
    /// Durable socket readiness and its dedicated capacity-one wake lane.
    ready_broker: Arc<SessionReadiness>,
    #[cfg(test)]
    session_resources: Arc<agentos_runtime::accounting::ResourceLedger>,
}

/// Deferred shutdown work for a session that has already been removed from
/// the manager. `finish()` joins the session thread and clears any call
/// routes the thread registered while shutting down. Callers must release
/// the SessionManager lock before calling `finish()`. Joining under the lock
/// deadlocks: the dispatch thread needs the lock to drain the event channel,
/// and the joined thread can be parked on a full event channel send.
pub struct SessionShutdown {
    session_id: String,
    output_generation: Option<u64>,
    join_handle: Option<thread::JoinHandle<()>>,
    call_id_router: CallIdRouter,
}

impl SessionShutdown {
    pub fn finish(mut self) {
        if let Some(handle) = self.join_handle.take() {
            if handle.join().is_err() {
                eprintln!(
                    "ERR_AGENTOS_VM_EXECUTOR_PANIC: session={} generation={:?}",
                    self.session_id, self.output_generation
                );
            }
        }
        self.call_id_router
            .cancel_session(&self.session_id, self.output_generation);
    }
}

/// Concurrency slot tracker shared across session threads
type SlotControl = Arc<(Mutex<usize>, Condvar)>;

/// An admitted V8 executor slot. It is acquired before spawning or assigning
/// an OS thread and remains owned by that generation until the thread exits.
/// Detached/stuck generations therefore stay quarantined instead of lending
/// their capacity to a successor VM.
struct SessionSlotPermit {
    control: SlotControl,
    metrics: RuntimeMetrics,
}

impl SessionSlotPermit {
    fn try_acquire(
        control: &SlotControl,
        maximum: usize,
        metrics: RuntimeMetrics,
    ) -> Result<Self, String> {
        let (lock, _) = &**control;
        let mut active = lock
            .lock()
            .map_err(|_| String::from("ERR_AGENTOS_VM_EXECUTOR_POISONED: slot lock poisoned"))?;
        if *active >= maximum {
            return Err(format!(
                "ERR_AGENTOS_VM_EXECUTOR_LIMIT: active V8 executors reached limit of {maximum}; raise runtime.executor.maxActiveVms"
            ));
        }
        *active += 1;
        metrics.observe_executor(ExecutorMetricClass::Vm, *active, 0);
        Ok(Self {
            control: Arc::clone(control),
            metrics,
        })
    }
}

impl Drop for SessionSlotPermit {
    fn drop(&mut self) {
        let (lock, cvar) = &*self.control;
        match lock.lock() {
            Ok(mut active) if *active > 0 => {
                *active -= 1;
                self.metrics
                    .observe_executor(ExecutorMetricClass::Vm, *active, 0);
                cvar.notify_all();
            }
            Ok(_) => eprintln!(
                "ERR_AGENTOS_VM_EXECUTOR_ACCOUNTING_UNDERFLOW: executor permit released at zero"
            ),
            Err(_) => {
                eprintln!("ERR_AGENTOS_VM_EXECUTOR_POISONED: executor permit could not be released")
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExecutionAbortReason {
    /// Caller explicitly terminated the execution (e.g. session destroy).
    Terminated,
    /// The opt-in WALL-CLOCK backstop (`TimeoutGuard`) elapsed. Counts elapsed
    /// real time INCLUDING idle/await, so it can cap a guest that blocks/awaits
    /// indefinitely. Armed only when `limits.jsRuntime.wallClockLimitMs` is set;
    /// independent of the CPU-time budget.
    #[cfg_attr(test, allow(dead_code))]
    WallClockTimedOut,
    /// The TRUE CPU-TIME budget (`CpuBudgetGuard`) was exhausted by active JS CPU.
    #[cfg_attr(test, allow(dead_code))]
    CpuBudgetExceeded,
}

struct ExecutionAbortState {
    sender: Option<crossbeam_channel::Sender<()>>,
    reason: Option<ExecutionAbortReason>,
}

pub(crate) struct SharedExecutionAbort(Arc<Mutex<Option<ExecutionAbortState>>>);

impl Clone for SharedExecutionAbort {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

pub(crate) fn new_execution_abort() -> SharedExecutionAbort {
    SharedExecutionAbort(Arc::new(Mutex::new(None)))
}

#[cfg_attr(test, allow(dead_code))]
pub(crate) struct ActiveExecutionAbort {
    shared: SharedExecutionAbort,
}

#[cfg_attr(test, allow(dead_code))]
impl ActiveExecutionAbort {
    pub(crate) fn arm(shared: &SharedExecutionAbort) -> (Self, crossbeam_channel::Receiver<()>) {
        let (tx, rx) = crossbeam_channel::bounded::<()>(0);
        let mut guard = shared.0.lock().unwrap();
        if let Some(reason) = guard.as_ref().and_then(|state| state.reason) {
            // Cancellation is durable across the short gap between dequeuing
            // Execute and arming its waiter. Leave the new receiver
            // disconnected so the execution observes the already-recorded
            // terminal reason immediately.
            drop(tx);
            *guard = Some(ExecutionAbortState {
                sender: None,
                reason: Some(reason),
            });
        } else {
            *guard = Some(ExecutionAbortState {
                sender: Some(tx),
                reason: None,
            });
        }
        (
            Self {
                shared: shared.clone(),
            },
            rx,
        )
    }
}

impl Drop for ActiveExecutionAbort {
    fn drop(&mut self) {
        *self.shared.0.lock().unwrap() = None;
    }
}

struct ExecutionActivityGuard(Arc<AtomicBool>);

impl Drop for ExecutionActivityGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

pub(crate) fn signal_execution_abort(shared: &SharedExecutionAbort, reason: ExecutionAbortReason) {
    let mut guard = shared.0.lock().unwrap();
    if let Some(state) = guard.as_mut() {
        state.reason.get_or_insert(reason);
        state.sender.take();
    }
}

fn signal_execution_abort_durable(shared: &SharedExecutionAbort, reason: ExecutionAbortReason) {
    let mut guard = shared.0.lock().unwrap();
    if let Some(state) = guard.as_mut() {
        state.reason.get_or_insert(reason);
        state.sender.take();
    } else {
        // Session teardown is durable even if the execution has not armed its
        // receiver yet. Ordinary TerminateExecution remains edge-scoped so a
        // request against an idle reusable session does not poison its next run.
        *guard = Some(ExecutionAbortState {
            sender: None,
            reason: Some(reason),
        });
    }
}

#[cfg(not(test))]
fn execution_abort_reason(shared: &SharedExecutionAbort) -> Option<ExecutionAbortReason> {
    shared
        .0
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|state| state.reason)
}

/// Manages V8 sessions with concurrency limiting.
/// Each session runs on a dedicated OS thread with its own V8 isolate.
pub struct SessionManager {
    sessions: HashMap<String, SessionEntry>,
    /// Detached generations remain owned until their executor exits. The
    /// thread itself retains the concurrency permit, so a successor cannot
    /// consume capacity that is still running untrusted code.
    quarantined: Vec<QuarantinedSession>,
    max_concurrency: usize,
    slot_control: SlotControl,
    /// Typed runtime event sender shared across session threads.
    event_tx: RuntimeEventSender,
    /// Call_id → session_id routing table for BridgeResponse dispatch
    call_id_router: CallIdRouter,
    /// Shared call_id counter — all sessions use this to generate globally unique
    /// call_ids, preventing collisions in the call_id_router
    shared_call_id: SharedCallIdCounter,
    /// Shared snapshot cache for fast isolate creation from pre-compiled bridge code
    snapshot_cache: Arc<SnapshotCache>,
    /// Ready-to-claim isolate workers keyed by snapshot digest and heap cap.
    warm_pool: Arc<WarmWorkerPool>,
    /// Process-owned scheduler and bounded blocking executor, injected when the
    /// session manager is constructed rather than discovered during refill.
    runtime: RuntimeContext,
    executor_teardown_timeout: Duration,
}

struct QuarantinedSession {
    session_id: String,
    output_generation: Option<u64>,
    join_handle: thread::JoinHandle<()>,
    quarantined_at: Instant,
    deadline_reported: bool,
}

impl SessionManager {
    pub fn new(
        max_concurrency: usize,
        event_tx: impl Into<RuntimeEventSender>,
        call_id_router: CallIdRouter,
        snapshot_cache: Arc<SnapshotCache>,
        runtime: RuntimeContext,
    ) -> Self {
        SessionManager {
            sessions: HashMap::new(),
            quarantined: Vec::new(),
            max_concurrency,
            slot_control: Arc::new((Mutex::new(0), Condvar::new())),
            event_tx: event_tx.into(),
            call_id_router,
            shared_call_id: Arc::new(AtomicU64::new(1)),
            snapshot_cache,
            warm_pool: Arc::new(WarmWorkerPool::default()),
            executor_teardown_timeout: runtime.vm_executor_teardown_timeout(),
            runtime,
        }
    }

    #[cfg(test)]
    pub(crate) fn max_concurrency(&self) -> usize {
        self.max_concurrency
    }

    /// Get the snapshot cache for pre-warming from WarmSnapshot messages.
    #[allow(dead_code)]
    pub fn snapshot_cache(&self) -> &Arc<SnapshotCache> {
        &self.snapshot_cache
    }

    pub fn pre_warm_workers(
        &self,
        bridge_code: String,
        userland_code: String,
        heap_limit_mb: Option<u32>,
        count: usize,
    ) {
        self.warm_pool.ensure_count(
            self.runtime.clone(),
            Arc::clone(&self.snapshot_cache),
            Arc::clone(&self.slot_control),
            bridge_code,
            userland_code,
            heap_limit_mb,
            count,
        );
    }

    /// Create a new session.
    /// Spawns a dedicated admitted thread with a V8 isolate. Admission happens
    /// before thread creation; overload returns a typed limit error.
    pub fn create_session(
        &mut self,
        session_id: String,
        heap_limit_mb: Option<u32>,
        cpu_time_limit_ms: Option<u32>,
        wall_clock_limit_ms: Option<u32>,
    ) -> Result<(), String> {
        self.create_session_with_output_generation(
            session_id,
            heap_limit_mb,
            cpu_time_limit_ms,
            wall_clock_limit_ms,
            None,
            None,
        )
    }

    pub fn create_session_with_output_generation(
        &mut self,
        session_id: String,
        heap_limit_mb: Option<u32>,
        cpu_time_limit_ms: Option<u32>,
        wall_clock_limit_ms: Option<u32>,
        output_generation: Option<u64>,
        warm_hint: Option<WarmSessionHint>,
    ) -> Result<(), String> {
        self.create_session_with_output_generation_and_sender(
            session_id,
            heap_limit_mb,
            cpu_time_limit_ms,
            wall_clock_limit_ms,
            output_generation,
            warm_hint,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_session_with_output_generation_and_sender(
        &mut self,
        session_id: String,
        heap_limit_mb: Option<u32>,
        cpu_time_limit_ms: Option<u32>,
        wall_clock_limit_ms: Option<u32>,
        output_generation: Option<u64>,
        warm_hint: Option<WarmSessionHint>,
        event_tx: Option<RuntimeEventSender>,
    ) -> Result<(), String> {
        // Serialized/standalone sessions do not carry VM reactor policy. Keep
        // their historical ceiling; sidecar VM sessions must call the explicit
        // `_and_runtime` path with `limits.reactor.workQuantum`.
        let ready_batch_handle_limit = configured_resource_capacity(
            &self.runtime,
            ResourceClass::ReadyHandles,
            "limits.reactor.maxReadyHandles",
            "runtime.resources.maxReadyHandles",
        )?;
        self.create_session_with_output_generation_sender_and_runtime(
            session_id,
            heap_limit_mb,
            cpu_time_limit_ms,
            wall_clock_limit_ms,
            output_generation,
            warm_hint,
            event_tx,
            self.runtime.clone(),
            ready_batch_handle_limit,
            crate::host_call::DEFAULT_BRIDGE_CALL_TIMEOUT,
        )
    }

    /// Create a session whose guest-owned work and resource reservations are
    /// charged to `session_runtime`. The manager's own runtime remains the
    /// process-scoped context used for shared snapshot and warm-pool work.
    #[allow(clippy::too_many_arguments)]
    pub fn create_session_with_output_generation_sender_and_runtime(
        &mut self,
        session_id: String,
        heap_limit_mb: Option<u32>,
        cpu_time_limit_ms: Option<u32>,
        wall_clock_limit_ms: Option<u32>,
        output_generation: Option<u64>,
        warm_hint: Option<WarmSessionHint>,
        event_tx: Option<RuntimeEventSender>,
        session_runtime: RuntimeContext,
        ready_batch_handle_limit: usize,
        bridge_call_timeout: Duration,
    ) -> Result<(), String> {
        self.reap_finished_quarantines();
        if self.sessions.contains_key(&session_id) {
            return Err(format!("session {} already exists", session_id));
        }

        let slot_permit = SessionSlotPermit::try_acquire(
            &self.slot_control,
            self.max_concurrency,
            self.runtime.metrics().clone(),
        )?;

        let cpu_time_limit_ms = normalize_cpu_time_limit_ms(cpu_time_limit_ms);
        let wall_clock_limit_ms = normalize_wall_clock_limit_ms(wall_clock_limit_ms);
        let command_capacity = configured_resource_capacity(
            &session_runtime,
            ResourceClass::HandleCommands,
            "limits.reactor.maxHandleCommands",
            "runtime.resources.maxHandleCommands",
        )?;
        let (tx, rx) = crossbeam_channel::bounded(command_capacity);
        let (shutdown_tx, shutdown_rx) = crossbeam_channel::bounded(1);
        let ready_generation = output_generation.unwrap_or(1);
        let (ready_broker, ready_rx) =
            SessionReadiness::new(ready_generation, &session_runtime, ready_batch_handle_limit)?;
        let isolate_handle = Arc::new(Mutex::new(None));
        let execution_abort = new_execution_abort();
        let execution_active = Arc::new(AtomicBool::new(false));
        let pause_control = Arc::new(SessionPauseControl::default());
        #[cfg(test)]
        let session_resources = Arc::clone(session_runtime.resources());
        let assignment = SessionAssignment {
            heap_limit_mb,
            cpu_time_limit_ms,
            wall_clock_limit_ms,
            rx,
            shutdown_rx,
            ready_rx,
            ready_broker: Arc::clone(&ready_broker),
            slot_permit,
            event_tx: event_tx.unwrap_or_else(|| self.event_tx.clone()),
            call_id_router: Arc::clone(&self.call_id_router),
            shared_call_id: Arc::clone(&self.shared_call_id),
            snapshot_cache: Arc::clone(&self.snapshot_cache),
            isolate_handle: Arc::clone(&isolate_handle),
            execution_abort: execution_abort.clone(),
            execution_active: Arc::clone(&execution_active),
            pause_control: Arc::clone(&pause_control),
            session_id: session_id.clone(),
            output_generation,
            runtime: session_runtime,
            bridge_call_timeout,
        };

        let join_handle = match self.claim_warm_worker(warm_hint.as_ref(), assignment) {
            Ok((join_handle, true)) => {
                if let Some(hint) = warm_hint {
                    self.warm_pool.ensure_count(
                        self.runtime.clone(),
                        Arc::clone(&self.snapshot_cache),
                        Arc::clone(&self.slot_control),
                        hint.bridge_code,
                        hint.userland_code,
                        hint.heap_limit_mb,
                        warm_worker_capacity_per_key(),
                    );
                }
                join_handle
            }
            Ok((join_handle, false)) => join_handle,
            Err(assignment) => {
                if let Some(hint) = warm_hint {
                    self.warm_pool.ensure_count(
                        self.runtime.clone(),
                        Arc::clone(&self.snapshot_cache),
                        Arc::clone(&self.slot_control),
                        hint.bridge_code,
                        hint.userland_code,
                        hint.heap_limit_mb,
                        warm_worker_capacity_per_key(),
                    );
                }
                spawn_session_thread(assignment)
                    .map_err(|e| format!("failed to spawn session thread: {}", e))?
            }
        };

        self.sessions.insert(
            session_id,
            SessionEntry {
                output_generation,
                tx,
                command_capacity,
                shutdown_tx,
                join_handle: Some(join_handle),
                isolate_handle,
                execution_abort,
                execution_active,
                pause_control,
                ready_broker,
                #[cfg(test)]
                session_resources,
            },
        );

        Ok(())
    }

    // The Err variant intentionally carries the whole SessionAssignment back to
    // the caller for the fallback spawn path — it is moved, not copied.
    #[allow(clippy::result_large_err)]
    fn claim_warm_worker(
        &self,
        warm_hint: Option<&WarmSessionHint>,
        assignment: SessionAssignment,
    ) -> Result<(thread::JoinHandle<()>, bool), SessionAssignment> {
        let Some(hint) = warm_hint else {
            return Err(assignment);
        };
        if warm_worker_capacity_per_key() == 0 {
            record_warm_worker_miss();
            return Err(assignment);
        }

        let key = warm_pool_key(&hint.bridge_code, &hint.userland_code, hint.heap_limit_mb);
        let Some(worker) = self.warm_pool.claim(&key) else {
            record_warm_worker_miss();
            eprintln!(
                "agentos-v8-runtime: warm worker pool-empty key={} heap={}",
                warm_key_prefix(&key),
                key.heap_limit_mb
            );
            return Err(assignment);
        };

        match worker.assignment_tx.send(assignment) {
            Ok(()) => {
                record_warm_worker_hit();
                eprintln!(
                    "agentos-v8-runtime: warm worker claimed key={} heap={}",
                    warm_key_prefix(&key),
                    key.heap_limit_mb
                );
                Ok((worker.join_handle, true))
            }
            Err(error) => {
                record_warm_worker_miss();
                let _ = worker.join_handle.join();
                Err(error.0)
            }
        }
    }

    pub fn destroy_session_if_output_generation(
        &mut self,
        session_id: &str,
        output_generation: u64,
    ) -> Result<bool, String> {
        match self.begin_destroy_session_if_output_generation(session_id, output_generation)? {
            Some(shutdown) => {
                shutdown.finish();
                Ok(true)
            }
            None => Ok(false),
        }
    }

    pub fn begin_destroy_session_if_output_generation(
        &mut self,
        session_id: &str,
        output_generation: u64,
    ) -> Result<Option<SessionShutdown>, String> {
        if self
            .sessions
            .get(session_id)
            .is_none_or(|entry| entry.output_generation != Some(output_generation))
        {
            return Ok(None);
        }

        self.begin_destroy_session(session_id).map(Some)
    }

    pub fn detach_session_if_output_generation(
        &mut self,
        session_id: &str,
        output_generation: u64,
    ) -> Result<bool, String> {
        if self
            .sessions
            .get(session_id)
            .is_none_or(|entry| entry.output_generation != Some(output_generation))
        {
            return Ok(false);
        }

        self.detach_session(session_id)?;
        Ok(true)
    }

    pub(crate) fn detach_session(&mut self, session_id: &str) -> Result<(), String> {
        let entry = self
            .sessions
            .get(session_id)
            .ok_or_else(|| format!("session {} does not exist", session_id))?;
        entry.pause_control.shutdown();

        #[cfg(not(test))]
        if let Some(handle) = entry
            .isolate_handle
            .lock()
            .ok()
            .and_then(|guard| guard.as_ref().cloned())
        {
            handle.terminate_execution();
        }
        signal_execution_abort_durable(&entry.execution_abort, ExecutionAbortReason::Terminated);
        self.clear_call_routes_for_session(session_id, entry.output_generation);
        let mut entry = self.sessions.remove(session_id).unwrap();
        signal_session_shutdown(&entry.shutdown_tx, session_id);
        drop(entry.tx);
        if let Some(join_handle) = entry.join_handle.take() {
            eprintln!(
                "WARN_AGENTOS_VM_EXECUTOR_QUARANTINED: session={} generation={:?}",
                session_id, entry.output_generation
            );
            self.quarantined.push(QuarantinedSession {
                session_id: session_id.to_owned(),
                output_generation: entry.output_generation,
                join_handle,
                quarantined_at: Instant::now(),
                deadline_reported: false,
            });
        }
        Ok(())
    }

    fn reap_finished_quarantines(&mut self) {
        let mut retained = Vec::with_capacity(self.quarantined.len());
        for mut quarantined in self.quarantined.drain(..) {
            if !quarantined.join_handle.is_finished() {
                if !quarantined.deadline_reported
                    && quarantined.quarantined_at.elapsed() >= self.executor_teardown_timeout
                {
                    quarantined.deadline_reported = true;
                    eprintln!(
                        "ERR_AGENTOS_VM_EXECUTOR_TEARDOWN_TIMEOUT: session={} generation={:?} deadline_ms={}; executor remains quarantined and retains its permit; raise runtime.executor.teardownTimeoutMs",
                        quarantined.session_id,
                        quarantined.output_generation,
                        self.executor_teardown_timeout.as_millis()
                    );
                }
                retained.push(quarantined);
                continue;
            }
            if quarantined.join_handle.join().is_err() {
                eprintln!(
                    "ERR_AGENTOS_VM_EXECUTOR_PANIC: quarantined session={} generation={:?}",
                    quarantined.session_id, quarantined.output_generation
                );
            } else {
                eprintln!(
                    "INFO_AGENTOS_VM_EXECUTOR_QUARANTINE_RELEASED: session={} generation={:?}",
                    quarantined.session_id, quarantined.output_generation
                );
            }
            self.call_id_router
                .cancel_session(&quarantined.session_id, quarantined.output_generation);
        }
        self.quarantined = retained;
    }

    /// Destroy a session inline. Joins the session thread before returning, so
    /// this must not be called while a shared lock on the manager is held. Lock
    /// holders use `begin_destroy_session` and call `finish()` after unlocking.
    pub fn destroy_session(&mut self, session_id: &str) -> Result<(), String> {
        self.begin_destroy_session(session_id)?.finish();
        Ok(())
    }

    /// First phase of destroying a session: terminate execution, signal abort,
    /// send shutdown, clear call routes, and remove the entry. The returned
    /// shutdown joins the session thread and must be finished after the
    /// SessionManager lock is released.
    pub fn begin_destroy_session(&mut self, session_id: &str) -> Result<SessionShutdown, String> {
        if !self.sessions.contains_key(session_id) {
            return Err(format!("session {} does not exist", session_id));
        }

        let output_generation = self
            .sessions
            .get(session_id)
            .and_then(|entry| entry.output_generation);
        self.clear_call_routes_for_session(session_id, output_generation);
        let mut entry = self
            .sessions
            .remove(session_id)
            .expect("checked session exists");
        entry.pause_control.shutdown();

        #[cfg(not(test))]
        if let Some(handle) = entry
            .isolate_handle
            .lock()
            .ok()
            .and_then(|guard| guard.as_ref().cloned())
        {
            handle.terminate_execution();
        }
        signal_execution_abort_durable(&entry.execution_abort, ExecutionAbortReason::Terminated);
        // Shutdown has a dedicated lane so a full ordinary command queue can
        // never turn the following join into a deadlock.
        signal_session_shutdown(&entry.shutdown_tx, session_id);
        let join_handle = entry.join_handle.take();
        drop(entry);
        Ok(SessionShutdown {
            session_id: session_id.to_owned(),
            output_generation,
            join_handle,
            call_id_router: Arc::clone(&self.call_id_router),
        })
    }

    pub(crate) fn take_session_shutdown_handles(&mut self) -> Vec<thread::JoinHandle<()>> {
        self.call_id_router.clear();

        let mut handles: Vec<_> = self
            .sessions
            .drain()
            .filter_map(|(session_id, mut entry)| {
                #[cfg(not(test))]
                if let Some(handle) = entry
                    .isolate_handle
                    .lock()
                    .ok()
                    .and_then(|guard| guard.as_ref().cloned())
                {
                    handle.terminate_execution();
                }
                signal_execution_abort_durable(
                    &entry.execution_abort,
                    ExecutionAbortReason::Terminated,
                );
                signal_session_shutdown(&entry.shutdown_tx, &session_id);
                drop(entry.tx);
                entry.join_handle.take()
            })
            .collect();
        handles.extend(self.warm_pool.shutdown_handles());
        handles.extend(
            self.quarantined
                .drain(..)
                .map(|quarantined| quarantined.join_handle),
        );
        handles
    }

    #[cfg(test)]
    pub(crate) fn clear_call_route(&self, call_id: u64) {
        self.call_id_router.cancel(call_id);
    }

    fn clear_call_routes_for_session(&self, session_id: &str, output_generation: Option<u64>) {
        self.call_id_router
            .cancel_session(session_id, output_generation);
    }

    /// Resolve a session's command sender and apply message side effects that
    /// must happen under the manager lock (isolate termination, abort signal).
    /// The caller sends on the returned channel after releasing the lock so a
    /// full command channel cannot block the manager mutex.
    pub fn session_command_sender(
        &self,
        session_id: &str,
        msg: &SessionMessage,
    ) -> Result<(Sender<SessionCommand>, usize, Arc<AtomicBool>), String> {
        let entry = self
            .sessions
            .get(session_id)
            .ok_or_else(|| format!("session {} does not exist", session_id))?;

        #[cfg(not(test))]
        if matches!(msg, SessionMessage::TerminateExecution) {
            if let Some(handle) = entry
                .isolate_handle
                .lock()
                .ok()
                .and_then(|guard| guard.as_ref().cloned())
            {
                handle.terminate_execution();
            }
        }
        if matches!(msg, SessionMessage::TerminateExecution) {
            if entry.execution_active.load(Ordering::Acquire) {
                signal_execution_abort_durable(
                    &entry.execution_abort,
                    ExecutionAbortReason::Terminated,
                );
            } else {
                signal_execution_abort(&entry.execution_abort, ExecutionAbortReason::Terminated);
            }
        }

        Ok((
            entry.tx.clone(),
            entry.command_capacity,
            Arc::clone(&entry.execution_active),
        ))
    }

    /// Admit an ordinary message without ever blocking the thread that also
    /// routes call-specific bridge responses. Readiness must use
    /// `publish_readiness`; ordinary events are never reclassified by name.
    pub fn try_send_to_session(&self, session_id: &str, msg: SessionMessage) -> Result<(), String> {
        let terminate_requested = matches!(&msg, SessionMessage::TerminateExecution);
        let execute_requested = matches!(&msg, SessionMessage::Execute { .. });
        let incoming_kind = match &msg {
            SessionMessage::InjectGlobals { .. } => String::from("inject_globals"),
            SessionMessage::Execute { .. } => String::from("execute"),
            SessionMessage::BridgeResponse(_) => String::from("bridge_response"),
            SessionMessage::StreamEvent(event) => format!("stream_event:{}", event.event_type),
            SessionMessage::TerminateExecution => String::from("terminate_execution"),
        };
        let (sender, command_capacity, execution_active) =
            self.session_command_sender(session_id, &msg)?;
        if execute_requested {
            execution_active.store(true, Ordering::Release);
        }
        let command = SessionCommand::Message(msg);

        match sender.try_send(command) {
            Ok(()) => Ok(()),
            Err(crossbeam_channel::TrySendError::Full(_)) if terminate_requested => {
                // session_command_sender already delivered termination through
                // the isolate handle and execution-abort channel.
                Ok(())
            }
            Err(crossbeam_channel::TrySendError::Full(_)) => {
                if execute_requested {
                    execution_active.store(false, Ordering::Release);
                }
                Err(format!(
                    "ERR_AGENTOS_SESSION_COMMAND_LIMIT: session {session_id} command queue exceeded limit of {command_capacity} while admitting {incoming_kind} (queued={}); raise limits.reactor.maxHandleCommands",
                    sender.len()
                ))
            }
            Err(crossbeam_channel::TrySendError::Disconnected(_)) => {
                if execute_requested {
                    execution_active.store(false, Ordering::Release);
                }
                Err(format!(
                    "session thread disconnected for session {session_id}"
                ))
            }
        }
    }

    pub fn publish_readiness(
        &self,
        session_id: &str,
        capability_id: u64,
        capability_generation: u64,
        flags: ReadyFlags,
    ) -> Result<(), String> {
        let entry = self
            .sessions
            .get(session_id)
            .ok_or_else(|| format!("session {session_id} does not exist"))?;
        entry
            .ready_broker
            .publish(capability_id, capability_generation, flags)
    }

    pub fn publish_signal(&self, session_id: &str, signal: i32) -> Result<(), String> {
        let entry = self
            .sessions
            .get(session_id)
            .ok_or_else(|| format!("session {session_id} does not exist"))?;
        entry.ready_broker.publish_signal(signal)
    }

    pub fn remove_readiness(
        &self,
        session_id: &str,
        capability_id: u64,
        capability_generation: u64,
    ) -> Result<(), String> {
        let entry = self
            .sessions
            .get(session_id)
            .ok_or_else(|| format!("session {session_id} does not exist"))?;
        entry
            .ready_broker
            .remove(capability_id, capability_generation)
    }

    pub fn set_application_read_interest(
        &self,
        session_id: &str,
        capability_id: u64,
        capability_generation: u64,
        enabled: bool,
    ) -> Result<(), String> {
        let entry = self
            .sessions
            .get(session_id)
            .ok_or_else(|| format!("session {session_id} does not exist"))?;
        entry.ready_broker.set_application_read_interest(
            capability_id,
            capability_generation,
            enabled,
        )
    }

    pub fn publish_timer(&self, session_id: &str, timer_id: u64) -> Result<(), String> {
        let entry = self
            .sessions
            .get(session_id)
            .ok_or_else(|| format!("session {session_id} does not exist"))?;
        entry.ready_broker.publish_timer(timer_id)
    }

    /// Get a session's command sender without a message (used for control commands
    /// like SetModuleReader that aren't a SessionMessage). Dispatch-thread only.
    pub fn session_sender(
        &self,
        session_id: &str,
    ) -> Result<(Sender<SessionCommand>, usize), String> {
        self.sessions
            .get(session_id)
            .map(|entry| (entry.tx.clone(), entry.command_capacity))
            .ok_or_else(|| format!("session {} does not exist", session_id))
    }

    /// Pause a session at the V8 execution boundary. A running synchronous
    /// script is interrupted on its isolate thread and remains parked with its
    /// JavaScript stack intact until `resume_session` is called.
    pub fn pause_session(&self, session_id: &str) -> Result<(), String> {
        let entry = self
            .sessions
            .get(session_id)
            .ok_or_else(|| format!("session {} does not exist", session_id))?;
        entry.pause_control.pause();
        #[cfg(not(test))]
        if let Some(handle) = entry
            .isolate_handle
            .lock()
            .ok()
            .and_then(|guard| guard.as_ref().cloned())
        {
            let raw = Arc::into_raw(Arc::clone(&entry.pause_control)) as *mut std::ffi::c_void;
            if !handle.request_interrupt(pause_isolate_interrupt, raw) {
                // SAFETY: V8 rejected the interrupt, so it did not consume the
                // strong reference transferred above.
                unsafe { drop(Arc::from_raw(raw.cast::<SessionPauseControl>())) };
            }
        }
        Ok(())
    }

    pub fn resume_session(&self, session_id: &str) -> Result<(), String> {
        let entry = self
            .sessions
            .get(session_id)
            .ok_or_else(|| format!("session {} does not exist", session_id))?;
        entry.pause_control.resume();
        Ok(())
    }

    /// Send a message to a session without blocking response/control progress.
    pub fn send_to_session(&self, session_id: &str, msg: SessionMessage) -> Result<(), String> {
        self.try_send_to_session(session_id, msg)
    }

    /// Destroy a set of sessions inline, ignoring sessions that were already
    /// removed. Joins session threads, so this must not be called while a
    /// shared lock on the manager is held.
    pub fn destroy_sessions<I>(&mut self, session_ids: I)
    where
        I: IntoIterator<Item = String>,
    {
        for shutdown in self.begin_destroy_sessions(session_ids) {
            shutdown.finish();
        }
    }

    /// Begin destroying a set of sessions, ignoring sessions that were already
    /// removed. Finish each returned shutdown after releasing the manager lock.
    pub fn begin_destroy_sessions<I>(&mut self, session_ids: I) -> Vec<SessionShutdown>
    where
        I: IntoIterator<Item = String>,
    {
        session_ids
            .into_iter()
            .filter_map(|sid| self.begin_destroy_session(&sid).ok())
            .collect()
    }

    /// Number of registered sessions (including those waiting for a slot).
    #[allow(dead_code)]
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    #[allow(dead_code)]
    pub fn quarantined_session_count(&mut self) -> usize {
        self.reap_finished_quarantines();
        self.quarantined.len()
    }

    /// Return all session IDs.
    #[allow(dead_code)]
    pub fn all_sessions(&self) -> Vec<String> {
        self.sessions.keys().cloned().collect()
    }

    /// Number of sessions that have acquired a concurrency slot.
    #[allow(dead_code)]
    pub fn active_slot_count(&self) -> usize {
        let (lock, _) = &*self.slot_control;
        *lock.lock().unwrap()
    }

    pub fn session_output_generation(&self, session_id: &str) -> Option<u64> {
        self.sessions
            .get(session_id)
            .and_then(|entry| entry.output_generation)
    }

    #[cfg(test)]
    pub fn session_resources(
        &self,
        session_id: &str,
    ) -> Option<Arc<agentos_runtime::accounting::ResourceLedger>> {
        self.sessions
            .get(session_id)
            .map(|entry| Arc::clone(&entry.session_resources))
    }

    /// Get the direct bridge-call response registry.
    pub fn call_id_router(&self) -> &CallIdRouter {
        &self.call_id_router
    }
}

/// Send a typed runtime event without re-serializing it on the session thread.
#[cfg(not(test))]
fn send_event_with_generation(
    event_tx: &RuntimeEventSender,
    output_generation: Option<u64>,
    event: RuntimeEvent,
) {
    if let Err(error) = event_tx.send(RuntimeEventEnvelope {
        output_generation,
        event,
    }) {
        eprintln!("failed to send runtime event: {error}");
    }
}

fn send_late_message_warning(
    event_tx: &RuntimeEventSender,
    session_id: &str,
    output_generation: Option<u64>,
    error_code: &str,
    detail: String,
) {
    let warning = RuntimeEvent::Log {
        session_id: session_id.to_string(),
        channel: 1,
        message: format!("[{error_code}] {detail}"),
    };
    if let Err(error) = event_tx.send(RuntimeEventEnvelope {
        output_generation,
        event: warning,
    }) {
        eprintln!("failed to send late-session warning: {error}");
    }
}

fn handle_late_session_message(
    event_tx: &RuntimeEventSender,
    session_id: &str,
    output_generation: Option<u64>,
    message: SessionMessage,
) {
    match message {
        SessionMessage::BridgeResponse(BridgeResponse {
            call_id,
            status,
            payload,
            reservation: _,
        }) => send_late_message_warning(
            event_tx,
            session_id,
            output_generation,
            LATE_BRIDGE_RESPONSE_ERROR_CODE,
            format!(
                "dropping BridgeResponse after execution completed (call_id={call_id}, status={status}, payload_len={})",
                payload.len()
            ),
        ),
        SessionMessage::StreamEvent(StreamEvent {
            event_type,
            payload,
        }) => {
            // Timer and socket-readiness events are wake hints, not data.
            // `stdin_end` is likewise an idempotent teardown notification. All
            // three can race execution completion by design and carry no data
            // to recover, so classify them as expected stale control events
            // instead of writing a false error into the guest's stderr.
            if event_type == "timer" || event_type == "net_socket" || event_type == "stdin_end" {
                return;
            }
            send_late_message_warning(
                event_tx,
                session_id,
                output_generation,
                LATE_STREAM_EVENT_ERROR_CODE,
                format!(
                    "dropping StreamEvent after execution completed (event_type={event_type}, payload_len={})",
                    payload.len()
                ),
            )
        }
        SessionMessage::TerminateExecution => send_late_message_warning(
            event_tx,
            session_id,
            output_generation,
            LATE_TERMINATE_EXECUTION_ERROR_CODE,
            String::from("dropping TerminateExecution after execution completed"),
        ),
        SessionMessage::InjectGlobals { .. } | SessionMessage::Execute { .. } => {}
    }
}

#[cfg(not(test))]
fn install_wasm_module_bytes_global<'s>(scope: &mut v8::HandleScope<'s>, bytes: &[u8]) -> bool {
    let global = scope.get_current_context().global(scope);
    let Some(name) = v8::String::new(scope, "__agentOSWasmModuleBytes") else {
        return false;
    };
    let len = bytes.len();
    let backing_store = v8::ArrayBuffer::new_backing_store_from_bytes(bytes.to_vec());
    let array_buffer = v8::ArrayBuffer::with_backing_store(scope, &backing_store.make_shared());
    let Some(bytes_value) = v8::Uint8Array::new(scope, array_buffer, 0, len) else {
        return false;
    };
    global.set(scope, name.into(), bytes_value.into()).is_some()
}

/// Session thread: acquires a concurrency slot, defers V8 isolate creation
/// to first Execute (when bridge code is known for snapshot lookup), and
/// processes commands until shutdown.
fn spawn_session_thread(assignment: SessionAssignment) -> std::io::Result<thread::JoinHandle<()>> {
    let name_prefix = if assignment.session_id.len() > 8 {
        assignment.session_id[..8].to_string()
    } else {
        assignment.session_id.clone()
    };
    // AGENTOS_THREAD_SITE: admitted-v8-session-executor
    thread::Builder::new()
        .name(format!("session-{}", name_prefix))
        .spawn(move || session_thread(assignment, None))
}

fn recv_session_command(
    rx: &Receiver<SessionCommand>,
    shutdown_rx: &Receiver<()>,
    ready_rx: &Receiver<ReadyWake>,
    ready_broker: &SessionReadiness,
) -> Option<SessionCommand> {
    loop {
        // Commands already admitted before a readiness publication establish
        // the V8 context that consumes that readiness. In particular, a kill
        // can publish a signal immediately after Execute is queued. Selecting
        // the later signal first would enter the pre-execution discard branch
        // below and lose the default termination. Preserve admission order
        // before falling back to the fair blocking selector.
        match shutdown_rx.try_recv() {
            Ok(()) | Err(crossbeam_channel::TryRecvError::Disconnected) => {
                return Some(SessionCommand::Shutdown);
            }
            Err(crossbeam_channel::TryRecvError::Empty) => {}
        }
        match rx.try_recv() {
            Ok(command) => return Some(command),
            Err(crossbeam_channel::TryRecvError::Disconnected) => return None,
            Err(crossbeam_channel::TryRecvError::Empty) => {}
        }
        match ready_rx.try_recv() {
            Ok(wake) => match ready_batch_command(ready_broker, wake) {
                Ok(command) => return Some(command),
                Err(error) => {
                    eprintln!("{error}");
                    continue;
                }
            },
            Err(crossbeam_channel::TryRecvError::Disconnected) => return None,
            Err(crossbeam_channel::TryRecvError::Empty) => {}
        }

        crossbeam_channel::select! {
            recv(shutdown_rx) -> _ => return Some(SessionCommand::Shutdown),
            recv(ready_rx) -> wake => {
                let wake = wake.ok()?;
                match ready_batch_command(ready_broker, wake) {
                    Ok(command) => return Some(command),
                    Err(error) => {
                        eprintln!("{error}");
                        continue;
                    }
                }
            },
            recv(rx) -> command => return command.ok(),
        }
    }
}

/// Every consumed wake must become a command, including control-only and
/// currently-empty batches. Dispatching the batch is what completes the
/// broker epoch; dropping it would strand the capacity-one wake lane forever.
fn ready_batch_command(
    ready_broker: &SessionReadiness,
    wake: ReadyWake,
) -> Result<SessionCommand, String> {
    ready_broker
        .take_batch(wake)
        .map(SessionCommand::ReadyBatch)
}

#[allow(clippy::too_many_arguments)]
fn session_thread(
    assignment: SessionAssignment,
    #[cfg_attr(test, allow(unused_variables))] precreated_isolate: Option<PrecreatedIsolate>,
) {
    let SessionAssignment {
        heap_limit_mb,
        cpu_time_limit_ms,
        wall_clock_limit_ms,
        rx,
        shutdown_rx,
        ready_rx,
        ready_broker,
        slot_permit: _slot_permit,
        event_tx,
        call_id_router,
        shared_call_id,
        snapshot_cache,
        isolate_handle,
        execution_abort,
        execution_active,
        pause_control,
        session_id,
        output_generation,
        runtime,
        bridge_call_timeout,
    } = assignment;
    #[cfg(not(test))]
    let execution_task_owner =
        output_generation.map(|generation| agentos_runtime::TaskOwner::Vm { generation });
    #[cfg(test)]
    let _ = (
        heap_limit_mb,
        cpu_time_limit_ms,
        wall_clock_limit_ms,
        call_id_router,
        shared_call_id,
        snapshot_cache,
        isolate_handle,
        execution_abort,
        &pause_control,
        &ready_broker,
        &runtime,
        bridge_call_timeout,
    );

    // Capture THIS session thread's per-thread CPU clock once. The clock id is
    // stable for the thread's lifetime and can be polled from the watchdog
    // thread; this is what lets the CPU-budget guard measure active JS CPU time
    // (excluding idle/await) without running on the execution thread itself.
    // Guest JS always runs on this thread, so this clock is the execution clock.
    #[cfg(all(not(test), unix))]
    let exec_thread_cpu_clock = crate::timeout::current_thread_cpu_clock();
    #[cfg(all(not(test), not(unix)))]
    let exec_thread_cpu_clock: Option<crate::timeout::ThreadCpuClock> = None;

    // Isolate creation is normally deferred to first Execute (when bridge code is
    // known for snapshot cache lookup). A claimed warm worker enters here with
    // the snapshot isolate already created on this same thread.
    #[cfg(not(test))]
    let (
        mut v8_isolate,
        mut _v8_context,
        mut from_snapshot,
        mut isolate_bridge_code,
        mut isolate_userland_code,
    ) = match precreated_isolate {
        Some(mut precreated) => (
            precreated.isolate.take(),
            precreated.context.take(),
            true,
            Some(std::mem::take(&mut precreated.bridge_code)),
            Some(std::mem::take(&mut precreated.userland_code)),
        ),
        None => (None, None, false, None, None),
    };

    #[cfg(not(test))]
    let mut pending = bridge::PendingPromises::new();

    // Store latest InjectGlobals V8 payload for re-injection into fresh contexts
    #[cfg(not(test))]
    let mut last_globals_payload: Option<Vec<u8>> = None;

    // Bridge code cache for V8 code caching across executions
    #[cfg(not(test))]
    let mut bridge_cache: Option<execution::BridgeCodeCache> = None;

    // Cached bridge code string to skip resending over IPC
    #[cfg(not(test))]
    let mut last_bridge_code: Option<String> = None;
    // Cached agent-SDK userland bundle (same 0-length = use cached convention).
    #[cfg(not(test))]
    let mut last_userland_code: Option<String> = None;

    // A session can reuse its isolate across Executes only while the effective
    // bridge code stays the same. Fresh contexts cloned from a snapshot inherit
    // the snapshot's bridge IIFE, so a bridge-code change must rebuild the
    // isolate before the next execution or the session will keep restoring the
    // old snapshot forever. The userland bundle is part of the same guard.
    #[cfg(not(test))]
    let mut high_resolution_time_origin = Instant::now();

    #[cfg(not(test))]
    if let Some(iso) = v8_isolate.as_mut() {
        *isolate_handle
            .lock()
            .expect("session isolate handle lock poisoned") = Some(iso.thread_safe_handle());
    }

    // Process commands until shutdown or channel close
    loop {
        let next_command = recv_session_command(&rx, &shutdown_rx, &ready_rx, &ready_broker);

        pause_control.wait_while_paused();
        match next_command {
            Some(SessionCommand::Shutdown) | None => break,
            Some(SessionCommand::ReadyBatch(batch)) => {
                if batch.signals_ready {
                    if let Err(error) = ready_broker.drain_signals(&batch) {
                        eprintln!("ERR_AGENTOS_READY_DISCARD: could not discard signals: {error}");
                    }
                }
                if batch.timers_ready {
                    if let Err(error) = ready_broker.drain_timers(&batch) {
                        eprintln!("ERR_AGENTOS_READY_DISCARD: could not discard timers: {error}");
                    }
                }
                if let Err(error) = ready_broker.complete_batch(&batch, &batch.entries) {
                    eprintln!("{error}");
                }
            }
            Some(SessionCommand::SetModuleReader(reader)) => {
                execution::install_session_guest_reader(Some(reader));
            }
            Some(SessionCommand::Message(msg)) => match msg {
                SessionMessage::InjectGlobals { payload } => {
                    #[cfg(not(test))]
                    {
                        // Store V8-serialized config for injection into fresh context at Execute time
                        last_globals_payload = Some(payload);
                    }
                    #[cfg(test)]
                    {
                        let _ = payload;
                    }
                }
                SessionMessage::Execute {
                    mode,
                    file_path,
                    bridge_code,
                    post_restore_script,
                    userland_code,
                    high_resolution_time,
                    user_code,
                    wasm_module_bytes,
                } => {
                    let _execution_activity = ExecutionActivityGuard(Arc::clone(&execution_active));
                    // `userland_code` is consumed only by the non-test snapshot
                    // path below; keep it bound (without a warning) under `test`.
                    #[cfg(test)]
                    let _ = &userland_code;
                    #[cfg(test)]
                    let _ = high_resolution_time;
                    #[cfg(test)]
                    let _ = &wasm_module_bytes;
                    #[cfg(not(test))]
                    {
                        let session_id = session_id.clone();
                        // Use cached bridge code when host sends empty (0-length = use cached)
                        let should_update_cached_bridge_code = !bridge_code.is_empty();
                        let effective_bridge_code = if bridge_code.is_empty() {
                            last_bridge_code.as_deref().unwrap_or("").to_string()
                        } else {
                            bridge_code
                        };
                        // Same 0-length = use-cached convention for the userland bundle.
                        let should_update_cached_userland_code = !userland_code.is_empty();
                        let effective_userland_code = if userland_code.is_empty() {
                            last_userland_code.as_deref().unwrap_or("").to_string()
                        } else {
                            userland_code
                        };

                        if let Err(message) =
                            snapshot::validate_bridge_code_size(&effective_bridge_code)
                        {
                            let result_frame = RuntimeEvent::ExecutionResult {
                                session_id,
                                exit_code: 1,
                                exports: None,
                                error: Some(ExecutionErrorBin {
                                    error_type: "Error".into(),
                                    message,
                                    stack: String::new(),
                                    code: snapshot::V8_BRIDGE_CODE_LIMIT_ERROR_CODE.into(),
                                }),
                            };
                            send_event_with_generation(&event_tx, output_generation, result_frame);
                            continue;
                        }

                        if should_update_cached_bridge_code {
                            last_bridge_code = Some(effective_bridge_code.clone());
                        }
                        if should_update_cached_userland_code {
                            last_userland_code = Some(effective_userland_code.clone());
                        }

                        if v8_isolate.is_some()
                            && (isolate_bridge_code.as_deref()
                                != Some(effective_bridge_code.as_str())
                                || isolate_userland_code.as_deref()
                                    != Some(effective_userland_code.as_str()))
                        {
                            *isolate_handle
                                .lock()
                                .expect("session isolate handle lock poisoned") = None;
                            // Reset pending promise-resolver Globals BEFORE this
                            // isolate is dropped. The registry is reused across
                            // isolate rebuilds, and a prior execution that was
                            // terminated early (Shutdown / timeout-abort) can
                            // leave resolvers registered, so they would otherwise
                            // outlive the isolate that created them.
                            reset_pending_promises(&mut pending);
                            drop(_v8_context.take());
                            isolate::drop_isolate(v8_isolate.take());
                            from_snapshot = false;
                            isolate_bridge_code = None;
                            isolate_userland_code = None;
                        }

                        // Deferred isolate creation: create on first Execute using snapshot cache
                        if v8_isolate.is_none() {
                            isolate::init_v8_platform();
                            // The snapshot captures the bridge AND (when present) the
                            // agent-SDK userland bundle, keyed process-wide by both, so
                            // the SDK is evaluated once per sidecar and reused here.
                            let phase_start = Instant::now();
                            let snapshot_blob = match snapshot_cache.get_or_create_with_userland(
                                &effective_bridge_code,
                                (!effective_userland_code.is_empty())
                                    .then_some(effective_userland_code.as_str()),
                            ) {
                                Ok(blob) => Some(blob),
                                Err(message) => {
                                    // Snapshot creation runs in a helper subprocess; if
                                    // that fails (unsupported platform, spawn failure),
                                    // degrade to a fresh isolate that evaluates the
                                    // bridge in-context rather than failing the session.
                                    eprintln!(
                                        "agentos-v8-runtime: snapshot creation failed, \
                                         falling back to fresh isolate: {message}"
                                    );
                                    None
                                }
                            };
                            record_v8_session_phase("snapshot_get", phase_start.elapsed());
                            let mut iso = match snapshot_blob {
                                Some(blob) => {
                                    from_snapshot = true;
                                    eprintln!(
                                        "agentos-v8-runtime: restored session isolate from_snapshot=true"
                                    );
                                    let phase_start = Instant::now();
                                    // rusty_v8 0.130's CreateParams::snapshot_blob
                                    // takes owned 'static data, so this copy remains
                                    // per exec until the API can accept cached bytes.
                                    let snapshot_blob = (*blob).clone();
                                    record_v8_session_phase("blob_clone", phase_start.elapsed());
                                    let phase_start = Instant::now();
                                    let isolate = snapshot::create_isolate_from_snapshot(
                                        snapshot_blob,
                                        heap_limit_mb,
                                    );
                                    record_v8_session_phase("isolate_new", phase_start.elapsed());
                                    isolate
                                }
                                None => {
                                    from_snapshot = false;
                                    let phase_start = Instant::now();
                                    let isolate = isolate::create_isolate(heap_limit_mb);
                                    record_v8_session_phase("isolate_new", phase_start.elapsed());
                                    isolate
                                }
                            };
                            iso.set_host_import_module_dynamically_callback(
                                execution::dynamic_import_callback,
                            );
                            iso.set_host_initialize_import_meta_object_callback(
                                execution::import_meta_object_callback,
                            );
                            high_resolution_time_origin = Instant::now();
                            *isolate_handle
                                .lock()
                                .expect("session isolate handle lock poisoned") =
                                Some(iso.thread_safe_handle());
                            let ctx = isolate::create_context(&mut iso);
                            _v8_context = Some(ctx);
                            v8_isolate = Some(iso);
                            isolate_bridge_code = Some(effective_bridge_code.clone());
                            isolate_userland_code = Some(effective_userland_code.clone());
                        }

                        let iso = v8_isolate.as_mut().unwrap();
                        iso.cancel_terminate_execution();

                        // Language executions set bit 1 to reuse the session's
                        // process-lifetime context. Normal process executions keep
                        // receiving a fresh context for every Execute message.
                        let retain_context = mode & 2 != 0;
                        let module_mode = mode & 1 != 0;
                        let exec_context = if retain_context {
                            _v8_context
                                .as_ref()
                                .expect("session context exists after isolate creation")
                                .clone()
                        } else {
                            isolate::create_context(iso)
                        };

                        if high_resolution_time {
                            let scope = &mut v8::HandleScope::new(iso);
                            let ctx = v8::Local::new(scope, &exec_context);
                            let scope = &mut v8::ContextScope::new(scope, ctx);
                            execution::install_high_resolution_time_global(
                                scope,
                                &high_resolution_time_origin as *const Instant,
                            );
                        }

                        // Inject globals from last InjectGlobals payload
                        if let Some(ref payload) = last_globals_payload {
                            let scope = &mut v8::HandleScope::new(iso);
                            let ctx = v8::Local::new(scope, &exec_context);
                            let scope = &mut v8::ContextScope::new(scope, ctx);
                            if let Err(error) =
                                execution::inject_globals_from_payload(scope, payload)
                            {
                                let result_frame = RuntimeEvent::ExecutionResult {
                                    session_id,
                                    exit_code: 1,
                                    exports: None,
                                    error: Some(ExecutionErrorBin {
                                        error_type: error.error_type,
                                        message: error.message,
                                        stack: error.stack,
                                        code: error.code.unwrap_or_default(),
                                    }),
                                };
                                send_event_with_generation(
                                    &event_tx,
                                    output_generation,
                                    result_frame,
                                );
                                continue;
                            }
                        }

                        // Arm a per-execution abort channel so timeouts and external
                        // terminate requests can unblock sync bridge waits.
                        let (_active_execution_abort, abort_rx) =
                            ActiveExecutionAbort::arm(&execution_abort);
                        if execution_abort_requested(&abort_rx) {
                            // Termination may arrive after Execute admission but
                            // before isolate startup completes. Finish without
                            // entering guest code; there is no running isolate
                            // frame for terminate_execution() to interrupt yet.
                            send_event_with_generation(
                                &event_tx,
                                output_generation,
                                RuntimeEvent::ExecutionResult {
                                    session_id,
                                    exit_code: 1,
                                    exports: None,
                                    error: Some(ExecutionErrorBin {
                                        error_type: String::from("Error"),
                                        message: String::from("Execution terminated"),
                                        stack: String::new(),
                                        code: String::new(),
                                    }),
                                },
                            );
                            continue;
                        }

                        // Async completions have a dedicated bounded lane.
                        // Synchronous calls register their own capacity-one
                        // waiter in the same call-specific registry.
                        let (async_response_tx, async_response_rx) =
                            crossbeam_channel::bounded(bridge::MAX_PENDING_PROMISES);
                        let bridge_ctx = BridgeCallContext::with_registry(
                            Box::new(ChannelRuntimeEventSender::new(
                                event_tx.clone(),
                                output_generation,
                            )),
                            session_id.clone(),
                            output_generation,
                            Arc::clone(&call_id_router),
                            Arc::clone(&shared_call_id),
                            async_response_tx,
                            abort_rx.clone(),
                            runtime.clone(),
                            Arc::clone(&pause_control),
                            bridge_call_timeout,
                        );

                        // Replace stub bridge functions with real session-local ones
                        // (on snapshot context) or register from scratch (on fresh context).
                        // Both paths use the same function — global.set() works for both.
                        let _sync_store;
                        let _async_store;
                        let sync_bridge_fns = sync_bridge_fns();
                        let async_bridge_fns = async_bridge_fns();
                        {
                            let scope = &mut v8::HandleScope::new(iso);
                            let ctx = v8::Local::new(scope, &exec_context);
                            let scope = &mut v8::ContextScope::new(scope, ctx);

                            (_sync_store, _async_store) = bridge::replace_bridge_fns(
                                scope,
                                &bridge_ctx as *const BridgeCallContext,
                                &pending as *const bridge::PendingPromises,
                                sync_bridge_fns,
                                async_bridge_fns,
                            );
                        }

                        // Run post-restore init script (config, mutable state reset)
                        // after bridge fn replacement but before user code
                        if !post_restore_script.is_empty() {
                            let scope = &mut v8::HandleScope::new(iso);
                            let ctx = v8::Local::new(scope, &exec_context);
                            let scope = &mut v8::ContextScope::new(scope, ctx);
                            let (prs_code, prs_err) =
                                execution::run_init_script(scope, &post_restore_script);
                            if prs_code != 0 {
                                let result_frame = RuntimeEvent::ExecutionResult {
                                    session_id,
                                    exit_code: prs_code,
                                    exports: None,
                                    error: prs_err.map(|e| ExecutionErrorBin {
                                        error_type: e.error_type,
                                        message: e.message,
                                        stack: e.stack,
                                        code: e.code.unwrap_or_default(),
                                    }),
                                };
                                send_event_with_generation(
                                    &event_tx,
                                    output_generation,
                                    result_frame,
                                );
                                continue;
                            }
                        }

                        if let Some(wasm_module_bytes) = wasm_module_bytes.as_ref() {
                            let scope = &mut v8::HandleScope::new(iso);
                            let ctx = v8::Local::new(scope, &exec_context);
                            let scope = &mut v8::ContextScope::new(scope, ctx);
                            if !install_wasm_module_bytes_global(scope, wasm_module_bytes) {
                                let result_frame = RuntimeEvent::ExecutionResult {
                                    session_id,
                                    exit_code: 1,
                                    exports: None,
                                    error: Some(ExecutionErrorBin {
                                        error_type: "Error".into(),
                                        message: "failed to install __agentOSWasmModuleBytes"
                                            .into(),
                                        stack: String::new(),
                                        code: String::new(),
                                    }),
                                };
                                send_event_with_generation(
                                    &event_tx,
                                    output_generation,
                                    result_frame,
                                );
                                continue;
                            }
                        }

                        // Arm the TRUE CPU-TIME budget watchdog before running
                        // guest code when the caller passes a nonzero
                        // `limits.jsRuntime.cpuTimeLimitMs` (normalized: `0`/unset =>
                        // `None` => not armed at this runtime layer). The sidecar
                        // supplies the bounded default for VM executions.
                        //
                        // The watchdog counts ACTIVE JS CPU only (idle/await
                        // excluded) by polling the execution thread's CPU clock, so
                        // a guest that mostly awaits is NOT killed by it. The
                        // INDEPENDENT wall-clock backstop (armed just below) covers
                        // the idle/await case when the operator opts into it.
                        let mut cpu_budget_guard = match cpu_time_limit_ms {
                            Some(budget_ms) => {
                                // Enforcing a CPU budget requires the execution
                                // thread's CPU clock captured at session start. If
                                // it is unavailable we cannot honor the operator's
                                // requested cap — surface that rather than silently
                                // running uncapped.
                                let cpu_clock = match exec_thread_cpu_clock {
                                    Some(clock) => clock,
                                    None => {
                                        let result_frame = RuntimeEvent::ExecutionResult {
                                            session_id,
                                            exit_code: 1,
                                            exports: None,
                                            error: Some(ExecutionErrorBin {
                                                error_type: "Error".into(),
                                                message: format!(
                                                    "{}: per-thread CPU clock unavailable; cannot enforce limits.jsRuntime.cpuTimeLimitMs",
                                                    crate::timeout::CPU_BUDGET_GUARD_START_ERROR_CODE
                                                ),
                                                stack: String::new(),
                                                code: crate::timeout::CPU_BUDGET_GUARD_START_ERROR_CODE
                                                    .into(),
                                            }),
                                        };
                                        send_event_with_generation(
                                            &event_tx,
                                            output_generation,
                                            result_frame,
                                        );
                                        continue;
                                    }
                                };
                                let handle = iso.thread_safe_handle();
                                match crate::timeout::CpuBudgetGuard::new(
                                    &runtime,
                                    execution_task_owner.clone(),
                                    budget_ms,
                                    cpu_clock,
                                    handle,
                                    execution_abort.clone(),
                                ) {
                                    Ok(guard) => Some(guard),
                                    Err(message) => {
                                        let result_frame = RuntimeEvent::ExecutionResult {
                                            session_id,
                                            exit_code: 1,
                                            exports: None,
                                            error: Some(ExecutionErrorBin {
                                                error_type: "Error".into(),
                                                message,
                                                stack: String::new(),
                                                code:
                                                    crate::timeout::CPU_BUDGET_GUARD_START_ERROR_CODE
                                                        .into(),
                                            }),
                                        };
                                        send_event_with_generation(
                                            &event_tx,
                                            output_generation,
                                            result_frame,
                                        );
                                        continue;
                                    }
                                }
                            }
                            _ => None,
                        };

                        // Arm the INDEPENDENT, opt-in WALL-CLOCK backstop alongside
                        // the CPU budget. Unlike the CPU budget, this counts elapsed
                        // real time INCLUDING idle/await, so it can cap a guest that
                        // blocks or awaits indefinitely. Armed only when the operator
                        // opts in via `limits.jsRuntime.wallClockLimitMs` (normalized:
                        // `0`/unset => `None` => not armed => NO wall-clock limit, so
                        // long-lived ACP adapters are never killed by a default).
                        // Whichever guard fires first calls `terminate_execution` and
                        // records its abort reason; the result frame reports which.
                        let mut wall_clock_guard = match wall_clock_limit_ms {
                            Some(limit_ms) => {
                                let handle = iso.thread_safe_handle();
                                match crate::timeout::TimeoutGuard::with_execution_abort(
                                    &runtime,
                                    execution_task_owner.clone(),
                                    limit_ms,
                                    handle,
                                    execution_abort.clone(),
                                ) {
                                    Ok(guard) => Some(guard),
                                    Err(message) => {
                                        let result_frame = RuntimeEvent::ExecutionResult {
                                            session_id,
                                            exit_code: 1,
                                            exports: None,
                                            error: Some(ExecutionErrorBin {
                                                error_type: "Error".into(),
                                                message,
                                                stack: String::new(),
                                                code:
                                                    crate::timeout::TIMEOUT_GUARD_START_ERROR_CODE
                                                        .into(),
                                            }),
                                        };
                                        send_event_with_generation(
                                            &event_tx,
                                            output_generation,
                                            result_frame,
                                        );
                                        continue;
                                    }
                                }
                            }
                            _ => None,
                        };

                        // On snapshot-restored context, skip bridge IIFE (already in
                        // snapshot) and run user code only. On fresh context, run full
                        // bridge code + user code as before.
                        let bridge_code_for_exec = if from_snapshot {
                            ""
                        } else {
                            &effective_bridge_code
                        };
                        let file_path_opt = if file_path.is_empty() {
                            None
                        } else {
                            Some(file_path.as_str())
                        };
                        let phase_start = Instant::now();
                        let (mut code, mut exports, mut error) = if !module_mode {
                            let scope = &mut v8::HandleScope::new(iso);
                            let ctx = v8::Local::new(scope, &exec_context);
                            let scope = &mut v8::ContextScope::new(scope, ctx);
                            let (c, e) = execution::execute_script_with_options(
                                scope,
                                Some(&bridge_ctx),
                                bridge_code_for_exec,
                                &user_code,
                                file_path_opt,
                                &mut bridge_cache,
                            );
                            (c, None, e)
                        } else {
                            let scope = &mut v8::HandleScope::new(iso);
                            let ctx = v8::Local::new(scope, &exec_context);
                            let scope = &mut v8::ContextScope::new(scope, ctx);
                            execution::execute_module(
                                scope,
                                &bridge_ctx,
                                bridge_code_for_exec,
                                &user_code,
                                file_path_opt,
                                &mut bridge_cache,
                            )
                        };

                        // Re-check async ESM completion once immediately so
                        // pure-microtask top-level await settles without
                        // needing a bridge event-loop round-trip.
                        if module_mode && error.is_none() {
                            let scope = &mut v8::HandleScope::new(iso);
                            let ctx = v8::Local::new(scope, &exec_context);
                            let scope = &mut v8::ContextScope::new(scope, ctx);
                            if let Some((next_code, next_exports, next_error)) =
                                execution::finalize_pending_module_evaluation(scope)
                            {
                                code = next_code;
                                exports = next_exports;
                                error = next_error;
                            }
                        }
                        record_v8_session_phase("user_code_execute", phase_start.elapsed());

                        // Run event loop while bridge work or async ESM
                        // evaluation is still pending. For ESM modules (mode != 0),
                        // always enter the event loop even if no pending promises
                        // are visible yet — the module body may have registered
                        // timers, stdin listeners, or child_process handles that
                        // need event loop pumping to deliver their callbacks.
                        let should_enter_event_loop = !pending.is_empty()
                            || execution::has_pending_module_evaluation()
                            || execution::has_pending_script_evaluation();
                        let event_loop_status = if should_enter_event_loop {
                            let scope = &mut v8::HandleScope::new(iso);
                            let ctx = v8::Local::new(scope, &exec_context);
                            let scope = &mut v8::ContextScope::new(scope, ctx);
                            run_event_loop_with_readiness(
                                scope,
                                EventLoopSources {
                                    commands: &rx,
                                    readiness: &ready_broker,
                                    readiness_wakes: &ready_rx,
                                    bridge_responses: Some(&async_response_rx),
                                    abort: Some(&abort_rx),
                                    pause: Some(&pause_control),
                                },
                                &pending,
                            )
                        } else {
                            EventLoopStatus::Completed
                        };

                        let mut terminated =
                            matches!(event_loop_status, EventLoopStatus::Terminated);
                        if let EventLoopStatus::Failed(next_code, next_error) = event_loop_status {
                            code = next_code;
                            error = Some(next_error);
                        }

                        // Finalize any entry-module top-level await that was
                        // waiting on bridge-driven async work (timers/network).
                        if !terminated && module_mode && error.is_none() {
                            let scope = &mut v8::HandleScope::new(iso);
                            let ctx = v8::Local::new(scope, &exec_context);
                            let scope = &mut v8::ContextScope::new(scope, ctx);
                            if let Some((next_code, next_exports, next_error)) =
                                execution::finalize_pending_module_evaluation(scope)
                            {
                                code = next_code;
                                exports = next_exports;
                                error = next_error;
                            }
                        }

                        // Keep the session alive while handles (timers, child
                        // processes, stdin listeners) are active. Long-lived
                        // ACP adapters often run as plain scripts, so this
                        // cannot be limited to ESM entrypoints.
                        if !terminated && error.is_none() {
                            // Destruction can race with the short gap before the
                            // active-handle pass. Observe its durable abort before
                            // calling into an isolate another thread terminated.
                            if execution_abort_requested(&abort_rx) {
                                terminated = true;
                            } else {
                                // Phase 1: call _waitForActiveHandles() once. Repeating
                                // this after it resolves can re-capture an idle HTTP
                                // keep-alive socket and prevent an otherwise-complete
                                // one-shot script from ever exiting.
                                {
                                    let scope = &mut v8::HandleScope::new(iso);
                                    let ctx = v8::Local::new(scope, &exec_context);
                                    let scope = &mut v8::ContextScope::new(scope, ctx);
                                    let global = ctx.global(scope);
                                    let key =
                                        v8::String::new(scope, "_waitForActiveHandles").unwrap();
                                    if let Some(func) = global.get(scope, key.into()) {
                                        if func.is_function() {
                                            let func =
                                                v8::Local::<v8::Function>::try_from(func).unwrap();
                                            let recv = v8::undefined(scope).into();
                                            if let Some(result) = func.call(scope, recv, &[]) {
                                                if result.is_promise() {
                                                    let promise =
                                                        v8::Local::<v8::Promise>::try_from(result)
                                                            .unwrap();
                                                    if promise.state() == v8::PromiseState::Pending
                                                    {
                                                        execution::set_pending_script_evaluation(
                                                            scope, promise,
                                                        );
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }

                                // Phase 2: pump the event loop for that quiescence wait.
                                if !pending.is_empty() || execution::has_pending_script_evaluation()
                                {
                                    let scope = &mut v8::HandleScope::new(iso);
                                    let ctx = v8::Local::new(scope, &exec_context);
                                    let scope = &mut v8::ContextScope::new(scope, ctx);
                                    let event_loop_status = run_event_loop_with_readiness(
                                        scope,
                                        EventLoopSources {
                                            commands: &rx,
                                            readiness: &ready_broker,
                                            readiness_wakes: &ready_rx,
                                            bridge_responses: Some(&async_response_rx),
                                            abort: Some(&abort_rx),
                                            pause: Some(&pause_control),
                                        },
                                        &pending,
                                    );

                                    if matches!(event_loop_status, EventLoopStatus::Terminated) {
                                        terminated = true;
                                    }
                                    if let EventLoopStatus::Failed(next_code, next_error) =
                                        event_loop_status
                                    {
                                        code = next_code;
                                        error = Some(next_error);
                                    }
                                }
                            }
                        }

                        if !terminated && !module_mode && error.is_none() {
                            let scope = &mut v8::HandleScope::new(iso);
                            let ctx = v8::Local::new(scope, &exec_context);
                            let scope = &mut v8::ContextScope::new(scope, ctx);
                            if let Some((next_code, next_error)) =
                                execution::finalize_pending_script_evaluation(scope)
                            {
                                code = next_code;
                                error = next_error;
                            }
                        }

                        // Async callbacks may assign process.exitCode after the entry script's
                        // initial synchronous completion. Re-read it after all active handles
                        // and pending script evaluation have drained so spawn() and exec()
                        // report the same final Node process status.
                        if !terminated && error.is_none() {
                            let scope = &mut v8::HandleScope::new(iso);
                            let ctx = v8::Local::new(scope, &exec_context);
                            let scope = &mut v8::ContextScope::new(scope, ctx);
                            if let Some(process_exit_code) =
                                execution::extract_global_process_exit_code(scope)
                            {
                                code = process_exit_code;
                            }
                        }

                        // Determine which execution budget (if any) fired. Both the
                        // CPU-time budget and the wall-clock backstop can be armed;
                        // whichever fired first recorded its abort reason. Prefer the
                        // recorded abort reason (first-writer-wins) so the result
                        // attributes termination to the guard that actually fired.
                        let abort_reason = execution_abort_reason(&execution_abort);
                        let wall_clock_timed_out =
                            wall_clock_guard.as_ref().is_some_and(|g| g.timed_out())
                                || matches!(
                                    abort_reason,
                                    Some(ExecutionAbortReason::WallClockTimedOut)
                                );
                        let cpu_budget_exceeded =
                            cpu_budget_guard.as_ref().is_some_and(|g| g.exceeded())
                                || matches!(
                                    abort_reason,
                                    Some(ExecutionAbortReason::CpuBudgetExceeded)
                                );
                        // If both happened to fire, the recorded abort reason is the
                        // authoritative first-fired guard; fall back to wall-clock
                        // only when no CPU-budget reason was recorded.
                        let cpu_budget_exceeded = cpu_budget_exceeded
                            && !matches!(
                                abort_reason,
                                Some(ExecutionAbortReason::WallClockTimedOut)
                            );
                        let wall_clock_timed_out = wall_clock_timed_out && !cpu_budget_exceeded;

                        // Cancel both watchdogs (joins their threads).
                        if let Some(ref mut guard) = cpu_budget_guard {
                            guard.cancel();
                        }
                        drop(cpu_budget_guard);
                        if let Some(ref mut guard) = wall_clock_guard {
                            guard.cancel();
                        }
                        drop(wall_clock_guard);

                        if matches!(abort_reason, Some(ExecutionAbortReason::Terminated)) {
                            terminated = true;
                            code = 1;
                            exports = None;
                            error = None;
                        }
                        if terminated || cpu_budget_exceeded || wall_clock_timed_out {
                            iso.cancel_terminate_execution();
                        }

                        // Send ExecutionResult
                        let result_frame = if cpu_budget_exceeded {
                            if let Some(budget_ms) = cpu_time_limit_ms {
                                let capacity = budget_ms as usize;
                                warn_limit_exhausted(TrackedLimit::V8CpuTimeMs, capacity, capacity);
                            }
                            RuntimeEvent::ExecutionResult {
                                session_id,
                                exit_code: 1,
                                exports: None,
                                error: Some(ExecutionErrorBin {
                                    error_type: "Error".into(),
                                    message: "Script execution exceeded the CPU-time budget \
                                         (limits.jsRuntime.cpuTimeLimitMs)"
                                        .into(),
                                    stack: String::new(),
                                    code: "ERR_SCRIPT_CPU_BUDGET_EXCEEDED".into(),
                                }),
                            }
                        } else if wall_clock_timed_out {
                            if let Some(limit_ms) = wall_clock_limit_ms {
                                let capacity = limit_ms as usize;
                                warn_limit_exhausted(
                                    TrackedLimit::V8WallClockMs,
                                    capacity,
                                    capacity,
                                );
                            }
                            RuntimeEvent::ExecutionResult {
                                session_id,
                                exit_code: 1,
                                exports: None,
                                error: Some(ExecutionErrorBin {
                                    error_type: "Error".into(),
                                    message: "Script execution exceeded the wall-clock limit \
                                         (limits.jsRuntime.wallClockLimitMs)"
                                        .into(),
                                    stack: String::new(),
                                    code: "ERR_SCRIPT_WALL_CLOCK_EXCEEDED".into(),
                                }),
                            }
                        } else if terminated {
                            RuntimeEvent::ExecutionResult {
                                session_id,
                                exit_code: 1,
                                exports: None,
                                error: Some(ExecutionErrorBin {
                                    error_type: "Error".into(),
                                    message: "Execution terminated".into(),
                                    stack: String::new(),
                                    code: String::new(),
                                }),
                            }
                        } else {
                            RuntimeEvent::ExecutionResult {
                                session_id,
                                exit_code: code,
                                exports,
                                error: error.map(|e| ExecutionErrorBin {
                                    error_type: e.error_type,
                                    message: e.message,
                                    stack: e.stack,
                                    code: e.code.unwrap_or_default(),
                                }),
                            }
                        };

                        execution::clear_pending_module_evaluation();
                        execution::clear_pending_script_evaluation();
                        execution::clear_module_state();

                        send_event_with_generation(&event_tx, output_generation, result_frame);
                    }
                    #[cfg(test)]
                    {
                        let _ = (mode, file_path, bridge_code, post_restore_script, user_code);
                    }
                }
                SessionMessage::BridgeResponse(_)
                | SessionMessage::StreamEvent(_)
                | SessionMessage::TerminateExecution => {
                    handle_late_session_message(&event_tx, &session_id, output_generation, msg);
                }
            },
        }
    }

    // Drop V8 resources (only present in non-test mode)
    #[cfg(not(test))]
    {
        *isolate_handle
            .lock()
            .expect("session isolate handle lock poisoned") = None;
        // Reset pending promise-resolver Globals BEFORE the isolate is dropped on
        // thread teardown. run_event_loop can exit early (Shutdown / timeout-abort)
        // with resolvers still registered, so without this the Globals would drop
        // after their isolate — leaking across session create/destroy churn and
        // violating the V8 lifetime contract.
        reset_pending_promises(&mut pending);
        drop(_v8_context.take());
        isolate::drop_isolate(v8_isolate.take());
    }

    // `_slot_permit` releases only after all thread-affine V8 state above has
    // been destroyed. A detached generation cannot leak its permit early.
}

/// Sync bridge functions block V8 while the host processes the call
/// (applySync/applySyncPromise). Async bridge functions return a Promise to V8.
struct BridgeFnPartitions {
    sync: Vec<&'static str>,
    async_fns: Vec<&'static str>,
}

pub(crate) fn sync_bridge_fns() -> &'static [&'static str] {
    &bridge_fn_partitions().sync
}

pub(crate) fn async_bridge_fns() -> &'static [&'static str] {
    &bridge_fn_partitions().async_fns
}

fn bridge_fn_partitions() -> &'static BridgeFnPartitions {
    static PARTITIONS: OnceLock<BridgeFnPartitions> = OnceLock::new();
    PARTITIONS.get_or_init(|| BridgeFnPartitions {
        sync: bridge_fns_for(|convention| {
            matches!(
                convention,
                BridgeCallConvention::Sync | BridgeCallConvention::SyncPromise
            )
        }),
        async_fns: bridge_fns_for(|convention| convention == BridgeCallConvention::Async),
    })
}

fn bridge_fns_for(filter: impl Fn(BridgeCallConvention) -> bool) -> Vec<&'static str> {
    bridge_contract()
        .groups
        .iter()
        .filter(|group| filter(group.convention))
        .flat_map(|group| group.names.iter().map(String::as_str))
        .collect()
}

/// Reset every pending promise-resolver `v8::Global` handle held by `pending`.
///
/// `v8::Global` handles MUST be reset/dropped *before* the `v8::Isolate` that
/// created them is torn down. The session reuses a single `PendingPromises`
/// registry across executions and across isolate rebuilds, and `run_event_loop`
/// can exit early (Shutdown at the `SessionCommand::Shutdown` arm, or
/// timeout-abort via the `abort_rx` branch) while resolvers are still
/// registered. On those paths the registry can outlive an isolate. Call this
/// immediately before every isolate drop (rebuild and thread teardown) so the
/// `Global<PromiseResolver>` handles are dropped while their isolate is still
/// alive — preventing both a leak across session create/destroy churn (bounded
/// by `MAX_PENDING_PROMISES`) and a V8 lifetime-contract violation.
#[doc(hidden)]
pub fn reset_pending_promises(pending: &mut crate::bridge::PendingPromises) {
    // Swap in an empty registry and drop the populated one in place. Dropping a
    // `PendingPromises` resets all of its `Global<PromiseResolver>` handles.
    drop(std::mem::take(pending));
}

/// Run the session event loop: dispatch incoming messages to V8.
///
/// Called after script/module execution when there are pending async promises.
/// Polls the ordinary session channel for events/control and the dedicated
/// async bridge-response lane, dispatching bounded work into V8.
///
/// When `abort_rx` is provided (timeout is configured), uses `select!` to
/// also monitor the abort channel — if the timeout fires and drops the sender,
/// the abort channel unblocks and terminates execution.
///
/// Returns true if execution completed normally, false if terminated.
#[doc(hidden)]
pub fn run_event_loop(
    scope: &mut v8::HandleScope,
    rx: &Receiver<SessionCommand>,
    pending: &crate::bridge::PendingPromises,
    abort_rx: Option<&crossbeam_channel::Receiver<()>>,
    bridge_rx: Option<&crossbeam_channel::Receiver<BridgeResponse>>,
    pause_control: Option<&SessionPauseControl>,
) -> EventLoopStatus {
    let (ready_broker, ready_rx) = match SessionReadiness::disabled(1) {
        Ok(readiness) => readiness,
        Err(error) => {
            eprintln!("{error}");
            return EventLoopStatus::Terminated;
        }
    };
    run_event_loop_with_readiness(
        scope,
        EventLoopSources {
            commands: rx,
            readiness: &ready_broker,
            readiness_wakes: &ready_rx,
            bridge_responses: bridge_rx,
            abort: abort_rx,
            pause: pause_control,
        },
        pending,
    )
}

struct EventLoopSources<'a> {
    commands: &'a Receiver<SessionCommand>,
    readiness: &'a SessionReadiness,
    readiness_wakes: &'a Receiver<ReadyWake>,
    bridge_responses: Option<&'a Receiver<BridgeResponse>>,
    abort: Option<&'a Receiver<()>>,
    pause: Option<&'a SessionPauseControl>,
}

fn run_event_loop_with_readiness(
    scope: &mut v8::HandleScope,
    sources: EventLoopSources<'_>,
    pending: &crate::bridge::PendingPromises,
) -> EventLoopStatus {
    let EventLoopSources {
        commands: rx,
        readiness: ready_broker,
        readiness_wakes: ready_rx,
        bridge_responses: bridge_rx,
        abort: abort_rx,
        pause: pause_control,
    } = sources;
    let mut bridge_lane_open = bridge_rx.is_some();
    loop {
        // An out-of-band isolate termination may arrive between event-loop
        // passes. Check the durable abort lane before any V8 API call; querying
        // promises or timers on an already-terminated isolate can otherwise
        // spin forever and prevent explicit session destruction from joining.
        if abort_rx.is_some_and(execution_abort_requested) {
            scope.terminate_execution();
            return EventLoopStatus::Terminated;
        }
        if pending.is_empty()
            && !execution::pending_module_evaluation_needs_wait(scope)
            && !execution::pending_script_evaluation_needs_wait(scope)
            && pending_guest_timer_count(scope) == 0
            && pending_guest_immediate_count(scope) == 0
        {
            break;
        }
        if let Some(control) = pause_control {
            control.wait_while_paused();
        }
        pump_v8_message_loop(scope);

        // Bound completion work per turn so a response flood cannot starve
        // ordinary stream/control events.
        if bridge_lane_open {
            let responses = bridge_rx.expect("open bridge lane must have a receiver");
            for _ in 0..64 {
                let response = match responses.try_recv() {
                    Ok(response) => response,
                    Err(crossbeam_channel::TryRecvError::Empty) => break,
                    Err(crossbeam_channel::TryRecvError::Disconnected) => {
                        bridge_lane_open = false;
                        break;
                    }
                };
                let status = dispatch_event_loop_frame(
                    scope,
                    SessionMessage::BridgeResponse(response),
                    pending,
                );
                if !matches!(status, EventLoopStatus::Completed) {
                    return status;
                }
            }
        }

        // Drain one JavaScript turn before blocking. A V8 microtask checkpoint
        // already drains recursively queued Promise continuations; the platform
        // pump likewise drains every currently runnable foreground task and
        // checkpoints after each one. Repeating both operations 100 times added
        // a fixed empty-work floor to every readiness and bridge completion.
        scope.perform_microtask_checkpoint();
        pump_v8_message_loop(scope);

        if pending_guest_immediate_count(scope) > 0 {
            match try_recv_session_command(scope, rx, ready_rx, ready_broker, bridge_rx, abort_rx) {
                Ok(Some(cmd)) => {
                    let status = dispatch_session_command(scope, cmd, pending, ready_broker);
                    if !matches!(status, EventLoopStatus::Completed) {
                        return status;
                    }
                }
                Ok(None) => {
                    let status = drain_guest_immediates(scope);
                    if !matches!(status, EventLoopStatus::Completed) {
                        return status;
                    }
                }
                Err(status) => return status,
            }
            scope.perform_microtask_checkpoint();
            pump_v8_message_loop(scope);
        }

        // Re-check exit conditions after microtask flush — the microtask may
        // have resolved all pending promises or registered new handles.
        if pending.is_empty()
            && !execution::pending_module_evaluation_needs_wait(scope)
            && !execution::pending_script_evaluation_needs_wait(scope)
            && pending_guest_timer_count(scope) == 0
            && pending_guest_immediate_count(scope) == 0
        {
            break;
        }

        // Receive next command with interleaved microtask processing.
        // Instead of blocking indefinitely, use a short timeout so we can
        // periodically flush microtasks (like Node.js's libuv + DrainTasks pattern).
        let cmd = loop {
            if pending_guest_immediate_count(scope) > 0 {
                match try_recv_session_command(
                    scope,
                    rx,
                    ready_rx,
                    ready_broker,
                    bridge_rx,
                    abort_rx,
                ) {
                    Ok(Some(cmd)) => break cmd,
                    Ok(None) => {
                        let status = drain_guest_immediates(scope);
                        if !matches!(status, EventLoopStatus::Completed) {
                            return status;
                        }
                        scope.perform_microtask_checkpoint();
                        pump_v8_message_loop(scope);
                        continue;
                    }
                    Err(status) => return status,
                }
            }
            if bridge_lane_open {
                let responses = bridge_rx.expect("open bridge lane must have a receiver");
                match responses.try_recv() {
                    Ok(response) => {
                        break SessionCommand::Message(SessionMessage::BridgeResponse(response));
                    }
                    Err(crossbeam_channel::TryRecvError::Empty) => {}
                    Err(crossbeam_channel::TryRecvError::Disconnected) => {
                        bridge_lane_open = false;
                    }
                }
            }
            // Preserve admission order between ordinary commands and readiness.
            // Level-triggered socket readiness can immediately rearm after every
            // batch; consuming that wake first on every pass starves stdin and
            // control commands until an idle keep-alive socket expires.
            match rx.try_recv() {
                Ok(command) => break command,
                Err(crossbeam_channel::TryRecvError::Empty) => {}
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    return EventLoopStatus::Completed;
                }
            }
            if let Ok(wake) = ready_rx.try_recv() {
                match ready_batch_command(ready_broker, wake) {
                    Ok(command) => break command,
                    Err(error) => {
                        eprintln!("{error}");
                        continue;
                    }
                }
            }
            // All externally driven work must be registered with the blocking
            // selector. The 1 ms timeout exists only to pump V8 platform work;
            // it must not become the delivery cadence for direct bridge
            // responses, readiness, ordinary commands, or abort.
            let mut selector = Select::new();
            let ordinary_index = selector.recv(rx);
            let ready_index = selector.recv(ready_rx);
            let bridge_selection = if bridge_lane_open {
                bridge_rx.map(|responses| (selector.recv(responses), responses))
            } else {
                None
            };
            let abort_selection = abort_rx.map(|abort| (selector.recv(abort), abort));
            let recv_result = match selector.select_timeout(Duration::from_millis(1)) {
                Ok(operation) => {
                    let index = operation.index();
                    if index == ordinary_index {
                        operation.recv(rx).ok()
                    } else if index == ready_index {
                        match operation.recv(ready_rx) {
                            Ok(wake) => match ready_batch_command(ready_broker, wake) {
                                Ok(command) => Some(command),
                                Err(error) => {
                                    eprintln!("{error}");
                                    None
                                }
                            },
                            Err(_) => None,
                        }
                    } else if let Some((bridge_index, responses)) = bridge_selection {
                        if index == bridge_index {
                            match operation.recv(responses) {
                                Ok(response) => Some(SessionCommand::Message(
                                    SessionMessage::BridgeResponse(response),
                                )),
                                Err(_) => {
                                    bridge_lane_open = false;
                                    None
                                }
                            }
                        } else if let Some((abort_index, abort)) = abort_selection {
                            debug_assert_eq!(index, abort_index);
                            let _ = operation.recv(abort);
                            scope.terminate_execution();
                            return EventLoopStatus::Terminated;
                        } else {
                            unreachable!("event-loop selector returned an unknown operation")
                        }
                    } else if let Some((abort_index, abort)) = abort_selection {
                        debug_assert_eq!(index, abort_index);
                        let _ = operation.recv(abort);
                        scope.terminate_execution();
                        return EventLoopStatus::Terminated;
                    } else {
                        unreachable!("event-loop selector returned an unknown operation")
                    }
                }
                Err(_) => None,
            };
            if let Some(cmd) = recv_result {
                break cmd;
            }
            if let Some(control) = pause_control {
                control.wait_while_paused();
            }
            // No command received — flush microtasks and re-check direct
            // response and exit conditions.
            scope.perform_microtask_checkpoint();
            pump_v8_message_loop(scope);
            // Check if we should exit
            if pending.is_empty()
                && !execution::pending_module_evaluation_needs_wait(scope)
                && !execution::pending_script_evaluation_needs_wait(scope)
                && pending_guest_timer_count(scope) == 0
                && pending_guest_immediate_count(scope) == 0
            {
                return EventLoopStatus::Completed;
            }
        };

        let status = dispatch_session_command(scope, cmd, pending, ready_broker);
        if !matches!(status, EventLoopStatus::Completed) {
            return status;
        }
    }
    EventLoopStatus::Completed
}

fn execution_abort_requested(abort: &crossbeam_channel::Receiver<()>) -> bool {
    !matches!(
        abort.try_recv(),
        Err(crossbeam_channel::TryRecvError::Empty)
    )
}

fn try_recv_session_command(
    scope: &mut v8::HandleScope,
    rx: &Receiver<SessionCommand>,
    ready_rx: &Receiver<ReadyWake>,
    ready_broker: &SessionReadiness,
    bridge_rx: Option<&crossbeam_channel::Receiver<BridgeResponse>>,
    abort_rx: Option<&crossbeam_channel::Receiver<()>>,
) -> Result<Option<SessionCommand>, EventLoopStatus> {
    if let Some(responses) = bridge_rx {
        match responses.try_recv() {
            Ok(response) => {
                return Ok(Some(SessionCommand::Message(
                    SessionMessage::BridgeResponse(response),
                )));
            }
            Err(crossbeam_channel::TryRecvError::Empty) => {}
            Err(crossbeam_channel::TryRecvError::Disconnected) => {}
        }
    }
    if let Some(abort) = abort_rx {
        match abort.try_recv() {
            Ok(()) | Err(crossbeam_channel::TryRecvError::Disconnected) => {
                scope.terminate_execution();
                return Err(EventLoopStatus::Terminated);
            }
            Err(crossbeam_channel::TryRecvError::Empty) => {}
        }
    }
    match rx.try_recv() {
        Ok(command) => return Ok(Some(command)),
        Err(crossbeam_channel::TryRecvError::Empty) => {}
        Err(crossbeam_channel::TryRecvError::Disconnected) => return Ok(None),
    }
    match ready_rx.try_recv() {
        Ok(wake) => match ready_batch_command(ready_broker, wake) {
            Ok(command) => return Ok(Some(command)),
            Err(error) => {
                eprintln!("{error}");
            }
        },
        Err(crossbeam_channel::TryRecvError::Empty) => {}
        Err(crossbeam_channel::TryRecvError::Disconnected) => {}
    }
    if let Some(abort) = abort_rx {
        crossbeam_channel::select! {
            recv(abort) -> _ => {
                scope.terminate_execution();
                Err(EventLoopStatus::Terminated)
            },
            recv(rx) -> result => Ok(result.ok()),
            default => Ok(None),
        }
    } else {
        match rx.try_recv() {
            Ok(cmd) => Ok(Some(cmd)),
            Err(crossbeam_channel::TryRecvError::Empty) => Ok(None),
            Err(crossbeam_channel::TryRecvError::Disconnected) => Ok(None),
        }
    }
}

fn dispatch_session_command(
    scope: &mut v8::HandleScope,
    cmd: SessionCommand,
    pending: &crate::bridge::PendingPromises,
    ready_broker: &SessionReadiness,
) -> EventLoopStatus {
    match cmd {
        SessionCommand::Message(frame) => dispatch_event_loop_frame(scope, frame, pending),
        SessionCommand::ReadyBatch(batch) => dispatch_ready_batch(scope, batch, ready_broker),
        SessionCommand::SetModuleReader(reader) => {
            execution::install_session_guest_reader(Some(reader));
            EventLoopStatus::Completed
        }
        SessionCommand::Shutdown => EventLoopStatus::Terminated,
    }
}

/// Dispatch one bounded readiness turn, then complete its wake on every exit.
///
/// Callback exceptions are execution failures, but they must not strand the
/// session broker in `WakeState::Outstanding`. Keeping dispatch in an inner
/// function gives this wrapper finally-style completion semantics: every inner
/// return reaches `complete_ready_batch_dispatch` before control returns to the
/// reusable session loop.
fn dispatch_ready_batch(
    scope: &mut v8::HandleScope,
    batch: RuntimeReadyBatch,
    ready_broker: &SessionReadiness,
) -> EventLoopStatus {
    let mut delivered = Vec::with_capacity(batch.entries.len());
    let dispatch_status =
        dispatch_ready_batch_callbacks(scope, &batch, ready_broker, &mut delivered);
    complete_ready_batch_dispatch(ready_broker, &batch, &delivered, dispatch_status)
}

fn dispatch_ready_batch_callbacks(
    scope: &mut v8::HandleScope,
    batch: &RuntimeReadyBatch,
    ready_broker: &SessionReadiness,
    delivered: &mut Vec<ReadyObservation>,
) -> EventLoopStatus {
    for entry in &batch.entries {
        let tc = &mut v8::TryCatch::new(scope);
        let dispatch = crate::stream::dispatch_readiness(
            tc,
            entry.capability_id,
            entry.capability_generation,
            entry.flags,
        );
        tc.perform_microtask_checkpoint();
        if let Some(exception) = tc.exception() {
            let (code, error) = execution::exception_to_result(tc, exception);
            return EventLoopStatus::Failed(code, error);
        }
        match dispatch {
            crate::stream::ReadinessDispatch::Delivered => delivered.push(*entry),
            crate::stream::ReadinessDispatch::TargetMissing => {
                // The bridge exists but this capability may not be registered
                // yet (for example, readiness raced the connect response).
                // Leave the observation unacknowledged so the durable
                // sidecar state schedules another coalesced wake.
            }
            crate::stream::ReadinessDispatch::BridgeMissing => {
                return EventLoopStatus::Failed(
                            1,
                            ExecutionError {
                                error_type: String::from("Error"),
                                message: String::from(
                                    "ERR_AGENTOS_READY_DISPATCH_MISSING: guest bridge does not expose _agentOSReadyDispatch",
                                ),
                                stack: String::new(),
                                code: Some(String::from("ERR_AGENTOS_READY_DISPATCH_MISSING")),
                            },
                        );
            }
        }
    }
    if batch.signals_ready {
        let signals = match ready_broker.drain_signals(batch) {
            Ok(signals) => signals,
            Err(error) => return readiness_dispatch_failure(error),
        };
        for signal in signals {
            let Some(signal_name) = signal_name_for_stream_event(signal) else {
                continue;
            };
            let tc = &mut v8::TryCatch::new(scope);
            crate::stream::dispatch_signal_event(tc, signal_name, signal);
            tc.perform_microtask_checkpoint();
            if let Some(exception) = tc.exception() {
                let (code, error) = execution::exception_to_result(tc, exception);
                return EventLoopStatus::Failed(code, error);
            }
            if let Some(error) = execution::take_unhandled_promise_rejection(tc) {
                return EventLoopStatus::Failed(1, error);
            }
        }
    }
    if batch.timers_ready {
        let timers = match ready_broker.drain_timers(batch) {
            Ok(timers) => timers,
            Err(error) => return readiness_dispatch_failure(error),
        };
        for timer_id in timers {
            let tc = &mut v8::TryCatch::new(scope);
            crate::stream::dispatch_timer_event(tc, timer_id);
            tc.perform_microtask_checkpoint();
            if let Some(exception) = tc.exception() {
                let (code, error) = execution::exception_to_result(tc, exception);
                return EventLoopStatus::Failed(code, error);
            }
            if let Some(error) = execution::take_unhandled_promise_rejection(tc) {
                return EventLoopStatus::Failed(1, error);
            }
        }
    }
    EventLoopStatus::Completed
}

fn complete_ready_batch_dispatch(
    ready_broker: &SessionReadiness,
    batch: &RuntimeReadyBatch,
    delivered: &[ReadyObservation],
    dispatch_status: EventLoopStatus,
) -> EventLoopStatus {
    if let Err(error) = ready_broker.complete_batch(batch, delivered) {
        if matches!(&dispatch_status, EventLoopStatus::Completed) {
            return readiness_dispatch_failure(error);
        }
        eprintln!(
            "ERR_AGENTOS_READY_COMPLETE_AFTER_DISPATCH_FAILURE: could not complete readiness wake after guest dispatch failed: {error}"
        );
    }
    dispatch_status
}

fn readiness_dispatch_failure(message: String) -> EventLoopStatus {
    EventLoopStatus::Failed(
        1,
        ExecutionError {
            error_type: String::from("Error"),
            message,
            stack: String::new(),
            code: Some(String::from("ERR_AGENTOS_READY_COMPLETE")),
        },
    )
}

fn signal_name_for_stream_event(signal: i32) -> Option<&'static str> {
    match signal {
        1 => Some("SIGHUP"),
        2 => Some("SIGINT"),
        10 => Some("SIGUSR1"),
        14 => Some("SIGALRM"),
        18 => Some("SIGCONT"),
        15 => Some("SIGTERM"),
        17 => Some("SIGCHLD"),
        28 => Some("SIGWINCH"),
        _ => None,
    }
}

fn pending_guest_timer_count(scope: &mut v8::HandleScope) -> usize {
    let tc = &mut v8::TryCatch::new(scope);
    let context = tc.get_current_context();
    let global = context.global(tc);
    let key = match v8::String::new(tc, "_getPendingTimerCount") {
        Some(key) => key,
        None => return 0,
    };
    let Some(func_value) = global.get(tc, key.into()) else {
        return 0;
    };
    let Ok(func) = v8::Local::<v8::Function>::try_from(func_value) else {
        return 0;
    };
    let Some(result) = func.call(tc, global.into(), &[]) else {
        return 0;
    };

    result
        .integer_value(tc)
        .and_then(|count| usize::try_from(count).ok())
        .unwrap_or(0)
}

fn pending_guest_immediate_count(scope: &mut v8::HandleScope) -> usize {
    let tc = &mut v8::TryCatch::new(scope);
    let context = tc.get_current_context();
    let global = context.global(tc);
    let key = match v8::String::new(tc, "_getPendingImmediateCount") {
        Some(key) => key,
        None => return 0,
    };
    let Some(func_value) = global.get(tc, key.into()) else {
        return 0;
    };
    let Ok(func) = v8::Local::<v8::Function>::try_from(func_value) else {
        return 0;
    };
    let Some(result) = func.call(tc, global.into(), &[]) else {
        return 0;
    };

    result
        .integer_value(tc)
        .and_then(|count| usize::try_from(count).ok())
        .unwrap_or(0)
}

fn drain_guest_immediates(scope: &mut v8::HandleScope) -> EventLoopStatus {
    let tc = &mut v8::TryCatch::new(scope);
    let context = tc.get_current_context();
    let global = context.global(tc);
    let key = match v8::String::new(tc, "_drainImmediates") {
        Some(key) => key,
        None => return EventLoopStatus::Completed,
    };
    let Some(func_value) = global.get(tc, key.into()) else {
        return EventLoopStatus::Completed;
    };
    let Ok(func) = v8::Local::<v8::Function>::try_from(func_value) else {
        return EventLoopStatus::Completed;
    };
    let _ = func.call(tc, global.into(), &[]);
    tc.perform_microtask_checkpoint();
    if let Some(exception) = tc.exception() {
        let (code, err) = execution::exception_to_result(tc, exception);
        return EventLoopStatus::Failed(code, err);
    }
    if let Some(err) = execution::take_unhandled_promise_rejection(tc) {
        return EventLoopStatus::Failed(1, err);
    }
    EventLoopStatus::Completed
}

fn pump_v8_message_loop(scope: &mut v8::HandleScope) {
    let platform = v8::V8::get_current_platform();
    while v8::Platform::pump_message_loop(&platform, scope, false) {
        scope.perform_microtask_checkpoint();
    }
}

/// Dispatch a single session message within the event loop.
/// Returns the event-loop status after handling the frame.
#[derive(Debug)]
#[doc(hidden)]
pub enum EventLoopStatus {
    Completed,
    Terminated,
    Failed(i32, ExecutionError),
}

fn dispatch_event_loop_frame(
    scope: &mut v8::HandleScope,
    frame: SessionMessage,
    pending: &crate::bridge::PendingPromises,
) -> EventLoopStatus {
    match frame {
        SessionMessage::BridgeResponse(BridgeResponse {
            call_id,
            status,
            payload,
            reservation: _reservation,
        }) => {
            let (result, error) = if status == 1 {
                (None, Some(String::from_utf8_lossy(&payload).to_string()))
            } else if status == 2 || !payload.is_empty() {
                // status=0: V8-serialized, status=2: raw binary (Uint8Array)
                (Some(payload), None)
            } else {
                (None, None)
            };
            let _ = crate::bridge::resolve_pending_promise(
                scope, pending, call_id, status, result, error,
            );
            // Microtasks already flushed in resolve_pending_promise
            EventLoopStatus::Completed
        }
        SessionMessage::StreamEvent(StreamEvent {
            event_type,
            payload,
        }) => {
            let tc = &mut v8::TryCatch::new(scope);
            crate::stream::dispatch_stream_event(tc, &event_type, &payload);
            tc.perform_microtask_checkpoint();
            if let Some(exception) = tc.exception() {
                let (code, err) = execution::exception_to_result(tc, exception);
                return EventLoopStatus::Failed(code, err);
            }
            if let Some(err) = execution::take_unhandled_promise_rejection(tc) {
                return EventLoopStatus::Failed(1, err);
            }
            EventLoopStatus::Completed
        }
        SessionMessage::TerminateExecution => {
            scope.terminate_execution();
            EventLoopStatus::Terminated
        }
        _ => {
            // Ignore other messages during event loop
            EventLoopStatus::Completed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    const TEST_READY_BATCH_HANDLES: usize = 64;

    /// Helper to create a SessionManager for tests
    fn test_manager(max: usize) -> SessionManager {
        test_manager_with_events(max).0
    }

    fn test_manager_with_events(max: usize) -> (SessionManager, Receiver<RuntimeEventEnvelope>) {
        let (tx, _rx) = crossbeam_channel::unbounded();
        let router: CallIdRouter = Arc::new(BridgeCallRegistry::with_default_limit());
        let snap_cache = Arc::new(SnapshotCache::new(4));
        let runtime =
            agentos_runtime::SidecarRuntime::process(&agentos_runtime::RuntimeConfig::default())
                .expect("create test process runtime")
                .context();
        let manager = SessionManager::new(max, tx, router, snap_cache, runtime);
        (manager, _rx)
    }

    #[test]
    fn zero_cpu_time_limit_is_normalized_to_no_timeout() {
        assert_eq!(normalize_cpu_time_limit_ms(None), None);
        assert_eq!(normalize_cpu_time_limit_ms(Some(0)), None);
        assert_eq!(normalize_cpu_time_limit_ms(Some(1)), Some(1));
    }

    #[test]
    fn vm_executor_permits_report_active_and_high_water_metrics() {
        let control: SlotControl = Arc::new((Mutex::new(0), Condvar::new()));
        let metrics = RuntimeMetrics::new();

        let first = SessionSlotPermit::try_acquire(&control, 2, metrics.clone())
            .expect("acquire first VM executor");
        let second = SessionSlotPermit::try_acquire(&control, 2, metrics.clone())
            .expect("acquire second VM executor");
        let active = metrics.snapshot().executors[ExecutorMetricClass::Vm.index()].active;
        assert_eq!(active.current, 2);
        assert_eq!(active.high_water, 2);

        drop(first);
        assert_eq!(
            metrics.snapshot().executors[ExecutorMetricClass::Vm.index()]
                .active
                .current,
            1
        );

        drop(second);
        let released = metrics.snapshot().executors[ExecutorMetricClass::Vm.index()].active;
        assert_eq!(released.current, 0);
        assert_eq!(released.high_water, 2);
    }

    #[test]
    fn configured_executor_and_command_bounds_drive_session_manager() {
        const SUBPROCESS_ENV: &str = "AGENTOS_V8_CONFIGURED_SESSION_MANAGER_SUBPROCESS";
        if std::env::var_os(SUBPROCESS_ENV).is_none() {
            let test_name =
                "session::tests::configured_executor_and_command_bounds_drive_session_manager";
            let output =
                std::process::Command::new(std::env::current_exe().expect("current test binary"))
                    .arg(test_name)
                    .arg("--exact")
                    .arg("--nocapture")
                    .env(SUBPROCESS_ENV, "1")
                    .output()
                    .unwrap_or_else(|error| panic!("spawn isolated test {test_name}: {error}"));
            assert!(
                output.status.success(),
                "isolated test {test_name} failed with status {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status.code(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            );
            return;
        }
        let mut config = agentos_runtime::RuntimeConfig {
            max_active_vm_executors: 3,
            vm_executor_teardown_timeout_ms: 23,
            ..agentos_runtime::RuntimeConfig::default()
        };
        config.resources.max_handle_commands = 7;
        let runtime = agentos_runtime::SidecarRuntime::process(&config)
            .expect("configured process runtime")
            .context();
        let (event_tx, _event_rx) = crossbeam_channel::unbounded();
        let router: CallIdRouter = Arc::new(BridgeCallRegistry::with_default_limit());
        let mut manager = SessionManager::new(
            runtime.max_active_vm_executors(),
            event_tx,
            router,
            Arc::new(SnapshotCache::new(1)),
            runtime,
        );

        assert_eq!(manager.max_concurrency, 3);
        assert_eq!(manager.executor_teardown_timeout, Duration::from_millis(23));
        manager
            .create_session("configured-bounds".into(), None, None, None)
            .expect("create bounded session");
        assert_eq!(manager.sessions["configured-bounds"].command_capacity, 7);
        manager
            .destroy_session("configured-bounds")
            .expect("destroy bounded session");
    }

    fn expect_late_message_warning(
        rx: &Receiver<RuntimeEventEnvelope>,
        session_id: &str,
        error_code: &str,
        detail_fragment: &str,
    ) {
        let event = rx
            .recv_timeout(std::time::Duration::from_millis(200))
            .expect("late-message warning");
        match event.event {
            RuntimeEvent::Log {
                session_id: observed_session_id,
                channel,
                message,
            } => {
                assert_eq!(observed_session_id, session_id);
                assert_eq!(channel, 1, "late warnings should use stderr channel");
                assert!(
                    message.contains(error_code),
                    "warning should contain error code {error_code}, got {message}"
                );
                assert!(
                    message.contains(detail_fragment),
                    "warning should mention {detail_fragment}, got {message}"
                );
            }
            other => panic!("expected late-message warning log, got {other:?}"),
        }
    }

    #[test]
    fn bridge_contract_function_partitions_cover_contract() {
        let contract = bridge_contract();

        let expected_sync = contract
            .groups
            .iter()
            .filter(|group| {
                matches!(
                    group.convention,
                    BridgeCallConvention::Sync | BridgeCallConvention::SyncPromise
                )
            })
            .flat_map(|group| group.names.iter().map(String::as_str))
            .collect::<HashSet<_>>();
        let expected_async = contract
            .groups
            .iter()
            .filter(|group| group.convention == BridgeCallConvention::Async)
            .flat_map(|group| group.names.iter().map(String::as_str))
            .collect::<HashSet<_>>();

        let sync_names = sync_bridge_fns();
        let async_names = async_bridge_fns();
        let registered_sync = sync_names.iter().copied().collect::<HashSet<_>>();
        let registered_async = async_names.iter().copied().collect::<HashSet<_>>();

        assert_eq!(
            registered_sync, expected_sync,
            "sync bridge function partition drifted from crates/bridge/bridge-contract.json"
        );
        assert_eq!(
            registered_async, expected_async,
            "async bridge function partition drifted from crates/bridge/bridge-contract.json"
        );
        assert!(
            registered_sync.is_disjoint(&registered_async),
            "sync and async bridge function partitions must not overlap"
        );
    }

    #[test]
    fn session_management() {
        // Consolidated test to avoid V8 inter-test SIGSEGV issues.
        // Covers: lifecycle and concurrency queuing.

        // --- Part 1: Single session create/destroy ---
        {
            let mut mgr = test_manager(4);

            mgr.create_session("session-aaa".into(), None, None, None)
                .expect("create session A");
            assert_eq!(mgr.session_count(), 1);

            // Wait for thread to acquire slot and create isolate
            std::thread::sleep(std::time::Duration::from_millis(200));

            // Destroy session A
            mgr.destroy_session("session-aaa")
                .expect("destroy session A");
            assert_eq!(mgr.session_count(), 0);
        }

        // --- Part 2: Multiple sessions ---
        {
            let mut mgr = test_manager(4);

            mgr.create_session("session-bbb".into(), None, None, None)
                .expect("create session B");
            mgr.create_session("session-ccc".into(), Some(16), None, None)
                .expect("create session C");
            assert_eq!(mgr.session_count(), 2);

            std::thread::sleep(std::time::Duration::from_millis(200));

            // Duplicate session ID is rejected
            let err = mgr.create_session("session-bbb".into(), None, None, None);
            assert!(err.is_err());
            assert!(err.unwrap_err().contains("already exists"));

            // Sending to a missing session still fails.
            let err = mgr.send_to_session("missing", SessionMessage::TerminateExecution);
            assert!(err.is_err());
            assert!(err.unwrap_err().contains("does not exist"));

            // Destroy non-existent session
            let err = mgr.destroy_session("no-such-session");
            assert!(err.is_err());
            assert!(err.unwrap_err().contains("does not exist"));

            mgr.destroy_sessions(["session-bbb".into(), "session-ccc".into()]);
            assert_eq!(mgr.session_count(), 0);
        }

        // --- Part 3: Max concurrency admission before thread creation ---
        {
            let mut mgr = test_manager(2);

            mgr.create_session("s1".into(), None, None, None)
                .expect("create s1");
            mgr.create_session("s2".into(), None, None, None)
                .expect("create s2");
            let error = mgr
                .create_session("s3".into(), None, None, None)
                .expect_err("third executor must be rejected before thread creation");
            assert!(error.contains("ERR_AGENTOS_VM_EXECUTOR_LIMIT"));

            // Allow threads to acquire slots
            std::thread::sleep(std::time::Duration::from_millis(300));

            // Only two admitted executor threads exist.
            assert_eq!(mgr.active_slot_count(), 2);
            assert_eq!(mgr.session_count(), 2);

            // Destroy s1, then a new generation can acquire the released slot.
            mgr.destroy_session("s1").expect("destroy s1");
            mgr.create_session("s3".into(), None, None, None)
                .expect("create s3 after release");
            std::thread::sleep(std::time::Duration::from_millis(300));
            assert_eq!(mgr.active_slot_count(), 2);
            assert_eq!(mgr.session_count(), 2);

            // Destroy remaining
            mgr.destroy_sessions(["s2".into(), "s3".into()]);
            std::thread::sleep(std::time::Duration::from_millis(100));
            assert_eq!(mgr.session_count(), 0);
            assert_eq!(mgr.active_slot_count(), 0);
        }
    }

    #[test]
    fn detach_session_clears_call_id_routes_for_session() {
        let mut mgr = test_manager(1);
        mgr.create_session_with_output_generation(
            "session-route".into(),
            None,
            None,
            None,
            Some(7),
            None,
        )
        .expect("create session");
        let _waiter = mgr
            .call_id_router()
            .register_sync(&mgr.runtime, 0, 1, 42, "session-route", Some(7))
            .expect("register bridge call target");

        assert!(
            mgr.detach_session_if_output_generation("session-route", 7)
                .expect("detach session"),
            "matching output generation should detach session"
        );
        assert!(
            mgr.call_id_router().pending_len() == 0,
            "detach should clear stale bridge call routes for the session"
        );
        assert_eq!(
            mgr.quarantined.len(),
            1,
            "detached executor join ownership must remain in the manager"
        );
        for handle in mgr.take_session_shutdown_handles() {
            handle.join().expect("join quarantined executor");
        }
        assert_eq!(mgr.active_slot_count(), 0);
    }

    #[test]
    fn begin_destroy_session_removes_entry_before_finish() {
        let mut mgr = test_manager(1);
        mgr.create_session("two-phase".into(), None, None, None)
            .expect("create session");

        let first_shutdown = mgr
            .begin_destroy_session("two-phase")
            .expect("begin destroy session");
        assert_eq!(
            mgr.session_count(),
            0,
            "entry should be removed before the shutdown is finished"
        );

        // Removing the registry entry does not release the executor permit.
        // Until the old thread joins, a successor generation is quarantined.
        let error = mgr
            .create_session("two-phase".into(), None, None, None)
            .expect_err("old generation must retain its executor permit");
        assert!(error.contains("ERR_AGENTOS_VM_EXECUTOR_LIMIT"));
        first_shutdown.finish();

        mgr.create_session("two-phase".into(), None, None, None)
            .expect("re-create session after old generation joins");
        let second_shutdown = mgr
            .begin_destroy_session("two-phase")
            .expect("begin destroy re-created session");
        second_shutdown.finish();
        assert_eq!(mgr.session_count(), 0);
    }

    #[test]
    fn shutdown_bypasses_a_full_ordinary_command_lane() {
        let mut mgr = test_manager(1);
        let command_capacity = mgr
            .runtime
            .resources()
            .usage(ResourceClass::HandleCommands)
            .limit
            .expect("configured command capacity");
        let (tx, rx) = crossbeam_channel::bounded(command_capacity);
        for index in 0..command_capacity {
            tx.send(SessionCommand::Message(SessionMessage::StreamEvent(
                StreamEvent {
                    event_type: format!("ordinary-{index}"),
                    payload: Vec::new(),
                },
            )))
            .expect("fill ordinary command lane");
        }
        let (shutdown_tx, shutdown_rx) = crossbeam_channel::bounded(1);
        let join_handle = thread::spawn(move || {
            shutdown_rx.recv().expect("dedicated shutdown token");
            drop(rx);
        });
        let (ready_broker, _ready_rx) =
            SessionReadiness::new(1, &mgr.runtime, TEST_READY_BATCH_HANDLES)
                .expect("create session readiness");
        let session_resources = Arc::clone(mgr.runtime.resources());
        mgr.sessions.insert(
            String::from("full-command-lane"),
            SessionEntry {
                output_generation: None,
                tx,
                command_capacity,
                shutdown_tx,
                join_handle: Some(join_handle),
                isolate_handle: Arc::new(Mutex::new(None)),
                execution_abort: new_execution_abort(),
                execution_active: Arc::new(AtomicBool::new(false)),
                pause_control: Arc::new(SessionPauseControl::default()),
                ready_broker,
                session_resources,
            },
        );

        mgr.begin_destroy_session("full-command-lane")
            .expect("begin destroy overloaded session")
            .finish();
        assert_eq!(mgr.session_count(), 0);
    }

    #[test]
    fn execution_abort_is_durable_when_signaled_before_waiter_arm() {
        let execution_abort = new_execution_abort();
        signal_execution_abort_durable(&execution_abort, ExecutionAbortReason::Terminated);

        let (_guard, receiver) = ActiveExecutionAbort::arm(&execution_abort);
        assert_eq!(
            receiver.recv_timeout(Duration::from_millis(10)),
            Err(crossbeam_channel::RecvTimeoutError::Disconnected),
            "a waiter armed after termination must observe it immediately"
        );
    }

    #[test]
    fn session_shutdown_finish_clears_late_call_routes() {
        let mut mgr = test_manager(1);
        mgr.create_session("late-route".into(), None, None, None)
            .expect("create session");

        let shutdown = mgr
            .begin_destroy_session("late-route")
            .expect("begin destroy session");
        // Simulate a route the session thread registered between the pre-join
        // route clear and thread exit.
        let _waiter = mgr
            .call_id_router()
            .register_sync(&mgr.runtime, 0, 1, 42, "late-route", None)
            .expect("register late bridge call target");

        shutdown.finish();
        assert!(
            mgr.call_id_router().pending_len() == 0,
            "finish should clear call routes registered during shutdown"
        );
    }

    #[test]
    fn late_terminate_execution_is_logged_instead_of_silently_dropped() {
        let (mut mgr, rx) = test_manager_with_events(1);
        mgr.create_session("late-terminate".into(), None, None, None)
            .expect("create session");

        mgr.send_to_session("late-terminate", SessionMessage::TerminateExecution)
            .expect("send late terminate");

        expect_late_message_warning(
            &rx,
            "late-terminate",
            LATE_TERMINATE_EXECUTION_ERROR_CODE,
            "TerminateExecution",
        );

        mgr.destroy_session("late-terminate")
            .expect("destroy session");
    }

    #[test]
    fn late_stdin_end_is_an_expected_stale_control_event() {
        let (event_tx, event_rx) = crossbeam_channel::unbounded();
        handle_late_session_message(
            &RuntimeEventSender::from(event_tx),
            "completed-session",
            Some(7),
            SessionMessage::StreamEvent(StreamEvent {
                event_type: String::from("stdin_end"),
                payload: Vec::new(),
            }),
        );

        assert!(
            event_rx.try_recv().is_err(),
            "an idempotent stdin EOF racing process exit must not become guest stderr"
        );
    }

    #[test]
    fn late_bridge_response_is_logged_instead_of_silently_dropped() {
        let (mut mgr, rx) = test_manager_with_events(1);
        mgr.create_session("late-bridge".into(), None, None, None)
            .expect("create session");

        mgr.send_to_session(
            "late-bridge",
            SessionMessage::BridgeResponse(BridgeResponse {
                call_id: 41,
                status: 0,
                payload: vec![0xAA, 0xBB],
                reservation: None,
            }),
        )
        .expect("send late bridge response");

        expect_late_message_warning(
            &rx,
            "late-bridge",
            LATE_BRIDGE_RESPONSE_ERROR_CODE,
            "BridgeResponse",
        );

        mgr.destroy_session("late-bridge").expect("destroy session");
    }

    #[test]
    fn control_only_readiness_wake_becomes_a_command_and_rearms() {
        let mgr = test_manager(1);
        let (broker, wake_rx) = SessionReadiness::new(23, &mgr.runtime, TEST_READY_BATCH_HANDLES)
            .expect("create session readiness");

        broker.publish_timer(91).expect("publish first timer");
        let first_wake = wake_rx.try_recv().expect("first timer wake");
        let first_batch = match ready_batch_command(&broker, first_wake).expect("timer command") {
            SessionCommand::ReadyBatch(batch) => batch,
            _ => panic!("timer wake must produce a readiness command"),
        };
        assert!(first_batch.entries.is_empty());
        assert!(first_batch.timers_ready);
        assert_eq!(
            broker
                .drain_timers(&first_batch)
                .expect("drain first timer"),
            vec![91]
        );
        broker
            .complete_batch(&first_batch, &[])
            .expect("complete first timer wake");

        broker.publish_timer(92).expect("publish second timer");
        let second_wake = wake_rx
            .try_recv()
            .expect("completing a control-only batch must rearm the wake lane");
        let second_batch = match ready_batch_command(&broker, second_wake).expect("second command")
        {
            SessionCommand::ReadyBatch(batch) => batch,
            _ => panic!("second timer wake must produce a readiness command"),
        };
        assert!(second_batch.entries.is_empty());
        assert!(second_batch.timers_ready);
    }

    #[test]
    fn admitted_session_command_precedes_later_signal_wake() {
        let mgr = test_manager(1);
        let (broker, ready_rx) = SessionReadiness::new(31, &mgr.runtime, TEST_READY_BATCH_HANDLES)
            .expect("create session readiness");
        let (tx, rx) = crossbeam_channel::bounded(2);
        let (_shutdown_tx, shutdown_rx) = crossbeam_channel::bounded(1);

        tx.send(SessionCommand::Message(SessionMessage::StreamEvent(
            StreamEvent {
                event_type: String::from("execute-admitted-first"),
                payload: Vec::new(),
            },
        )))
        .expect("queue ordinary session command");
        broker.publish_signal(15).expect("publish later SIGTERM");

        assert!(matches!(
            recv_session_command(&rx, &shutdown_rx, &ready_rx, &broker),
            Some(SessionCommand::Message(SessionMessage::StreamEvent(_)))
        ));
        let Some(SessionCommand::ReadyBatch(batch)) =
            recv_session_command(&rx, &shutdown_rx, &ready_rx, &broker)
        else {
            panic!("later signal wake must remain queued after the admitted command");
        };
        assert!(batch.signals_ready);
        assert_eq!(
            broker.drain_signals(&batch).expect("drain signal"),
            vec![15]
        );
        broker
            .complete_batch(&batch, &[])
            .expect("complete signal wake");
    }

    #[test]
    fn readiness_flood_uses_one_wake_and_carries_only_capability_identity() {
        let mgr = test_manager(1);
        let (broker, wake_rx) = SessionReadiness::new(7, &mgr.runtime, TEST_READY_BATCH_HANDLES)
            .expect("create session readiness");
        for _ in 0..1_000_000 {
            broker
                .publish(41, 3, ReadyFlags::READABLE)
                .expect("publish readiness");
        }

        let wake = wake_rx.try_recv().expect("one coalesced wake");
        assert!(
            wake_rx.try_recv().is_err(),
            "wake lane must have capacity one"
        );
        let batch = broker.take_batch(wake).expect("take readiness batch");
        assert_eq!(batch.entries.len(), 1);
        assert_eq!(batch.entries[0].capability_id, 41);
        assert_eq!(batch.entries[0].capability_generation, 3);
        assert_eq!(batch.entries[0].flags, ReadyFlags::READABLE);
        assert_eq!(batch.entries[0].revision, 1_000_000);
        broker
            .complete_batch(&batch, &batch.entries)
            .expect("complete readiness");
        assert_eq!(
            broker.broker.pending_handle_count().expect("pending count"),
            0
        );
    }

    #[test]
    fn readiness_batch_honors_vm_reactor_work_quantum_override() {
        let mgr = test_manager(1);
        let (broker, wake_rx) =
            SessionReadiness::new(29, &mgr.runtime, 2).expect("create bounded readiness");
        for capability_id in 1..=3 {
            broker
                .publish(capability_id, 1, ReadyFlags::READABLE)
                .expect("publish readiness");
        }

        let first_wake = wake_rx.recv().expect("first coalesced wake");
        let first_batch = broker.take_batch(first_wake).expect("first bounded batch");
        assert_eq!(
            first_batch.entries.len(),
            2,
            "limits.reactor.workQuantum must cap one V8 readiness turn"
        );
        broker
            .complete_batch(&first_batch, &first_batch.entries)
            .expect("complete first bounded batch");

        let second_wake = wake_rx.recv().expect("replacement wake for remaining work");
        let second_batch = broker
            .take_batch(second_wake)
            .expect("second bounded batch");
        assert_eq!(second_batch.entries.len(), 1);
    }

    #[test]
    fn readiness_batch_rejects_zero_vm_reactor_work_quantum() {
        let mgr = test_manager(1);
        let error = SessionReadiness::new(30, &mgr.runtime, 0)
            .expect_err("zero work quantum must fail closed");
        assert!(error.contains("limits.reactor.workQuantum"));
    }

    #[test]
    fn readiness_rejects_a_stale_capability_generation_without_replacing_state() {
        let mgr = test_manager(1);
        let (broker, wake_rx) = SessionReadiness::new(13, &mgr.runtime, TEST_READY_BATCH_HANDLES)
            .expect("create session readiness");
        broker
            .publish(9, 4, ReadyFlags::READABLE)
            .expect("publish live capability");
        let error = broker
            .publish(9, 3, ReadyFlags::CLOSE)
            .expect_err("stale capability generation must fail");
        assert!(error.contains("ERR_AGENTOS_READY_STALE_CAPABILITY"));
        assert!(wake_rx.len() <= 1, "wake lane must stay capacity one");
        let wake = wake_rx.recv().expect("durable wake");
        let batch = broker.take_batch(wake).expect("take readiness batch");
        assert_eq!(batch.entries.len(), 1);
        assert_eq!(batch.entries[0].capability_generation, 4);
        assert_eq!(batch.entries[0].flags, ReadyFlags::READABLE);
    }

    #[test]
    fn readiness_before_guest_registration_remains_pending_until_dispatch_succeeds() {
        let mgr = test_manager(1);
        let (broker, wake_rx) = SessionReadiness::new(17, &mgr.runtime, TEST_READY_BATCH_HANDLES)
            .expect("create session readiness");
        broker
            .publish(33, 8, ReadyFlags::READABLE)
            .expect("publish readiness before guest registration");

        let first_wake = wake_rx.recv().expect("initial wake");
        let first_batch = broker.take_batch(first_wake).expect("initial batch");
        broker
            .complete_batch(&first_batch, &[])
            .expect("preserve readiness when guest target is absent");
        assert_eq!(
            broker.broker.pending_handle_count().expect("pending count"),
            1
        );

        let retry_wake = wake_rx.recv().expect("retry wake after registration");
        let retry_batch = broker.take_batch(retry_wake).expect("retry batch");
        broker
            .complete_batch(&retry_batch, &retry_batch.entries)
            .expect("acknowledge readiness after target runs");
        assert_eq!(
            broker.broker.pending_handle_count().expect("pending count"),
            0
        );
    }

    #[test]
    fn readiness_dispatch_failure_completes_wake_before_session_reuse() {
        let mgr = test_manager(1);
        let (broker, wake_rx) = SessionReadiness::new(18, &mgr.runtime, TEST_READY_BATCH_HANDLES)
            .expect("create reusable session readiness");
        broker
            .publish(34, 9, ReadyFlags::READABLE)
            .expect("publish readiness before failing dispatch");

        let first_wake = wake_rx.recv().expect("initial wake");
        let first_batch = broker.take_batch(first_wake).expect("initial batch");
        let status = complete_ready_batch_dispatch(
            &broker,
            &first_batch,
            &[],
            EventLoopStatus::Failed(
                1,
                ExecutionError {
                    error_type: String::from("Error"),
                    message: String::from("injected readiness handler failure"),
                    stack: String::new(),
                    code: Some(String::from("ERR_TEST_READY_DISPATCH")),
                },
            ),
        );
        assert!(matches!(status, EventLoopStatus::Failed(1, _)));

        let retry_wake = wake_rx
            .try_recv()
            .expect("failed dispatch must complete and rearm the wake for session reuse");
        let retry_batch = broker.take_batch(retry_wake).expect("reused-session batch");
        assert_eq!(retry_batch.entries, first_batch.entries);
        broker
            .complete_batch(&retry_batch, &retry_batch.entries)
            .expect("reused session can acknowledge the retried readiness");
    }

    /// Regression test for the pending-promise-resolver leak / V8 lifetime-contract
    /// violation: when `run_event_loop` exits early (Shutdown or timeout-abort) the
    /// `PendingPromises` registry can still hold `Global<PromiseResolver>` handles,
    /// and the session-thread teardown must reset them *before* dropping the isolate.
    ///
    /// This drives the real cleanup seam (`reset_pending_promises`) used on every
    /// isolate-drop path. It populates the registry with live resolver Globals (as a
    /// terminated execution would leave behind), runs the cleanup while the isolate
    /// is still alive, and asserts the registry is empty (every Global dropped).
    ///
    /// Fast + bounded (a handful of resolvers, then the safeguard fires) — it asserts
    /// the cleanup happens, it does not saturate `MAX_PENDING_PROMISES`.
    #[test]
    fn reset_pending_promises_drops_resolver_globals_before_isolate_teardown() {
        use crate::bridge::{register_async_bridge_fns, PendingPromises};
        use crate::host_call::BridgeCallContext;
        use crate::isolate;
        use std::process::Command;

        // V8 isolates must be created in an isolated process: doing it inline in a
        // parallel `cargo test` thread races the process-global V8 platform and
        // segfaults. Re-exec this one test as a subprocess (matching the crate's
        // bridge_v8_hardening_* / vm_context_registry convention).
        const SUBPROCESS_ENV: &str = "AGENTOS_V8_RESET_PENDING_PROMISES_SUBPROCESS";
        if std::env::var_os(SUBPROCESS_ENV).is_none() {
            let output = Command::new(std::env::current_exe().expect("current test binary"))
                .arg("session::tests::reset_pending_promises_drops_resolver_globals_before_isolate_teardown")
                .arg("--exact")
                .arg("--nocapture")
                .env(SUBPROCESS_ENV, "1")
                .output()
                .expect("spawn reset-pending-promises subprocess");
            assert!(
                output.status.success(),
                "reset-pending-promises subprocess failed with status {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status.code(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        isolate::init_v8_platform();

        let mut v8_isolate = isolate::create_isolate(None);
        let context = isolate::create_context(&mut v8_isolate);
        let scope = &mut v8::HandleScope::new(&mut v8_isolate);
        let context = v8::Local::new(scope, &context);
        let scope = &mut v8::ContextScope::new(scope, context);

        let bridge_ctx = BridgeCallContext::new(
            Box::new(std::io::sink()),
            Box::new(std::io::empty()),
            String::from("reset-pending-test"),
        );
        let mut pending = PendingPromises::new();

        // Each `_asyncFn(i)` call synchronously registers a pending promise
        // resolver Global in `pending` and returns an unresolved Promise —
        // exactly what remains registered when the event loop exits early on
        // Shutdown / timeout-abort.
        const REGISTERED: usize = 8;
        let _async_fns = register_async_bridge_fns(
            scope,
            &bridge_ctx as *const BridgeCallContext,
            &pending as *const PendingPromises,
            &["_asyncFn"],
        );
        let source = format!("for (let i = 0; i < {REGISTERED}; i++) {{ _asyncFn(i); }}");
        {
            let tc = &mut v8::TryCatch::new(scope);
            let code = v8::String::new(tc, &source).unwrap();
            let script = v8::Script::compile(tc, code, None).unwrap();
            assert!(
                script.run(tc).is_some(),
                "async bridge calls should register resolvers, not throw"
            );
            assert!(!tc.has_caught(), "async bridge calls should not throw");
        }
        assert_eq!(
            pending.len(),
            REGISTERED,
            "each _asyncFn call must register a pending resolver Global"
        );

        // The cleanup invoked on every session-thread isolate-drop path. It must
        // empty the registry (resetting every Global<PromiseResolver>) while the
        // isolate is still alive.
        reset_pending_promises(&mut pending);

        assert_eq!(
            pending.len(),
            0,
            "reset_pending_promises must drop all pending resolver Globals before isolate teardown"
        );

        // Isolate is still alive here: the Globals were reset above, so dropping
        // the scope/isolate below honors the V8 lifetime contract.
    }
}
