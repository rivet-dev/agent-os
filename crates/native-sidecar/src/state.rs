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
    v8_host::V8SessionHandle, JavascriptExecution, JavascriptExecutionEngine,
    JavascriptSyncRpcRequest, PythonExecution, PythonExecutionEngine, PythonVfsRpcRequest,
    PythonVfsRpcResponder, WasmExecution, WasmExecutionEngine,
};
use agentos_kernel::fd_table::TransferredFd;
use agentos_kernel::kernel::{KernelProcessHandle, KernelVm};
use agentos_kernel::mount_table::MountTable;
use agentos_kernel::root_fs::RootFilesystemMode;
use agentos_kernel::socket_table::SocketId;
use agentos_native_sidecar_core::VmLayerStore;
use agentos_runtime::accounting::{Reservation, ResourceClass, ResourceLedger, SharedReservation};
use agentos_runtime::RuntimeContext;
use agentos_vm_config as vm_config;
use agentos_vm_config::PermissionsPolicy;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use socket2::Socket;
use std::borrow::Borrow as StdBorrow;
use std::cell::{Ref, RefCell, RefMut};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::error::Error;
use std::fmt;
use std::fs::File;
use std::future::Future;
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::pin::Pin;
use std::rc::Rc;
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

/// Retains the first capability lease committed for one open socket
/// description. Process-local aliases may own additional leases, but the
/// original reservation and registry row must survive until the final
/// dup/SCM_RIGHTS alias drops.
#[derive(Debug, Default)]
pub(crate) struct SocketDescriptionLease {
    lease: Mutex<Option<Arc<agentos_runtime::capability::CapabilityLease>>>,
}

impl SocketDescriptionLease {
    pub(crate) fn retain(&self, lease: Arc<agentos_runtime::capability::CapabilityLease>) {
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

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn used(&self) -> usize {
        self.used.load(Ordering::Acquire)
    }

    pub(crate) fn limit(&self) -> usize {
        self.limit
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

    pub(crate) fn set(&self, value_us: u64, interval_us: u64) -> (u64, u64) {
        let mut timer = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let now = Instant::now();
        refresh_real_interval_timer(&mut timer, now);
        let previous = real_interval_timer_values(&timer, now);
        timer.deadline = (value_us != 0)
            .then(|| now.checked_add(Duration::from_micros(value_us)))
            .flatten();
        timer.interval = Duration::from_micros(interval_us);
        previous
    }

    pub(crate) fn take_expiry(&self) -> bool {
        let mut timer = self.state.lock().unwrap_or_else(|error| error.into_inner());
        refresh_real_interval_timer(&mut timer, Instant::now());
        std::mem::take(&mut timer.pending_expiry)
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
}

/// One completion admitted against the VM-wide aggregate count. The local
/// channel capacity remains an independent per-lane bound; this reservation
/// prevents N handles from each consuming that capacity simultaneously.
#[derive(Debug)]
struct QueuedAsyncCompletion<T> {
    value: T,
    _reservation: Reservation,
}

pub(crate) struct AsyncCompletionSender<T> {
    inner: TokioSender<QueuedAsyncCompletion<T>>,
    runtime: RuntimeContext,
}

impl<T> Clone for AsyncCompletionSender<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            runtime: self.runtime.clone(),
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
) -> (AsyncCompletionSender<T>, AsyncCompletionReceiver<T>) {
    let (sender, receiver) = tokio::sync::mpsc::channel(capacity);
    (
        AsyncCompletionSender {
            inner: sender,
            runtime,
        },
        AsyncCompletionReceiver { inner: receiver },
    )
}

impl<T> AsyncCompletionSender<T> {
    pub(crate) async fn send(&self, value: T) -> Result<(), String> {
        let resources = Arc::clone(self.runtime.resources());
        let reservation = tokio::select! {
            biased;
            () = self.runtime.admission_closed() => {
                return Err(String::from(
                    "ERR_AGENTOS_ASYNC_COMPLETION_CLOSED: VM completion admission is closed",
                ));
            }
            () = self.inner.closed() => {
                return Err(String::from(
                    "ERR_AGENTOS_ASYNC_COMPLETION_DISCONNECTED: completion consumer disconnected",
                ));
            }
            reservation = resources.reserve_when_available(ResourceClass::AsyncCompletions, 1) => {
                reservation.map_err(|error| error.to_string())?
            }
        };
        let queued = QueuedAsyncCompletion {
            value,
            _reservation: reservation,
        };
        tokio::select! {
            biased;
            () = self.runtime.admission_closed() => Err(String::from(
                "ERR_AGENTOS_ASYNC_COMPLETION_CLOSED: VM completion admission closed before queue insertion",
            )),
            result = self.inner.send(queued) => result.map_err(|_| String::from(
                "ERR_AGENTOS_ASYNC_COMPLETION_DISCONNECTED: completion consumer disconnected",
            )),
        }
    }

    pub(crate) fn try_send(&self, value: T) -> Result<(), String> {
        if !self.runtime.admission_is_open() {
            return Err(String::from(
                "ERR_AGENTOS_ASYNC_COMPLETION_CLOSED: VM completion admission is closed",
            ));
        }
        let reservation = self
            .runtime
            .resources()
            .reserve(ResourceClass::AsyncCompletions, 1)
            .map_err(|error| error.to_string())?;
        self.inner
            .try_send(QueuedAsyncCompletion {
                value,
                _reservation: reservation,
            })
            .map_err(|error| match error {
                tokio::sync::mpsc::error::TrySendError::Full(_) => String::from(
                    "ERR_AGENTOS_ASYNC_COMPLETION_LANE_LIMIT: completion lane is full; raise limits.reactor.maxAsyncCompletions",
                ),
                tokio::sync::mpsc::error::TrySendError::Closed(_) => String::from(
                    "ERR_AGENTOS_ASYNC_COMPLETION_DISCONNECTED: completion consumer disconnected",
                ),
            })
    }
}

impl<T> AsyncCompletionReceiver<T> {
    pub(crate) fn try_recv(&mut self) -> Result<T, tokio::sync::mpsc::error::TryRecvError> {
        self.inner.try_recv().map(|queued| queued.value)
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
pub(crate) const PYTHON_VFS_RPC_GUEST_ROOT: &str = "/";
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
pub(crate) const DEFAULT_JAVASCRIPT_NET_BACKLOG: u32 = 511;
pub(crate) const LOOPBACK_EXEMPT_PORTS_ENV: &str = "AGENTOS_LOOPBACK_EXEMPT_PORTS";
pub(crate) const BINDING_DRIVER_NAME: &str = "agentos-host-callbacks";
pub(crate) const MAPPED_HOST_FD_START: u32 = 1_000_000_000;

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
    pub runtime: agentos_runtime::RuntimeConfig,
}

impl Default for NativeSidecarConfig {
    fn default() -> Self {
        Self {
            sidecar_id: String::from("agentos-native-sidecar"),
            max_frame_bytes: DEFAULT_MAX_FRAME_BYTES,
            compile_cache_root: None,
            expected_auth_token: None,
            acp_termination_grace: Duration::from_secs(3),
            runtime: agentos_runtime::RuntimeConfig::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidecarError {
    ResourceLimit(agentos_runtime::accounting::LimitError),
    RequestAdmission {
        code: &'static str,
        message: String,
        configuration_path: Option<&'static str>,
        retryable: bool,
        errno: &'static str,
    },
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
    Bridge(String),
    Io(String),
}

impl fmt::Display for SidecarError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ResourceLimit(error) => error.fmt(f),
            Self::RequestAdmission { message, .. } => f.write_str(message),
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
        }
    }
}

impl Error for SidecarError {}

/// Format a resource-limit failure for an untrusted guest. VM-local occupancy
/// is safe to expose to that VM; process occupancy includes other VMs and must
/// not become a cross-tenant resource oracle.
pub(crate) fn guest_limit_message(limit: &agentos_runtime::accounting::LimitError) -> String {
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

impl From<agentos_runtime::accounting::LimitError> for SidecarError {
    fn from(error: agentos_runtime::accounting::LimitError) -> Self {
        Self::ResourceLimit(error)
    }
}

impl From<agentos_runtime::capability::CapabilityError> for SidecarError {
    fn from(error: agentos_runtime::capability::CapabilityError) -> Self {
        match error {
            agentos_runtime::capability::CapabilityError::Limit(limit) => {
                Self::ResourceLimit(limit)
            }
            other => Self::Execution(other.to_string()),
        }
    }
}

impl From<agentos_runtime::TaskSpawnError> for SidecarError {
    fn from(error: agentos_runtime::TaskSpawnError) -> Self {
        match error {
            agentos_runtime::TaskSpawnError::ResourceLimit(limit) => Self::ResourceLimit(limit),
            agentos_runtime::TaskSpawnError::AdmissionClosed { scope } => Self::Execution(format!(
                "ERR_AGENTOS_TASK_ADMISSION_CLOSED: scope={scope} is closing"
            )),
        }
    }
}

impl From<agentos_runtime::BlockingJobError> for SidecarError {
    fn from(error: agentos_runtime::BlockingJobError) -> Self {
        match error {
            agentos_runtime::BlockingJobError::ResourceLimit(limit) => Self::ResourceLimit(limit),
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

    /// Async callback delivery for extension request tasks. Transports with a
    /// native async waiter override this so a callback never parks the sidecar
    /// runtime thread. The default preserves compatibility for in-process test
    /// transports that only implement the synchronous API.
    fn send_request_async<'a>(
        &'a self,
        request: SidecarRequestFrame,
        timeout: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<SidecarResponseFrame, SidecarError>> + 'a>> {
        Box::pin(async move { self.send_request(request, timeout) })
    }
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

    pub(crate) async fn invoke_async(
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
        let response = transport.send_request_async(request, timeout).await?;
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
}

impl<B> Clone for SharedBridge<B> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            permissions: Arc::clone(&self.permissions),
            #[cfg(test)]
            set_vm_permissions_outcomes: Arc::clone(&self.set_vm_permissions_outcomes),
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

/// VM-local language-engine ownership.
///
/// Engine context maps are mutable and must not remain process-global: a long
/// launch in one VM would otherwise serialize every other VM behind the same
/// engine owner. A launch clones this owner out through [`VmHandle`], releases
/// the outer `VmState` borrow, and may then hold only the selected language's
/// `RefCell` borrow across startup awaits. That deliberately preserves narrow
/// per-VM/per-runtime ordering while leaving the same VM's kernel, filesystem,
/// sockets, and other language engines independently accessible.
#[derive(Clone)]
pub(crate) struct VmExecutionEngines {
    inner: Rc<VmExecutionEnginesInner>,
}

struct VmExecutionEnginesInner {
    vm_id: String,
    javascript: RefCell<JavascriptExecutionEngine>,
    python: RefCell<PythonExecutionEngine>,
    wasm: RefCell<WasmExecutionEngine>,
}

impl VmExecutionEngines {
    pub(crate) fn new(
        vm_id: String,
        runtime: agentos_runtime::RuntimeContext,
        event_notify: Arc<tokio::sync::Notify>,
    ) -> Self {
        let mut javascript = JavascriptExecutionEngine::new(runtime.clone());
        javascript.set_event_notify(Some(Arc::clone(&event_notify)));
        let mut python = PythonExecutionEngine::new(runtime.clone());
        python.set_event_notify(Some(Arc::clone(&event_notify)));
        let mut wasm = WasmExecutionEngine::new(runtime);
        wasm.set_event_notify(Some(event_notify));
        Self {
            inner: Rc::new(VmExecutionEnginesInner {
                vm_id,
                javascript: RefCell::new(javascript),
                python: RefCell::new(python),
                wasm: RefCell::new(wasm),
            }),
        }
    }

    pub(crate) fn javascript(
        &self,
        operation: &'static str,
    ) -> Result<RefMut<'_, JavascriptExecutionEngine>, SidecarError> {
        self.inner.javascript.try_borrow_mut().map_err(|_| {
            execution_engine_conflict_error(&self.inner.vm_id, "JavaScript", operation)
        })
    }

    pub(crate) fn python(
        &self,
        operation: &'static str,
    ) -> Result<RefMut<'_, PythonExecutionEngine>, SidecarError> {
        self.inner
            .python
            .try_borrow_mut()
            .map_err(|_| execution_engine_conflict_error(&self.inner.vm_id, "Python", operation))
    }

    pub(crate) fn wasm(
        &self,
        operation: &'static str,
    ) -> Result<RefMut<'_, WasmExecutionEngine>, SidecarError> {
        self.inner.wasm.try_borrow_mut().map_err(|_| {
            execution_engine_conflict_error(&self.inner.vm_id, "WebAssembly", operation)
        })
    }
}

impl Drop for VmExecutionEnginesInner {
    fn drop(&mut self) {
        self.javascript.get_mut().dispose_vm(&self.vm_id);
        self.python.get_mut().dispose_vm(&self.vm_id);
        self.wasm.get_mut().dispose_vm(&self.vm_id);
    }
}

fn execution_engine_conflict_error(vm_id: &str, runtime: &str, operation: &str) -> SidecarError {
    SidecarError::InvalidState(format!(
        "ERR_AGENTOS_VM_EXECUTION_CONFLICT: {runtime} execution state for VM {vm_id} is already in use during {operation}"
    ))
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
    /// Child of the one process ledger owned by RuntimeContext.
    pub(crate) resources: Arc<agentos_runtime::accounting::ResourceLedger>,
    /// VM-scoped admission view over the process's single Tokio runtime and
    /// fixed blocking executor. This owns no runtime or worker of its own.
    pub(crate) runtime_context: agentos_runtime::RuntimeContext,
    /// Language runtime context maps owned by this VM generation.
    pub(crate) execution_engines: VmExecutionEngines,
    /// One resolved SQLite transport shared by every durable VM subsystem.
    pub(crate) database: Option<crate::vm_sqlite::SharedVmSqliteDatabase>,
    /// Common lifecycle/identity registry for native and kernel backends.
    pub(crate) capabilities: agentos_runtime::capability::CapabilityRegistry,
    pub(crate) dns: VmDnsConfig,
    pub(crate) listen_policy: VmListenPolicy,
    pub(crate) create_loopback_exempt_ports: BTreeSet<u16>,
    pub(crate) guest_env: BTreeMap<String, String>,
    pub(crate) requested_runtime: GuestRuntimeKind,
    pub(crate) root_filesystem_mode: RootFilesystemMode,
    pub(crate) guest_cwd: String,
    pub(crate) cwd: PathBuf,
    pub(crate) host_cwd: PathBuf,
    pub(crate) kernel: SidecarKernel,
    pub(crate) kernel_socket_readiness: KernelSocketReadinessRegistry,
    /// Sidecar-only host-network descriptions currently retained by an opaque
    /// SCM_RIGHTS transfer. Weak entries make queue discard/receive lifecycle
    /// automatic while allowing VM-wide limit accounting to see descriptions
    /// that temporarily have no process-map entry.
    pub(crate) host_net_transfer_descriptions: HostNetTransferDescriptionRegistry,
    pub(crate) loaded_snapshot: Option<FilesystemSnapshot>,
    pub(crate) configuration: VmConfiguration,
    pub(crate) layers: VmLayerStore,
    pub(crate) command_guest_paths: BTreeMap<String, String>,
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
    pub(crate) signal_states: BTreeMap<String, BTreeMap<u32, SignalHandlerRegistration>>,
    /// Legacy staging root slot retained for same-version internal state shape.
    /// The current `/opt/agentos` projection mounts package tars and synthetic
    /// symlink leaves directly, so this remains `None`.
    pub(crate) packages_staging_root: Option<PathBuf>,
    /// Projected agent launch surface, keyed by agent id. Sourced from the
    /// packed vbare manifests at `ConfigureVm`/`LinkPackage` time — packed
    /// packages ship no `agentos-package.json`, so agent enumeration and
    /// resolution read this instead of the guest filesystem.
    pub(crate) projected_agent_launch: BTreeMap<String, ProjectedAgentLaunch>,
    /// Guest paths that were present in the VM shadow root during the last
    /// shadow->kernel sync walk. The next walk diffs the current shadow tree
    /// against this set so guest deletions performed directly on the shadow
    /// (host-side runtimes, WASI passthrough writes) propagate into the kernel
    /// VFS instead of being resurrected by the otherwise additive sync.
    /// Memory is bounded by the shadow tree itself, which is capped by the
    /// kernel filesystem inode/byte resource limits that bound what the walk
    /// can materialize.
    pub(crate) shadow_sync_inventory: BTreeMap<String, ShadowSyncInventoryEntry>,
    pub(crate) unix_address_registry: GuestUnixAddressRegistry,
    pub(crate) unix_socket_host_dir: PathBuf,
}

/// Cloneable, thread-affine access to one VM's mutable state.
///
/// Guest execution is deliberately driven by the protocol process's
/// `LocalSet`, so this is an `Rc` boundary rather than a cross-thread mutex.
/// Callers must release a borrow before awaiting external work; cloning a
/// handle never grants access to another VM.
#[derive(Clone)]
pub(crate) struct VmHandle {
    inner: Rc<RefCell<VmState>>,
}

impl VmHandle {
    pub(crate) fn new(state: VmState) -> Self {
        Self {
            inner: Rc::new(RefCell::new(state)),
        }
    }

    pub(crate) fn borrow(&self) -> Ref<'_, VmState> {
        self.inner.as_ref().borrow()
    }

    pub(crate) fn borrow_mut(&self) -> RefMut<'_, VmState> {
        self.inner.borrow_mut()
    }

    pub(crate) fn try_borrow_mut(
        &self,
        operation: &'static str,
    ) -> Result<RefMut<'_, VmState>, SidecarError> {
        self.inner.try_borrow_mut().map_err(|_| {
            SidecarError::InvalidState(format!(
                "ERR_AGENTOS_VM_COORDINATOR_CONFLICT: VM state is already in a critical section during {operation}"
            ))
        })
    }

    pub(crate) fn try_read<T>(
        &self,
        operation: &'static str,
        read: impl FnOnce(&VmState) -> T,
    ) -> Result<T, SidecarError> {
        let state = self.inner.try_borrow().map_err(|_| {
            SidecarError::InvalidState(format!(
                "ERR_AGENTOS_VM_COORDINATOR_CONFLICT: VM state is already in a mutable critical section during {operation}"
            ))
        })?;
        Ok(read(&state))
    }

    pub(crate) fn try_command<T>(
        &self,
        operation: &'static str,
        command: impl FnOnce(&mut VmState) -> Result<T, SidecarError>,
    ) -> Result<T, SidecarError> {
        let mut state = self.inner.try_borrow_mut().map_err(|_| {
            SidecarError::InvalidState(format!(
                "ERR_AGENTOS_VM_COORDINATOR_CONFLICT: VM state is already in a critical section during {operation}"
            ))
        })?;
        command(&mut state)
    }
}

impl std::fmt::Debug for VmHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.borrow();
        formatter
            .debug_struct("VmHandle")
            .field("connection_id", &state.connection_id)
            .field("session_id", &state.session_id)
            .field("generation", &state.generation)
            .finish_non_exhaustive()
    }
}

/// Registry facade that preserves the sidecar's existing short-borrow API
/// while storing each VM behind an independently cloneable handle.
#[derive(Debug, Default)]
pub(crate) struct VmRegistry {
    vms: BTreeMap<String, VmHandle>,
}

impl VmRegistry {
    pub(crate) fn len(&self) -> usize {
        self.vms.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.vms.is_empty()
    }

    pub(crate) fn contains_key<Q>(&self, key: &Q) -> bool
    where
        String: StdBorrow<Q>,
        Q: Ord + ?Sized,
    {
        self.vms.contains_key(key)
    }

    pub(crate) fn get<Q>(&self, key: &Q) -> Option<Ref<'_, VmState>>
    where
        String: StdBorrow<Q>,
        Q: Ord + ?Sized,
    {
        self.vms.get(key).map(VmHandle::borrow)
    }

    pub(crate) fn get_mut<Q>(&self, key: &Q) -> Option<RefMut<'_, VmState>>
    where
        String: StdBorrow<Q>,
        Q: Ord + ?Sized,
    {
        self.vms.get(key).map(VmHandle::borrow_mut)
    }

    pub(crate) fn handle<Q>(&self, key: &Q) -> Option<VmHandle>
    where
        String: StdBorrow<Q>,
        Q: Ord + ?Sized,
    {
        self.vms.get(key).cloned()
    }

    pub(crate) fn insert(
        &mut self,
        key: String,
        state: VmState,
    ) -> Result<Option<VmState>, SidecarError> {
        let replacement = match self.vms.remove(&key) {
            Some(handle) => match Rc::try_unwrap(handle.inner) {
                Ok(state) => Some(state.into_inner()),
                Err(inner) => {
                    self.vms.insert(key.clone(), VmHandle { inner });
                    return Err(vm_handle_conflict_error(&key, "replace"));
                }
            },
            None => None,
        };
        self.vms.insert(key, VmHandle::new(state));
        Ok(replacement)
    }

    pub(crate) fn try_remove<Q>(
        &mut self,
        key: &Q,
        operation: &'static str,
    ) -> Result<Option<VmState>, SidecarError>
    where
        String: StdBorrow<Q>,
        Q: Ord + ?Sized,
    {
        let Some((owned_key, handle)) = self.vms.remove_entry(key) else {
            return Ok(None);
        };
        match Rc::try_unwrap(handle.inner) {
            Ok(state) => Ok(Some(state.into_inner())),
            Err(inner) => {
                self.vms.insert(owned_key.clone(), VmHandle { inner });
                Err(vm_handle_conflict_error(&owned_key, operation))
            }
        }
    }

    pub(crate) fn clear(&mut self) -> Result<(), SidecarError> {
        if let Some((key, _)) = self
            .vms
            .iter()
            .find(|(_, handle)| Rc::strong_count(&handle.inner) != 1)
        {
            return Err(vm_handle_conflict_error(key, "clear"));
        }
        for (key, handle) in std::mem::take(&mut self.vms) {
            match Rc::try_unwrap(handle.inner) {
                Ok(state) => drop(state.into_inner()),
                Err(inner) => {
                    self.vms.insert(key.clone(), VmHandle { inner });
                    return Err(vm_handle_conflict_error(&key, "clear"));
                }
            }
        }
        Ok(())
    }

    pub(crate) fn retain(&mut self, mut keep: impl FnMut(&String, &mut VmState) -> bool) {
        self.vms.retain(|key, handle| {
            match handle.inner.try_borrow_mut() {
                Ok(mut state) => {
                    let retained = keep(key, &mut state);
                    if !retained && Rc::strong_count(&handle.inner) != 1 {
                        eprintln!(
                            "ERR_AGENTOS_VM_COORDINATOR_CONFLICT: deferring removal of VM {key} while an owned operation retains its handle"
                        );
                        true
                    } else {
                        retained
                    }
                }
                Err(_) => {
                    eprintln!(
                        "ERR_AGENTOS_VM_COORDINATOR_CONFLICT: deferring retain-filter for VM {key} while a short critical section is active"
                    );
                    true
                }
            }
        });
    }

    pub(crate) fn keys(&self) -> impl Iterator<Item = &String> {
        self.vms.keys()
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&String, Ref<'_, VmState>)> {
        self.vms.iter().map(|(key, handle)| (key, handle.borrow()))
    }

    pub(crate) fn values(&self) -> impl Iterator<Item = Ref<'_, VmState>> {
        self.vms.values().map(VmHandle::borrow)
    }

    pub(crate) fn values_mut(&self) -> impl Iterator<Item = RefMut<'_, VmState>> {
        self.vms.values().filter_map(|handle| {
            handle.inner.try_borrow_mut().map_err(|_| {
                eprintln!(
                    "ERR_AGENTOS_VM_COORDINATOR_CONFLICT: deferring mutable VM iteration while a short critical section is active"
                );
            }).ok()
        })
    }
}

fn vm_handle_conflict_error(vm_id: &str, operation: &str) -> SidecarError {
    SidecarError::InvalidState(format!(
        "ERR_AGENTOS_VM_COORDINATOR_CONFLICT: cannot {operation} VM {vm_id} while an owned operation retains its handle"
    ))
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
    pub(crate) _capability: agentos_runtime::capability::CapabilityLease,
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
    pub(crate) resources: Arc<agentos_runtime::accounting::ResourceLedger>,
    pub(crate) runtime_context: agentos_runtime::RuntimeContext,
    pub(crate) capabilities: agentos_runtime::capability::CapabilityRegistry,
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

/// Filesystem object kind captured during a shadow-root inventory walk.
///
/// Tracking the kind as well as the path is required for Linux replacement
/// semantics: a regular file replacing a symlink (or a directory replacing a
/// file) must replace the directory entry itself, never follow the stale
/// object that happened to occupy the same pathname in the kernel VFS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShadowNodeType {
    Directory,
    File,
    Symlink,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ShadowSyncInventoryEntry {
    pub(crate) node_type: ShadowNodeType,
    /// The previous kernel entry could not be removed. Keeping the tombstone
    /// makes the next walk retry instead of permanently forgetting a failed
    /// reconciliation.
    pub(crate) deletion_pending: bool,
}

impl ShadowSyncInventoryEntry {
    pub(crate) fn present(node_type: ShadowNodeType) -> Self {
        Self {
            node_type,
            deletion_pending: false,
        }
    }
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
pub(crate) struct JavascriptSocketPathContext {
    pub(crate) sandbox_root: PathBuf,
    pub(crate) unix_abstract_namespace: [u8; 32],
    pub(crate) unix_socket_host_dir: PathBuf,
    pub(crate) unix_bound_addresses: GuestUnixAddressRegistry,
    pub(crate) host_net_transfer_descriptions: HostNetTransferDescriptionRegistry,
    pub(crate) mounts: Vec<MountDescriptor>,
    pub(crate) listen_policy: VmListenPolicy,
    pub(crate) loopback_exempt_ports: BTreeSet<u16>,
    pub(crate) tcp_loopback_guest_to_host_ports: BTreeMap<(JavascriptSocketFamily, u16), u16>,
    pub(crate) http_loopback_targets:
        BTreeMap<(JavascriptSocketFamily, u16), JavascriptHttpLoopbackTarget>,
    pub(crate) udp_loopback_guest_to_host_ports: BTreeMap<(JavascriptSocketFamily, u16), u16>,
    pub(crate) udp_loopback_host_to_guest_ports: BTreeMap<(JavascriptSocketFamily, u16), u16>,
    pub(crate) used_tcp_guest_ports: BTreeMap<JavascriptSocketFamily, BTreeSet<u16>>,
    pub(crate) used_udp_guest_ports: BTreeMap<JavascriptSocketFamily, BTreeSet<u16>>,
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
    pub(crate) pending_connections: VecDeque<Arc<GuestUnixConnectionState>>,
}

pub(crate) type GuestUnixAddressRegistry =
    Arc<Mutex<BTreeMap<String, GuestUnixAddressRegistryEntry>>>;

#[derive(Debug, Clone)]
pub(crate) struct JavascriptHttpLoopbackTarget {
    pub(crate) process_id: String,
    pub(crate) server_id: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum JavascriptSocketFamily {
    Ipv4,
    Ipv6,
}

impl JavascriptSocketFamily {
    pub(crate) fn from_ip(ip: IpAddr) -> Self {
        match ip {
            IpAddr::V4(_) => Self::Ipv4,
            IpAddr::V6(_) => Self::Ipv6,
        }
    }
}

impl From<JavascriptUdpFamily> for JavascriptSocketFamily {
    fn from(value: JavascriptUdpFamily) -> Self {
        match value {
            JavascriptUdpFamily::Ipv4 => Self::Ipv4,
            JavascriptUdpFamily::Ipv6 => Self::Ipv6,
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
    /// VM-scoped admission/accounting view over the process-owned runtime.
    /// Child processes inherit this exact generation-bound context; they must
    /// never rediscover the process context through a global lookup.
    pub(crate) runtime_context: agentos_runtime::RuntimeContext,
    /// Immutable limits for the owning VM generation. Protocol tasks read
    /// their bounds from this snapshot instead of process-wide constants.
    pub(crate) limits: crate::limits::VmLimits,
    pub(crate) kernel_stdin_writer_fd: Option<u32>,
    /// Whether fd 0 was installed by POSIX spawn actions and must be read
    /// directly from the kernel instead of the JavaScript local-stdin bridge.
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
    pub(crate) detached: bool,
    pub(crate) execution: ActiveExecution,
    pub(crate) guest_cwd: String,
    pub(crate) env: BTreeMap<String, String>,
    pub(crate) host_cwd: PathBuf,
    /// VM-owned host root used only for kernel/runtime shadow reconciliation.
    /// This must not be inferred from a mapped host cwd or exposed through envp.
    pub(crate) shadow_root: Option<PathBuf>,
    pub(crate) host_write_dirty: bool,
    pub(crate) mapped_host_fds: BTreeMap<u32, ActiveMappedHostFd>,
    pub(crate) next_mapped_host_fd: u32,
    /// Wakes the shared process-event pump after durable local events are
    /// queued. `Notify` coalesces repeated wakes while the deque preserves all
    /// event data.
    pub(crate) process_event_notify: Arc<Notify>,
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
    /// Serializes push delivery from this child into its parent V8 session.
    /// Once an event leaves the child queue for bounded retry, later output or
    /// exit events stay on the source child until that exact relay completes.
    pub(crate) child_bridge_relay_in_flight: Arc<AtomicBool>,
    pub(crate) pending_javascript_net_connects:
        BTreeMap<u64, Arc<Mutex<PendingJavascriptNetConnectState>>>,
    pub(crate) pending_self_signal_exit: Option<i32>,
    /// Actual terminating signal observed from the runtime process (or the
    /// signal used for a shared-runtime synthetic exit). This is distinct from
    /// a requested kill signal: handlers may catch one signal and later exit
    /// for another reason.
    pub(crate) exit_signal: Option<i32>,
    pub(crate) exit_core_dumped: bool,
    /// Pending standard signals use a set, matching Linux's coalescing rule:
    /// multiple instances of the same standard signal occupy one pending bit.
    pub(crate) pending_wasm_signals: BTreeSet<i32>,
    pub(crate) pending_wasm_signals_gauge: Arc<QueueGauge>,
    pub(crate) real_interval_timer: ActiveRealIntervalTimer,
    pub(crate) child_processes: BTreeMap<String, ActiveProcess>,
    pub(crate) next_child_process_id: usize,
    /// In-flight `spawnSync`/Python subprocess calls owned by this process.
    /// Child runtime events advance these records from the shared process pump;
    /// no sidecar or Tokio worker blocks waiting for child output.
    pub(crate) pending_child_process_sync: BTreeMap<String, PendingChildProcessSync>,
    pub(crate) http_servers: BTreeMap<u64, ActiveHttpServer>,
    pub(crate) pending_http_requests: BTreeMap<(u64, u64), PendingHttpRequest>,
    pub(crate) http2: ActiveHttp2State,
    /// Capability leases are the lifecycle truth for every network handle in
    /// the legacy guest-facing maps below. Dropping a map entry without its
    /// lease is prevented by the typed insert/release helpers.
    pub(crate) capability_leases:
        BTreeMap<NativeCapabilityKey, Arc<agentos_runtime::capability::CapabilityLease>>,
    pub(crate) tcp_listeners: BTreeMap<String, ActiveTcpListener>,
    pub(crate) next_tcp_listener_id: usize,
    pub(crate) tcp_sockets: BTreeMap<String, ActiveTcpSocket>,
    pub(crate) next_tcp_socket_id: usize,
    pub(crate) tcp_port_reservations: BTreeMap<String, (JavascriptSocketFamily, u16)>,
    pub(crate) next_tcp_port_reservation_id: usize,
    pub(crate) unix_listeners: BTreeMap<String, ActiveUnixListener>,
    pub(crate) next_unix_listener_id: usize,
    pub(crate) unix_sockets: BTreeMap<String, ActiveUnixSocket>,
    pub(crate) next_unix_socket_id: usize,
    pub(crate) udp_sockets: BTreeMap<String, ActiveUdpSocket>,
    pub(crate) next_udp_socket_id: usize,
    /// Adapter handles returned to the guest Python `socket` bridge. These
    /// reference the same sidecar-owned capabilities in `tcp_sockets` and
    /// `udp_sockets`; Python does not own a parallel descriptor or I/O task.
    pub(crate) python_sockets: BTreeMap<u64, PythonHostSocket>,
    pub(crate) next_python_socket_id: u64,
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
    pub(crate) deferred_kernel_wait_rpc: Option<(JavascriptSyncRpcRequest, Option<Instant>)>,
    pub(crate) deferred_child_write_timer: Option<tokio::task::JoinHandle<()>>,
    /// Per-process module resolution cache, persisted across module sync-RPCs
    /// (`__resolve_module` / `__load_file` / `__module_format` /
    /// `__batch_resolve_modules`) for the lifetime of this process so cold-start
    /// resolution does not rebuild it on every dispatch. The resolver reads the
    /// kernel VFS; the node_modules tree is mounted read-only, so cached
    /// stat/exists/package.json results under it stay valid for the process run.
    pub(crate) module_resolution_cache: agentos_execution::LocalModuleResolutionCache,
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
}

pub(crate) enum PendingChildProcessSyncCompletion {
    Javascript(tokio::sync::oneshot::Sender<Result<Value, DeferredRpcError>>),
    Python {
        request_id: u64,
        responder: PythonVfsRpcResponder,
    },
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

pub(crate) struct ActiveMappedHostFd {
    pub(crate) file: File,
    pub(crate) path: PathBuf,
    pub(crate) guest_path: Option<String>,
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
    pub(crate) event_session: Option<V8SessionHandle>,
    pub(crate) servers: BTreeMap<u64, ActiveHttp2Server>,
    pub(crate) sessions: BTreeMap<u64, ActiveHttp2Session>,
    pub(crate) streams: BTreeMap<u64, ActiveHttp2Stream>,
    pub(crate) capability_leases:
        BTreeMap<NativeCapabilityKey, agentos_runtime::capability::CapabilityLease>,
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
    pub(crate) tls: Option<JavascriptTlsBridgeOptions>,
    pub(crate) closed: Arc<AtomicBool>,
    pub(crate) close_notify: Arc<tokio::sync::Notify>,
}

#[derive(Debug, Clone)]
pub(crate) struct ActiveHttp2Session {
    pub(crate) command_tx: TokioSender<QueuedHttp2Command>,
    pub(crate) capability_id: u64,
    pub(crate) vm_generation: u64,
    pub(crate) fairness: agentos_runtime::fairness::FairWorkBroker,
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
        pending_capability: agentos_runtime::capability::PendingCapability,
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
        pending_capability: agentos_runtime::capability::PendingCapability,
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
pub(crate) enum JavascriptTcpListenerEvent {
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
pub(crate) enum JavascriptTcpSocketEvent {
    Data {
        bytes: Vec<u8>,
        reservation: agentos_runtime::accounting::SharedReservation,
        /// Protocol-specific ownership that remains live until the payload is
        /// transferred out of the transport/event layer.
        source_reservations: Vec<agentos_runtime::accounting::SharedReservation>,
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

#[derive(Clone, Debug)]
pub(crate) struct JavascriptSocketEventPusher {
    pub(crate) session: V8SessionHandle,
    pub(crate) capability_id: agentos_runtime::capability::CapabilityId,
    pub(crate) capability_generation: agentos_runtime::capability::CapabilityGeneration,
}

#[derive(Debug)]
struct JavascriptSocketReadinessSubscriber {
    target: JavascriptSocketEventPusher,
    application_read_interest: bool,
}

/// Bounded readiness fanout shared by every alias of one open socket
/// description. Payloads stay in the description-owned transport queue; this
/// registry only coalesces level hints to each VM capability that refers to it.
#[derive(Debug)]
pub(crate) struct SocketReadinessSubscribers {
    subscribers: Mutex<BTreeMap<(u64, u64), JavascriptSocketReadinessSubscriber>>,
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
        target: JavascriptSocketEventPusher,
    ) -> Result<bool, SidecarError> {
        let identity = (target.capability_id, target.capability_generation);
        let mut subscribers = self.subscribers.lock().map_err(|_| {
            SidecarError::InvalidState(String::from(
                "ERR_AGENTOS_READY_STATE_POISONED: socket readiness subscriber lock poisoned",
            ))
        })?;
        let preserved_interest = subscribers
            .get(&identity)
            .map(|subscriber| subscriber.application_read_interest)
            .unwrap_or(false);
        if previous != Some(identity) {
            if let Some(previous) = previous {
                subscribers.remove(&previous);
            }
            if !subscribers.contains_key(&identity) && subscribers.len() >= self.maximum {
                return Err(SidecarError::Execution(format!(
                    "ERR_AGENTOS_SOCKET_READINESS_SUBSCRIBER_LIMIT: socket description readiness subscribers exceeded {}; raise limits.reactor.maxCapabilities",
                    self.maximum
                )));
            }
        }
        subscribers.insert(
            identity,
            JavascriptSocketReadinessSubscriber {
                target,
                application_read_interest: preserved_interest,
            },
        );
        Ok(subscribers
            .values()
            .any(|subscriber| subscriber.application_read_interest))
    }

    fn unregister(&self, identity: (u64, u64)) -> bool {
        self.subscribers
            .lock()
            .map(|mut subscribers| {
                subscribers.remove(&identity);
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
                SidecarError::InvalidState(String::from(
                    "ERR_AGENTOS_READY_STATE_POISONED: socket readiness subscriber lock poisoned",
                ))
            })?;
            subscribers
                .get(&identity)
                .map(|subscriber| subscriber.target.clone())
        };
        let Some(target) = target else {
            return Ok(false);
        };
        target
            .session
            .set_application_read_interest(
                target.capability_id,
                target.capability_generation,
                enabled,
            )
            .map_err(|error| SidecarError::Execution(error.to_string()))?;
        self.set_application_read_interest_state(identity, enabled)
    }

    fn set_application_read_interest_state(
        &self,
        identity: (u64, u64),
        enabled: bool,
    ) -> Result<bool, SidecarError> {
        let mut subscribers = self.subscribers.lock().map_err(|_| {
            SidecarError::InvalidState(String::from(
                "ERR_AGENTOS_READY_STATE_POISONED: socket readiness subscriber lock poisoned",
            ))
        })?;
        if let Some(subscriber) = subscribers.get_mut(&identity) {
            subscriber.application_read_interest = enabled;
        }
        Ok(subscribers
            .values()
            .any(|subscriber| subscriber.application_read_interest))
    }

    pub(crate) fn targets(&self) -> Vec<JavascriptSocketEventPusher> {
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
    identity: Mutex<Option<(u64, u64)>>,
    aggregate_interest: Option<Arc<AtomicBool>>,
    interest_notify: Option<Arc<Notify>>,
}

impl SocketReadinessRegistration {
    pub(crate) fn new(
        subscribers: Arc<SocketReadinessSubscribers>,
        aggregate_interest: Option<Arc<AtomicBool>>,
        interest_notify: Option<Arc<Notify>>,
    ) -> Self {
        Self {
            subscribers,
            identity: Mutex::new(None),
            aggregate_interest,
            interest_notify,
        }
    }

    pub(crate) fn register(
        &self,
        session: Option<V8SessionHandle>,
        identity: Option<(u64, u64)>,
        replay_flags: agentos_runtime::readiness::ReadyFlags,
    ) {
        let (Some(session), Some((capability_id, capability_generation))) = (session, identity)
        else {
            return;
        };
        let target = JavascriptSocketEventPusher {
            session,
            capability_id,
            capability_generation,
        };
        let previous = self
            .identity
            .lock()
            .map(|identity| *identity)
            .unwrap_or_else(|_| {
                eprintln!(
                    "ERR_AGENTOS_READY_STATE_POISONED: socket readiness registration lock poisoned"
                );
                None
            });
        let aggregate = match self.subscribers.register(previous, target.clone()) {
            Ok(aggregate) => aggregate,
            Err(error) => {
                eprintln!("{error}");
                return;
            }
        };
        if let Ok(mut registered) = self.identity.lock() {
            *registered = Some((capability_id, capability_generation));
        }
        self.update_aggregate_interest(aggregate);
        // Readiness is level state. Replaying one coalesced hint after
        // registration closes the race where data arrived before this alias
        // was added; the subsequent bounded poll validates the actual level.
        if let Err(error) =
            target
                .session
                .publish_readiness(capability_id, capability_generation, replay_flags)
        {
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
            let identity = self.identity.lock().map_err(|_| {
                SidecarError::InvalidState(String::from(
                    "ERR_AGENTOS_READY_STATE_POISONED: socket readiness registration lock poisoned",
                ))
            })?;
            let Some(identity) = *identity else {
                return Ok(false);
            };
            identity
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
}

impl Drop for SocketReadinessRegistration {
    fn drop(&mut self) {
        let identity = self
            .identity
            .get_mut()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if let Some(identity) = identity {
            let aggregate = self.subscribers.unregister(identity);
            self.update_aggregate_interest(aggregate);
        }
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
    pub(crate) session: Option<V8SessionHandle>,
    pub(crate) notify: Option<Arc<Notify>>,
    pub(crate) capability_id: agentos_runtime::capability::CapabilityId,
    pub(crate) capability_generation: agentos_runtime::capability::CapabilityGeneration,
    pub(crate) target_id: String,
    pub(crate) event: KernelSocketReadinessEvent,
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
            SidecarError::InvalidState(String::from(
                "ERR_AGENTOS_KERNEL_READINESS_REGISTRY_POISONED: readiness registry lock poisoned",
            ))
        })?;
        let already_registered = targets
            .get(&socket_id)
            .is_some_and(|socket_targets| socket_targets.contains_key(&identity));
        if !already_registered {
            let registered = targets.values().map(BTreeMap::len).sum::<usize>();
            if registered >= self.maximum {
                return Err(SidecarError::Execution(format!(
                    "ERR_AGENTOS_KERNEL_READINESS_TARGET_LIMIT: kernel readiness targets exceeded {}; raise limits.reactor.maxCapabilities",
                    self.maximum
                )));
            }
        }
        targets
            .entry(socket_id)
            .or_default()
            .insert(identity, target);
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

#[derive(Debug)]
pub(crate) struct ActiveTcpSocket {
    pub(crate) runtime_context: agentos_runtime::RuntimeContext,
    pub(crate) reactor_limits: ReactorIoLimits,
    pub(crate) fairness_identity: Arc<OnceLock<(u64, u64)>>,
    pub(crate) fairness_identity_committed: Arc<Notify>,
    pub(crate) fairness_retirement: Arc<SocketFairnessRetirement>,
    pub(crate) description_lease: Arc<SocketDescriptionLease>,
    pub(crate) stream: Option<Arc<Mutex<TcpStream>>>,
    pub(crate) pending_read_stream: Option<Arc<Mutex<Option<TcpStream>>>>,
    pub(crate) plain_reader_running: Arc<AtomicBool>,
    pub(crate) plain_reader_stopped: Arc<Notify>,
    pub(crate) events: Option<Arc<Mutex<AsyncCompletionReceiver<JavascriptTcpSocketEvent>>>>,
    pub(crate) event_sender: Option<AsyncCompletionSender<JavascriptTcpSocketEvent>>,
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
    pub(crate) pending_read_event: Arc<Mutex<Option<JavascriptTcpSocketEvent>>>,
    /// Bytes already read from the transport but not yet consumed by the
    /// shared open socket description. Keeping this in the sidecar (rather
    /// than per runner fd) preserves dup/SCM_RIGHTS read and MSG_PEEK
    /// semantics across processes.
    pub(crate) read_buffer: Arc<Mutex<VecDeque<u8>>>,
    /// One strong reference per guest-visible open socket description. This is
    /// separate from transport/TLS worker Arcs so SCM_RIGHTS can decide when a
    /// close is the final description close.
    pub(crate) description_handles: Arc<()>,
    pub(crate) listener_connection_retirement: Option<Arc<ListenerConnectionRetirement>>,
    /// Kernel open-description guard used after this socket first crosses
    /// SCM_RIGHTS. It keeps owner-0 kernel sockets alive while queued or held
    /// by another process and lets the kernel prune discarded transfers.
    pub(crate) kernel_transfer_guard: Option<TransferredFd>,
    pub(crate) resources: Arc<agentos_runtime::accounting::ResourceLedger>,
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
        _command_reservation: agentos_runtime::accounting::SharedReservation,
        completion: SyncSender<Result<Value, DeferredRpcError>>,
    },
    Close {
        _command_reservation: agentos_runtime::accounting::SharedReservation,
    },
}

#[derive(Debug)]
pub(crate) enum NativePlainSocketCommand {
    Write {
        payload: PlainSocketWritePayload,
        completion: tokio::sync::oneshot::Sender<Result<Value, DeferredRpcError>>,
    },
    Shutdown {
        _command_reservation: agentos_runtime::accounting::SharedReservation,
        completion: tokio::sync::oneshot::Sender<Result<Value, DeferredRpcError>>,
    },
}

#[derive(Debug)]
pub(crate) struct PlainSocketWritePayload {
    pub(crate) bytes: Vec<u8>,
    pub(crate) _command_reservation: agentos_runtime::accounting::SharedReservation,
    pub(crate) _bytes_reservation: agentos_runtime::accounting::SharedReservation,
    pub(crate) _buffered_reservation: agentos_runtime::accounting::SharedReservation,
}

#[derive(Debug)]
pub(crate) struct TlsWritePayload {
    pub(crate) bytes: Vec<u8>,
    pub(crate) _command_reservation: agentos_runtime::accounting::SharedReservation,
    pub(crate) _command_bytes_reservation: agentos_runtime::accounting::SharedReservation,
    pub(crate) _buffered_reservation: agentos_runtime::accounting::SharedReservation,
    pub(crate) _tls_reservation: agentos_runtime::accounting::SharedReservation,
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
    pub(crate) resources: Arc<agentos_runtime::accounting::ResourceLedger>,
}

#[derive(Debug, Default)]
pub(crate) struct LoopbackTlsTransportPairState {
    pub(crate) lower_to_higher: VecDeque<u8>,
    pub(crate) higher_to_lower: VecDeque<u8>,
    pub(crate) lower_to_higher_reservations: VecDeque<agentos_runtime::accounting::Reservation>,
    pub(crate) higher_to_lower_reservations: VecDeque<agentos_runtime::accounting::Reservation>,
    pub(crate) lower_to_higher_tls_reservations: VecDeque<agentos_runtime::accounting::Reservation>,
    pub(crate) higher_to_lower_tls_reservations: VecDeque<agentos_runtime::accounting::Reservation>,
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
pub(crate) struct JavascriptTlsClientHello {
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
pub(crate) struct JavascriptTlsBridgeOptions {
    pub(crate) is_server: bool,
    pub(crate) host: Option<String>,
    pub(crate) servername: Option<String>,
    pub(crate) reject_unauthorized: Option<bool>,
    pub(crate) request_cert: Option<bool>,
    pub(crate) session: Option<String>,
    pub(crate) key: Option<JavascriptTlsMaterial>,
    pub(crate) cert: Option<JavascriptTlsMaterial>,
    pub(crate) ca: Option<JavascriptTlsMaterial>,
    pub(crate) passphrase: Option<String>,
    pub(crate) ciphers: Option<String>,
    #[serde(alias = "ALPNProtocols")]
    pub(crate) alpn_protocols: Option<Vec<String>>,
    pub(crate) min_version: Option<String>,
    pub(crate) max_version: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub(crate) enum JavascriptTlsMaterial {
    Single(JavascriptTlsDataValue),
    Many(Vec<JavascriptTlsDataValue>),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub(crate) enum JavascriptTlsDataValue {
    Buffer { data: String },
    String { data: String },
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ActiveTlsState {
    pub(crate) client_hello: Option<JavascriptTlsClientHello>,
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
}

// ---------------------------------------------------------------------------
// Unix socket types
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub(crate) enum JavascriptUnixListenerEvent {
    Connection {
        socket: PendingUnixSocket,
        capability: agentos_runtime::capability::PendingCapability,
    },
    Error {
        code: Option<String>,
        message: String,
    },
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
    pub(crate) events: Arc<Mutex<AsyncCompletionReceiver<JavascriptTcpSocketEvent>>>,
    pub(crate) event_sender: AsyncCompletionSender<JavascriptTcpSocketEvent>,
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
    pub(crate) pending_read_event: Arc<Mutex<Option<JavascriptTcpSocketEvent>>>,
    /// Bytes already drained from the async completion lane but not yet
    /// consumed by the shared Unix open description. Duplicated and
    /// SCM_RIGHTS-transferred descriptors retain this same buffer so partial
    /// reads and `MSG_PEEK` observe one Linux-style read cursor.
    pub(crate) read_buffer: Arc<Mutex<VecDeque<u8>>>,
    pub(crate) description_handles: Arc<()>,
    pub(crate) listener_connection_retirement: Option<Arc<ListenerConnectionRetirement>>,
    pub(crate) resources: Arc<agentos_runtime::accounting::ResourceLedger>,
}

#[derive(Debug)]
pub(crate) struct ActiveUnixListener {
    pub(crate) listener: Option<UnixListener>,
    pub(crate) bound_socket: Option<Socket>,
    pub(crate) events: Arc<Mutex<AsyncCompletionReceiver<JavascriptUnixListenerEvent>>>,
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
}

// ---------------------------------------------------------------------------
// UDP types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JavascriptUdpFamily {
    Ipv4,
    Ipv6,
}

impl JavascriptUdpFamily {
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

#[derive(Debug)]
pub(crate) enum JavascriptUdpSocketEvent {
    Message {
        data: Vec<u8>,
        remote_addr: SocketAddr,
        _byte_reservation: agentos_runtime::accounting::SharedReservation,
        _datagram_reservation: agentos_runtime::accounting::SharedReservation,
        _udp_byte_reservation: agentos_runtime::accounting::SharedReservation,
        _udp_datagram_reservation: agentos_runtime::accounting::SharedReservation,
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
        completion: SyncSender<Result<Option<JavascriptUdpSocketEvent>, DeferredRpcError>>,
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
pub(crate) enum PythonHostSocket {
    Tcp {
        socket_id: String,
        pending_read: Option<PythonTcpReadBuffer>,
    },
    Udp {
        socket_id: String,
    },
}

#[derive(Debug)]
pub(crate) struct PythonTcpReadBuffer {
    pub(crate) data: Vec<u8>,
    pub(crate) offset: usize,
    pub(crate) _reservation: agentos_runtime::accounting::SharedReservation,
    pub(crate) _source_reservations: Vec<agentos_runtime::accounting::SharedReservation>,
}

#[derive(Debug)]
pub(crate) struct ActiveUdpSocket {
    pub(crate) family: JavascriptUdpFamily,
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
    pub(crate) resources: Arc<agentos_runtime::accounting::ResourceLedger>,
    pub(crate) runtime_context: agentos_runtime::RuntimeContext,
    pub(crate) reactor_limits: ReactorIoLimits,
    pub(crate) fairness_identity: Arc<OnceLock<(u64, u64)>>,
    pub(crate) fairness_identity_committed: Arc<Notify>,
    pub(crate) fairness_retirement: Arc<SocketFairnessRetirement>,
    pub(crate) description_lease: Arc<SocketDescriptionLease>,
    pub(crate) read_event_notify: Arc<Notify>,
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
    pub(crate) pending_events: Arc<Mutex<VecDeque<ActiveExecutionEvent>>>,
    pub(crate) event_overflow_reason: Arc<Mutex<Option<String>>>,
    pub(crate) pending_event_bytes: Arc<AtomicUsize>,
    pub(crate) pending_event_count_limit: Arc<AtomicUsize>,
    pub(crate) pending_event_bytes_limit: Arc<AtomicUsize>,
    pub(crate) vm_pending_event_bytes_budget: Arc<VmPendingByteBudget>,
    pub(crate) event_notify: Arc<Notify>,
}

impl Default for BindingExecution {
    fn default() -> Self {
        Self::with_event_notify(
            Arc::new(Notify::new()),
            agentos_runtime::DEFAULT_PROTOCOL_MAX_PROCESS_EVENTS,
        )
    }
}

impl BindingExecution {
    pub(crate) fn with_event_notify(event_notify: Arc<Notify>, event_capacity: usize) -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
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
        }
    }
}

#[derive(Debug)]
pub(crate) enum ActiveExecutionEvent {
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
    JavascriptSyncRpcRequest(JavascriptSyncRpcRequest),
    JavascriptSyncRpcCompletion(JavascriptSyncRpcCompletion),
    PythonVfsRpcRequest(Box<PythonVfsRpcRequest>),
    PythonSocketConnectCompletion(Box<PythonSocketConnectCompletion>),
    SignalState {
        signal: u32,
        registration: SignalHandlerRegistration,
    },
    Exited(i32),
}

#[derive(Debug)]
pub(crate) struct JavascriptSyncRpcCompletion {
    pub(crate) request_id: u64,
    pub(crate) result: Result<Value, DeferredRpcError>,
}

#[derive(Debug)]
pub(crate) struct PythonSocketConnectCompletion {
    pub(crate) request_id: u64,
    pub(crate) result: Result<PendingPythonTcpConnect, DeferredRpcError>,
}

#[derive(Debug)]
pub(crate) struct PendingPythonTcpConnect {
    pub(crate) native_socket_id: String,
    pub(crate) python_socket_id: u64,
    pub(crate) socket: ActiveTcpSocket,
    pub(crate) pending_capability: agentos_runtime::capability::PendingCapability,
}

#[derive(Debug)]
pub(crate) enum PendingJavascriptNetConnect {
    Tcp {
        socket_id: String,
        socket: Box<ActiveTcpSocket>,
        pending_capability: agentos_runtime::capability::PendingCapability,
        local_reservation_id: Option<String>,
    },
    Unix {
        socket_id: String,
        socket: Box<ActiveUnixSocket>,
        pending_capability: agentos_runtime::capability::PendingCapability,
        remote_path: String,
        remote_abstract_path_hex: Option<String>,
    },
}

#[derive(Debug, Default)]
pub(crate) struct PendingJavascriptNetConnectState {
    pub(crate) connected: Option<PendingJavascriptNetConnect>,
    /// A bound-but-unlistened Unix socket is removed from the process table
    /// while its nonblocking connect is in flight. Keep the original handle
    /// here so a failed connect can restore the guest descriptor unchanged.
    pub(crate) bound_unix_listener: Option<(String, ActiveUnixListener)>,
}

#[derive(Debug)]
pub(crate) struct DeferredRpcError {
    pub(crate) code: String,
    pub(crate) message: String,
}

#[derive(Debug)]
pub(crate) struct ProcessEventEnvelope {
    pub(crate) connection_id: String,
    pub(crate) session_id: String,
    pub(crate) vm_id: String,
    /// Exact descendant locator relative to `process_id`. Empty targets the root.
    pub(crate) child_path: Vec<String>,
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
    use agentos_runtime::accounting::ResourceLimit;

    fn completion_runtime(
        maximum: usize,
        generation: u64,
    ) -> (RuntimeContext, Arc<ResourceLedger>) {
        let process =
            agentos_runtime::SidecarRuntime::process(&agentos_runtime::RuntimeConfig::default())
                .expect("test process runtime")
                .context();
        let resources = Arc::new(ResourceLedger::child(
            format!("completion-test-vm-{generation}"),
            [(
                ResourceClass::AsyncCompletions,
                ResourceLimit::new(maximum, "limits.reactor.maxAsyncCompletions"),
            )],
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
        let (first_tx, mut first_rx) = async_completion_channel(runtime.clone(), 2);
        let (second_tx, second_rx) = async_completion_channel(runtime.clone(), 2);

        first_tx.try_send("first").expect("first lane admission");
        second_tx.try_send("second").expect("second lane admission");
        assert_eq!(resources.usage(ResourceClass::AsyncCompletions).used, 2);

        let error = first_tx
            .try_send("aggregate overflow")
            .expect_err("per-VM completion limit must span both lanes");
        assert!(error.contains("limits.reactor.maxAsyncCompletions"));

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

        let (disconnected_tx, disconnected_rx) = async_completion_channel(runtime, 1);
        drop(disconnected_rx);
        let error = disconnected_tx
            .try_send("disconnected")
            .expect_err("disconnected lane rejects insertion");
        assert!(error.contains("ERR_AGENTOS_ASYNC_COMPLETION_DISCONNECTED"));
        assert_eq!(resources.usage(ResourceClass::AsyncCompletions).used, 0);
    }

    #[test]
    fn failed_completion_send_and_vm_close_release_reservations() {
        let (runtime, resources) = completion_runtime(1, 92);
        let (held_tx, _held_rx) = async_completion_channel(runtime.clone(), 1);
        let (waiting_tx, waiting_rx) = async_completion_channel(runtime.clone(), 1);
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
            assert!(error.contains("ERR_AGENTOS_ASYNC_COMPLETION_CLOSED"));
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
}

#[cfg(test)]
mod socket_readiness_registry_tests {
    use super::*;
    use agentos_execution::v8_host::V8RuntimeHost;
    use agentos_runtime::accounting::ResourceLimit;

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
        }
    }

    fn javascript_target(
        session: &V8SessionHandle,
        capability_id: u64,
    ) -> JavascriptSocketEventPusher {
        JavascriptSocketEventPusher {
            session: session.clone(),
            capability_id,
            capability_generation: 1,
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
    fn native_alias_registration_is_raii_and_read_interest_is_aggregate_or() {
        let process_runtime =
            agentos_runtime::SidecarRuntime::process(&agentos_runtime::RuntimeConfig::default())
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
        let session = host.session_handle(String::from("socket-subscriber-test"));
        let subscribers = SocketReadinessSubscribers::new(&resources);
        let aggregate_interest = Arc::new(AtomicBool::new(false));
        let interest_notify = Arc::new(Notify::new());

        let mut parent = SocketReadinessRegistration::new(
            Arc::clone(&subscribers),
            Some(Arc::clone(&aggregate_interest)),
            Some(Arc::clone(&interest_notify)),
        );
        subscribers
            .register(None, javascript_target(&session, 1))
            .expect("register parent alias");
        *parent.identity.get_mut().expect("parent identity") = Some((1, 1));

        let mut child = SocketReadinessRegistration::new(
            Arc::clone(&subscribers),
            Some(Arc::clone(&aggregate_interest)),
            Some(Arc::clone(&interest_notify)),
        );
        subscribers
            .register(None, javascript_target(&session, 2))
            .expect("register child alias");
        *child.identity.get_mut().expect("child identity") = Some((2, 1));
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
