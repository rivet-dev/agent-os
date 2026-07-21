//! Centralized bounded-queue usage tracker.
//!
//! agentos streams guest output through a *chain* of bounded queues: the
//! V8 -> host event channel, the sidecar stdout/stdin frame queues, and so on.
//! Each queue applies backpressure when full (it parks the producer until the
//! consumer drains) rather than crashing, but backpressure is invisible: a slow
//! host consumer silently stalls a session with nothing in the logs.
//!
//! This module gives that whole chain a single, inspectable home:
//!
//! * Every bounded queue registers a [`QueueGauge`] (with a stable name and its
//!   capacity) in a process-global [`QueueRegistry`].
//! * Producers report depth as they enqueue (either by an exact count for
//!   manually-tracked queues via [`TrackedSyncSender`], or by sampling the live
//!   depth of a Tokio channel via [`QueueGauge::observe_depth`]).
//! * When a queue crosses [`WARN_FILL_PERCENT`] of capacity the gauge emits a
//!   single `warn!`, so "the consumer is falling behind" shows up *before* the
//!   queue saturates and backpressure stalls the session. It re-arms once the
//!   queue drains back below [`REARM_FILL_PERCENT`].
//! * [`queue_snapshot`] returns the live depth / high-water / capacity of every
//!   registered queue for debugging or a status endpoint.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{
    Receiver, RecvError, RecvTimeoutError, SendError, SyncSender, TryRecvError, TrySendError,
};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

/// Fill fraction (percent of capacity) at or above which a queue is considered
/// "near full" and emits a warning. Edge-triggered so a steadily-full queue logs
/// once, not on every enqueue.
pub const WARN_FILL_PERCENT: usize = 80;

/// Fill fraction a near-full queue must drain back below before it will warn
/// again. The gap to [`WARN_FILL_PERCENT`] provides hysteresis so a queue
/// hovering at the threshold does not flap.
pub const REARM_FILL_PERCENT: usize = 50;

/// What class of bounded resource a gauge tracks. Lets a snapshot / a host hook
/// group and reason about limits beyond just queues.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitCategory {
    /// A bounded channel/buffer with enqueue/dequeue flow (the default).
    Queue,
    /// A saturating resource counter (fds, processes, sockets, bytes in use).
    Resource,
    /// A memory/heap envelope.
    Memory,
    /// A CPU or wall-clock execution budget.
    Cpu,
}

impl LimitCategory {
    /// Stable lowercase tag for logs and snapshots.
    pub fn as_str(self) -> &'static str {
        match self {
            LimitCategory::Queue => "queue",
            LimitCategory::Resource => "resource",
            LimitCategory::Memory => "memory",
            LimitCategory::Cpu => "cpu",
        }
    }
}

/// Stable catalog of tracked limits that may emit near-capacity or exhaustion
/// warnings. Keep `website/src/content/docs/docs/features/resource-limits.mdx`
/// in sync when adding, removing, or renaming variants so host-visible warning
/// names and the documented constants do not drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrackedLimit {
    JavascriptEventChannel,
    V8SessionFrames,
    SidecarStdinFrames,
    SidecarStdoutFrames,
    CompletedSidecarResponses,
    PendingProcessEvents,
    PendingProcessEventBytes,
    PendingExecutionEvents,
    PendingExecutionEventBytes,
    PendingKernelStdinBytes,
    PendingWasmSignals,
    PendingSidecarResponses,
    OutboundSidecarRequests,
    VmProcesses,
    VmOpenFds,
    VmPipes,
    VmPtys,
    VmSockets,
    VmConnections,
    VmSocketBufferedBytes,
    VmSocketDatagramQueueLen,
    VmFilesystemBytes,
    VmInodes,
    VmRecursiveFsDepth,
    VmRecursiveFsEntries,
    V8HeapBytes,
    V8CpuTimeMs,
    V8WallClockMs,
    WasmFuelMs,
    WasmMemoryBytes,
}

impl TrackedLimit {
    /// Stable lowercase tag emitted in logs, snapshots, and host
    /// `limit_warning` events.
    pub fn as_str(self) -> &'static str {
        match self {
            TrackedLimit::JavascriptEventChannel => "javascript_event_channel",
            TrackedLimit::V8SessionFrames => "v8_session_frames",
            TrackedLimit::SidecarStdinFrames => "sidecar_stdin_frames",
            TrackedLimit::SidecarStdoutFrames => "sidecar_stdout_frames",
            TrackedLimit::CompletedSidecarResponses => "completed_sidecar_responses",
            TrackedLimit::PendingProcessEvents => "pending_process_events",
            TrackedLimit::PendingProcessEventBytes => "pending_process_event_bytes",
            TrackedLimit::PendingExecutionEvents => "pending_execution_events",
            TrackedLimit::PendingExecutionEventBytes => "pending_execution_event_bytes",
            TrackedLimit::PendingKernelStdinBytes => "pending_kernel_stdin_bytes",
            TrackedLimit::PendingWasmSignals => "pending_wasm_signals",
            TrackedLimit::PendingSidecarResponses => "pending_sidecar_responses",
            TrackedLimit::OutboundSidecarRequests => "outbound_sidecar_requests",
            TrackedLimit::VmProcesses => "vm_processes",
            TrackedLimit::VmOpenFds => "vm_open_fds",
            TrackedLimit::VmPipes => "vm_pipes",
            TrackedLimit::VmPtys => "vm_ptys",
            TrackedLimit::VmSockets => "vm_sockets",
            TrackedLimit::VmConnections => "vm_connections",
            TrackedLimit::VmSocketBufferedBytes => "vm_socket_buffered_bytes",
            TrackedLimit::VmSocketDatagramQueueLen => "vm_socket_datagram_queue_len",
            TrackedLimit::VmFilesystemBytes => "vm_filesystem_bytes",
            TrackedLimit::VmInodes => "vm_inodes",
            TrackedLimit::VmRecursiveFsDepth => "vm_recursive_fs_depth",
            TrackedLimit::VmRecursiveFsEntries => "vm_recursive_fs_entries",
            TrackedLimit::V8HeapBytes => "v8_heap_bytes",
            TrackedLimit::V8CpuTimeMs => "v8_cpu_time_ms",
            TrackedLimit::V8WallClockMs => "v8_wall_clock_ms",
            TrackedLimit::WasmFuelMs => "wasm_fuel_ms",
            TrackedLimit::WasmMemoryBytes => "wasm_memory_bytes",
        }
    }

    pub fn category(self) -> LimitCategory {
        match self {
            TrackedLimit::JavascriptEventChannel
            | TrackedLimit::V8SessionFrames
            | TrackedLimit::SidecarStdinFrames
            | TrackedLimit::SidecarStdoutFrames
            | TrackedLimit::CompletedSidecarResponses
            | TrackedLimit::PendingProcessEvents
            | TrackedLimit::PendingProcessEventBytes
            | TrackedLimit::PendingExecutionEvents
            | TrackedLimit::PendingExecutionEventBytes
            | TrackedLimit::PendingKernelStdinBytes
            | TrackedLimit::PendingWasmSignals
            | TrackedLimit::PendingSidecarResponses
            | TrackedLimit::OutboundSidecarRequests => LimitCategory::Queue,
            TrackedLimit::VmProcesses
            | TrackedLimit::VmOpenFds
            | TrackedLimit::VmPipes
            | TrackedLimit::VmPtys
            | TrackedLimit::VmSockets
            | TrackedLimit::VmConnections
            | TrackedLimit::VmSocketBufferedBytes
            | TrackedLimit::VmSocketDatagramQueueLen
            | TrackedLimit::VmFilesystemBytes
            | TrackedLimit::VmInodes
            | TrackedLimit::VmRecursiveFsDepth
            | TrackedLimit::VmRecursiveFsEntries => LimitCategory::Resource,
            TrackedLimit::V8HeapBytes | TrackedLimit::WasmMemoryBytes => LimitCategory::Memory,
            TrackedLimit::V8CpuTimeMs | TrackedLimit::V8WallClockMs | TrackedLimit::WasmFuelMs => {
                LimitCategory::Cpu
            }
        }
    }
}

/// A near-capacity event for one limit, delivered to the global warning sink at
/// the same edge as the `tracing::warn!`. This is the structured payload a host
/// hook (e.g. agentOS `onLimitWarning`) is built from.
#[derive(Debug, Clone)]
pub struct LimitWarning {
    pub name: TrackedLimit,
    pub category: LimitCategory,
    pub observed: usize,
    pub capacity: usize,
    pub fill_percent: usize,
}

type LimitWarningHandler = Arc<dyn Fn(&LimitWarning) + Send + Sync>;

fn warning_handler_slot() -> &'static Mutex<Option<LimitWarningHandler>> {
    static HANDLER: OnceLock<Mutex<Option<LimitWarningHandler>>> = OnceLock::new();
    HANDLER.get_or_init(|| Mutex::new(None))
}

/// Install a process-global sink that is invoked on the same edge-triggered,
/// hysteresis-gated boundary as the `tracing::warn!` whenever a tracked limit
/// crosses [`WARN_FILL_PERCENT`]. The sidecar uses this to forward limit warnings
/// to the host as structured events (the `onLimitWarning` hook). The handler must
/// be cheap and non-blocking; it runs on the producer's thread.
pub fn set_limit_warning_handler(handler: Box<dyn Fn(&LimitWarning) + Send + Sync>) {
    if let Ok(mut slot) = warning_handler_slot().lock() {
        *slot = Some(Arc::from(handler));
    }
}

fn dispatch_warning(warning: &LimitWarning) {
    // Clone the handler Arc out and DROP the registry mutex before invoking it,
    // so the handler never runs while we hold the global lock. The sink can be
    // reached while a kernel lock is held (e.g. fd_tables -> warning_mutex ->
    // handler); keeping the invocation outside the mutex avoids a lock-order
    // hazard if the handler ever does non-trivial work.
    let handler = match warning_handler_slot().lock() {
        Ok(slot) => slot.as_ref().cloned(),
        Err(_) => None,
    };
    if let Some(handler) = handler {
        handler(warning);
    }
}

/// Emit a structured/logged warning for a limit that has already been exhausted.
/// Use this for runtime caps such as CPU or heap exhaustion where there is no
/// continuously sampled queue depth to observe before the terminal edge.
pub fn warn_limit_exhausted(name: TrackedLimit, observed: usize, capacity: usize) {
    let fill_percent = observed
        .saturating_mul(100)
        .checked_div(capacity)
        .unwrap_or(0);
    let category = name.category();
    tracing::warn!(
        limit = name.as_str(),
        category = category.as_str(),
        observed,
        capacity,
        fill_percent,
        "bounded limit exhausted"
    );
    dispatch_warning(&LimitWarning {
        name,
        category,
        observed,
        capacity,
        fill_percent,
    });
}

/// Live usage gauge for a single bounded queue.
///
/// Cloneable handles share one gauge through an [`Arc`]; the registry keeps a
/// [`Weak`] so a gauge is auto-pruned from snapshots once its queue is dropped.
#[derive(Debug)]
pub struct QueueGauge {
    name: TrackedLimit,
    category: LimitCategory,
    capacity: usize,
    depth: AtomicUsize,
    high_water: AtomicUsize,
    warned: AtomicBool,
}

impl QueueGauge {
    fn new(name: TrackedLimit, capacity: usize, category: LimitCategory) -> Self {
        Self {
            name,
            category,
            capacity,
            depth: AtomicUsize::new(0),
            high_water: AtomicUsize::new(0),
            warned: AtomicBool::new(false),
        }
    }

    /// Stable limit name (used in logs and snapshots).
    pub fn name(&self) -> TrackedLimit {
        self.name
    }

    /// The class of bounded resource this gauge tracks.
    pub fn category(&self) -> LimitCategory {
        self.category
    }

    /// Configured queue capacity (slots). `0` means unbounded / untracked.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Current observed depth.
    pub fn depth(&self) -> usize {
        self.depth.load(Ordering::Acquire)
    }

    /// Highest depth observed over the gauge's lifetime.
    pub fn high_water(&self) -> usize {
        self.high_water.load(Ordering::Acquire)
    }

    /// Fill fraction (0–100) at the given depth. Saturates rather than dividing
    /// by zero for untracked (capacity 0) queues.
    fn fill_percent(&self, depth: usize) -> usize {
        depth
            .saturating_mul(100)
            .checked_div(self.capacity)
            .unwrap_or(0)
    }

    /// Record a new depth: refresh the high-water mark and emit an edge-triggered
    /// near-capacity warning (or recovery debug line).
    fn evaluate(&self, depth: usize) {
        self.high_water.fetch_max(depth, Ordering::AcqRel);
        if self.capacity == 0 {
            return;
        }
        let percent = self.fill_percent(depth);
        if percent >= WARN_FILL_PERCENT {
            if !self.warned.swap(true, Ordering::AcqRel) {
                tracing::warn!(
                    limit = self.name.as_str(),
                    category = self.category.as_str(),
                    observed = depth,
                    capacity = self.capacity,
                    fill_percent = percent,
                    "bounded limit near capacity"
                );
                // Same edge as the log: notify the structured warning sink so the
                // host can surface it (e.g. an onLimitWarning hook). Edge-triggered
                // + hysteresis keep this from firing more than once per crossing.
                dispatch_warning(&LimitWarning {
                    name: self.name,
                    category: self.category,
                    observed: depth,
                    capacity: self.capacity,
                    fill_percent: percent,
                });
            }
        } else if percent <= REARM_FILL_PERCENT && self.warned.swap(false, Ordering::AcqRel) {
            tracing::debug!(
                limit = self.name.as_str(),
                category = self.category.as_str(),
                depth,
                capacity = self.capacity,
                fill_percent = percent,
                "bounded limit drained back below threshold"
            );
        }
    }

    /// Report the queue's exact current depth (for queues whose backing channel
    /// exposes its live length, e.g. a Tokio mpsc via `max_capacity - capacity`).
    pub fn observe_depth(&self, depth: usize) {
        self.depth.store(depth, Ordering::Release);
        self.evaluate(depth);
    }

    /// Account for one item entering the queue (for manually-tracked queues).
    pub fn record_enqueue(&self) {
        let depth = self.depth.fetch_add(1, Ordering::AcqRel) + 1;
        self.evaluate(depth);
    }

    /// Account for one item leaving the queue. Saturates at zero so a stray
    /// dequeue can never underflow the depth counter. Re-evaluates so a gauge
    /// that latched "warned" while full re-arms once the queue drains back below
    /// the re-arm threshold, even if the producer has since gone idle.
    pub fn record_dequeue(&self) {
        let mut current = self.depth.load(Ordering::Acquire);
        loop {
            if current == 0 {
                return;
            }
            match self.depth.compare_exchange_weak(
                current,
                current - 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    self.evaluate(current - 1);
                    break;
                }
                Err(actual) => current = actual,
            }
        }
    }
}

/// Immutable view of a tracked limit's usage, returned by [`queue_snapshot`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueSnapshot {
    pub name: TrackedLimit,
    pub category: LimitCategory,
    pub depth: usize,
    pub high_water: usize,
    pub capacity: usize,
    pub fill_percent: usize,
}

/// Process-global registry of every live [`QueueGauge`].
#[derive(Default)]
pub struct QueueRegistry {
    gauges: Mutex<Vec<Weak<QueueGauge>>>,
}

impl QueueRegistry {
    /// The shared registry. All `agentos` bounded queues register here so
    /// their usage can be inspected from one place.
    pub fn global() -> &'static QueueRegistry {
        static REGISTRY: OnceLock<QueueRegistry> = OnceLock::new();
        REGISTRY.get_or_init(QueueRegistry::default)
    }

    /// Register a new bounded limit and return its gauge. Dropping the returned
    /// `Arc` (and all clones) removes the limit from future snapshots.
    pub fn register(&self, name: TrackedLimit, capacity: usize) -> Arc<QueueGauge> {
        let category = name.category();
        let gauge = Arc::new(QueueGauge::new(name, capacity, category));
        let mut gauges = self.gauges.lock().expect("queue registry mutex poisoned");
        gauges.retain(|weak| weak.strong_count() > 0);
        gauges.push(Arc::downgrade(&gauge));
        gauge
    }

    /// Snapshot the live usage of every registered queue, pruning dead entries.
    pub fn snapshot(&self) -> Vec<QueueSnapshot> {
        let mut gauges = self.gauges.lock().expect("queue registry mutex poisoned");
        gauges.retain(|weak| weak.strong_count() > 0);
        gauges
            .iter()
            .filter_map(Weak::upgrade)
            .map(|gauge| {
                let depth = gauge.depth();
                QueueSnapshot {
                    name: gauge.name(),
                    category: gauge.category(),
                    depth,
                    high_water: gauge.high_water(),
                    capacity: gauge.capacity(),
                    fill_percent: gauge.fill_percent(depth),
                }
            })
            .collect()
    }
}

/// Register a bounded queue (the [`LimitCategory::Queue`] case) with the global
/// registry. Convenience over [`QueueRegistry::global`] + [`QueueRegistry::register`].
pub fn register_queue(name: TrackedLimit, capacity: usize) -> Arc<QueueGauge> {
    debug_assert_eq!(name.category(), LimitCategory::Queue);
    QueueRegistry::global().register(name, capacity)
}

/// Register a non-queue bounded limit (a saturating resource or memory envelope)
/// with the global registry, so it shares the same approach-warning + snapshot
/// machinery as queues. Observe usage with [`QueueGauge::observe_depth`].
pub fn register_limit(name: TrackedLimit, capacity: usize) -> Arc<QueueGauge> {
    QueueRegistry::global().register(name, capacity)
}

/// Snapshot every registered queue from the global registry.
pub fn queue_snapshot() -> Vec<QueueSnapshot> {
    QueueRegistry::global().snapshot()
}

/// Emit a `debug!` line for every registered queue. Useful for an on-demand dump
/// of the queue chain when diagnosing a stall.
pub fn log_queue_snapshot() {
    for stat in queue_snapshot() {
        tracing::debug!(
            limit = stat.name.as_str(),
            category = stat.category.as_str(),
            depth = stat.depth,
            high_water = stat.high_water,
            capacity = stat.capacity,
            fill_percent = stat.fill_percent,
            "limit usage"
        );
    }
}

/// A `std::sync::mpsc::SyncSender` that feeds a [`QueueGauge`] as items flow
/// through it, so a queue whose backing channel cannot report its own length
/// still participates in the centralized tracker.
///
/// `send` keeps the underlying blocking-backpressure semantics; it just records
/// the enqueue first so near-capacity warnings fire as the queue fills.
#[derive(Debug)]
pub struct TrackedSyncSender<T> {
    inner: SyncSender<T>,
    gauge: Arc<QueueGauge>,
}

impl<T> Clone for TrackedSyncSender<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            gauge: Arc::clone(&self.gauge),
        }
    }
}

impl<T> TrackedSyncSender<T> {
    /// Blocking send: record the enqueue, then hand off to the bounded channel
    /// (which parks the caller until a slot is free: clean backpressure).
    pub fn send(&self, value: T) -> Result<(), SendError<T>> {
        self.gauge.record_enqueue();
        self.inner.send(value)
    }

    /// Non-blocking send: record the enqueue only on success. Lets a caller with
    /// its own deadline poll instead of parking indefinitely on a full queue.
    pub fn try_send(&self, value: T) -> Result<(), TrySendError<T>> {
        match self.inner.try_send(value) {
            Ok(()) => {
                self.gauge.record_enqueue();
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    /// The gauge backing this sender.
    pub fn gauge(&self) -> &Arc<QueueGauge> {
        &self.gauge
    }
}

/// Receiver half of a [`tracked_sync_channel`]; records a dequeue for every
/// item it yields so the gauge depth tracks the real backlog.
#[derive(Debug)]
pub struct TrackedReceiver<T> {
    inner: Receiver<T>,
    gauge: Arc<QueueGauge>,
}

impl<T> TrackedReceiver<T> {
    /// Blocking receive that decrements the gauge for the item it returns.
    pub fn recv(&self) -> Result<T, RecvError> {
        let value = self.inner.recv()?;
        self.gauge.record_dequeue();
        Ok(value)
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<T, RecvTimeoutError> {
        let value = self.inner.recv_timeout(timeout)?;
        self.gauge.record_dequeue();
        Ok(value)
    }

    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        let value = self.inner.try_recv()?;
        self.gauge.record_dequeue();
        Ok(value)
    }
}

/// Create a bounded `std::sync::mpsc` sync-channel whose depth is tracked by a
/// registered [`QueueGauge`]. Drop-in for `std::sync::mpsc::sync_channel` plus
/// centralized usage tracking + near-capacity warnings.
pub fn tracked_sync_channel<T>(
    name: TrackedLimit,
    capacity: usize,
) -> (TrackedSyncSender<T>, TrackedReceiver<T>) {
    let (tx, rx) = std::sync::mpsc::sync_channel(capacity);
    let gauge = register_queue(name, capacity);
    (
        TrackedSyncSender {
            inner: tx,
            gauge: Arc::clone(&gauge),
        },
        TrackedReceiver { inner: rx, gauge },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn warning_handler_test_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn gauge_tracks_depth_and_high_water() {
        let gauge = QueueGauge::new(
            TrackedLimit::JavascriptEventChannel,
            10,
            LimitCategory::Queue,
        );
        assert_eq!(gauge.depth(), 0);
        gauge.record_enqueue();
        gauge.record_enqueue();
        assert_eq!(gauge.depth(), 2);
        assert_eq!(gauge.high_water(), 2);
        gauge.record_dequeue();
        assert_eq!(gauge.depth(), 1);
        // High-water never regresses.
        assert_eq!(gauge.high_water(), 2);
        // Dequeue never underflows below zero.
        gauge.record_dequeue();
        gauge.record_dequeue();
        assert_eq!(gauge.depth(), 0);
    }

    #[test]
    fn gauge_warn_flag_is_edge_triggered_with_hysteresis() {
        let gauge = QueueGauge::new(TrackedLimit::V8SessionFrames, 10, LimitCategory::Queue);
        // Below 80%: not warned.
        gauge.observe_depth(7);
        assert!(!gauge.warned.load(Ordering::Acquire));
        // Cross 80%: warned.
        gauge.observe_depth(8);
        assert!(gauge.warned.load(Ordering::Acquire));
        // Still near full: stays armed (single warning, not re-fired).
        gauge.observe_depth(9);
        assert!(gauge.warned.load(Ordering::Acquire));
        // Drain to <=50%: re-arms.
        gauge.observe_depth(5);
        assert!(!gauge.warned.load(Ordering::Acquire));
    }

    #[test]
    fn gauge_rearms_on_dequeue_drain() {
        // record_enqueue/record_dequeue gauges (TrackedSyncSender/Receiver) must
        // also re-arm as they drain, not only stay latched after the producer idles.
        let gauge = QueueGauge::new(TrackedLimit::SidecarStdoutFrames, 10, LimitCategory::Queue);
        for _ in 0..9 {
            gauge.record_enqueue(); // climbs to 90% -> warned
        }
        assert_eq!(gauge.depth(), 9);
        assert!(gauge.warned.load(Ordering::Acquire));
        for _ in 0..6 {
            gauge.record_dequeue(); // drains to 30% (<=50%) -> re-arm on dequeue
        }
        assert_eq!(gauge.depth(), 3);
        assert!(!gauge.warned.load(Ordering::Acquire));
    }

    #[test]
    fn tracked_channel_reports_usage_through_registry() {
        let (tx, rx) = tracked_sync_channel::<u32>(TrackedLimit::SidecarStdoutFrames, 4);
        tx.send(1).unwrap();
        tx.send(2).unwrap();

        let snapshot = queue_snapshot();
        let entry = snapshot
            .iter()
            .find(|stat| stat.name == TrackedLimit::SidecarStdoutFrames)
            .expect("registered queue should appear in snapshot");
        assert_eq!(entry.depth, 2);
        assert_eq!(entry.capacity, 4);
        assert_eq!(entry.high_water, 2);
        assert_eq!(entry.fill_percent, 50);
        assert_eq!(entry.category, LimitCategory::Queue);

        assert_eq!(rx.recv().unwrap(), 1);
        assert_eq!(tx.gauge().depth(), 1);

        // Dropping the channel removes it from later snapshots.
        drop(tx);
        drop(rx);
        assert!(queue_snapshot()
            .iter()
            .all(|stat| stat.name != TrackedLimit::SidecarStdoutFrames));
    }

    #[test]
    fn warning_sink_fires_once_per_crossing() {
        let _handler_guard = warning_handler_test_lock()
            .lock()
            .expect("warning-handler test lock");
        let captured: Arc<Mutex<Vec<LimitWarning>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&captured);
        // The handler is global; filter by our unique name so a gauge from a
        // concurrently-running test can never pollute this assertion.
        set_limit_warning_handler(Box::new(move |warning| {
            if warning.name == TrackedLimit::VmPipes {
                sink.lock().expect("sink mutex").push(warning.clone());
            }
        }));

        let gauge = register_limit(TrackedLimit::VmPipes, 10);
        gauge.observe_depth(7); // below 80%: no warning
        assert!(captured.lock().unwrap().is_empty());
        gauge.observe_depth(9); // crosses 80%: fires once
        gauge.observe_depth(10); // still near full: must NOT re-fire (edge-triggered)

        let warnings = captured.lock().unwrap();
        assert_eq!(
            warnings.len(),
            1,
            "warning sink must fire once per crossing"
        );
        assert_eq!(warnings[0].category, LimitCategory::Resource);
        assert_eq!(warnings[0].capacity, 10);
        assert!(warnings[0].fill_percent >= WARN_FILL_PERCENT);
    }

    #[test]
    fn exhausted_warning_sink_fires_immediately() {
        let _handler_guard = warning_handler_test_lock()
            .lock()
            .expect("warning-handler test lock");
        let captured: Arc<Mutex<Vec<LimitWarning>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&captured);
        set_limit_warning_handler(Box::new(move |warning| {
            if warning.name == TrackedLimit::V8CpuTimeMs {
                sink.lock().expect("sink mutex").push(warning.clone());
            }
        }));

        warn_limit_exhausted(TrackedLimit::V8CpuTimeMs, 30_000, 30_000);

        let warnings = captured.lock().unwrap();
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].category, LimitCategory::Cpu);
        assert_eq!(warnings[0].fill_percent, 100);
    }
}
