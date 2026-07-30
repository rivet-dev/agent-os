//! Shared state types used across sidecar domain modules.
//!
//! Contains VM state, session state, configuration types, active process/socket
//! types, and other shared data structures extracted from service.rs.

use crate::protocol::{
    ExecutionCompletedResponse, ExecutionDescriptor, ExecutionOutputCapture, ExecutionOutputEvent,
    GuestRuntimeKind, MountDescriptor, ProjectedModuleDescriptor, RegisterHostCallbacksRequest,
    SidecarRequestFrame, SidecarRequestPayload, SidecarResponseFrame, SidecarResponsePayload,
    SignalHandlerRegistration, SoftwareDescriptor, WasmPermissionTier,
};
use crate::wire::DEFAULT_MAX_FRAME_BYTES;
use agentos_bridge::{
    queue_tracker::{self, QueueGauge, TrackedLimit},
    BridgeTypes, FilesystemSnapshot,
};
use agentos_execution::{
    backend::{
        DescendantOutputOwnership, DescendantWaitOwnership, DirectHostReplyHandle,
        ExecutionBackendKind, ExecutionEvent, ExecutionWakeHandle, HostServiceError,
    },
    host::{
        BoundedUsize, BoundedVec, KernelPollInterest, SocketAddress,
        SocketDomain as HostSocketDomain, SocketKind as HostSocketKind, WaitTarget,
    },
    HostRpcRequest, JavascriptExecution, PythonExecution, StandaloneWasmBackend, WasmExecution,
};
use agentos_kernel::fd_table::TransferredFd;
use agentos_kernel::kernel::{KernelProcessHandle, KernelVm};
use agentos_kernel::mount_table::MountTable;
use agentos_kernel::root_fs::RootFilesystemMode;
use agentos_kernel::socket_table::SocketId;
use agentos_native_sidecar_core::VmLayerStore;
use agentos_runtime_tokio::accounting::{
    LimitError, Reservation, ResourceClass, ResourceLedger, SharedReservation,
};
use agentos_runtime_tokio::fairness::FairWorkTurn;
use agentos_runtime_tokio::RuntimeContext;
use agentos_vm_config as vm_config;
use agentos_vm_config::PermissionsPolicy;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use socket2::Socket;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::error::Error;
use std::fmt;
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::{Receiver as TokioReceiver, Sender as TokioSender};
use tokio::sync::oneshot::Sender as SyncSender;
use tokio::sync::Notify;

const DEFAULT_MAX_SOCKET_READINESS_SUBSCRIBERS: usize = 16_384;

// ---------------------------------------------------------------------------
// Type aliases
// ---------------------------------------------------------------------------

pub(crate) type BridgeError<B> = <B as BridgeTypes>::Error;
pub(crate) type SidecarKernel = KernelVm<MountTable>;
pub(crate) type KernelSocketReadinessRegistry = Arc<KernelSocketReadinessRegistryState>;
pub(crate) type HostNetTransferDescriptionRegistry =
    Arc<Mutex<BTreeMap<usize, HostNetTransferDescription>>>;
pub(crate) type ManagedHostNetDescriptionRegistry =
    Arc<Mutex<BTreeMap<u64, ManagedHostNetDescription>>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeferredGuestWaitKind {
    Process { target: WaitTarget, options: u32 },
    Sleep,
}

#[derive(Debug)]
pub(crate) struct DeferredGuestWait {
    pub(crate) kind: DeferredGuestWaitKind,
    pub(crate) reply: DirectHostReplyHandle,
    pub(crate) deadline: Option<Instant>,
    pub(crate) wake_task: Option<tokio::task::JoinHandle<()>>,
}

/// One bounded kernel-poll request parked on the executor's direct reply
/// lane. The kernel remains the readiness source of truth; the retained task
/// owns only cloneable notifier/deadline state and authorizes a later
/// zero-timeout probe on the sidecar owner thread.
#[derive(Debug)]
pub(crate) struct DeferredKernelPoll {
    pub(crate) interests: BoundedVec<KernelPollInterest>,
    pub(crate) reply: DirectHostReplyHandle,
    pub(crate) deadline: Option<Instant>,
    pub(crate) wake_task: Option<tokio::task::JoinHandle<()>>,
    /// Kernel-owned temporary mask scope for a combined ppoll. The sidecar
    /// restores this before publishing a caught signal or settling the reply.
    pub(crate) temporary_signal_mask_token: Option<u64>,
    pub(crate) temporary_signal_thread_id: Option<u32>,
    /// Distinguishes the combined kernel/managed-fd path from the legacy
    /// kernel-only compatibility operation.
    pub(crate) combined: bool,
}

/// One bounded descriptor read parked on the executor's direct reply lane.
/// The sidecar owner thread performs every destructive read; the retained task
/// owns only cloneable readiness/deadline state and schedules a later probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeferredKernelReadResponse {
    DescriptorBytes,
    KernelStdin,
}

#[derive(Debug)]
pub(crate) struct DeferredKernelRead {
    pub(crate) fd: u32,
    pub(crate) max_bytes: BoundedUsize,
    pub(crate) response: DeferredKernelReadResponse,
    pub(crate) reply: DirectHostReplyHandle,
    pub(crate) deadline: Instant,
    pub(crate) wake_task: Option<tokio::task::JoinHandle<()>>,
}

/// Sidecar-owned semantic state for one compatibility WASM host-network open
/// description. The kernel description id is the map key, so dup/fork and
/// SCM_RIGHTS aliases observe one transport and one option/address lifecycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ManagedHostNetRoute {
    Unbound,
    TcpBound { reservation_id: String },
    UnixBound { listener_id: String },
    TcpSocket(String),
    UnixSocket(String),
    TcpListener(String),
    UnixListener(String),
    UdpSocket(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManagedHostNetDescription {
    pub(crate) domain: HostSocketDomain,
    pub(crate) kind: HostSocketKind,
    pub(crate) lease: Arc<TransferredFd>,
    /// Per-process reactor projection for this one canonical kernel
    /// description. Mutable guest semantics below are shared once per
    /// description; only the opaque process-local resource id varies.
    pub(crate) routes: BTreeMap<u32, ManagedHostNetRoute>,
    pub(crate) bound_address: Option<SocketAddress>,
    pub(crate) local_address: Option<SocketAddress>,
    pub(crate) peer_address: Option<SocketAddress>,
    pub(crate) receive_timeout_ms: Option<u64>,
    pub(crate) reuse_address: bool,
    pub(crate) linger_enabled: bool,
    pub(crate) linger_seconds: u32,
    pub(crate) no_delay: bool,
    pub(crate) keep_alive: bool,
}

impl ManagedHostNetDescription {
    pub(crate) fn new(
        domain: HostSocketDomain,
        kind: HostSocketKind,
        lease: TransferredFd,
        kernel_pid: u32,
    ) -> Self {
        let mut routes = BTreeMap::new();
        routes.insert(kernel_pid, ManagedHostNetRoute::Unbound);
        Self {
            domain,
            kind,
            lease: Arc::new(lease),
            routes,
            bound_address: None,
            local_address: None,
            peer_address: None,
            receive_timeout_ms: None,
            reuse_address: false,
            linger_enabled: false,
            linger_seconds: 0,
            no_delay: false,
            keep_alive: false,
        }
    }

    pub(crate) fn route_for(&self, kernel_pid: u32) -> Option<&ManagedHostNetRoute> {
        self.routes.get(&kernel_pid)
    }
}

/// Retains the first capability lease committed for one open socket
/// description. Process-local aliases may own additional leases, but the
/// original reservation and registry row must survive until the final
/// dup/SCM_RIGHTS alias drops.
#[derive(Debug, Default)]
pub(crate) struct SocketDescriptionLease {
    lease: Mutex<Option<Arc<agentos_runtime_tokio::capability::CapabilityLease>>>,
}

impl SocketDescriptionLease {
    pub(crate) fn retain(&self, lease: Arc<agentos_runtime_tokio::capability::CapabilityLease>) {
        let mut retained = self.lease.lock().unwrap_or_else(|error| error.into_inner());
        if retained.is_none() {
            *retained = Some(lease);
        }
    }

    pub(crate) fn is_retained(&self) -> bool {
        self.lease
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_some()
    }
}

/// Keeps the scheduler identity for an open socket description alive across
/// SCM_RIGHTS aliases. The first capability owns the transport's fairness
/// membership; aliases may come and go without retiring work shared by the
/// underlying description.
#[derive(Debug)]
pub(crate) struct SocketFairnessRetirement {
    pub(crate) identity: Arc<OnceLock<(u64, u64)>>,
    runtime: RuntimeContext,
}

/// Removes one accepted connection from its listener exactly when the final
/// alias of the accepted open description disappears.
#[derive(Debug)]
pub(crate) struct ListenerConnectionRetirement {
    connections: std::sync::Weak<Mutex<BTreeSet<String>>>,
    socket_id: String,
}

impl ListenerConnectionRetirement {
    pub(crate) fn new(connections: &Arc<Mutex<BTreeSet<String>>>, socket_id: String) -> Arc<Self> {
        Arc::new(Self {
            connections: Arc::downgrade(connections),
            socket_id,
        })
    }
}

impl Drop for ListenerConnectionRetirement {
    fn drop(&mut self) {
        if let Some(connections) = self.connections.upgrade() {
            connections
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&self.socket_id);
        }
    }
}

impl SocketFairnessRetirement {
    pub(crate) fn new(identity: Arc<OnceLock<(u64, u64)>>, runtime: RuntimeContext) -> Arc<Self> {
        Arc::new(Self { identity, runtime })
    }
}

impl Drop for SocketFairnessRetirement {
    fn drop(&mut self) {
        let Some((capability_id, vm_generation)) = self.identity.get().copied() else {
            return;
        };
        if let Err(error) = self
            .runtime
            .fairness()
            .retire_capability(vm_generation, capability_id)
        {
            eprintln!(
                "ERR_AGENTOS_FAIRNESS_RETIRE: socket-description capability={capability_id} vm_generation={vm_generation}: {error}"
            );
        }
    }
}

/// One VM-wide retained-byte envelope shared by every process queue of a
/// particular class. Per-process limits remain independently enforced; this
/// aggregate prevents `maxProcesses` from multiplying the VM's memory bound.
#[derive(Debug)]
pub(crate) struct VmPendingByteBudget {
    used: AtomicUsize,
    limit: usize,
    gauge: Arc<QueueGauge>,
}

impl VmPendingByteBudget {
    pub(crate) fn new(limit: usize, tracked_limit: TrackedLimit) -> Arc<Self> {
        Arc::new(Self {
            used: AtomicUsize::new(0),
            limit,
            gauge: queue_tracker::register_queue(tracked_limit, limit),
        })
    }

    pub(crate) fn try_reserve(&self, bytes: usize) -> bool {
        if bytes == 0 {
            return true;
        }
        let reserved = self
            .used
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current
                    .checked_add(bytes)
                    .filter(|next| *next <= self.limit)
            });
        match reserved {
            Ok(previous) => {
                self.gauge.observe_depth(previous.saturating_add(bytes));
                true
            }
            Err(current) => {
                self.gauge.observe_depth(current);
                false
            }
        }
    }

    pub(crate) fn release(&self, bytes: usize) {
        if bytes == 0 {
            return;
        }
        let mut current = self.used.load(Ordering::Acquire);
        loop {
            let Some(next) = current.checked_sub(bytes) else {
                tracing::error!(
                    released_bytes = bytes,
                    accounted_bytes = current,
                    limit = self.limit,
                    "pending-byte aggregate release exceeded accounted usage"
                );
                self.gauge.observe_depth(current);
                return;
            };
            match self.used.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    self.gauge.observe_depth(next);
                    return;
                }
                Err(actual) => current = actual,
            }
        }
    }

    pub(crate) fn used(&self) -> usize {
        self.used.load(Ordering::Acquire)
    }

    pub(crate) fn limit(&self) -> usize {
        self.limit
    }
}

/// Exact RAII ownership of one VM pending-budget reservation. A failed launch,
/// completed child, or process teardown all reclaim capacity through the same
/// drop path.
#[derive(Debug)]
pub(crate) struct VmPendingBudgetReservation {
    budget: Arc<VmPendingByteBudget>,
    amount: usize,
}

impl VmPendingBudgetReservation {
    pub(crate) fn try_new(budget: Arc<VmPendingByteBudget>, amount: usize) -> Option<Self> {
        budget.try_reserve(amount).then(|| Self { budget, amount })
    }
}

impl Drop for VmPendingBudgetReservation {
    fn drop(&mut self) {
        self.budget.release(self.amount);
    }
}

#[derive(Debug)]
pub(crate) struct HostNetTransferDescription {
    pub(crate) handles: Weak<()>,
    pub(crate) connected: bool,
}

#[derive(Debug)]
struct RealIntervalTimerState {
    deadline: Option<Instant>,
    interval: Duration,
    pending_expiry: bool,
}

/// Sidecar-clocked ITIMER_REAL state. Expiration is advanced lazily when the
/// WASM runner queries timer/signal state at a syscall boundary. This matches
/// standard coalesced SIGALRM behavior without a thread or Tokio task per VM.
pub(crate) struct ActiveRealIntervalTimer {
    state: Mutex<RealIntervalTimerState>,
}

impl ActiveRealIntervalTimer {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(RealIntervalTimerState {
                deadline: None,
                interval: Duration::ZERO,
                pending_expiry: false,
            }),
        }
    }

    pub(crate) fn get(&self) -> (u64, u64) {
        let mut timer = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let now = Instant::now();
        refresh_real_interval_timer(&mut timer, now);
        real_interval_timer_values(&timer, now)
    }

    pub(crate) fn set(&self, value_us: u64, interval_us: u64) -> (u64, u64, bool) {
        let mut timer = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let now = Instant::now();
        refresh_real_interval_timer(&mut timer, now);
        let previous = real_interval_timer_values(&timer, now);
        // Replacing or disabling a timer must atomically transfer an expiry
        // that became due before this call to the kernel signal plane. Leaving
        // the executor-local bit set would publish a second SIGALRM after an
        // already-pending blocked signal is unmasked.
        let pending_expiry = std::mem::take(&mut timer.pending_expiry);
        timer.deadline = (value_us != 0)
            .then(|| now.checked_add(Duration::from_micros(value_us)))
            .flatten();
        timer.interval = Duration::from_micros(interval_us);
        (previous.0, previous.1, pending_expiry)
    }

    pub(crate) fn take_expiry(&self) -> bool {
        let mut timer = self.state.lock().unwrap_or_else(|error| error.into_inner());
        refresh_real_interval_timer(&mut timer, Instant::now());
        std::mem::take(&mut timer.pending_expiry)
    }

    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        let mut timer = self.state.lock().unwrap_or_else(|error| error.into_inner());
        refresh_real_interval_timer(&mut timer, Instant::now());
        timer.deadline
    }
}

fn refresh_real_interval_timer(timer: &mut RealIntervalTimerState, now: Instant) {
    let Some(deadline) = timer.deadline else {
        return;
    };
    if now < deadline {
        return;
    }

    timer.pending_expiry = true;
    if timer.interval.is_zero() {
        timer.deadline = None;
        return;
    }

    let interval_nanos = timer.interval.as_nanos();
    let elapsed_nanos = now.saturating_duration_since(deadline).as_nanos();
    let remainder_nanos = elapsed_nanos % interval_nanos;
    let until_next_nanos = interval_nanos - remainder_nanos;
    let until_next = Duration::new(
        (until_next_nanos / 1_000_000_000) as u64,
        (until_next_nanos % 1_000_000_000) as u32,
    );
    timer.deadline = now.checked_add(until_next);
}

fn real_interval_timer_values(timer: &RealIntervalTimerState, now: Instant) -> (u64, u64) {
    let remaining = timer
        .deadline
        .map(|deadline| deadline.saturating_duration_since(now).as_micros())
        .unwrap_or_default()
        .min(u128::from(u64::MAX)) as u64;
    let interval = timer.interval.as_micros().min(u128::from(u64::MAX)) as u64;
    (remaining, interval)
}

#[cfg(test)]
mod real_interval_timer_tests {
    use super::*;

    #[test]
    fn one_shot_expiry_is_coalesced_and_disarmed() {
        let now = Instant::now();
        let mut timer = RealIntervalTimerState {
            deadline: now.checked_sub(Duration::from_millis(1)),
            interval: Duration::ZERO,
            pending_expiry: false,
        };

        refresh_real_interval_timer(&mut timer, now);
        assert!(timer.pending_expiry);
        assert_eq!(timer.deadline, None);

        refresh_real_interval_timer(&mut timer, now + Duration::from_secs(1));
        assert!(
            timer.pending_expiry,
            "expiry remains one coalesced pending bit"
        );
    }

    #[test]
    fn periodic_expiry_advances_to_first_deadline_after_now() {
        let now = Instant::now();
        let interval = Duration::from_millis(10);
        let mut timer = RealIntervalTimerState {
            deadline: now.checked_sub(Duration::from_millis(25)),
            interval,
            pending_expiry: false,
        };

        refresh_real_interval_timer(&mut timer, now);
        let next = timer.deadline.expect("periodic timer remains armed");
        assert!(timer.pending_expiry);
        assert!(next > now);
        assert!(next <= now + interval);

        timer.pending_expiry = false;
        refresh_real_interval_timer(&mut timer, now);
        assert!(
            !timer.pending_expiry,
            "no duplicate expiry before next deadline"
        );
        assert_eq!(timer.deadline, Some(next));
    }

    #[test]
    fn replacement_transfers_one_pending_expiry_and_clears_local_state() {
        let timer = ActiveRealIntervalTimer {
            state: Mutex::new(RealIntervalTimerState {
                deadline: Instant::now().checked_sub(Duration::from_millis(1)),
                interval: Duration::from_millis(10),
                pending_expiry: false,
            }),
        };

        let (_, interval_us, pending_expiry) = timer.set(0, 0);
        assert_eq!(interval_us, 10_000);
        assert!(pending_expiry);
        assert!(!timer.take_expiry());
        assert_eq!(timer.get(), (0, 0));
    }
}

/// One completion admitted against the VM-wide aggregate count. The local
/// channel capacity remains an independent per-lane bound; this reservation
/// prevents N handles from each consuming that capacity simultaneously.
#[derive(Debug)]
struct QueuedAsyncCompletion<T> {
    value: Option<T>,
    _count_reservation: Reservation,
    _byte_reservation: Reservation,
    count_gauge: Arc<QueueGauge>,
    byte_gauge: Arc<QueueGauge>,
    byte_depth: Arc<AtomicUsize>,
    retained_bytes: usize,
}

impl<T> Drop for QueuedAsyncCompletion<T> {
    fn drop(&mut self) {
        self.count_gauge.record_dequeue();
        if self.retained_bytes != 0 {
            let previous = self
                .byte_depth
                .fetch_sub(self.retained_bytes, Ordering::AcqRel);
            self.byte_gauge
                .observe_depth(previous.saturating_sub(self.retained_bytes));
        }
    }
}

pub(crate) struct AsyncCompletionSender<T> {
    inner: TokioSender<QueuedAsyncCompletion<T>>,
    runtime: RuntimeContext,
    retained_bytes: fn(&T) -> usize,
    count_gauge: Arc<QueueGauge>,
    byte_gauge: Arc<QueueGauge>,
    byte_depth: Arc<AtomicUsize>,
}

impl<T> Clone for AsyncCompletionSender<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            runtime: self.runtime.clone(),
            retained_bytes: self.retained_bytes,
            count_gauge: Arc::clone(&self.count_gauge),
            byte_gauge: Arc::clone(&self.byte_gauge),
            byte_depth: Arc::clone(&self.byte_depth),
        }
    }
}

pub(crate) struct AsyncCompletionReceiver<T> {
    inner: TokioReceiver<QueuedAsyncCompletion<T>>,
}

impl<T> fmt::Debug for AsyncCompletionSender<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AsyncCompletionSender")
            .field("capacity", &self.inner.capacity())
            .finish_non_exhaustive()
    }
}

impl<T> fmt::Debug for AsyncCompletionReceiver<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AsyncCompletionReceiver")
            .field("capacity", &self.inner.capacity())
            .finish_non_exhaustive()
    }
}

pub(crate) fn async_completion_channel<T>(
    runtime: RuntimeContext,
    capacity: usize,
    retained_bytes: fn(&T) -> usize,
) -> (AsyncCompletionSender<T>, AsyncCompletionReceiver<T>) {
    let (sender, receiver) = tokio::sync::mpsc::channel(capacity);
    let resources = runtime.resources();
    let count_capacity = resources
        .configured_limit(ResourceClass::AsyncCompletions)
        .map_or(capacity, |limit| limit.maximum);
    let byte_capacity = resources
        .configured_limit(ResourceClass::AsyncCompletionBytes)
        .map_or(0, |limit| limit.maximum);
    let count_gauge =
        queue_tracker::register_queue(TrackedLimit::AsyncCompletionCount, count_capacity);
    let byte_gauge =
        queue_tracker::register_queue(TrackedLimit::AsyncCompletionBytes, byte_capacity);
    let byte_depth = Arc::new(AtomicUsize::new(0));
    (
        AsyncCompletionSender {
            inner: sender,
            runtime,
            retained_bytes,
            count_gauge,
            byte_gauge,
            byte_depth,
        },
        AsyncCompletionReceiver { inner: receiver },
    )
}

impl<T> AsyncCompletionSender<T> {
    pub(crate) async fn send(&self, value: T) -> Result<(), HostServiceError> {
        let resources = Arc::clone(self.runtime.resources());
        let retained_bytes = (self.retained_bytes)(&value);
        reject_impossible_completion_size(&resources, retained_bytes)?;
        let (count_reservation, byte_reservation) = loop {
            if !self.runtime.admission_is_open() {
                return Err(async_completion_error(
                    "ERR_AGENTOS_ASYNC_COMPLETION_CLOSED",
                    "VM completion admission is closed",
                ));
            }
            if self.inner.is_closed() {
                return Err(async_completion_error(
                    "ERR_AGENTOS_ASYNC_COMPLETION_DISCONNECTED",
                    "completion consumer disconnected",
                ));
            }
            match reserve_async_completion(&resources, retained_bytes) {
                Ok(reservations) => break reservations,
                Err(_) => tokio::select! {
                    biased;
                    () = self.runtime.admission_closed() => {
                        return Err(async_completion_error(
                            "ERR_AGENTOS_ASYNC_COMPLETION_CLOSED",
                            "VM completion admission is closed",
                        ));
                    }
                    () = self.inner.closed() => {
                        return Err(async_completion_error(
                            "ERR_AGENTOS_ASYNC_COMPLETION_DISCONNECTED",
                            "completion consumer disconnected",
                        ));
                    }
                    () = resources.capacity_changed() => {}
                },
            }
        };
        self.count_gauge.record_enqueue();
        if retained_bytes != 0 {
            let previous = self.byte_depth.fetch_add(retained_bytes, Ordering::AcqRel);
            self.byte_gauge
                .observe_depth(previous.saturating_add(retained_bytes));
        }
        let queued = QueuedAsyncCompletion {
            value: Some(value),
            _count_reservation: count_reservation,
            _byte_reservation: byte_reservation,
            count_gauge: Arc::clone(&self.count_gauge),
            byte_gauge: Arc::clone(&self.byte_gauge),
            byte_depth: Arc::clone(&self.byte_depth),
            retained_bytes,
        };
        tokio::select! {
            biased;
            () = self.runtime.admission_closed() => Err(async_completion_error(
                "ERR_AGENTOS_ASYNC_COMPLETION_CLOSED",
                "VM completion admission closed before queue insertion",
            )),
            result = self.inner.send(queued) => result.map_err(|_| async_completion_error(
                "ERR_AGENTOS_ASYNC_COMPLETION_DISCONNECTED",
                "completion consumer disconnected",
            )),
        }
    }

    pub(crate) fn try_send(&self, value: T) -> Result<(), HostServiceError> {
        if !self.runtime.admission_is_open() {
            return Err(async_completion_error(
                "ERR_AGENTOS_ASYNC_COMPLETION_CLOSED",
                "VM completion admission is closed",
            ));
        }
        let retained_bytes = (self.retained_bytes)(&value);
        let resources = self.runtime.resources();
        let (count_reservation, byte_reservation) =
            reserve_async_completion(resources, retained_bytes)?;
        self.count_gauge.record_enqueue();
        if retained_bytes != 0 {
            let previous = self.byte_depth.fetch_add(retained_bytes, Ordering::AcqRel);
            self.byte_gauge
                .observe_depth(previous.saturating_add(retained_bytes));
        }
        self.inner
            .try_send(QueuedAsyncCompletion {
                value: Some(value),
                _count_reservation: count_reservation,
                _byte_reservation: byte_reservation,
                count_gauge: Arc::clone(&self.count_gauge),
                byte_gauge: Arc::clone(&self.byte_gauge),
                byte_depth: Arc::clone(&self.byte_depth),
                retained_bytes,
            })
            .map_err(|error| match error {
                tokio::sync::mpsc::error::TrySendError::Full(_) => async_completion_error(
                    "ERR_AGENTOS_ASYNC_COMPLETION_LANE_LIMIT",
                    "completion lane is full; raise limits.reactor.maxAsyncCompletions",
                ),
                tokio::sync::mpsc::error::TrySendError::Closed(_) => async_completion_error(
                    "ERR_AGENTOS_ASYNC_COMPLETION_DISCONNECTED",
                    "completion consumer disconnected",
                ),
            })
    }
}

fn reserve_async_completion(
    resources: &ResourceLedger,
    retained_bytes: usize,
) -> Result<(Reservation, Reservation), HostServiceError> {
    let count = resources
        .reserve(ResourceClass::AsyncCompletions, 1)
        .map_err(async_completion_limit_error)?;
    let bytes = resources
        .reserve(ResourceClass::AsyncCompletionBytes, retained_bytes)
        .map_err(async_completion_limit_error)?;
    Ok((count, bytes))
}

fn reject_impossible_completion_size(
    resources: &ResourceLedger,
    retained_bytes: usize,
) -> Result<(), HostServiceError> {
    if let Some(limit) = resources.configured_limit(ResourceClass::AsyncCompletionBytes) {
        if retained_bytes > limit.maximum {
            return Err(HostServiceError::limit(
                "ERR_AGENTOS_RESOURCE_LIMIT",
                "limits.reactor.maxAsyncCompletionBytes",
                limit.maximum as u64,
                retained_bytes as u64,
            ));
        }
    }
    Ok(())
}

fn async_completion_limit_error(error: LimitError) -> HostServiceError {
    HostServiceError::new("ERR_AGENTOS_RESOURCE_LIMIT", error.to_string()).with_details(json!({
        "scope": error.scope,
        "resource": error.resource.name(),
        "used": error.used,
        "requested": error.requested,
        "limit": error.limit,
        "limitName": error.config_path,
    }))
}

fn async_completion_error(code: &'static str, message: &'static str) -> HostServiceError {
    HostServiceError::new(code, message)
}

impl<T> AsyncCompletionReceiver<T> {
    pub(crate) fn try_recv(&mut self) -> Result<T, tokio::sync::mpsc::error::TryRecvError> {
        self.inner.try_recv().map(|mut queued| {
            queued
                .value
                .take()
                .expect("queued completion contains value")
        })
    }
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub(crate) const EXECUTION_DRIVER_NAME: &str = "agentos-native-sidecar-execution";
pub(crate) const JAVASCRIPT_COMMAND: &str = "node";
pub(crate) const PYTHON_COMMAND: &str = "python";
pub(crate) const WASM_COMMAND: &str = "wasm";
// The Python runtime addresses the whole guest VFS (the kernel enforces fs
// permissions and mount-confinement on every op, identical to what the JS/WASM
// runtimes and `vm.readFile()` see), so the VFS-RPC root is `/`, not a single
// workspace dir.
pub(crate) const EXECUTION_SANDBOX_ROOT_ENV: &str = "AGENTOS_SANDBOX_ROOT";
pub(crate) const WASM_STDIO_SYNC_RPC_ENV: &str = "AGENTOS_WASI_STDIO_SYNC_RPC";
pub(crate) const WASM_EXEC_COMMIT_RPC_ENV: &str = "AGENTOS_WASM_EXEC_COMMIT_RPC";
#[cfg(test)]
#[allow(dead_code)]
pub(crate) const HOST_REALPATH_MAX_SYMLINK_DEPTH: usize = 40;
pub(crate) const DISPOSE_VM_SIGTERM_GRACE: std::time::Duration =
    std::time::Duration::from_millis(100);
pub(crate) const DISPOSE_VM_SIGKILL_GRACE: std::time::Duration =
    std::time::Duration::from_millis(100);
pub(crate) const VM_DNS_SERVERS_METADATA_KEY: &str = "network.dns.servers";
#[cfg(test)]
#[allow(dead_code)]
pub(crate) const VM_LISTEN_PORT_MIN_METADATA_KEY: &str = "network.listen.port_min";
#[cfg(test)]
#[allow(dead_code)]
pub(crate) const VM_LISTEN_PORT_MAX_METADATA_KEY: &str = "network.listen.port_max";
pub(crate) const VM_LISTEN_ALLOW_PRIVILEGED_METADATA_KEY: &str = "network.listen.allow_privileged";
pub(crate) const DEFAULT_NET_BACKLOG: u32 = 511;
pub(crate) const LOOPBACK_EXEMPT_PORTS_ENV: &str = "AGENTOS_LOOPBACK_EXEMPT_PORTS";
pub(crate) const BINDING_DRIVER_NAME: &str = "agentos-host-callbacks";

// ---------------------------------------------------------------------------
// Public API types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct NativeSidecarConfig {
    pub sidecar_id: String,
    pub max_frame_bytes: usize,
    pub compile_cache_root: Option<PathBuf>,
    pub expected_auth_token: Option<String>,
    pub acp_termination_grace: Duration,
    pub runtime: agentos_runtime_tokio::RuntimeConfig,
}

impl Default for NativeSidecarConfig {
    fn default() -> Self {
        Self {
            sidecar_id: String::from("agentos-native-sidecar"),
            max_frame_bytes: DEFAULT_MAX_FRAME_BYTES,
            compile_cache_root: None,
            expected_auth_token: None,
            acp_termination_grace: Duration::from_secs(3),
            runtime: agentos_runtime_tokio::RuntimeConfig::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidecarError {
    ResourceLimit(agentos_runtime_tokio::accounting::LimitError),
    Host(agentos_execution::backend::HostServiceError),
    InvalidState(String),
    ProtocolVersionMismatch(String),
    BridgeVersionMismatch(String),
    Conflict(String),
    Unauthorized(String),
    Unsupported(String),
    FrameTooLarge(String),
    Kernel(String),
    Plugin(String),
    Execution(String),
    ExecutionEventChannelClosed { backend: ExecutionBackendKind },
    Bridge(String),
    Io(String),
}

impl SidecarError {
    pub(crate) fn host(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Host(agentos_execution::backend::HostServiceError::new(
            code, message,
        ))
    }

    pub(crate) fn code(&self) -> Option<&str> {
        match self {
            Self::Host(error) => Some(error.code.as_str()),
            Self::ResourceLimit(_) => Some("ERR_AGENTOS_RESOURCE_LIMIT"),
            _ => None,
        }
    }

    pub(crate) fn host_resource_limit(
        limit_name: &'static str,
        limit: usize,
        observed: usize,
        message: impl Into<String>,
    ) -> Self {
        Self::Host(
            agentos_execution::backend::HostServiceError::new(
                "ERR_AGENTOS_RESOURCE_LIMIT",
                message,
            )
            .with_details(serde_json::json!({
                "limitName": limit_name,
                "limit": limit,
                "observed": observed,
                "configPath": limit_name,
            })),
        )
    }
}

impl fmt::Display for SidecarError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ResourceLimit(error) => error.fmt(f),
            Self::Host(error) => error.fmt(f),
            Self::InvalidState(message)
            | Self::ProtocolVersionMismatch(message)
            | Self::BridgeVersionMismatch(message)
            | Self::Conflict(message)
            | Self::Unauthorized(message)
            | Self::Unsupported(message)
            | Self::FrameTooLarge(message)
            | Self::Kernel(message)
            | Self::Plugin(message)
            | Self::Execution(message)
            | Self::Bridge(message)
            | Self::Io(message) => f.write_str(message),
            Self::ExecutionEventChannelClosed { backend } => {
                write!(f, "{backend:?} execution event channel closed unexpectedly")
            }
        }
    }
}

impl Error for SidecarError {}

impl From<agentos_execution::backend::HostServiceError> for SidecarError {
    fn from(error: agentos_execution::backend::HostServiceError) -> Self {
        Self::Host(error)
    }
}

impl From<agentos_native_sidecar_core::SidecarCoreError> for SidecarError {
    fn from(error: agentos_native_sidecar_core::SidecarCoreError) -> Self {
        match error.code() {
            Some(code) => Self::host(code, error.message()),
            None => Self::InvalidState(error.message().to_owned()),
        }
    }
}

/// Format a resource-limit failure for an untrusted guest. VM-local occupancy
/// is safe to expose to that VM; process occupancy includes other VMs and must
/// not become a cross-tenant resource oracle.
pub(crate) fn guest_limit_message(limit: &agentos_runtime_tokio::accounting::LimitError) -> String {
    if limit.scope.starts_with("vm=") {
        return limit.to_string();
    }
    format!(
        "ERR_AGENTOS_RESOURCE_LIMIT: scope=process resource={} requested={} limit={}; raise {}",
        limit.resource.name(),
        limit.requested,
        limit.limit,
        limit.config_path
    )
}

impl From<agentos_runtime_tokio::accounting::LimitError> for SidecarError {
    fn from(error: agentos_runtime_tokio::accounting::LimitError) -> Self {
        Self::ResourceLimit(error)
    }
}

impl From<agentos_runtime_tokio::capability::CapabilityError> for SidecarError {
    fn from(error: agentos_runtime_tokio::capability::CapabilityError) -> Self {
        match error {
            agentos_runtime_tokio::capability::CapabilityError::Limit(limit) => {
                Self::ResourceLimit(limit)
            }
            other => Self::Execution(other.to_string()),
        }
    }
}

impl From<agentos_runtime_tokio::TaskSpawnError> for SidecarError {
    fn from(error: agentos_runtime_tokio::TaskSpawnError) -> Self {
        match error {
            agentos_runtime_tokio::TaskSpawnError::ResourceLimit(limit) => {
                Self::ResourceLimit(limit)
            }
            agentos_runtime_tokio::TaskSpawnError::AdmissionClosed { scope } => Self::Execution(
                format!("ERR_AGENTOS_TASK_ADMISSION_CLOSED: scope={scope} is closing"),
            ),
        }
    }
}

impl From<agentos_runtime_tokio::BlockingJobError> for SidecarError {
    fn from(error: agentos_runtime_tokio::BlockingJobError) -> Self {
        match error {
            agentos_runtime_tokio::BlockingJobError::ResourceLimit(limit) => {
                Self::ResourceLimit(limit)
            }
            other => Self::Execution(other.to_string()),
        }
    }
}

pub trait SidecarRequestTransport: Send + Sync {
    fn send_request(
        &self,
        request: SidecarRequestFrame,
        timeout: Duration,
    ) -> Result<SidecarResponseFrame, SidecarError>;
}

#[derive(Clone)]
pub(crate) struct SharedSidecarRequestClient {
    transport: Option<Arc<dyn SidecarRequestTransport>>,
    next_request_id: Arc<AtomicI64>,
}

impl Default for SharedSidecarRequestClient {
    fn default() -> Self {
        Self {
            transport: None,
            next_request_id: Arc::new(AtomicI64::new(-1)),
        }
    }
}

impl SharedSidecarRequestClient {
    pub(crate) fn set_transport(&mut self, transport: Arc<dyn SidecarRequestTransport>) {
        self.transport = Some(transport);
    }

    pub(crate) fn invoke(
        &self,
        ownership: crate::protocol::OwnershipScope,
        payload: SidecarRequestPayload,
        timeout: Duration,
    ) -> Result<SidecarResponsePayload, SidecarError> {
        let transport = self.transport.as_ref().ok_or_else(|| {
            SidecarError::Unsupported(String::from("sidecar request transport is not configured"))
        })?;
        let request_id = self.next_request_id.fetch_sub(1, Ordering::Relaxed);
        let request = SidecarRequestFrame::new(request_id, ownership.clone(), payload);
        let response = transport.send_request(request, timeout)?;
        if response.request_id != request_id {
            return Err(SidecarError::InvalidState(format!(
                "sidecar response {} did not match request {request_id}",
                response.request_id
            )));
        }
        if response.ownership != ownership {
            return Err(SidecarError::InvalidState(String::from(
                "sidecar response ownership did not match request ownership",
            )));
        }
        Ok(response.payload)
    }
}

/// Fire-and-forget live event sink. Lets an extension emit a `session/update`
/// (or any other) event frame to the host *mid-dispatch*, instead of having to
/// return it from the dispatch and wait for the whole request to resolve before
/// the stdio loop flushes it. Mirrors `SidecarRequestTransport`, but events have
/// no response, no request id, and no timeout — they are written to the same
/// outbound stdout channel the batch path uses.
pub trait EventSinkTransport: Send + Sync {
    fn emit_event(&self, event: crate::wire::EventFrame) -> Result<(), SidecarError>;
}

#[derive(Clone, Default)]
pub(crate) struct SharedEventSink {
    transport: Option<Arc<dyn EventSinkTransport>>,
}

impl SharedEventSink {
    pub(crate) fn set_transport(&mut self, transport: Arc<dyn EventSinkTransport>) {
        self.transport = Some(transport);
    }

    /// Emit `event` live if a transport is configured (the stdio path). Returns
    /// `Ok(None)` when the event was handed to the live transport, or
    /// `Ok(Some(event))` when no transport is configured (e.g. an in-process
    /// `NativeSidecar` with no stdout loop) so the caller can fall back to the
    /// batch path and still deliver the event when the dispatch resolves.
    pub(crate) fn try_emit(
        &self,
        event: crate::wire::EventFrame,
    ) -> Result<Option<crate::wire::EventFrame>, SidecarError> {
        match self.transport.as_ref() {
            Some(transport) => {
                transport.emit_event(event)?;
                Ok(None)
            }
            None => Ok(Some(event)),
        }
    }
}

// ---------------------------------------------------------------------------
// Bridge wrapper
// ---------------------------------------------------------------------------

pub(crate) struct SharedBridge<B> {
    pub(crate) inner: Arc<Mutex<B>>,
    pub(crate) permissions: Arc<Mutex<BTreeMap<String, PermissionsPolicy>>>,
    #[cfg(test)]
    pub(crate) set_vm_permissions_outcomes: Arc<Mutex<VecDeque<Option<SidecarError>>>>,
    #[cfg(test)]
    pub(crate) emit_lifecycle_outcomes: Arc<Mutex<VecDeque<Option<SidecarError>>>>,
}

impl<B> Clone for SharedBridge<B> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            permissions: Arc::clone(&self.permissions),
            #[cfg(test)]
            set_vm_permissions_outcomes: Arc::clone(&self.set_vm_permissions_outcomes),
            #[cfg(test)]
            emit_lifecycle_outcomes: Arc::clone(&self.emit_lifecycle_outcomes),
        }
    }
}

// ---------------------------------------------------------------------------
// Connection / session / VM state
// ---------------------------------------------------------------------------

#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct ConnectionState {
    pub(crate) auth_token: String,
    pub(crate) sessions: BTreeSet<String>,
}

#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct SessionState {
    pub(crate) connection_id: String,
    pub(crate) placement: crate::protocol::SidecarPlacement,
    pub(crate) metadata: BTreeMap<String, String>,
    pub(crate) vm_ids: BTreeSet<String>,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(crate) struct VmConfiguration {
    pub(crate) mounts: Vec<MountDescriptor>,
    pub(crate) software: Vec<SoftwareDescriptor>,
    pub(crate) permissions: PermissionsPolicy,
    pub(crate) module_access_cwd: Option<String>,
    pub(crate) instructions: Vec<String>,
    pub(crate) projected_modules: Vec<ProjectedModuleDescriptor>,
    pub(crate) command_permissions: BTreeMap<String, WasmPermissionTier>,
    pub(crate) provided_commands: BTreeMap<String, Vec<String>>,
    /// Guest JavaScript host-environment config (platform / module resolution /
    /// builtin allow-list). Set at `create_vm` from `CreateVmConfig.jsRuntime`
    /// and preserved across `configure_vm`. `None` => full Node.js emulation.
    pub(crate) js_runtime: Option<vm_config::JsRuntimeConfig>,
    /// Agent SDK bundle read by the sidecar from the configured package dir and
    /// evaluated into the shared V8 startup snapshot.
    pub(crate) snapshot_userland_code: Option<String>,
    pub(crate) loopback_exempt_ports: Vec<u16>,
}

impl Default for VmConfiguration {
    fn default() -> Self {
        Self {
            mounts: Vec::new(),
            software: Vec::new(),
            permissions: agentos_native_sidecar_core::permissions::deny_all_policy(),
            module_access_cwd: None,
            instructions: Vec::new(),
            projected_modules: Vec::new(),
            command_permissions: BTreeMap::new(),
            provided_commands: BTreeMap::new(),
            js_runtime: None,
            snapshot_userland_code: None,
            loopback_exempt_ports: Vec::new(),
        }
    }
}

#[allow(dead_code)]
pub(crate) struct VmState {
    pub(crate) connection_id: String,
    pub(crate) session_id: String,
    /// Process-unique VM generation. Capability and completion identities are
    /// never valid outside this generation.
    pub(crate) generation: u64,
    /// Operator-tunable VM-scoped runtime limits. Immutable for the VM's lifetime;
    /// `ConfigureVm` does not mutate limits.
    pub(crate) limits: crate::limits::VmLimits,
    pub(crate) pending_stdin_bytes_budget: Arc<VmPendingByteBudget>,
    pub(crate) pending_event_bytes_budget: Arc<VmPendingByteBudget>,
    pub(crate) pending_child_sync_count_budget: Arc<VmPendingByteBudget>,
    pub(crate) pending_child_sync_bytes_budget: Arc<VmPendingByteBudget>,
    /// Child of the one process ledger owned by RuntimeContext.
    pub(crate) resources: Arc<agentos_runtime_tokio::accounting::ResourceLedger>,
    /// VM-scoped admission view over the process's single Tokio runtime and
    /// fixed blocking executor. This owns no runtime or worker of its own.
    pub(crate) runtime_context: agentos_runtime_tokio::RuntimeContext,
    /// One resolved SQLite transport shared by every durable VM subsystem.
    pub(crate) database: Option<crate::vm_sqlite::SharedVmSqliteDatabase>,
    /// Common lifecycle/identity registry for native and kernel backends.
    pub(crate) capabilities: agentos_runtime_tokio::capability::CapabilityRegistry,
    pub(crate) dns: VmDnsConfig,
    pub(crate) listen_policy: VmListenPolicy,
    pub(crate) create_loopback_exempt_ports: BTreeSet<u16>,
    pub(crate) guest_env: BTreeMap<String, String>,
    /// VM-wide standalone-WASM engine policy. JavaScript itself remains on V8;
    /// this affinity is copied to every top-level process for spawn/exec.
    pub(crate) standalone_wasm_backend: StandaloneWasmBackend,
    pub(crate) requested_runtime: GuestRuntimeKind,
    pub(crate) root_filesystem_mode: RootFilesystemMode,
    pub(crate) guest_cwd: String,
    /// Private host directory for executor launch assets and host-backed Unix
    /// socket implementation details. It is never a guest filesystem view;
    /// mutable guest state lives only in `kernel`.
    pub(crate) runtime_scratch_root: PathBuf,
    pub(crate) host_cwd: PathBuf,
    pub(crate) kernel: SidecarKernel,
    pub(crate) kernel_socket_readiness: KernelSocketReadinessRegistry,
    /// Canonical semantic state for sidecar-backed socket descriptions. Kernel
    /// description ids are VM-global, so fork, dup, SCM_RIGHTS, and spawn all
    /// resolve the exact same mutable route/options/address state.
    pub(crate) managed_host_net_descriptions: ManagedHostNetDescriptionRegistry,
    /// Sidecar-only host-network descriptions currently retained by an opaque
    /// SCM_RIGHTS transfer. Weak entries make queue discard/receive lifecycle
    /// automatic while allowing VM-wide limit accounting to see descriptions
    /// that temporarily have no process-map entry.
    pub(crate) host_net_transfer_descriptions: HostNetTransferDescriptionRegistry,
    pub(crate) loaded_snapshot: Option<FilesystemSnapshot>,
    pub(crate) configuration: VmConfiguration,
    pub(crate) layers: VmLayerStore,
    pub(crate) provided_commands: BTreeMap<String, Vec<String>>,
    pub(crate) command_permissions: BTreeMap<String, WasmPermissionTier>,
    pub(crate) bindings: BTreeMap<String, RegisterHostCallbacksRequest>,
    pub(crate) active_processes: BTreeMap<String, ActiveProcess>,
    /// Pull-driven host fetches retained between sidecar requests. A stream
    /// owns exactly one kernel socket and capability lease; reads advance it
    /// only when the trusted client asks for another bounded chunk.
    pub(crate) vm_fetch_streams: BTreeMap<String, VmFetchStreamState>,
    pub(crate) next_vm_fetch_stream_id: u64,
    /// Public language/process executions keyed by their sole lifecycle ID.
    /// Process IDs remain internal routing details in `execution_processes`.
    pub(crate) executions: BTreeMap<String, ManagedLanguageExecution>,
    pub(crate) execution_processes: BTreeMap<String, String>,
    pub(crate) next_public_execution_id: u64,
    pub(crate) execution_retention_wake_deadline_ms: Option<u64>,
    pub(crate) execution_retention_wake_task: Option<tokio::task::JoinHandle<()>>,
    /// The VM filesystem is shared across executions, so package-manager
    /// mutations must never run concurrently.
    pub(crate) package_mutation_execution_id: Option<String>,
    pub(crate) typescript_compiler_staged: bool,
    pub(crate) exited_process_snapshots: VecDeque<ExitedProcessSnapshot>,
    pub(crate) detached_child_processes: BTreeSet<String>,
    /// Rotating start positions for bounded child-process event turns. Durable
    /// runtime queues retain the events; these cursors prevent a hot, sorted
    /// child ID from monopolizing every coalesced wake.
    pub(crate) attached_child_event_cursor: usize,
    pub(crate) detached_child_event_cursor: usize,
    /// Legacy staging root slot retained for same-version internal state shape.
    /// The current `/opt/agentos` projection mounts package tars and synthetic
    /// symlink leaves directly, so this remains `None`.
    pub(crate) packages_staging_root: Option<PathBuf>,
    /// Projected agent launch surface, keyed by agent id. Sourced from the
    /// packed vbare manifests at `ConfigureVm`/`LinkPackage` time — packed
    /// packages ship no `agentos-package.json`, so agent enumeration and
    /// resolution read this instead of the guest filesystem.
    pub(crate) projected_agent_launch: BTreeMap<String, ProjectedAgentLaunch>,
    pub(crate) unix_address_registry: GuestUnixAddressRegistry,
    pub(crate) unix_socket_host_dir: PathBuf,
}

#[derive(Debug)]
pub(crate) enum VmFetchBodyMode {
    Empty,
    ContentLength { remaining: usize },
    Chunked { chunk_remaining: Option<usize> },
    UntilClose,
}

#[derive(Debug)]
pub(crate) struct VmFetchStreamState {
    pub(crate) target_process_id: String,
    pub(crate) kernel_pid: u32,
    pub(crate) socket_id: SocketId,
    pub(crate) _capability: agentos_runtime_tokio::capability::CapabilityLease,
    pub(crate) raw_buffer: Vec<u8>,
    pub(crate) decoded_buffer: VecDeque<u8>,
    pub(crate) body_mode: VmFetchBodyMode,
    pub(crate) peer_closed: bool,
    pub(crate) response_bytes: usize,
    pub(crate) max_response_bytes: usize,
    pub(crate) last_progress_at: Instant,
}

/// Minimal ownership retained when a VM generation misses its teardown
/// barrier. Kernel, adapter, filesystem, and routing state are deliberately not
/// retained; only the handles needed to prove eventual reconciliation survive.
#[derive(Debug)]
pub(crate) struct QuarantinedVmGeneration {
    pub(crate) connection_id: String,
    pub(crate) session_id: String,
    pub(crate) vm_id: String,
    pub(crate) generation: u64,
    pub(crate) resources: Arc<agentos_runtime_tokio::accounting::ResourceLedger>,
    pub(crate) runtime_context: agentos_runtime_tokio::RuntimeContext,
    pub(crate) capabilities: agentos_runtime_tokio::capability::CapabilityRegistry,
    pub(crate) reason: VmQuarantineReason,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum VmQuarantineReason {
    TeardownDeadline,
    ResourceIntegrity,
    CapabilityRegistryIntegrity,
    FairnessIntegrity,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct VmReconciliationSnapshot {
    pub(crate) active_tasks: usize,
    pub(crate) outstanding_capabilities: usize,
    pub(crate) ledger_zero: bool,
    pub(crate) integrity_ok: bool,
}

impl QuarantinedVmGeneration {
    pub(crate) fn reconciliation_snapshot(&self) -> VmReconciliationSnapshot {
        VmReconciliationSnapshot {
            active_tasks: self.runtime_context.tasks().active_scoped(),
            outstanding_capabilities: self.capabilities.outstanding_len(),
            ledger_zero: self.resources.is_zero(),
            integrity_ok: self.resources.integrity_ok(),
        }
    }

    pub(crate) fn can_reap(&self) -> bool {
        if self.reason != VmQuarantineReason::TeardownDeadline {
            return false;
        }
        let snapshot = self.reconciliation_snapshot();
        snapshot.active_tasks == 0
            && snapshot.outstanding_capabilities == 0
            && snapshot.ledger_zero
            && snapshot.integrity_ok
    }
}

/// Launch parameters for one projected agent package.
#[derive(Debug, Clone)]
pub(crate) struct ProjectedAgentLaunch {
    pub(crate) acp_entrypoint: String,
    pub(crate) env: BTreeMap<String, String>,
    pub(crate) launch_args: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ExitedProcessSnapshot {
    pub(crate) captured_at: Instant,
    pub(crate) process: crate::protocol::ProcessSnapshotEntry,
}

// ---------------------------------------------------------------------------
// DNS configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub(crate) struct VmDnsConfig {
    pub(crate) name_servers: Vec<SocketAddr>,
    pub(crate) overrides: BTreeMap<String, Vec<IpAddr>>,
}

#[derive(Debug, Clone)]
pub(crate) struct SocketPathContext {
    pub(crate) sandbox_root: PathBuf,
    pub(crate) unix_abstract_namespace: [u8; 32],
    pub(crate) unix_socket_host_dir: PathBuf,
    pub(crate) unix_bound_addresses: GuestUnixAddressRegistry,
    pub(crate) host_net_transfer_descriptions: HostNetTransferDescriptionRegistry,
    pub(crate) mounts: Vec<MountDescriptor>,
    pub(crate) listen_policy: VmListenPolicy,
    pub(crate) loopback_exempt_ports: BTreeSet<u16>,
    pub(crate) tcp_loopback_guest_to_host_ports: BTreeMap<(SocketFamily, u16), u16>,
    pub(crate) http_loopback_targets: BTreeMap<(SocketFamily, u16), HttpLoopbackTarget>,
    pub(crate) udp_loopback_guest_to_host_ports: BTreeMap<(SocketFamily, u16), u16>,
    pub(crate) udp_loopback_host_to_guest_ports: BTreeMap<(SocketFamily, u16), u16>,
    pub(crate) used_tcp_guest_ports: BTreeMap<SocketFamily, BTreeSet<u16>>,
    pub(crate) used_udp_guest_ports: BTreeMap<SocketFamily, BTreeSet<u16>>,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct NetworkResourceCounts {
    pub(crate) sockets: usize,
    pub(crate) connections: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct GuestUnixAddress {
    pub(crate) path: String,
    pub(crate) abstract_path_hex: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct GuestUnixAddressRegistryEntry {
    pub(crate) host_address_key: String,
    pub(crate) address: GuestUnixAddress,
    pub(crate) guest_device_inode: Option<(u64, u64)>,
    pub(crate) host_path: Option<PathBuf>,
    pub(crate) generation: u64,
    pub(crate) active_bindings: usize,
    pub(crate) queued_by_target: BTreeMap<String, usize>,
    pub(crate) pending_connection_limit: usize,
    pub(crate) pending_connections: VecDeque<Arc<GuestUnixConnectionState>>,
}

pub(crate) type GuestUnixAddressRegistry =
    Arc<Mutex<BTreeMap<String, GuestUnixAddressRegistryEntry>>>;

#[derive(Debug, Clone)]
pub(crate) struct HttpLoopbackTarget {
    pub(crate) process_id: String,
    pub(crate) server_id: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum SocketFamily {
    Ipv4,
    Ipv6,
}

impl SocketFamily {
    pub(crate) fn from_ip(ip: IpAddr) -> Self {
        match ip {
            IpAddr::V4(_) => Self::Ipv4,
            IpAddr::V6(_) => Self::Ipv6,
        }
    }
}

impl From<UdpFamily> for SocketFamily {
    fn from(value: UdpFamily) -> Self {
        match value {
            UdpFamily::Ipv4 => Self::Ipv4,
            UdpFamily::Ipv6 => Self::Ipv6,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct VmListenPolicy {
    pub(crate) port_min: u16,
    pub(crate) port_max: u16,
    pub(crate) allow_privileged: bool,
}

impl Default for VmListenPolicy {
    fn default() -> Self {
        Self {
            port_min: 1,
            port_max: u16::MAX,
            allow_privileged: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Active process state
// ---------------------------------------------------------------------------

/// Stdin bytes accepted from a parent's `child_process.write_stdin` but not
/// yet written into the child's kernel stdin pipe. The kernel pipe holds at
/// most `MAX_PIPE_BUFFER_BYTES` (64 KiB) and `fd_write` reports partial
/// writes with POSIX pipe semantics, so multi-buffer stdin payloads (for
/// example git's spooled pack fed to `index-pack --stdin`) must be queued
/// host-side and flushed as the child drains its pipe. `close_requested`
/// defers the writer-fd close until the backlog fully drains so the child
/// never observes an early EOF.
#[derive(Default)]
pub(crate) struct PendingKernelStdin {
    pub(crate) chunks: VecDeque<Vec<u8>>,
    /// Bytes of the front chunk already written into the pipe.
    pub(crate) front_offset: usize,
    /// Total unwritten bytes across all queued chunks.
    pub(crate) total: usize,
    pub(crate) close_requested: bool,
}

/// One execute-authorized, immutable image retained while a WASM executor
/// copies it through bounded bridge replies. This is sidecar-private state:
/// it consumes no guest fd and cannot be observed through `/proc/self/fd`.
pub(crate) struct ActiveExecutableImage {
    pub(crate) handle: u64,
    pub(crate) bytes: Vec<u8>,
    pub(crate) _retained_bytes: Reservation,
}

impl PendingKernelStdin {
    const CHUNK_BYTES: usize = 64 * 1024;

    pub(crate) fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }

    pub(crate) fn push(&mut self, chunk: &[u8]) {
        self.total += chunk.len();
        let mut remaining = chunk;
        if let Some(back) = self.chunks.back_mut() {
            let available = Self::CHUNK_BYTES.saturating_sub(back.len());
            let take = available.min(remaining.len());
            back.extend_from_slice(&remaining[..take]);
            remaining = &remaining[take..];
        }
        for part in remaining.chunks(Self::CHUNK_BYTES) {
            self.chunks.push_back(part.to_vec());
        }
    }

    pub(crate) fn clear(&mut self) {
        self.chunks.clear();
        self.front_offset = 0;
        self.total = 0;
    }
}

#[allow(dead_code)]
pub(crate) struct ActiveProcess {
    pub(crate) kernel_pid: u32,
    pub(crate) kernel_handle: KernelProcessHandle,
    /// Generation/PID-bound kernel-to-runtime controls. The endpoint producer
    /// is owned by the kernel process table; only this execution owns the
    /// receiver.
    pub(crate) runtime_control: agentos_kernel::process_runtime::RuntimeControlReceiver,
    /// VM-scoped admission/accounting view over the process-owned runtime.
    /// Child processes inherit this exact generation-bound context; they must
    /// never rediscover the process context through a global lookup.
    pub(crate) runtime_context: agentos_runtime_tokio::RuntimeContext,
    /// Immutable limits for the owning VM generation. Protocol tasks read
    /// their bounds from this snapshot instead of process-wide constants.
    pub(crate) limits: crate::limits::VmLimits,
    pub(crate) kernel_stdin_writer_fd: Option<u32>,
    /// Whether POSIX spawn actions installed fd 0 instead of allocating a
    /// sidecar-owned host-input pipe. All managed executors still read the
    /// resulting descriptor through the kernel.
    pub(crate) direct_posix_stdin: bool,
    /// Kernel descriptor backing guest fd 0. POSIX spawn actions can retain
    /// the transported description at a sidecar-private descriptor number.
    pub(crate) kernel_stdin_reader_fd: u32,
    /// Backlog for pipe-backed kernel stdin awaiting pipe capacity; see
    /// [`PendingKernelStdin`].
    pub(crate) pending_kernel_stdin: PendingKernelStdin,
    pub(crate) pending_kernel_stdin_gauge: Arc<QueueGauge>,
    pub(crate) vm_pending_stdin_bytes_budget: Arc<VmPendingByteBudget>,
    /// For a TTY (PTY-backed) process, the master-end fd whose output buffer
    /// carries cooked-mode echo plus ONLCR-processed guest output. When set,
    /// this master output is the single ordered output stream surfaced to the
    /// host (instead of the raw guest stdout/stderr execution events).
    pub(crate) tty_master_fd: Option<u32>,
    pub(crate) runtime: GuestRuntimeKind,
    /// Standalone-WASM engine affinity inherited across spawn and exec. This
    /// is independent of the current image so a Wasmtime process that execs
    /// JavaScript and later spawns WASM retains its selected backend.
    pub(crate) standalone_wasm_backend: StandaloneWasmBackend,
    /// Executor-selected transport facts consumed by common POSIX paths.
    /// This is intentionally independent of `runtime`: compatibility WASM and
    /// Wasmtime share a guest kind but may use different host-call transports.
    pub(crate) adapter_policy: ExecutionAdapterPolicy,
    pub(crate) detached: bool,
    pub(crate) execution: ActiveExecution,
    pub(crate) guest_cwd: String,
    pub(crate) env: BTreeMap<String, String>,
    pub(crate) host_cwd: PathBuf,
    pub(crate) executable_image: Option<ActiveExecutableImage>,
    pub(crate) next_executable_image_handle: u64,
    /// Wakes the shared process-event pump after durable local events are
    /// queued. `Notify` coalesces repeated wakes while the deque preserves all
    /// event data.
    pub(crate) process_event_notify: Arc<Notify>,
    /// Mutable wake destination retained by the runtime-neutral event queue.
    /// Joining the VM-wide broker updates this cell without replacing the
    /// generation-bound submission capability.
    pub(crate) common_event_notify: Arc<Mutex<Arc<Notify>>>,
    /// Runtime-neutral host services and their bounded common-event receiver.
    /// Neither side retains an executor-specific object.
    pub(crate) host_capabilities: agentos_execution::host::ProcessHostCapabilitySet,
    pub(crate) common_execution_events: agentos_execution::backend::ExecutionEventReceiver,
    /// Durable event backlog bound inherited from
    /// `runtime.protocol.maxProcessEvents` when this process is admitted.
    pub(crate) process_event_capacity: usize,
    /// Kernel file descriptors held on behalf of WASI `File::lock` calls,
    /// keyed by canonical guest path. These preserve advisory-lock ownership
    /// until the guest unlocks the file or the process exits.
    pub(crate) wasm_flock_fds: BTreeMap<String, u32>,
    pub(crate) pending_execution_events: VecDeque<ActiveExecutionEvent>,
    pub(crate) pending_execution_event_bytes: usize,
    pub(crate) pending_execution_event_count_limit: usize,
    pub(crate) pending_execution_event_bytes_limit: usize,
    pub(crate) pending_execution_event_count_gauge: Arc<QueueGauge>,
    pub(crate) pending_execution_event_bytes_gauge: Arc<QueueGauge>,
    pub(crate) vm_pending_event_bytes_budget: Arc<VmPendingByteBudget>,
    pub(crate) pending_net_connects: BTreeMap<u64, Arc<Mutex<PendingNetConnectState>>>,
    /// Deferred native connects complete off the owner thread; this binds the
    /// direct call id back to the canonical description that receives the
    /// resulting transport.
    pub(crate) pending_managed_host_net_connects: BTreeMap<u64, u64>,
    /// Synthetic terminal event reserved outside the ordinary bounded output
    /// queue so kernel termination cannot be dropped under backpressure.
    pub(crate) pending_runtime_exit: Option<agentos_kernel::process_runtime::ProcessExit>,
    /// Actual terminating signal observed from the runtime process (or the
    /// signal used for a shared-runtime synthetic exit). This is distinct from
    /// a requested kill signal: handlers may catch one signal and later exit
    /// for another reason.
    pub(crate) exit_signal: Option<i32>,
    pub(crate) exit_core_dumped: bool,
    pub(crate) real_interval_timer: ActiveRealIntervalTimer,
    pub(crate) child_processes: BTreeMap<String, ActiveProcess>,
    pub(crate) next_child_process_id: usize,
    /// In-flight `spawnSync`/Python subprocess calls owned by this process.
    /// Child runtime events advance these records from the shared process pump;
    /// no sidecar or Tokio worker blocks waiting for child output.
    pub(crate) pending_child_process_sync: BTreeMap<String, PendingChildProcessSync>,
    /// The Node-compatible `child_process` bridge owns this child's stdout and
    /// stderr delivery. Kernel fd 1/2 still carry the Linux process image, but
    /// their inherited descriptions must not bypass JavaScript pipes,
    /// spawnSync capture, or stdout-framed fork IPC.
    pub(crate) child_process_bridge_owns_output: bool,
    pub(crate) http_servers: BTreeMap<u64, ActiveHttpServer>,
    pub(crate) pending_http_requests: BTreeMap<(u64, u64), PendingHttpRequest>,
    pub(crate) http2: ActiveHttp2State,
    /// Capability leases are the lifecycle truth for every network handle in
    /// the legacy guest-facing maps below. Dropping a map entry without its
    /// lease is prevented by the typed insert/release helpers.
    pub(crate) capability_leases:
        BTreeMap<NativeCapabilityKey, Arc<agentos_runtime_tokio::capability::CapabilityLease>>,
    pub(crate) tcp_listeners: BTreeMap<String, ActiveTcpListener>,
    pub(crate) next_tcp_listener_id: usize,
    pub(crate) tcp_sockets: BTreeMap<String, ActiveTcpSocket>,
    pub(crate) next_tcp_socket_id: usize,
    pub(crate) tcp_port_reservations: BTreeMap<String, (SocketFamily, u16)>,
    pub(crate) next_tcp_port_reservation_id: usize,
    pub(crate) unix_listeners: BTreeMap<String, ActiveUnixListener>,
    pub(crate) next_unix_listener_id: usize,
    pub(crate) unix_sockets: BTreeMap<String, ActiveUnixSocket>,
    pub(crate) next_unix_socket_id: usize,
    pub(crate) udp_sockets: BTreeMap<String, ActiveUdpSocket>,
    pub(crate) next_udp_socket_id: usize,
    pub(crate) hash_sessions: BTreeMap<u64, ActiveHashSession>,
    pub(crate) next_hash_session_id: u64,
    pub(crate) cipher_sessions: BTreeMap<u64, ActiveCipherSession>,
    pub(crate) next_cipher_session_id: u64,
    pub(crate) diffie_hellman_sessions: BTreeMap<u64, ActiveDiffieHellmanSession>,
    pub(crate) next_diffie_hellman_session_id: u64,
    pub(crate) sqlite_databases: BTreeMap<u64, ActiveSqliteDatabase>,
    /// Host-side SQLite materializations must not be keyed by the guest PID:
    /// each VM starts a fresh PID namespace, while the native sidecar process
    /// and its temporary directory survive across VM generations.
    pub(crate) sqlite_host_namespace: String,
    pub(crate) next_sqlite_database_id: u64,
    pub(crate) sqlite_statements: BTreeMap<u64, ActiveSqliteStatement>,
    pub(crate) next_sqlite_statement_id: u64,
    /// For a child process whose stdio is the SHARED terminal (its kernel fd 1
    /// is the same PTY slave as the shell's), the `(kernel pid, master fd)` of
    /// the process that owns the host-facing PTY master. Set at spawn. Such a
    /// child's stdio writes surface ONLY through master drains attributed to
    /// the owner — never as child stdout events — exactly like a native
    /// terminal reading the PTY master (a shell never relays its child's tty
    /// output).
    pub(crate) tty_master_owner: Option<(u32, u32)>,
    /// Generation of the foreground PTY raw-mode lease owned by this process.
    /// Cleanup releases only this generation, so an unrelated/background child
    /// or a newer terminal mutation cannot restore a stale termios snapshot.
    pub(crate) tty_raw_mode_generation: Option<u64>,
    /// A parked `__kernel_stdin_read` / `__kernel_poll` sync RPC awaiting
    /// kernel readiness (reply-by-token deferral so servicing never blocks the
    /// dispatch loop). At most one per process: the guest thread is blocked in
    /// this RPC, so it cannot issue another. The optional absolute deadline is
    /// `None` for a readiness-only wait with no recurring timeout.
    pub(crate) deferred_kernel_wait_rpc: Option<(ExecutionHostCall, Option<Instant>)>,
    /// The one-shot readiness task associated with
    /// `deferred_kernel_wait_rpc`. Clearing the parked request aborts this
    /// task so a signal or teardown cannot enqueue a retry after settlement.
    pub(crate) deferred_kernel_wait_task: Option<tokio::task::JoinHandle<()>>,
    /// Preserves the one-shot 80% operation-deadline warning across readiness
    /// wakes and re-parks of the same root-process `fd_write` RPC.
    pub(crate) deferred_kernel_wait_deadline_warned: bool,
    pub(crate) deferred_child_write_timer: Option<tokio::task::JoinHandle<()>>,
    /// One durable process wait or sleep owned by the sidecar. The guest is
    /// synchronously parked on its direct reply lane, so one slot is the exact
    /// per-process admission bound.
    pub(crate) deferred_guest_wait: Option<DeferredGuestWait>,
    pub(crate) deferred_guest_wait_interrupted: bool,
    /// Adapter handshake: a caught signal has been published but the guest
    /// has not yet drained the checkpoint queue through `take_signal`.
    pub(crate) guest_signal_checkpoint_pending: bool,
    /// At most one typed kernel poll may be pending because the guest thread
    /// is synchronously parked on the corresponding direct reply.
    pub(crate) deferred_kernel_poll: Option<DeferredKernelPoll>,
    /// At most one typed descriptor read may be pending because the guest
    /// thread is synchronously parked on the corresponding direct reply.
    pub(crate) deferred_kernel_read: Option<DeferredKernelRead>,
    /// Per-process module resolution cache, persisted across module sync-RPCs
    /// (`__resolve_module` / `__load_file` / `__module_format` /
    /// `__batch_resolve_modules`) for the lifetime of this process so cold-start
    /// resolution does not rebuild it on every dispatch. The resolver reads the
    /// kernel VFS and the cache is invalidated before the next module RPC when
    /// its mutation generation changes, so the kernel remains the source of
    /// truth without rebuilding large immutable package graphs every dispatch.
    pub(crate) module_resolution_cache: agentos_execution::LocalModuleResolutionCache,
    /// Kernel VFS generation represented by `module_resolution_cache`.
    pub(crate) module_resolution_cache_generation: Option<u64>,
}

pub(crate) struct PendingChildProcessSync {
    pub(crate) pid: u32,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
    pub(crate) max_buffer: usize,
    pub(crate) deadline: Option<Instant>,
    pub(crate) timeout_signal: String,
    pub(crate) kill_sent: bool,
    pub(crate) timed_out: bool,
    pub(crate) max_buffer_exceeded: bool,
    pub(crate) completion: PendingChildProcessSyncCompletion,
    pub(crate) _count_reservation: VmPendingBudgetReservation,
    pub(crate) _bytes_reservation: VmPendingBudgetReservation,
}

pub(crate) enum PendingChildProcessSyncCompletion {
    Javascript(tokio::sync::oneshot::Sender<Result<Value, DeferredRpcError>>),
    Direct(DirectHostReplyHandle),
}

#[derive(Debug, Clone, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum NativeCapabilityKey {
    HttpServer(u64),
    Http2Server(u64),
    Http2Session(u64),
    Http2Stream(u64),
    TcpListener(String),
    TcpSocket(String),
    TlsSocket(String),
    UnixListener(String),
    UnixSocket(String),
    UdpSocket(String),
}

pub(crate) struct ActiveCipherSession {
    pub(crate) context: crate::crypto_cipher::StreamCipherSession,
}

pub(crate) struct ActiveHashSession {
    pub(crate) context: openssl::hash::Hasher,
}

pub(crate) struct ActiveSqliteDatabase {
    pub(crate) connection: Connection,
    pub(crate) host_path: Option<PathBuf>,
    pub(crate) vm_path: Option<String>,
    pub(crate) dirty: bool,
    pub(crate) transaction_depth: usize,
    pub(crate) read_only: bool,
}

#[derive(Clone)]
pub(crate) struct ActiveSqliteStatement {
    pub(crate) database_id: u64,
    pub(crate) sql: String,
    pub(crate) return_arrays: bool,
    pub(crate) read_bigints: bool,
    pub(crate) allow_bare_named_parameters: bool,
    pub(crate) allow_unknown_named_parameters: bool,
}

pub(crate) enum ActiveDiffieHellmanSession {
    Dh(ActiveDhSession),
    Ecdh(ActiveEcdhSession),
}

pub(crate) struct ActiveDhSession {
    pub(crate) params: openssl::dh::Dh<openssl::pkey::Params>,
    pub(crate) key_pair: Option<openssl::dh::Dh<openssl::pkey::Private>>,
}

pub(crate) struct ActiveEcdhSession {
    pub(crate) curve: String,
    pub(crate) key_pair: Option<openssl::ec::EcKey<openssl::pkey::Private>>,
}

#[derive(Debug)]
pub(crate) struct ActiveHttpServer {
    pub(crate) listener: TcpListener,
    pub(crate) guest_local_addr: SocketAddr,
    pub(crate) next_request_id: u64,
    pub(crate) closed: Arc<AtomicBool>,
    pub(crate) close_notify: Arc<tokio::sync::Notify>,
}

#[derive(Debug)]
pub(crate) enum PendingHttpRequest {
    Buffered(Option<String>),
    Deferred(tokio::sync::oneshot::Sender<Result<Value, DeferredRpcError>>),
}

#[derive(Clone, Default)]
pub(crate) struct ActiveHttp2State {
    pub(crate) shared: Arc<Mutex<Http2SharedState>>,
}

pub(crate) struct Http2SharedState {
    pub(crate) next_session_id: u64,
    pub(crate) next_stream_id: u64,
    pub(crate) ready: Arc<tokio::sync::Notify>,
    pub(crate) event_capacity_notify: Arc<tokio::sync::Notify>,
    pub(crate) event_session: Option<ExecutionWakeHandle>,
    pub(crate) servers: BTreeMap<u64, ActiveHttp2Server>,
    pub(crate) sessions: BTreeMap<u64, ActiveHttp2Session>,
    pub(crate) streams: BTreeMap<u64, ActiveHttp2Stream>,
    pub(crate) capability_leases:
        BTreeMap<NativeCapabilityKey, agentos_runtime_tokio::capability::CapabilityLease>,
    pub(crate) server_events: BTreeMap<u64, VecDeque<QueuedHttp2Event>>,
    pub(crate) session_events: BTreeMap<u64, VecDeque<QueuedHttp2Event>>,
    pub(crate) limits: crate::limits::VmLimits,
    pub(crate) resources: Option<Arc<ResourceLedger>>,
    pub(crate) vm_generation: u64,
}

impl Default for Http2SharedState {
    fn default() -> Self {
        Self {
            next_session_id: 0,
            next_stream_id: 0,
            ready: Arc::new(tokio::sync::Notify::new()),
            event_capacity_notify: Arc::new(tokio::sync::Notify::new()),
            event_session: None,
            servers: BTreeMap::new(),
            sessions: BTreeMap::new(),
            streams: BTreeMap::new(),
            capability_leases: BTreeMap::new(),
            server_events: BTreeMap::new(),
            session_events: BTreeMap::new(),
            limits: crate::limits::VmLimits::default(),
            resources: None,
            vm_generation: 0,
        }
    }
}

#[derive(Debug)]
pub(crate) struct ActiveHttp2Server {
    pub(crate) actual_local_addr: SocketAddr,
    pub(crate) guest_local_addr: SocketAddr,
    pub(crate) secure: bool,
    pub(crate) tls: Option<TlsBridgeOptions>,
    pub(crate) closed: Arc<AtomicBool>,
    pub(crate) close_notify: Arc<tokio::sync::Notify>,
}

#[derive(Debug, Clone)]
pub(crate) struct ActiveHttp2Session {
    pub(crate) command_tx: TokioSender<QueuedHttp2Command>,
    pub(crate) capability_id: u64,
    pub(crate) vm_generation: u64,
    pub(crate) fairness: agentos_runtime_tokio::fairness::FairWorkBroker,
    pub(crate) command_timeout: Duration,
    pub(crate) close_requested: Arc<AtomicBool>,
    pub(crate) close_abrupt: Arc<AtomicBool>,
    pub(crate) close_notify: Arc<tokio::sync::Notify>,
    pub(crate) _reservations: Vec<SharedReservation>,
    pub(crate) resources: Arc<ResourceLedger>,
    pub(crate) stream_resources: Arc<ResourceLedger>,
}

#[derive(Debug, Clone)]
pub(crate) struct ActiveHttp2Stream {
    pub(crate) session_id: u64,
    pub(crate) paused: Arc<AtomicBool>,
    pub(crate) resume_notify: Arc<tokio::sync::Notify>,
    pub(crate) _reservations: Vec<SharedReservation>,
}

#[derive(Debug)]
pub(crate) struct QueuedHttp2Event {
    pub(crate) event: Http2BridgeEvent,
    pub(crate) reservations: Vec<Reservation>,
}

#[derive(Debug)]
pub(crate) struct QueuedHttp2Command {
    pub(crate) command: Http2SessionCommand,
    pub(crate) reservations: Vec<Reservation>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct Http2SocketSnapshot {
    pub(crate) encrypted: bool,
    pub(crate) allow_half_open: bool,
    pub(crate) local_address: Option<String>,
    pub(crate) local_port: Option<u16>,
    pub(crate) local_family: Option<String>,
    pub(crate) remote_address: Option<String>,
    pub(crate) remote_port: Option<u16>,
    pub(crate) remote_family: Option<String>,
    pub(crate) servername: Option<String>,
    pub(crate) alpn_protocol: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct Http2RuntimeSnapshot {
    pub(crate) effective_local_window_size: u32,
    pub(crate) local_window_size: u32,
    pub(crate) remote_window_size: u32,
    pub(crate) next_stream_id: u32,
    pub(crate) outbound_queue_size: u32,
    pub(crate) deflate_dynamic_table_size: u32,
    pub(crate) inflate_dynamic_table_size: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct Http2SessionSnapshot {
    pub(crate) capability_id: Option<u64>,
    pub(crate) capability_generation: Option<u64>,
    pub(crate) encrypted: bool,
    pub(crate) alpn_protocol: Option<String>,
    pub(crate) origin_set: Vec<String>,
    pub(crate) local_settings: BTreeMap<String, Value>,
    pub(crate) remote_settings: BTreeMap<String, Value>,
    pub(crate) state: Http2RuntimeSnapshot,
    pub(crate) socket: Http2SocketSnapshot,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct Http2BridgeEvent {
    pub(crate) kind: String,
    pub(crate) id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) data: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) extra: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) extra_number: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) extra_headers: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) flags: Option<u64>,
}

#[derive(Debug)]
pub(crate) struct Http2ResponseSender(SyncSender<Result<Value, DeferredRpcError>>);

impl Http2ResponseSender {
    pub(crate) fn new(sender: SyncSender<Result<Value, DeferredRpcError>>) -> Self {
        Self(sender)
    }

    pub(crate) fn settle(self, result: Result<Value, String>) {
        if self
            .0
            .send(result.map_err(|message| DeferredRpcError {
                code: String::from("ERR_AGENTOS_HTTP2_COMMAND"),
                message,
                details: None,
            }))
            .is_err()
        {
            eprintln!(
                "INFO_AGENTOS_STALE_HTTP2_COMPLETION: HTTP/2 command waiter was dropped before settlement"
            );
        }
    }
}

#[derive(Debug)]
pub(crate) enum Http2SessionCommand {
    Request {
        headers_json: String,
        options_json: String,
        pending_capability: agentos_runtime_tokio::capability::PendingCapability,
        stream_reservations: Vec<Reservation>,
        respond_to: Http2ResponseSender,
    },
    Settings {
        settings_json: String,
        respond_to: Http2ResponseSender,
    },
    SetLocalWindowSize {
        size: u32,
        respond_to: Http2ResponseSender,
    },
    Goaway {
        error_code: u32,
        last_stream_id: u32,
        opaque_data: Option<Vec<u8>>,
        respond_to: Http2ResponseSender,
    },
    StreamRespond {
        stream_id: u64,
        headers_json: String,
        respond_to: Http2ResponseSender,
    },
    StreamPush {
        stream_id: u64,
        headers_json: String,
        pending_capability: agentos_runtime_tokio::capability::PendingCapability,
        stream_reservations: Vec<Reservation>,
        respond_to: Http2ResponseSender,
    },
    StreamWrite {
        stream_id: u64,
        chunk: Vec<u8>,
        end_stream: bool,
        respond_to: Http2ResponseSender,
    },
    StreamClose {
        stream_id: u64,
        error_code: Option<u32>,
        respond_to: Http2ResponseSender,
    },
    StreamRespondWithFile {
        stream_id: u64,
        body: Vec<u8>,
        headers_json: String,
        options_json: String,
        respond_to: Http2ResponseSender,
    },
}

// ---------------------------------------------------------------------------
// TCP types
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub(crate) enum TcpListenerEvent {
    Connection(PendingTcpSocket),
    Error {
        code: Option<String>,
        message: String,
    },
}

#[derive(Debug)]
pub(crate) struct PendingTcpSocket {
    pub(crate) stream: Option<TcpStream>,
    pub(crate) kernel_socket_id: Option<SocketId>,
    pub(crate) guest_local_addr: SocketAddr,
    pub(crate) guest_remote_addr: SocketAddr,
}

#[derive(Debug)]
pub(crate) enum TcpSocketEvent {
    Data {
        bytes: Vec<u8>,
        reservation: agentos_runtime_tokio::accounting::SharedReservation,
        /// Protocol-specific ownership that remains live until the payload is
        /// transferred out of the transport/event layer.
        source_reservations: Vec<agentos_runtime_tokio::accounting::SharedReservation>,
    },
    End,
    Close {
        had_error: bool,
    },
    Error {
        code: Option<String>,
        message: String,
    },
}

pub(crate) fn tcp_socket_event_retained_bytes(event: &TcpSocketEvent) -> usize {
    match event {
        TcpSocketEvent::Data { bytes, .. } => bytes.len(),
        TcpSocketEvent::Error { code, message } => code
            .as_ref()
            .map_or(0, String::len)
            .saturating_add(message.len()),
        TcpSocketEvent::End | TcpSocketEvent::Close { .. } => 0,
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SocketEventPusher {
    pub(crate) session: Option<ExecutionWakeHandle>,
    pub(crate) capability_id: agentos_runtime_tokio::capability::CapabilityId,
    pub(crate) capability_generation: agentos_runtime_tokio::capability::CapabilityGeneration,
    live: Arc<AtomicBool>,
    /// Coalesced sidecar owner wake. Readiness payload remains in the socket
    /// description; this only causes a parked combined poll to re-probe it.
    owner_notify: Arc<Notify>,
}

impl SocketEventPusher {
    pub(crate) fn is_live(&self) -> bool {
        self.live.load(Ordering::Acquire)
    }

    pub(crate) fn publish_readiness(
        &self,
        flags: agentos_runtime_tokio::readiness::ReadyFlags,
    ) -> Result<bool, agentos_execution::backend::ExecutionWakeError> {
        if !self.is_live() {
            return Ok(false);
        }
        self.owner_notify.notify_one();
        if let Some(session) = &self.session {
            session.publish_readiness(
                self.capability_id,
                self.capability_generation,
                agentos_execution::backend::ExecutionReadyFlags::from_bits(flags.bits()),
            )?;
        }
        Ok(true)
    }
}

#[derive(Debug)]
struct SocketReadinessSubscriber {
    target: SocketEventPusher,
    application_read_interest: bool,
}

/// Bounded readiness fanout shared by every alias of one open socket
/// description. Payloads stay in the description-owned transport queue; this
/// registry only coalesces level hints to each VM capability that refers to it.
#[derive(Debug)]
pub(crate) struct SocketReadinessSubscribers {
    subscribers: Mutex<BTreeMap<(u64, u64), SocketReadinessSubscriber>>,
    maximum: usize,
}

impl SocketReadinessSubscribers {
    pub(crate) fn new(resources: &ResourceLedger) -> Arc<Self> {
        let maximum = resources
            .usage(ResourceClass::Capabilities)
            .limit
            .unwrap_or(DEFAULT_MAX_SOCKET_READINESS_SUBSCRIBERS)
            .max(1);
        Arc::new(Self {
            subscribers: Mutex::new(BTreeMap::new()),
            maximum,
        })
    }

    fn register(
        &self,
        previous: Option<(u64, u64)>,
        target: SocketEventPusher,
    ) -> Result<bool, SidecarError> {
        let identity = (target.capability_id, target.capability_generation);
        let mut subscribers = self.subscribers.lock().map_err(|_| {
            SidecarError::host(
                "ERR_AGENTOS_READY_STATE_POISONED",
                String::from("socket readiness subscriber lock poisoned"),
            )
        })?;
        let preserved_interest = subscribers
            .get(&identity)
            .map(|subscriber| subscriber.application_read_interest)
            .unwrap_or(false);
        if previous != Some(identity) {
            if let Some(previous) = previous {
                if let Some(previous) = subscribers.remove(&previous) {
                    previous.target.live.store(false, Ordering::Release);
                }
            }
            if !subscribers.contains_key(&identity) && subscribers.len() >= self.maximum {
                return Err(SidecarError::host("ERR_AGENTOS_SOCKET_READINESS_SUBSCRIBER_LIMIT", format!("socket description readiness subscribers exceeded {}; raise limits.reactor.maxCapabilities",
                    self.maximum
                )));
            }
        }
        if let Some(previous) = subscribers.insert(
            identity,
            SocketReadinessSubscriber {
                target,
                application_read_interest: preserved_interest,
            },
        ) {
            previous.target.live.store(false, Ordering::Release);
        }
        Ok(subscribers
            .values()
            .any(|subscriber| subscriber.application_read_interest))
    }

    fn unregister(&self, identity: (u64, u64)) -> bool {
        self.subscribers
            .lock()
            .map(|mut subscribers| {
                if let Some(subscriber) = subscribers.remove(&identity) {
                    subscriber.target.live.store(false, Ordering::Release);
                }
                subscribers
                    .values()
                    .any(|subscriber| subscriber.application_read_interest)
            })
            .unwrap_or_else(|_| {
                eprintln!(
                    "ERR_AGENTOS_READY_STATE_POISONED: socket readiness subscriber lock poisoned"
                );
                false
            })
    }

    fn set_application_read_interest(
        &self,
        identity: (u64, u64),
        enabled: bool,
    ) -> Result<bool, SidecarError> {
        let target = {
            let subscribers = self.subscribers.lock().map_err(|_| {
                SidecarError::host(
                    "ERR_AGENTOS_READY_STATE_POISONED",
                    String::from("socket readiness subscriber lock poisoned"),
                )
            })?;
            subscribers
                .get(&identity)
                .map(|subscriber| subscriber.target.clone())
        };
        let Some(target) = target else {
            return Ok(false);
        };
        if !target.is_live() {
            return Ok(false);
        }
        if let Some(session) = &target.session {
            session
                .set_application_read_interest(
                    target.capability_id,
                    target.capability_generation,
                    enabled,
                )
                .map_err(|error| SidecarError::Execution(error.to_string()))?;
        }
        self.set_application_read_interest_state(identity, enabled)
    }

    fn set_application_read_interest_state(
        &self,
        identity: (u64, u64),
        enabled: bool,
    ) -> Result<bool, SidecarError> {
        let mut subscribers = self.subscribers.lock().map_err(|_| {
            SidecarError::host(
                "ERR_AGENTOS_READY_STATE_POISONED",
                String::from("socket readiness subscriber lock poisoned"),
            )
        })?;
        if let Some(subscriber) = subscribers.get_mut(&identity) {
            subscriber.application_read_interest = enabled;
        }
        Ok(subscribers
            .values()
            .any(|subscriber| subscriber.application_read_interest))
    }

    pub(crate) fn targets(&self) -> Vec<SocketEventPusher> {
        self.subscribers
            .lock()
            .map(|subscribers| {
                subscribers
                    .values()
                    .map(|subscriber| subscriber.target.clone())
                    .collect()
            })
            .unwrap_or_else(|_| {
                eprintln!(
                    "ERR_AGENTOS_READY_STATE_POISONED: socket readiness subscriber lock poisoned"
                );
                Vec::new()
            })
    }
}

/// Per-alias registration. Transfer clones deliberately receive a fresh empty
/// token, so dropping a queued or rejected SCM_RIGHTS transfer cannot remove
/// the sender's readiness subscription.
#[derive(Debug)]
pub(crate) struct SocketReadinessRegistration {
    subscribers: Arc<SocketReadinessSubscribers>,
    registration: Mutex<Option<SocketReadinessRegistrationState>>,
    aggregate_interest: Option<Arc<AtomicBool>>,
    interest_notify: Option<Arc<Notify>>,
}

#[derive(Debug)]
struct SocketReadinessRegistrationState {
    identity: (u64, u64),
    live: Arc<AtomicBool>,
}

impl SocketReadinessRegistration {
    pub(crate) fn new(
        subscribers: Arc<SocketReadinessSubscribers>,
        aggregate_interest: Option<Arc<AtomicBool>>,
        interest_notify: Option<Arc<Notify>>,
    ) -> Self {
        Self {
            subscribers,
            registration: Mutex::new(None),
            aggregate_interest,
            interest_notify,
        }
    }

    pub(crate) fn register(
        &self,
        session: Option<ExecutionWakeHandle>,
        identity: Option<(u64, u64)>,
        owner_notify: Arc<Notify>,
        replay_flags: agentos_runtime_tokio::readiness::ReadyFlags,
    ) {
        let Some((capability_id, capability_generation)) = identity else {
            return;
        };
        let live = Arc::new(AtomicBool::new(true));
        let target = SocketEventPusher {
            session,
            capability_id,
            capability_generation,
            live: Arc::clone(&live),
            owner_notify,
        };
        let mut registration = self.registration.lock().unwrap_or_else(|error| {
            eprintln!(
                "ERR_AGENTOS_READY_STATE_POISONED: socket readiness registration lock poisoned"
            );
            error.into_inner()
        });
        let previous = registration.take().map(|previous| {
            previous.live.store(false, Ordering::Release);
            previous.identity
        });
        let aggregate = match self.subscribers.register(previous, target.clone()) {
            Ok(aggregate) => aggregate,
            Err(error) => {
                eprintln!("{error}");
                return;
            }
        };
        *registration = Some(SocketReadinessRegistrationState {
            identity: (capability_id, capability_generation),
            live,
        });
        drop(registration);
        self.update_aggregate_interest(aggregate);
        // Readiness is level state. Replaying one coalesced hint after
        // registration closes the race where data arrived before this alias
        // was added; the subsequent bounded poll validates the actual level.
        if let Err(error) = target.publish_readiness(replay_flags) {
            eprintln!(
                "ERR_AGENTOS_NET_SOCKET_WAKE: capability={capability_id} generation={capability_generation} registration replay: {error}"
            );
        }
    }

    pub(crate) fn set_application_read_interest(
        &self,
        enabled: bool,
    ) -> Result<bool, SidecarError> {
        let identity = {
            let registration = self.registration.lock().map_err(|_| {
                SidecarError::host(
                    "ERR_AGENTOS_READY_STATE_POISONED",
                    String::from("socket readiness registration lock poisoned"),
                )
            })?;
            let Some(registration) = registration.as_ref() else {
                return Ok(false);
            };
            registration.identity
        };
        let aggregate = self
            .subscribers
            .set_application_read_interest(identity, enabled)?;
        self.update_aggregate_interest(aggregate);
        Ok(aggregate)
    }

    fn update_aggregate_interest(&self, enabled: bool) {
        if let Some(interest) = &self.aggregate_interest {
            interest.store(enabled, Ordering::Release);
        }
        if let Some(notify) = &self.interest_notify {
            notify.notify_waiters();
        }
    }

    pub(crate) fn retire(&self) {
        let registration = self
            .registration
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if let Some(registration) = registration {
            // Retire before removing the registry entry. Any publisher that
            // already cloned this target will observe the same guard.
            registration.live.store(false, Ordering::Release);
            let aggregate = self.subscribers.unregister(registration.identity);
            self.update_aggregate_interest(aggregate);
        }
    }
}

impl Drop for SocketReadinessRegistration {
    fn drop(&mut self) {
        self.retire();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KernelSocketReadinessEvent {
    Data,
    Datagram,
    Accept,
}

#[derive(Clone, Debug)]
pub(crate) struct KernelSocketReadinessTarget {
    pub(crate) session: Option<ExecutionWakeHandle>,
    pub(crate) notify: Option<Arc<Notify>>,
    pub(crate) capability_id: agentos_runtime_tokio::capability::CapabilityId,
    pub(crate) capability_generation: agentos_runtime_tokio::capability::CapabilityGeneration,
    pub(crate) target_id: String,
    pub(crate) event: KernelSocketReadinessEvent,
    pub(crate) live: Arc<AtomicBool>,
}

type KernelSocketReadinessIdentity = (u64, u64);
type KernelSocketReadinessTargets =
    BTreeMap<SocketId, BTreeMap<KernelSocketReadinessIdentity, KernelSocketReadinessTarget>>;

#[derive(Debug)]
pub(crate) struct KernelSocketReadinessRegistryState {
    targets: Mutex<KernelSocketReadinessTargets>,
    maximum: usize,
}

impl KernelSocketReadinessRegistryState {
    pub(crate) fn new(maximum: usize) -> Self {
        Self {
            targets: Mutex::new(BTreeMap::new()),
            maximum: maximum.max(1),
        }
    }

    pub(crate) fn register(
        &self,
        socket_id: SocketId,
        target: KernelSocketReadinessTarget,
    ) -> Result<(), SidecarError> {
        let identity = (target.capability_id, target.capability_generation);
        let mut targets = self.targets.lock().map_err(|_| {
            SidecarError::host(
                "ERR_AGENTOS_KERNEL_READINESS_REGISTRY_POISONED",
                String::from("readiness registry lock poisoned"),
            )
        })?;
        let already_registered = targets
            .get(&socket_id)
            .is_some_and(|socket_targets| socket_targets.contains_key(&identity));
        if !already_registered {
            let registered = targets.values().map(BTreeMap::len).sum::<usize>();
            if registered >= self.maximum {
                return Err(SidecarError::host("ERR_AGENTOS_KERNEL_READINESS_TARGET_LIMIT", format!("kernel readiness targets exceeded {}; raise limits.reactor.maxCapabilities",
                    self.maximum
                )));
            }
        }
        if let Some(previous) = targets
            .entry(socket_id)
            .or_default()
            .insert(identity, target)
        {
            previous.live.store(false, Ordering::Release);
        }
        Ok(())
    }

    pub(crate) fn unregister(&self, socket_id: SocketId, identity: (u64, u64)) {
        let Ok(mut targets) = self.targets.lock() else {
            eprintln!(
                "ERR_AGENTOS_KERNEL_READINESS_REGISTRY_POISONED: readiness registry lock poisoned"
            );
            return;
        };
        if let Some(socket_targets) = targets.get_mut(&socket_id) {
            if let Some(target) = socket_targets.get(&identity) {
                target.live.store(false, Ordering::Release);
            }
            socket_targets.remove(&identity);
            if socket_targets.is_empty() {
                targets.remove(&socket_id);
            }
        }
    }

    pub(crate) fn targets(&self, socket_id: SocketId) -> Vec<KernelSocketReadinessTarget> {
        self.targets
            .lock()
            .map(|targets| {
                targets
                    .get(&socket_id)
                    .map(|targets| targets.values().cloned().collect())
                    .unwrap_or_default()
            })
            .unwrap_or_else(|_| {
                eprintln!(
                    "ERR_AGENTOS_KERNEL_READINESS_REGISTRY_POISONED: readiness registry lock poisoned"
                );
                Vec::new()
            })
    }
}

impl Default for KernelSocketReadinessRegistryState {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_SOCKET_READINESS_SUBSCRIBERS)
    }
}

/// Read-side state owned by one sidecar socket open description.
///
/// Transport completions can be larger than one guest read and poll must be
/// observational. The bytes therefore remain here until a non-peeking read
/// consumes them. Source reservations stay live for exactly as long as any
/// retained bytes do, so moving buffering out of an executor adapter cannot
/// bypass the VM's buffered-byte accounting.
#[derive(Debug, Default)]
pub(crate) struct SocketReadState {
    pub(crate) bytes: VecDeque<u8>,
    pub(crate) source_reservations: Vec<SharedReservation>,
    pub(crate) terminal: Option<SocketReadTerminal>,
}

#[derive(Debug, Clone)]
pub(crate) enum SocketReadTerminal {
    End,
    Closed {
        had_error: bool,
    },
    Error {
        code: Option<String>,
        message: String,
    },
}

#[derive(Debug)]
pub(crate) struct ActiveTcpSocket {
    pub(crate) runtime_context: agentos_runtime_tokio::RuntimeContext,
    pub(crate) reactor_limits: ReactorIoLimits,
    pub(crate) fairness_identity: Arc<OnceLock<(u64, u64)>>,
    pub(crate) fairness_identity_committed: Arc<Notify>,
    pub(crate) fairness_retirement: Arc<SocketFairnessRetirement>,
    pub(crate) description_lease: Arc<SocketDescriptionLease>,
    pub(crate) stream: Option<Arc<Mutex<TcpStream>>>,
    pub(crate) pending_read_stream: Option<Arc<Mutex<Option<TcpStream>>>>,
    pub(crate) plain_reader_running: Arc<AtomicBool>,
    pub(crate) plain_reader_stopped: Arc<Notify>,
    pub(crate) events: Option<Arc<Mutex<AsyncCompletionReceiver<TcpSocketEvent>>>>,
    pub(crate) event_sender: Option<AsyncCompletionSender<TcpSocketEvent>>,
    /// Durable per-operation wait source shared by adapters. Event data stays
    /// in `events`; this is only a coalesced readiness hint.
    pub(crate) read_event_notify: Arc<Notify>,
    pub(crate) event_pusher: Arc<SocketReadinessSubscribers>,
    pub(crate) readiness_registration: SocketReadinessRegistration,
    pub(crate) application_read_interest: Arc<AtomicBool>,
    pub(crate) application_read_notify: Arc<Notify>,
    pub(crate) kernel_socket_id: Option<SocketId>,
    pub(crate) no_delay: bool,
    pub(crate) keep_alive: bool,
    pub(crate) keep_alive_initial_delay_secs: Option<u64>,
    pub(crate) guest_local_addr: SocketAddr,
    pub(crate) guest_remote_addr: SocketAddr,
    pub(crate) listener_id: Option<String>,
    pub(crate) tls_mode: Arc<AtomicBool>,
    pub(crate) native_tls_commands: Arc<Mutex<Option<TokioSender<NativeTlsCommand>>>>,
    pub(crate) plain_commands: Option<TokioSender<NativePlainSocketCommand>>,
    pub(crate) tls_state: Arc<Mutex<Option<ActiveTlsState>>>,
    pub(crate) saw_local_shutdown: Arc<AtomicBool>,
    pub(crate) saw_remote_end: Arc<AtomicBool>,
    pub(crate) close_notified: Arc<AtomicBool>,
    /// A transport event may contain more bytes than the guest requested from
    /// `net.socket_read`. Retain the unread suffix on the shared socket
    /// description so the next read observes it before later transport events.
    pub(crate) pending_read_event: Arc<Mutex<Option<TcpSocketEvent>>>,
    /// Bytes and terminal state already observed from the transport but not
    /// yet consumed by the shared open socket description. Keeping the source
    /// reservations with the bytes makes the sidecar the durable, accounted
    /// readiness owner across dup/SCM_RIGHTS aliases and executor adapters.
    pub(crate) read_state: Arc<Mutex<SocketReadState>>,
    /// One strong reference per guest-visible open socket description. This is
    /// separate from transport/TLS worker Arcs so SCM_RIGHTS can decide when a
    /// close is the final description close.
    pub(crate) description_handles: Arc<()>,
    pub(crate) listener_connection_retirement: Option<Arc<ListenerConnectionRetirement>>,
    /// Kernel open-description guard used after this socket first crosses
    /// SCM_RIGHTS. It keeps owner-0 kernel sockets alive while queued or held
    /// by another process and lets the kernel prune discarded transfers.
    pub(crate) kernel_transfer_guard: Option<TransferredFd>,
    pub(crate) resources: Arc<agentos_runtime_tokio::accounting::ResourceLedger>,
}

#[derive(Debug)]
pub(crate) enum NativeTlsCommand {
    Write {
        payload: TlsWritePayload,
        /// Present once the TLS handshake has completed and the bridge can
        /// wait for transport completion. A loopback write admitted while the
        /// peer is still upgrading has no waiter: blocking that synchronous
        /// bridge call would prevent the peer VM callback from starting the
        /// handshake. The transport still owns the charged payload and reports
        /// any eventual failure through the socket event path.
        completion: Option<SyncSender<Result<Value, DeferredRpcError>>>,
    },
    Shutdown {
        _command_reservation: agentos_runtime_tokio::accounting::SharedReservation,
        completion: SyncSender<Result<Value, DeferredRpcError>>,
    },
    Close {
        _command_reservation: agentos_runtime_tokio::accounting::SharedReservation,
    },
}

#[derive(Debug)]
pub(crate) enum NativePlainSocketCommand {
    Write {
        payload: PlainSocketWritePayload,
        completion: tokio::sync::oneshot::Sender<Result<Value, DeferredRpcError>>,
    },
    Shutdown {
        _command_reservation: agentos_runtime_tokio::accounting::SharedReservation,
        completion: tokio::sync::oneshot::Sender<Result<Value, DeferredRpcError>>,
    },
}

#[derive(Debug)]
pub(crate) struct PlainSocketWritePayload {
    pub(crate) bytes: Vec<u8>,
    pub(crate) _command_reservation: agentos_runtime_tokio::accounting::SharedReservation,
    pub(crate) _bytes_reservation: agentos_runtime_tokio::accounting::SharedReservation,
    pub(crate) _buffered_reservation: agentos_runtime_tokio::accounting::SharedReservation,
}

#[derive(Debug)]
pub(crate) struct TlsWritePayload {
    pub(crate) bytes: Vec<u8>,
    pub(crate) _command_reservation: agentos_runtime_tokio::accounting::SharedReservation,
    pub(crate) _command_bytes_reservation: agentos_runtime_tokio::accounting::SharedReservation,
    pub(crate) _buffered_reservation: agentos_runtime_tokio::accounting::SharedReservation,
    pub(crate) _tls_reservation: agentos_runtime_tokio::accounting::SharedReservation,
}

/// VM-scoped scheduling bounds copied into each native handle owner. Keeping
/// these beside the handle prevents transport tasks from consulting process
/// globals after the VM generation has been admitted.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ReactorIoLimits {
    pub(crate) operation_quantum: usize,
    pub(crate) byte_quantum: usize,
    pub(crate) accept_quantum: usize,
    pub(crate) datagram_quantum: usize,
    pub(crate) max_handle_commands: usize,
    pub(crate) max_async_completions: usize,
    pub(crate) operation_deadline: Duration,
}

#[derive(Debug)]
pub(crate) struct LoopbackTlsTransportPair {
    pub(crate) state: Mutex<LoopbackTlsTransportPairState>,
    pub(crate) ready: Condvar,
    pub(crate) resources: Arc<agentos_runtime_tokio::accounting::ResourceLedger>,
}

#[derive(Debug, Default)]
pub(crate) struct LoopbackTlsTransportPairState {
    pub(crate) lower_to_higher: VecDeque<u8>,
    pub(crate) higher_to_lower: VecDeque<u8>,
    pub(crate) lower_to_higher_reservations:
        VecDeque<agentos_runtime_tokio::accounting::Reservation>,
    pub(crate) higher_to_lower_reservations:
        VecDeque<agentos_runtime_tokio::accounting::Reservation>,
    pub(crate) lower_to_higher_tls_reservations:
        VecDeque<agentos_runtime_tokio::accounting::Reservation>,
    pub(crate) higher_to_lower_tls_reservations:
        VecDeque<agentos_runtime_tokio::accounting::Reservation>,
    pub(crate) lower_write_closed: bool,
    pub(crate) higher_write_closed: bool,
    pub(crate) lower_closed: bool,
    pub(crate) higher_closed: bool,
    pub(crate) lower_read_interrupt: bool,
    pub(crate) higher_read_interrupt: bool,
    pub(crate) lower_read_waker: Option<std::task::Waker>,
    pub(crate) higher_read_waker: Option<std::task::Waker>,
    pub(crate) lower_write_waker: Option<std::task::Waker>,
    pub(crate) higher_write_waker: Option<std::task::Waker>,
}

pub(crate) struct LoopbackTlsEndpoint {
    pub(crate) pair: Arc<LoopbackTlsTransportPair>,
    pub(crate) is_lower_socket: bool,
    /// Registry key (`vm_id:lower:higher`) under which this endpoint's transport
    /// pair is registered in the loopback-TLS transport registry. Stored so the
    /// endpoint's `Drop` can eagerly prune its own registry entry once it is the
    /// last owner of the pair, instead of leaking a dead `Weak` entry until the
    /// next lazy `retain()` in `loopback_tls_endpoint()`. `None` means the
    /// endpoint was not registered (e.g. test-constructed) and Drop skips pruning.
    pub(crate) registry_key: Option<String>,
}

impl fmt::Debug for LoopbackTlsEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoopbackTlsEndpoint")
            .field("is_lower_socket", &self.is_lower_socket)
            .finish()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct TlsClientHello {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) servername: Option<String>,
    #[serde(
        rename = "ALPNProtocols",
        alias = "ALPNProtocols",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) alpn_protocols: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct TlsBridgeOptions {
    pub(crate) is_server: bool,
    pub(crate) host: Option<String>,
    pub(crate) servername: Option<String>,
    pub(crate) reject_unauthorized: Option<bool>,
    pub(crate) request_cert: Option<bool>,
    pub(crate) session: Option<String>,
    pub(crate) key: Option<TlsMaterial>,
    pub(crate) cert: Option<TlsMaterial>,
    pub(crate) ca: Option<TlsMaterial>,
    pub(crate) passphrase: Option<String>,
    pub(crate) ciphers: Option<String>,
    #[serde(alias = "ALPNProtocols")]
    pub(crate) alpn_protocols: Option<Vec<String>>,
    pub(crate) min_version: Option<String>,
    pub(crate) max_version: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub(crate) enum TlsMaterial {
    Single(TlsDataValue),
    Many(Vec<TlsDataValue>),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub(crate) enum TlsDataValue {
    Buffer { data: String },
    String { data: String },
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ActiveTlsState {
    pub(crate) client_hello: Option<TlsClientHello>,
    pub(crate) local_certificates: Vec<Vec<u8>>,
    pub(crate) peer_certificates: Vec<Vec<u8>>,
    pub(crate) protocol: Option<String>,
    pub(crate) cipher: Option<Value>,
    pub(crate) session_reused: bool,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ResolvedTcpConnectAddr {
    pub(crate) actual_addr: SocketAddr,
    pub(crate) guest_remote_addr: SocketAddr,
    pub(crate) use_kernel_loopback: bool,
}

#[derive(Debug)]
pub(crate) struct ActiveTcpListener {
    pub(crate) listener: Option<TcpListener>,
    pub(crate) kernel_socket_id: Option<SocketId>,
    pub(crate) local_addr: Option<SocketAddr>,
    pub(crate) guest_local_addr: SocketAddr,
    pub(crate) backlog: usize,
    pub(crate) active_connection_ids: Arc<Mutex<BTreeSet<String>>>,
    /// One strong reference per guest-visible listener description, including
    /// descriptors queued in SCM_RIGHTS messages.
    pub(crate) description_handles: Arc<()>,
    pub(crate) description_lease: Arc<SocketDescriptionLease>,
    pub(crate) kernel_transfer_guard: Option<TransferredFd>,
    /// One accept/error event observed by poll(2) but not consumed by accept.
    pub(crate) pending_event: Arc<Mutex<Option<TcpListenerEvent>>>,
}

// ---------------------------------------------------------------------------
// Unix socket types
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub(crate) enum UnixListenerEvent {
    Connection {
        socket: PendingUnixSocket,
        capability: agentos_runtime_tokio::capability::PendingCapability,
    },
    Error {
        code: Option<String>,
        message: String,
    },
}

pub(crate) fn unix_listener_event_retained_bytes(event: &UnixListenerEvent) -> usize {
    match event {
        UnixListenerEvent::Connection { socket, .. } => [
            socket.local_path.as_ref(),
            socket.remote_path.as_ref(),
            socket.local_abstract_path_hex.as_ref(),
            socket.remote_abstract_path_hex.as_ref(),
        ]
        .into_iter()
        .flatten()
        .map(String::len)
        .fold(0, usize::saturating_add),
        UnixListenerEvent::Error { code, message } => code
            .as_ref()
            .map_or(0, String::len)
            .saturating_add(message.len()),
    }
}

#[derive(Debug)]
pub(crate) struct PendingUnixSocket {
    pub(crate) stream: UnixStream,
    pub(crate) local_path: Option<String>,
    pub(crate) remote_path: Option<String>,
    pub(crate) local_abstract_path_hex: Option<String>,
    pub(crate) remote_abstract_path_hex: Option<String>,
    pub(crate) connection_guard: PendingUnixConnectionGuard,
}

#[derive(Debug)]
pub(crate) struct GuestUnixConnectionState {
    pub(crate) accepted_peer_open: AtomicBool,
}

#[derive(Debug)]
pub(crate) struct PendingUnixConnectionGuard {
    pub(crate) state: Option<Arc<GuestUnixConnectionState>>,
}

#[derive(Debug)]
pub(crate) struct ActiveUnixSocket {
    pub(crate) reactor_limits: ReactorIoLimits,
    pub(crate) fairness_identity: Arc<OnceLock<(u64, u64)>>,
    pub(crate) fairness_identity_committed: Arc<Notify>,
    pub(crate) fairness_retirement: Arc<SocketFairnessRetirement>,
    pub(crate) description_lease: Arc<SocketDescriptionLease>,
    pub(crate) stream: Arc<Mutex<UnixStream>>,
    pub(crate) plain_commands: TokioSender<NativePlainSocketCommand>,
    pub(crate) events: Arc<Mutex<AsyncCompletionReceiver<TcpSocketEvent>>>,
    pub(crate) event_sender: AsyncCompletionSender<TcpSocketEvent>,
    /// Coalesced wake source for blocking common host reads. Payload remains
    /// in `events`/`read_state` and is consumed only on the owner thread.
    pub(crate) read_event_notify: Arc<Notify>,
    pub(crate) event_pusher: Arc<SocketReadinessSubscribers>,
    pub(crate) readiness_registration: SocketReadinessRegistration,
    pub(crate) application_read_interest: Arc<AtomicBool>,
    pub(crate) application_read_notify: Arc<Notify>,
    pub(crate) listener_id: Option<String>,
    pub(crate) local_path: Option<String>,
    pub(crate) remote_path: Option<String>,
    pub(crate) local_abstract_path_hex: Option<String>,
    pub(crate) remote_abstract_path_hex: Option<String>,
    pub(crate) local_registry_binding_id: Option<String>,
    pub(crate) remote_registry_binding_id: Option<String>,
    pub(crate) connection_state: Option<Arc<GuestUnixConnectionState>>,
    pub(crate) private_host_path: Option<PathBuf>,
    pub(crate) saw_local_shutdown: Arc<AtomicBool>,
    pub(crate) saw_remote_end: Arc<AtomicBool>,
    pub(crate) close_notified: Arc<AtomicBool>,
    pub(crate) pending_read_event: Arc<Mutex<Option<TcpSocketEvent>>>,
    /// Durable, accounted read/EOF/error state shared by every alias of this
    /// Unix open description. Adapters observe this state; they never retain
    /// transport payload or readiness truth themselves.
    pub(crate) read_state: Arc<Mutex<SocketReadState>>,
    pub(crate) description_handles: Arc<()>,
    pub(crate) listener_connection_retirement: Option<Arc<ListenerConnectionRetirement>>,
    pub(crate) resources: Arc<agentos_runtime_tokio::accounting::ResourceLedger>,
}

#[derive(Debug)]
pub(crate) struct ActiveUnixListener {
    pub(crate) listener: Option<UnixListener>,
    pub(crate) bound_socket: Option<Socket>,
    pub(crate) events: Arc<Mutex<AsyncCompletionReceiver<UnixListenerEvent>>>,
    pub(crate) event_pusher: Arc<SocketReadinessSubscribers>,
    pub(crate) readiness_registration: SocketReadinessRegistration,
    pub(crate) close_notify: Arc<Notify>,
    pub(crate) close_completion: Arc<Mutex<Option<tokio::sync::oneshot::Receiver<()>>>>,
    pub(crate) acceptor_started: bool,
    pub(crate) path: String,
    pub(crate) abstract_path_hex: Option<String>,
    pub(crate) registry_binding_id: String,
    pub(crate) private_host_path: Option<PathBuf>,
    pub(crate) guest_node_path: Option<String>,
    pub(crate) backlog: usize,
    pub(crate) active_connection_ids: Arc<Mutex<BTreeSet<String>>>,
    pub(crate) description_handles: Arc<()>,
    pub(crate) description_lease: Arc<SocketDescriptionLease>,
    /// One accept/error event observed by poll(2) but not consumed by accept.
    pub(crate) pending_event: Arc<Mutex<Option<UnixListenerEvent>>>,
}

// ---------------------------------------------------------------------------
// UDP types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UdpFamily {
    Ipv4,
    Ipv6,
}

impl UdpFamily {
    pub(crate) fn from_socket_type(value: &str) -> Result<Self, SidecarError> {
        match value {
            "udp4" => Ok(Self::Ipv4),
            "udp6" => Ok(Self::Ipv6),
            other => Err(SidecarError::InvalidState(format!(
                "unsupported dgram socket type {other}"
            ))),
        }
    }

    pub(crate) fn socket_type(self) -> &'static str {
        match self {
            Self::Ipv4 => "udp4",
            Self::Ipv6 => "udp6",
        }
    }

    pub(crate) fn matches_addr(self, addr: &SocketAddr) -> bool {
        matches!(
            (self, addr),
            (Self::Ipv4, SocketAddr::V4(_)) | (Self::Ipv6, SocketAddr::V6(_))
        )
    }
}

#[derive(Clone, Debug)]
pub(crate) enum DatagramEvent {
    Message {
        data: Vec<u8>,
        remote_addr: SocketAddr,
        _byte_reservation: agentos_runtime_tokio::accounting::SharedReservation,
        _datagram_reservation: agentos_runtime_tokio::accounting::SharedReservation,
        _udp_byte_reservation: agentos_runtime_tokio::accounting::SharedReservation,
        _udp_datagram_reservation: agentos_runtime_tokio::accounting::SharedReservation,
    },
    Error {
        code: Option<String>,
        message: String,
    },
}

#[derive(Debug)]
pub(crate) struct NativeUdpSendPayload {
    pub(crate) bytes: Vec<u8>,
    pub(crate) _command_reservation: SharedReservation,
    pub(crate) _command_bytes_reservation: SharedReservation,
    pub(crate) _buffered_reservation: SharedReservation,
    pub(crate) _udp_bytes_reservation: SharedReservation,
}

#[derive(Debug)]
pub(crate) enum NativeUdpSocketOption {
    Broadcast(bool),
    Ttl(u32),
    MulticastTtl(u32),
    MulticastLoopback(bool),
    MulticastInterface(String),
    Membership {
        group: IpAddr,
        interface: Option<String>,
        join: bool,
    },
    SourceMembership {
        source: IpAddr,
        group: IpAddr,
        interface: Option<String>,
        join: bool,
    },
}

#[derive(Debug)]
pub(crate) enum NativeUdpCommand {
    Send {
        payload: NativeUdpSendPayload,
        remote_addr: Option<SocketAddr>,
        guest_local_addr: SocketAddr,
        completion: SyncSender<Result<Value, DeferredRpcError>>,
    },
    Poll {
        _command_reservation: SharedReservation,
        completion: SyncSender<Result<Option<DatagramEvent>, DeferredRpcError>>,
    },
    Connect {
        _command_reservation: SharedReservation,
        remote_addr: SocketAddr,
        guest_local_addr: SocketAddr,
        guest_remote_addr: SocketAddr,
        completion: SyncSender<Result<Value, DeferredRpcError>>,
    },
    Disconnect {
        _command_reservation: SharedReservation,
        completion: SyncSender<Result<Value, DeferredRpcError>>,
    },
    RemoteAddress {
        _command_reservation: SharedReservation,
        completion: SyncSender<Result<Value, DeferredRpcError>>,
    },
    SetOption {
        _command_reservation: SharedReservation,
        option: NativeUdpSocketOption,
        guest_local_addr: SocketAddr,
        completion: SyncSender<Result<Value, DeferredRpcError>>,
    },
    SetBufferSize {
        _command_reservation: SharedReservation,
        which: String,
        size: usize,
        completion: SyncSender<Result<Value, DeferredRpcError>>,
    },
    GetBufferSize {
        _command_reservation: SharedReservation,
        which: String,
        completion: SyncSender<Result<Value, DeferredRpcError>>,
    },
}

#[derive(Debug)]
pub(crate) struct ActiveUdpSocket {
    pub(crate) family: UdpFamily,
    pub(crate) native_commands: Option<TokioSender<NativeUdpCommand>>,
    pub(crate) kernel_socket_id: Option<SocketId>,
    pub(crate) guest_local_addr: Option<SocketAddr>,
    pub(crate) native_local_addr: Option<SocketAddr>,
    pub(crate) kernel_connected_remote_addr: Option<SocketAddr>,
    pub(crate) recv_buffer_size: usize,
    pub(crate) send_buffer_size: usize,
    /// One strong reference per guest-visible datagram socket description.
    pub(crate) description_handles: Arc<()>,
    pub(crate) kernel_transfer_guard: Option<TransferredFd>,
    pub(crate) resources: Arc<agentos_runtime_tokio::accounting::ResourceLedger>,
    pub(crate) runtime_context: agentos_runtime_tokio::RuntimeContext,
    pub(crate) reactor_limits: ReactorIoLimits,
    pub(crate) fairness_identity: Arc<OnceLock<(u64, u64)>>,
    pub(crate) fairness_identity_committed: Arc<Notify>,
    pub(crate) fairness_retirement: Arc<SocketFairnessRetirement>,
    pub(crate) description_lease: Arc<SocketDescriptionLease>,
    pub(crate) read_event_notify: Arc<Notify>,
    /// The next datagram observed by poll but not consumed by recv. This is
    /// shared by every alias of the open description so MSG_PEEK and poll are
    /// observational across dup/SCM_RIGHTS and every executor adapter.
    pub(crate) pending_datagram: Arc<Mutex<Option<DatagramEvent>>>,
    pub(crate) event_pusher: Arc<SocketReadinessSubscribers>,
    pub(crate) readiness_registration: SocketReadinessRegistration,
    pub(crate) native_read_wake_pending: Arc<AtomicBool>,
}

// ---------------------------------------------------------------------------
// Execution types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExecutionValueKind {
    None,
    JavaScript,
    Python,
    TypeScriptCheck,
}

#[derive(Debug)]
pub(crate) struct ManagedLanguageExecution {
    /// False for an attached ephemeral operation. Its map key is an internal
    /// correlation token and is never exposed through lifecycle enumeration.
    pub(crate) public: bool,
    /// True only for caller-created reusable contexts. Background processes are
    /// retained internally but must not appear in the context collection.
    pub(crate) context: bool,
    pub(crate) descriptor: ExecutionDescriptor,
    pub(crate) result: Option<ExecutionCompletedResponse>,
    pub(crate) events: VecDeque<ExecutionOutputEvent>,
    pub(crate) retained_event_bytes: usize,
    pub(crate) output_truncated: bool,
    pub(crate) next_sequence: u64,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
    pub(crate) stdout_truncated: bool,
    pub(crate) stderr_truncated: bool,
    pub(crate) output_limit_bytes: usize,
    pub(crate) output_limit_setting: &'static str,
    pub(crate) capture: ExecutionOutputCapture,
    pub(crate) retain_events: bool,
    pub(crate) event_limit: usize,
    pub(crate) event_bytes_limit: usize,
    pub(crate) uses_pty: bool,
    pub(crate) value_kind: ExecutionValueKind,
    pub(crate) semantic_result_path: Option<String>,
    pub(crate) pending_outcome: Option<crate::protocol::ExecutionOutcome>,
    pub(crate) deadline_ms: Option<u64>,
    pub(crate) expires_at_ms: Option<u64>,
    pub(crate) deadline_task: Option<tokio::task::JoinHandle<()>>,
    /// Internal process whose V8/Pyodide context is parked while the public
    /// execution is idle. It is never exposed as a second lifecycle identity.
    pub(crate) resident_process_id: Option<String>,
}

#[derive(Debug)]
#[allow(clippy::large_enum_variant)] // execution state is process-registry owned and preserves backend drop affinity
pub(crate) enum ActiveExecution {
    Javascript(JavascriptExecution),
    Python(PythonExecution),
    Wasm(Box<WasmExecution>),
    Binding(BindingExecution),
}

#[derive(Debug, Clone)]
pub(crate) struct BindingExecution {
    pub(crate) cancelled: Arc<AtomicBool>,
    /// Durable kernel stop state. Binding callbacks may finish trusted host
    /// work already in flight, but no adapter event is exposed to the process
    /// while it is stopped.
    pub(crate) paused: Arc<AtomicBool>,
    pub(crate) pause_notify: Arc<Notify>,
    pub(crate) pending_events: Arc<Mutex<VecDeque<ActiveExecutionEvent>>>,
    pub(crate) event_overflow_reason:
        Arc<Mutex<Option<agentos_execution::backend::HostServiceError>>>,
    pub(crate) pending_event_bytes: Arc<AtomicUsize>,
    pub(crate) pending_event_count_limit: Arc<AtomicUsize>,
    pub(crate) pending_event_bytes_limit: Arc<AtomicUsize>,
    pub(crate) vm_pending_event_bytes_budget: Arc<VmPendingByteBudget>,
    pub(crate) event_notify: Arc<Notify>,
    pub(crate) host_capabilities: Option<agentos_execution::host::ProcessHostCapabilitySet>,
    pub(crate) descendant_wait_ownership: DescendantWaitOwnership,
    pub(crate) descendant_output_ownership: DescendantOutputOwnership,
}

impl Default for BindingExecution {
    fn default() -> Self {
        Self::with_event_notify(
            Arc::new(Notify::new()),
            agentos_runtime_tokio::DEFAULT_PROTOCOL_MAX_PROCESS_EVENTS,
        )
    }
}

impl BindingExecution {
    pub(crate) fn with_event_notify(event_notify: Arc<Notify>, event_capacity: usize) -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            paused: Arc::new(AtomicBool::new(false)),
            pause_notify: Arc::new(Notify::new()),
            pending_events: Arc::new(Mutex::new(VecDeque::new())),
            event_overflow_reason: Arc::new(Mutex::new(None)),
            pending_event_bytes: Arc::new(AtomicUsize::new(0)),
            pending_event_count_limit: Arc::new(AtomicUsize::new(event_capacity)),
            pending_event_bytes_limit: Arc::new(AtomicUsize::new(
                agentos_native_sidecar_core::limits::DEFAULT_PROCESS_PENDING_EVENT_BYTES,
            )),
            vm_pending_event_bytes_budget: VmPendingByteBudget::new(
                agentos_native_sidecar_core::limits::DEFAULT_PROCESS_PENDING_EVENT_BYTES,
                TrackedLimit::PendingExecutionEventBytes,
            ),
            event_notify,
            host_capabilities: None,
            descendant_wait_ownership: DescendantWaitOwnership::Sidecar,
            descendant_output_ownership: DescendantOutputOwnership::SidecarBridge,
        }
    }
}

#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum ActiveExecutionEvent {
    Common(ExecutionEvent),
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
    HostRpcRequest(ExecutionHostCall),
    HostCallCompletion(HostCallCompletion),
    /// Durable broker event emitted by a deferred POSIX-poll readiness or
    /// deadline task. The poll state remains process-owned; this payload only
    /// guarantees that the owner lane re-enters to probe it.
    DeferredPosixPollWake,
    ManagedStreamReadRecheck(Box<ManagedStreamReadRecheck>),
    ManagedUdpPollRecheck(Box<ManagedUdpPollRecheck>),
    SignalState {
        signal: u32,
        registration: SignalHandlerRegistration,
    },
    Exited(i32),
}

#[derive(Debug)]
pub(crate) struct ManagedStreamReadRecheck {
    pub(crate) root_process_id: String,
    pub(crate) process_path: Vec<String>,
    pub(crate) socket_id: String,
    pub(crate) max_bytes: u64,
    pub(crate) peek: bool,
    pub(crate) deadline: Instant,
    pub(crate) reply: DirectHostReplyHandle,
}

#[derive(Debug)]
pub(crate) struct ManagedUdpPollRecheck {
    /// VM process-map key plus a bounded descendant path. Re-entry always uses
    /// the root process event lane, then resolves the generation-bound target
    /// from kernel-owned process state before each readiness probe.
    pub(crate) root_process_id: String,
    pub(crate) process_path: Vec<String>,
    pub(crate) socket_id: String,
    pub(crate) peek: bool,
    pub(crate) max_bytes: Option<BoundedUsize>,
    pub(crate) deadline: Instant,
    pub(crate) operation_deadline: Option<Duration>,
    pub(crate) deadline_warning_emitted: bool,
    /// Native UDP owners are probed from the Tokio reactor, but guest-visible
    /// completion is settled only after the result re-enters the process lane.
    pub(crate) native_probe_completed: bool,
    pub(crate) native_event: Option<DatagramEvent>,
    pub(crate) reply: DirectHostReplyHandle,
    pub(crate) fair_turn: Option<FairWorkTurn>,
}

#[derive(Debug, Clone)]
pub(crate) struct ExecutionHostCall {
    pub(crate) request: HostRpcRequest,
    pub(crate) reply: DirectHostReplyHandle,
}

impl std::ops::Deref for ExecutionHostCall {
    type Target = HostRpcRequest;

    fn deref(&self) -> &Self::Target {
        &self.request
    }
}

#[derive(Debug)]
pub(crate) struct HostCallCompletion {
    pub(crate) reply: DirectHostReplyHandle,
    pub(crate) result: Result<Value, DeferredRpcError>,
}

#[derive(Debug)]
pub(crate) enum PendingNetConnect {
    Tcp {
        socket_id: String,
        socket: Box<ActiveTcpSocket>,
        pending_capability: agentos_runtime_tokio::capability::PendingCapability,
        local_reservation_id: Option<String>,
    },
    Unix {
        socket_id: String,
        socket: Box<ActiveUnixSocket>,
        pending_capability: agentos_runtime_tokio::capability::PendingCapability,
        remote_path: String,
        remote_abstract_path_hex: Option<String>,
    },
}

#[derive(Debug, Default)]
pub(crate) struct PendingNetConnectState {
    pub(crate) connected: Option<PendingNetConnect>,
    /// A bound-but-unlistened Unix socket is removed from the process table
    /// while its nonblocking connect is in flight. Keep the original handle
    /// here so a failed connect can restore the guest descriptor unchanged.
    pub(crate) bound_unix_listener: Option<(String, ActiveUnixListener)>,
}

#[derive(Debug)]
pub(crate) struct DeferredRpcError {
    pub(crate) code: String,
    pub(crate) message: String,
    pub(crate) details: Option<Value>,
}

impl From<agentos_execution::backend::HostServiceError> for DeferredRpcError {
    fn from(error: agentos_execution::backend::HostServiceError) -> Self {
        Self {
            code: error.code,
            message: error.message,
            details: error.details,
        }
    }
}

impl From<DeferredRpcError> for SidecarError {
    fn from(error: DeferredRpcError) -> Self {
        Self::Host(agentos_execution::backend::HostServiceError {
            code: error.code,
            message: error.message,
            details: error.details,
        })
    }
}

#[derive(Debug)]
pub(crate) struct ProcessEventEnvelope {
    pub(crate) connection_id: String,
    pub(crate) session_id: String,
    pub(crate) vm_id: String,
    pub(crate) process_id: String,
    pub(crate) event: ActiveExecutionEvent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SocketQueryKind {
    TcpListener,
    UdpBound,
}

// ---------------------------------------------------------------------------
// Command resolution
// ---------------------------------------------------------------------------

/// Transport facts selected by an executor adapter during resolution.
///
/// Common process/descriptor code consumes these capabilities and never
/// branches on an engine or language identity. The future Wasmtime adapter can
/// therefore choose its own transport profile even though it has the same
/// guest runtime kind as compatibility WASM.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ExecutionAdapterPolicy {
    pub(crate) accepts_inherited_host_network_fds: bool,
    pub(crate) materializes_direct_runtime_stdio: bool,
    pub(crate) canonicalizes_runtime_stdin: bool,
    pub(crate) supports_prepared_in_place_exec: bool,
    pub(crate) captured_output_limit: fn(&crate::limits::VmLimits) -> usize,
    pub(crate) captured_output_limit_setting: &'static str,
    pub(crate) kernel_driver_command: &'static str,
    pub(crate) forwards_kernel_stdin_rpc: bool,
    pub(crate) encodes_inherited_fd_bootstrap: bool,
    pub(crate) uses_javascript_entrypoint_projection: bool,
}

fn javascript_captured_output_limit(limits: &crate::limits::VmLimits) -> usize {
    limits.js_runtime.captured_output_limit_bytes
}

fn python_captured_output_limit(limits: &crate::limits::VmLimits) -> usize {
    limits.python.output_buffer_max_bytes
}

fn wasm_captured_output_limit(limits: &crate::limits::VmLimits) -> usize {
    limits.wasm.captured_output_limit_bytes
}

impl ExecutionAdapterPolicy {
    pub(crate) const BINDING: Self = Self {
        accepts_inherited_host_network_fds: false,
        materializes_direct_runtime_stdio: false,
        canonicalizes_runtime_stdin: false,
        supports_prepared_in_place_exec: false,
        captured_output_limit: javascript_captured_output_limit,
        captured_output_limit_setting: "limits.jsRuntime.capturedOutputLimitBytes",
        kernel_driver_command: BINDING_DRIVER_NAME,
        forwards_kernel_stdin_rpc: false,
        encodes_inherited_fd_bootstrap: false,
        uses_javascript_entrypoint_projection: false,
    };

    pub(crate) const DIRECT_RUNTIME: Self = Self {
        accepts_inherited_host_network_fds: false,
        materializes_direct_runtime_stdio: true,
        canonicalizes_runtime_stdin: true,
        supports_prepared_in_place_exec: false,
        captured_output_limit: javascript_captured_output_limit,
        captured_output_limit_setting: "limits.jsRuntime.capturedOutputLimitBytes",
        kernel_driver_command: JAVASCRIPT_COMMAND,
        forwards_kernel_stdin_rpc: true,
        encodes_inherited_fd_bootstrap: false,
        uses_javascript_entrypoint_projection: true,
    };

    pub(crate) const DIRECT_PYTHON_RUNTIME: Self = Self {
        accepts_inherited_host_network_fds: false,
        materializes_direct_runtime_stdio: true,
        canonicalizes_runtime_stdin: true,
        supports_prepared_in_place_exec: false,
        captured_output_limit: python_captured_output_limit,
        captured_output_limit_setting: "limits.python.outputBufferMaxBytes",
        kernel_driver_command: PYTHON_COMMAND,
        forwards_kernel_stdin_rpc: false,
        encodes_inherited_fd_bootstrap: false,
        uses_javascript_entrypoint_projection: false,
    };

    pub(crate) const KERNEL_HOST_CALL_POSIX: Self = Self {
        accepts_inherited_host_network_fds: true,
        materializes_direct_runtime_stdio: false,
        canonicalizes_runtime_stdin: false,
        supports_prepared_in_place_exec: true,
        captured_output_limit: wasm_captured_output_limit,
        captured_output_limit_setting: "limits.wasm.capturedOutputLimitBytes",
        kernel_driver_command: WASM_COMMAND,
        forwards_kernel_stdin_rpc: false,
        encodes_inherited_fd_bootstrap: true,
        uses_javascript_entrypoint_projection: false,
    };
}

#[derive(Debug)]
pub(crate) struct ResolvedChildProcessExecution {
    pub(crate) command: String,
    pub(crate) process_args: Vec<String>,
    pub(crate) runtime: GuestRuntimeKind,
    pub(crate) entrypoint: String,
    pub(crate) execution_args: Vec<String>,
    pub(crate) env: BTreeMap<String, String>,
    pub(crate) guest_cwd: String,
    pub(crate) host_cwd: PathBuf,
    pub(crate) wasm_permission_tier: Option<WasmPermissionTier>,
    pub(crate) binding_command: bool,
    pub(crate) adapter_policy: ExecutionAdapterPolicy,
}

// ---------------------------------------------------------------------------
// Utility types
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub(crate) struct ProcNetEntry {
    pub(crate) local_host: String,
    pub(crate) local_port: u16,
    pub(crate) state: String,
    pub(crate) inode: u64,
}

#[cfg(test)]
mod async_completion_tests {
    use super::*;
    use agentos_runtime_tokio::accounting::ResourceLimit;

    fn completion_runtime(
        maximum: usize,
        generation: u64,
    ) -> (RuntimeContext, Arc<ResourceLedger>) {
        completion_runtime_with_limits(maximum, maximum * 16, generation)
    }

    fn completion_runtime_with_limits(
        count_maximum: usize,
        byte_maximum: usize,
        generation: u64,
    ) -> (RuntimeContext, Arc<ResourceLedger>) {
        let process = agentos_runtime_tokio::SidecarRuntime::process(
            &agentos_runtime_tokio::RuntimeConfig::default(),
        )
        .expect("test process runtime")
        .context();
        let resources = Arc::new(ResourceLedger::child(
            format!("completion-test-vm-{generation}"),
            [
                (
                    ResourceClass::AsyncCompletions,
                    ResourceLimit::new(count_maximum, "limits.reactor.maxAsyncCompletions"),
                ),
                (
                    ResourceClass::AsyncCompletionBytes,
                    ResourceLimit::new(byte_maximum, "limits.reactor.maxAsyncCompletionBytes"),
                ),
            ],
            Arc::clone(process.resources()),
        ));
        (
            process.scoped_for_vm(Arc::clone(&resources), generation),
            resources,
        )
    }

    #[test]
    fn completion_reservations_bound_all_lanes_in_one_vm() {
        let (runtime, resources) = completion_runtime(2, 91);
        let (first_tx, mut first_rx) =
            async_completion_channel(runtime.clone(), 2, |value: &&str| value.len());
        let (second_tx, second_rx) =
            async_completion_channel(runtime.clone(), 2, |value: &&str| value.len());

        first_tx.try_send("first").expect("first lane admission");
        second_tx.try_send("second").expect("second lane admission");
        assert_eq!(resources.usage(ResourceClass::AsyncCompletions).used, 2);

        let error = first_tx
            .try_send("aggregate overflow")
            .expect_err("per-VM completion limit must span both lanes");
        assert_eq!(error.code, "ERR_AGENTOS_RESOURCE_LIMIT");
        assert!(error.message.contains("limits.reactor.maxAsyncCompletions"));

        assert_eq!(
            first_rx.try_recv().expect("release first completion"),
            "first"
        );
        second_tx
            .try_send("replacement")
            .expect("released aggregate slot can move to another lane");
        assert_eq!(resources.usage(ResourceClass::AsyncCompletions).used, 2);

        drop(first_rx);
        drop(second_rx);
        assert_eq!(
            resources.usage(ResourceClass::AsyncCompletions).used,
            0,
            "dropping queued lanes must release every completion reservation"
        );

        let (disconnected_tx, disconnected_rx) =
            async_completion_channel(runtime, 1, |value: &&str| value.len());
        drop(disconnected_rx);
        let error = disconnected_tx
            .try_send("disconnected")
            .expect_err("disconnected lane rejects insertion");
        assert_eq!(error.code, "ERR_AGENTOS_ASYNC_COMPLETION_DISCONNECTED");
        assert_eq!(resources.usage(ResourceClass::AsyncCompletions).used, 0);
    }

    #[test]
    fn failed_completion_send_and_vm_close_release_reservations() {
        let (runtime, resources) = completion_runtime(1, 92);
        let (held_tx, _held_rx) = async_completion_channel(runtime.clone(), 1, |_: &u8| 1);
        let (waiting_tx, waiting_rx) = async_completion_channel(runtime.clone(), 1, |_: &u8| 1);
        held_tx.try_send(1_u8).expect("fill aggregate limit");

        runtime.handle().block_on(async {
            let waiter = tokio::spawn(async move { waiting_tx.send(2_u8).await });
            tokio::task::yield_now().await;
            runtime.close_admission();
            let error = tokio::time::timeout(Duration::from_secs(1), waiter)
                .await
                .expect("VM close wakes completion admission waiter")
                .expect("completion waiter joins")
                .expect_err("closed VM rejects queued completion");
            assert_eq!(error.code, "ERR_AGENTOS_ASYNC_COMPLETION_CLOSED");
        });

        drop(held_tx);
        assert_eq!(
            resources.usage(ResourceClass::AsyncCompletions).used,
            1,
            "the queued item owns the only remaining reservation"
        );
        drop(_held_rx);
        drop(waiting_rx);
        assert_eq!(resources.usage(ResourceClass::AsyncCompletions).used, 0);
    }

    #[test]
    fn completion_byte_limit_plus_one_is_typed_and_rolls_back_count() {
        let (runtime, resources) = completion_runtime_with_limits(3, 4, 93);
        let (sender, receiver) =
            async_completion_channel(runtime.clone(), 3, |value: &Vec<u8>| value.len());
        sender.try_send(vec![1, 2, 3, 4]).expect("fill byte limit");
        assert_eq!(resources.usage(ResourceClass::AsyncCompletions).used, 1);
        assert_eq!(resources.usage(ResourceClass::AsyncCompletionBytes).used, 4);
        assert_eq!(
            sender.count_gauge.name(),
            TrackedLimit::AsyncCompletionCount
        );
        assert_eq!(sender.byte_gauge.name(), TrackedLimit::AsyncCompletionBytes);
        assert_eq!(sender.count_gauge.depth(), 1);
        assert_eq!(sender.byte_gauge.depth(), 4);

        let error = sender
            .try_send(vec![5])
            .expect_err("byte limit + 1 must fail atomically");
        assert_eq!(error.code, "ERR_AGENTOS_RESOURCE_LIMIT");
        assert_eq!(
            error
                .details
                .as_ref()
                .and_then(|value| value["resource"].as_str()),
            Some("asyncCompletionBytes")
        );
        assert_eq!(
            resources.usage(ResourceClass::AsyncCompletions).used,
            1,
            "failed byte admission must roll back its provisional count"
        );
        assert_eq!(resources.usage(ResourceClass::AsyncCompletionBytes).used, 4);

        let async_error = runtime
            .handle()
            .block_on(sender.send(vec![0; 5]))
            .expect_err(
                "a completion larger than the configured byte maximum must fail without waiting",
            );
        assert_eq!(async_error.code, "ERR_AGENTOS_RESOURCE_LIMIT");
        drop(receiver);
        assert_eq!(resources.usage(ResourceClass::AsyncCompletions).used, 0);
        assert_eq!(resources.usage(ResourceClass::AsyncCompletionBytes).used, 0);
        assert_eq!(sender.count_gauge.depth(), 0);
        assert_eq!(sender.byte_gauge.depth(), 0);
    }

    #[test]
    fn network_completion_callers_share_byte_limit_and_drain_gauges() {
        let (runtime, resources) = completion_runtime_with_limits(3, 10, 94);
        let (tcp_tx, mut tcp_rx) =
            async_completion_channel(runtime.clone(), 3, tcp_socket_event_retained_bytes);
        let (unix_tx, mut unix_rx) =
            async_completion_channel(runtime, 3, unix_listener_event_retained_bytes);

        tcp_tx
            .try_send(TcpSocketEvent::Error {
                code: Some(String::from("EC")),
                message: String::from("123456"),
            })
            .expect("TCP caller reaches the 80% near-limit warning threshold");
        assert_eq!(tcp_tx.byte_gauge.depth(), 8);
        assert_eq!(tcp_tx.byte_gauge.capacity(), 10);

        unix_tx
            .try_send(UnixListenerEvent::Error {
                code: None,
                message: String::from("12"),
            })
            .expect("Unix listener caller fills the shared byte budget exactly");
        assert_eq!(resources.usage(ResourceClass::AsyncCompletions).used, 2);
        assert_eq!(
            resources.usage(ResourceClass::AsyncCompletionBytes).used,
            10
        );

        let error = unix_tx
            .try_send(UnixListenerEvent::Error {
                code: None,
                message: String::from("x"),
            })
            .expect_err("aggregate network completion bytes at limit + 1 must fail");
        assert_eq!(error.code, "ERR_AGENTOS_RESOURCE_LIMIT");
        assert_eq!(
            error
                .details
                .as_ref()
                .and_then(|value| value["resource"].as_str()),
            Some("asyncCompletionBytes")
        );
        assert_eq!(
            resources.usage(ResourceClass::AsyncCompletions).used,
            2,
            "failed byte admission rolls back its provisional count reservation"
        );
        assert_eq!(unix_tx.count_gauge.depth(), 1);
        assert_eq!(unix_tx.byte_gauge.depth(), 2);

        assert!(matches!(
            tcp_rx.try_recv().expect("drain TCP completion"),
            TcpSocketEvent::Error { .. }
        ));
        assert!(matches!(
            unix_rx.try_recv().expect("drain Unix completion"),
            UnixListenerEvent::Error { .. }
        ));
        assert_eq!(resources.usage(ResourceClass::AsyncCompletions).used, 0);
        assert_eq!(resources.usage(ResourceClass::AsyncCompletionBytes).used, 0);
        assert_eq!(tcp_tx.count_gauge.depth(), 0);
        assert_eq!(tcp_tx.byte_gauge.depth(), 0);
        assert_eq!(unix_tx.count_gauge.depth(), 0);
        assert_eq!(unix_tx.byte_gauge.depth(), 0);
    }
}

#[cfg(test)]
mod socket_readiness_registry_tests {
    use super::*;
    use agentos_execution::backend::{ExecutionWakeError, ExecutionWakeTarget};
    use agentos_execution::v8_host::V8RuntimeHost;
    use agentos_runtime_tokio::accounting::ResourceLimit;

    #[derive(Default)]
    struct RecordingWakeTarget {
        readiness_publishes: AtomicUsize,
    }

    impl ExecutionWakeTarget for RecordingWakeTarget {
        fn publish_readiness(
            &self,
            _capability_id: u64,
            _capability_generation: u64,
            _flags: agentos_execution::backend::ExecutionReadyFlags,
        ) -> Result<(), ExecutionWakeError> {
            self.readiness_publishes.fetch_add(1, Ordering::AcqRel);
            Ok(())
        }

        fn remove_readiness(
            &self,
            _capability_id: u64,
            _capability_generation: u64,
        ) -> Result<(), ExecutionWakeError> {
            Ok(())
        }

        fn set_application_read_interest(
            &self,
            _capability_id: u64,
            _capability_generation: u64,
            _enabled: bool,
        ) -> Result<(), ExecutionWakeError> {
            Ok(())
        }

        fn publish_signal(
            &self,
            _signal: i32,
            _delivery_token: u64,
        ) -> Result<(), ExecutionWakeError> {
            Ok(())
        }

        fn send_adapter_event(
            &self,
            _event_type: &str,
            _payload: &Value,
            _encoded_limit_name: &'static str,
            _max_encoded_bytes: usize,
        ) -> Result<(), ExecutionWakeError> {
            Ok(())
        }
    }

    fn kernel_target(
        capability_id: u64,
        capability_generation: u64,
        target_id: &str,
    ) -> KernelSocketReadinessTarget {
        KernelSocketReadinessTarget {
            session: None,
            notify: Some(Arc::new(Notify::new())),
            capability_id,
            capability_generation,
            target_id: target_id.to_owned(),
            event: KernelSocketReadinessEvent::Data,
            live: Arc::new(AtomicBool::new(true)),
        }
    }

    #[test]
    fn kernel_registry_keeps_aliases_independent_until_each_unregisters() {
        let registry = KernelSocketReadinessRegistryState::new(2);
        registry
            .register(41, kernel_target(1, 1, "parent"))
            .expect("register parent alias");
        registry
            .register(41, kernel_target(2, 1, "child"))
            .expect("register child alias");

        let targets = registry.targets(41);
        assert_eq!(targets.len(), 2);
        assert!(targets.iter().any(|target| target.target_id == "parent"));
        assert!(targets.iter().any(|target| target.target_id == "child"));

        registry.unregister(41, (2, 1));
        let targets = registry.targets(41);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].target_id, "parent");

        registry.unregister(41, (1, 1));
        assert!(registry.targets(41).is_empty());
    }

    #[test]
    fn kernel_registry_rebind_upserts_without_growing_and_enforces_bound() {
        let registry = KernelSocketReadinessRegistryState::new(1);
        registry
            .register(41, kernel_target(1, 1, "before-exec"))
            .expect("register initial target");
        registry
            .register(41, kernel_target(1, 1, "after-exec"))
            .expect("rebind same alias");
        let targets = registry.targets(41);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].target_id, "after-exec");

        let error = registry
            .register(42, kernel_target(2, 1, "other"))
            .expect_err("registry must enforce its configured bound");
        assert!(error
            .to_string()
            .contains("ERR_AGENTOS_KERNEL_READINESS_TARGET_LIMIT"));
    }

    #[test]
    fn retired_socket_subscription_drops_cloned_late_end_wake() {
        let resources = ResourceLedger::root(
            "late-socket-wake-test",
            [(
                ResourceClass::Capabilities,
                ResourceLimit::new(1, "limits.reactor.maxCapabilities"),
            )],
        );
        let wake_target = Arc::new(RecordingWakeTarget::default());
        let session = ExecutionWakeHandle::new(
            agentos_execution::backend::ExecutionWakeIdentity {
                generation: 1,
                pid: 1,
            },
            wake_target.clone(),
        );
        let subscribers = SocketReadinessSubscribers::new(&resources);
        let registration = SocketReadinessRegistration::new(Arc::clone(&subscribers), None, None);
        registration.register(
            Some(session),
            Some((1, 1)),
            Arc::new(Notify::new()),
            agentos_runtime_tokio::readiness::ReadyFlags::READABLE,
        );
        assert_eq!(wake_target.readiness_publishes.load(Ordering::Acquire), 1);

        // A reader task can already hold this clone when close retires the
        // capability. It must not recreate readiness with a late END wake.
        let late_transport_target = subscribers.targets().pop().expect("registered target");
        registration.retire();
        assert!(!late_transport_target
            .publish_readiness(agentos_runtime_tokio::readiness::ReadyFlags::END)
            .expect("retired readiness publish must be ignored"));
        assert_eq!(wake_target.readiness_publishes.load(Ordering::Acquire), 1);
        assert!(subscribers.targets().is_empty());
    }

    #[test]
    fn socket_subscription_without_executor_wake_notifies_posix_poll_owner() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("build notify test runtime");
        let take_notify_permit = |notify: &Notify| {
            runtime.block_on(async {
                tokio::time::timeout(Duration::from_millis(10), notify.notified())
                    .await
                    .is_ok()
            })
        };
        let resources = ResourceLedger::root(
            "runtime-neutral-socket-wake-test",
            [(
                ResourceClass::Capabilities,
                ResourceLimit::new(1, "limits.reactor.maxCapabilities"),
            )],
        );
        let subscribers = SocketReadinessSubscribers::new(&resources);
        let registration = SocketReadinessRegistration::new(Arc::clone(&subscribers), None, None);
        let owner_notify = Arc::new(Notify::new());
        registration.register(
            None,
            Some((1, 1)),
            Arc::clone(&owner_notify),
            agentos_runtime_tokio::readiness::ReadyFlags::READABLE,
        );

        assert!(take_notify_permit(&owner_notify));
        let target = subscribers.targets().pop().expect("registered target");
        assert!(target
            .publish_readiness(agentos_runtime_tokio::readiness::ReadyFlags::READABLE)
            .expect("runtime-neutral readiness publish"));
        assert!(take_notify_permit(&owner_notify));
    }

    #[test]
    fn native_alias_registration_is_raii_and_read_interest_is_aggregate_or() {
        let process_runtime = agentos_runtime_tokio::SidecarRuntime::process(
            &agentos_runtime_tokio::RuntimeConfig::default(),
        )
        .expect("create subscriber test runtime");
        let resources = ResourceLedger::root(
            "socket-subscriber-test",
            [(
                ResourceClass::Capabilities,
                ResourceLimit::new(2, "limits.reactor.maxCapabilities"),
            )],
        );
        let host = V8RuntimeHost::spawn(&process_runtime.context())
            .expect("spawn subscriber test V8 host");
        let session = ExecutionWakeHandle::new(
            agentos_execution::backend::ExecutionWakeIdentity {
                generation: 1,
                pid: 1,
            },
            Arc::new(host.session_handle(String::from("socket-subscriber-test"))),
        );
        let subscribers = SocketReadinessSubscribers::new(&resources);
        let aggregate_interest = Arc::new(AtomicBool::new(false));
        let interest_notify = Arc::new(Notify::new());

        let parent = SocketReadinessRegistration::new(
            Arc::clone(&subscribers),
            Some(Arc::clone(&aggregate_interest)),
            Some(Arc::clone(&interest_notify)),
        );
        parent.register(
            Some(session.clone()),
            Some((1, 1)),
            Arc::new(Notify::new()),
            agentos_runtime_tokio::readiness::ReadyFlags::READABLE,
        );

        let child = SocketReadinessRegistration::new(
            Arc::clone(&subscribers),
            Some(Arc::clone(&aggregate_interest)),
            Some(Arc::clone(&interest_notify)),
        );
        child.register(
            Some(session),
            Some((2, 1)),
            Arc::new(Notify::new()),
            agentos_runtime_tokio::readiness::ReadyFlags::READABLE,
        );
        assert_eq!(subscribers.targets().len(), 2);

        let aggregate = subscribers
            .set_application_read_interest_state((1, 1), true)
            .expect("enable parent interest");
        parent.update_aggregate_interest(aggregate);
        assert!(aggregate_interest.load(Ordering::Acquire));

        let aggregate = subscribers
            .set_application_read_interest_state((2, 1), true)
            .expect("enable child interest");
        child.update_aggregate_interest(aggregate);
        let aggregate = subscribers
            .set_application_read_interest_state((1, 1), false)
            .expect("disable parent interest");
        parent.update_aggregate_interest(aggregate);
        assert!(
            aggregate_interest.load(Ordering::Acquire),
            "one paused alias must not stop an interested sibling"
        );

        drop(child);
        assert!(!aggregate_interest.load(Ordering::Acquire));
        assert_eq!(subscribers.targets().len(), 1);
        assert_eq!(subscribers.targets()[0].capability_id, 1);

        drop(parent);
        assert!(subscribers.targets().is_empty());
    }
}
