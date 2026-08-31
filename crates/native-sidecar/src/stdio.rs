use crate::extension::{ExtensionRequestClass, ExtensionServices};
use crate::extension_services::{
    prepare_extension_service_command, prepare_owned_process_event_service,
    CompletedExtensionServiceCommand, ExtensionServiceCommand, OwnedProcessEventService,
    PreparedExtensionServiceCommand, RoutedExtensionServices, VmEventAdmissionResult,
};
use crate::ownership_coordinator::{
    CoordinatorOperationPermit, OwnershipCoordinator, OwnershipCoordinatorError, VmDisposal,
};
use crate::request_operations::{
    ForcedRequestOutcome, OperationCancellationReason, OperationTable, ProgressOperationView,
    ProgressRequest, ProgressRequestAdmissionError, RequestAdmissionError, RequestOperation,
    RequestOperationKey, RequestOperationMetadata, VmConcurrencyClass,
};
use crate::service::CompletedExtensionRequest;
use crate::service::{CompletedRequest, PreparedMembershipCommit, PreparedRequest};
use crate::vm::{
    CompletedCreateVm, CompletedDisposeVm, DisposeVmPlan, PreparedCreateVm, PreparedDisposeVm,
};
use crate::wire::{
    self, AuthenticatedResponse, OwnershipScope, ProtocolCodecError, ProtocolFrame, RequestFrame,
    RequestId, RequestPayload, ResponseFrame, ResponsePayload, SessionOpenedResponse,
    SidecarResponseFrame, WireDispatchResult, WireFrameCodec,
};
use crate::{
    EventSinkTransport, Extension, NativeSidecar, NativeSidecarConfig, SidecarError,
    SidecarRequestTransport,
};
use agentos_bridge::queue_tracker::TrackedLimit;
use agentos_bridge::{
    BridgeTypes, ChmodRequest, ClockBridge, ClockRequest, CommandPermissionRequest,
    CreateDirRequest, CreateJavascriptContextRequest, CreateWasmContextRequest, DiagnosticRecord,
    DirectoryEntry, EnvironmentPermissionRequest, EventBridge, ExecutionBridge, ExecutionEvent,
    ExecutionHandleRequest, FileMetadata, FilesystemBridge, FilesystemPermissionRequest,
    FilesystemSnapshot, FlushFilesystemStateRequest, GuestContextHandle, KillExecutionRequest,
    LifecycleEventRecord, LoadFilesystemStateRequest, LogRecord, NetworkPermissionRequest,
    PathRequest, PermissionBridge, PermissionDecision, PersistenceBridge,
    PollExecutionEventRequest, RandomBridge, RandomBytesRequest, ReadDirRequest, ReadFileRequest,
    RenameRequest, ScheduleTimerRequest, ScheduledTimer, StartExecutionRequest, StartedExecution,
    StructuredEventRecord, SymlinkRequest, TruncateRequest, WriteExecutionStdinRequest,
    WriteFileRequest,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::error::Error;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::fs::{symlink as create_symlink, MetadataExt, PermissionsExt};
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc::{channel, Receiver, Sender};
use tokio::sync::Notify;
use tokio::task::JoinSet;

// Cadence of sidecar→host heartbeat frames. The host treats sustained inbound
// silence (several missed beats) as a dead or wedged sidecar and tears the
// process down, so this is a fixed protocol constant, not a tunable. Emitted
// from a dedicated thread so beats keep flowing while the dispatch loop is
// busy inside one long request.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
// Connection id stamped on heartbeat frames. Heartbeats are transport-level
// liveness — not tied to an authenticated connection — and the host consumes
// them at its frame layer without routing by ownership, so a fixed synthetic
// id is correct even before any client authenticates.
const HEARTBEAT_CONNECTION_ID: &str = "sidecar-transport";
const MAX_EVENT_READY_QUEUE: usize = 1;
const MAX_SHUTDOWN_QUEUE: usize = 1;
const MAX_TRANSPORT_ERROR_QUEUE: usize = 4;
const MAX_LIMIT_WARNING_QUEUE: usize = 128;
const STDIO_INGRESS_LIMIT_ERROR_CODE: &str = "ERR_AGENTOS_STDIO_INGRESS_LIMIT";
const STDIO_CONTROL_LIMIT_ERROR_CODE: &str = "ERR_AGENTOS_STDIO_CONTROL_LIMIT";
const PENDING_RESPONSE_COUNT_ERROR_CODE: &str = "ERR_AGENTOS_STDIO_PENDING_RESPONSE_COUNT_LIMIT";
const PENDING_RESPONSE_BYTES_ERROR_CODE: &str = "ERR_AGENTOS_STDIO_PENDING_RESPONSE_BYTE_LIMIT";
const PENDING_RESPONSE_COUNT_CONFIG_PATH: &str = "runtime.protocol.maxPendingResponses";
const PENDING_RESPONSE_BYTES_CONFIG_PATH: &str = "runtime.protocol.maxPendingResponseBytes";

#[derive(Clone, Copy, Debug)]
struct ProtocolBudgetConfig {
    max_frames: usize,
    max_bytes: usize,
    frame_path: &'static str,
    byte_path: &'static str,
    label: &'static str,
    metric: agentos_runtime::metrics::ChannelMetricClass,
}

#[derive(Debug, Default)]
struct ProtocolBudgetState {
    frames: usize,
    bytes: usize,
    warned: bool,
}

#[derive(Clone, Debug)]
struct ProtocolBudget {
    config: ProtocolBudgetConfig,
    state: Arc<Mutex<ProtocolBudgetState>>,
    changed: Arc<Notify>,
    metrics: agentos_runtime::metrics::RuntimeMetrics,
}

#[derive(Clone, Debug)]
struct ProtocolLimitError {
    code: &'static str,
    path: &'static str,
    label: &'static str,
    used: usize,
    requested: usize,
    limit: usize,
    unit: &'static str,
}

impl fmt::Display for ProtocolLimitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}: {} used={} requested={} limit={} {}; raise {}",
            self.code, self.label, self.used, self.requested, self.limit, self.unit, self.path
        )
    }
}

#[derive(Debug)]
struct ProtocolReservation {
    budget: ProtocolBudget,
    frames: usize,
    bytes: usize,
}

struct DetachedExtensionCompletion {
    request: RequestFrame,
    class: ExtensionRequestClass,
    operation: Option<RequestOperation>,
    progress_request: Option<ProgressRequest>,
    coordinator_permit: Option<CoordinatorOperationPermit>,
    output_reservation: ProtocolReservation,
    result: Result<CompletedExtensionRequest, SidecarError>,
}

struct DetachedRequestCompletion {
    request: RequestFrame,
    operation: RequestOperation,
    coordinator_permit: Option<CoordinatorOperationPermit>,
    output_reservation: ProtocolReservation,
    result: DetachedRequestResult,
}

enum DetachedRequestResult {
    Generic(Result<CompletedRequest, SidecarError>),
    Create(Result<CompletedCreateVm<LocalBridge>, SidecarError>),
    DisposeDrained {
        plan: DisposeVmPlan<LocalBridge>,
        disposal: VmDisposal,
    },
    DisposeExecuted {
        result: Result<CompletedDisposeVm, SidecarError>,
        disposal: VmDisposal,
    },
}

#[derive(Debug)]
struct ProtocolDrainState {
    reason: OperationCancellationReason,
    deadline: tokio::time::Instant,
    terminal_error: Option<String>,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct ProtocolFinalizeReport {
    forced_terminal_responses: usize,
    forced_progress_acknowledgements: usize,
    failed_deliveries: usize,
    control_drained: bool,
}

struct HeartbeatThread {
    stop: mpsc::SyncSender<()>,
    join: thread::JoinHandle<()>,
}

impl HeartbeatThread {
    fn stop(self) {
        match self.stop.try_send(()) {
            Ok(()) | Err(mpsc::TrySendError::Disconnected(())) => {}
            Err(mpsc::TrySendError::Full(())) => {}
        }
        if self.join.join().is_err() {
            eprintln!("ERR_AGENTOS_HEARTBEAT_THREAD: heartbeat thread panicked during shutdown");
        }
    }
}

impl ProtocolReservation {
    fn try_grow_bytes(&mut self, retained_bytes: usize) -> Result<(), ProtocolLimitError> {
        if retained_bytes <= self.bytes {
            return Ok(());
        }
        let requested = retained_bytes - self.bytes;
        let mut state = self.budget.state.lock().unwrap_or_else(|poisoned| {
            eprintln!(
                "ERR_AGENTOS_PROTOCOL_BUDGET_POISONED: recovering {} budget during growth",
                self.budget.config.label
            );
            poisoned.into_inner()
        });
        let next_bytes = state
            .bytes
            .checked_add(requested)
            .ok_or(ProtocolLimitError {
                code: "ERR_AGENTOS_PROTOCOL_BYTE_LIMIT",
                path: self.budget.config.byte_path,
                label: self.budget.config.label,
                used: state.bytes,
                requested,
                limit: self.budget.config.max_bytes,
                unit: "bytes",
            })?;
        if next_bytes > self.budget.config.max_bytes {
            return Err(ProtocolLimitError {
                code: "ERR_AGENTOS_PROTOCOL_BYTE_LIMIT",
                path: self.budget.config.byte_path,
                label: self.budget.config.label,
                used: state.bytes,
                requested,
                limit: self.budget.config.max_bytes,
                unit: "bytes",
            });
        }
        state.bytes = next_bytes;
        self.bytes = retained_bytes;
        self.budget
            .metrics
            .observe_channel(self.budget.config.metric, state.frames, state.bytes);
        drop(state);
        Ok(())
    }

    fn shrink_bytes(&mut self, retained_bytes: usize) {
        assert!(
            retained_bytes <= self.bytes,
            "protocol reservation can only shrink"
        );
        let released = self.bytes - retained_bytes;
        if released == 0 {
            return;
        }
        let mut state = self.budget.state.lock().unwrap_or_else(|poisoned| {
            eprintln!(
                "ERR_AGENTOS_PROTOCOL_BUDGET_POISONED: recovering {} budget during resize",
                self.budget.config.label
            );
            poisoned.into_inner()
        });
        if state.bytes < released {
            eprintln!(
                "ERR_AGENTOS_PROTOCOL_ACCOUNTING_UNDERFLOW: {} resize bytes={}/{}",
                self.budget.config.label, state.bytes, released,
            );
            state.bytes = 0;
        } else {
            state.bytes -= released;
        }
        self.bytes = retained_bytes;
        drop(state);
        self.budget.changed.notify_waiters();
    }
}

impl ProtocolBudget {
    fn new(
        config: ProtocolBudgetConfig,
        metrics: agentos_runtime::metrics::RuntimeMetrics,
    ) -> Self {
        Self {
            config,
            state: Arc::new(Mutex::new(ProtocolBudgetState::default())),
            changed: Arc::new(Notify::new()),
            metrics,
        }
    }

    fn reserve(&self, bytes: usize) -> Result<ProtocolReservation, ProtocolLimitError> {
        let mut state = self.state.lock().unwrap_or_else(|poisoned| {
            eprintln!(
                "ERR_AGENTOS_PROTOCOL_BUDGET_POISONED: recovering {} budget",
                self.config.label
            );
            poisoned.into_inner()
        });
        let next_frames = state.frames.checked_add(1).ok_or(ProtocolLimitError {
            code: "ERR_AGENTOS_PROTOCOL_FRAME_LIMIT",
            path: self.config.frame_path,
            label: self.config.label,
            used: state.frames,
            requested: 1,
            limit: self.config.max_frames,
            unit: "frames",
        })?;
        if next_frames > self.config.max_frames {
            return Err(ProtocolLimitError {
                code: "ERR_AGENTOS_PROTOCOL_FRAME_LIMIT",
                path: self.config.frame_path,
                label: self.config.label,
                used: state.frames,
                requested: 1,
                limit: self.config.max_frames,
                unit: "frames",
            });
        }
        let next_bytes = state.bytes.checked_add(bytes).ok_or(ProtocolLimitError {
            code: "ERR_AGENTOS_PROTOCOL_BYTE_LIMIT",
            path: self.config.byte_path,
            label: self.config.label,
            used: state.bytes,
            requested: bytes,
            limit: self.config.max_bytes,
            unit: "bytes",
        })?;
        if next_bytes > self.config.max_bytes {
            return Err(ProtocolLimitError {
                code: "ERR_AGENTOS_PROTOCOL_BYTE_LIMIT",
                path: self.config.byte_path,
                label: self.config.label,
                used: state.bytes,
                requested: bytes,
                limit: self.config.max_bytes,
                unit: "bytes",
            });
        }
        state.frames = next_frames;
        state.bytes = next_bytes;
        self.metrics
            .observe_channel(self.config.metric, state.frames, state.bytes);
        let fill = state
            .frames
            .saturating_mul(100)
            .checked_div(self.config.max_frames)
            .unwrap_or(0)
            .max(
                state
                    .bytes
                    .saturating_mul(100)
                    .checked_div(self.config.max_bytes)
                    .unwrap_or(0),
            );
        if fill >= 80 && !state.warned {
            state.warned = true;
            eprintln!(
                "ERR_AGENTOS_PROTOCOL_NEAR_LIMIT: {} frames={}/{} bytes={}/{}; raise {} or {}",
                self.config.label,
                state.frames,
                self.config.max_frames,
                state.bytes,
                self.config.max_bytes,
                self.config.frame_path,
                self.config.byte_path,
            );
        }
        drop(state);
        Ok(ProtocolReservation {
            budget: self.clone(),
            frames: 1,
            bytes,
        })
    }

    fn usage(&self) -> (usize, usize) {
        self.state
            .lock()
            .map(|state| (state.frames, state.bytes))
            .unwrap_or_else(|poisoned| {
                eprintln!(
                    "ERR_AGENTOS_PROTOCOL_BUDGET_POISONED: recovering {} budget while reading usage",
                    self.config.label
                );
                let state = poisoned.into_inner();
                (state.frames, state.bytes)
            })
    }
}

impl Drop for ProtocolReservation {
    fn drop(&mut self) {
        let mut state = self.budget.state.lock().unwrap_or_else(|poisoned| {
            eprintln!(
                "ERR_AGENTOS_PROTOCOL_BUDGET_POISONED: recovering {} budget during release",
                self.budget.config.label
            );
            poisoned.into_inner()
        });
        if state.frames < self.frames || state.bytes < self.bytes {
            eprintln!(
                "ERR_AGENTOS_PROTOCOL_ACCOUNTING_UNDERFLOW: {} frames={}/{} bytes={}/{}",
                self.budget.config.label, state.frames, self.frames, state.bytes, self.bytes,
            );
            state.frames = state.frames.saturating_sub(self.frames);
            state.bytes = state.bytes.saturating_sub(self.bytes);
        } else {
            state.frames -= self.frames;
            state.bytes -= self.bytes;
        }
        let fill = state
            .frames
            .saturating_mul(100)
            .checked_div(self.budget.config.max_frames)
            .unwrap_or(0)
            .max(
                state
                    .bytes
                    .saturating_mul(100)
                    .checked_div(self.budget.config.max_bytes)
                    .unwrap_or(0),
            );
        if fill < 50 {
            state.warned = false;
        }
        drop(state);
        self.budget.changed.notify_waiters();
    }
}

#[derive(Debug)]
struct AccountedProtocolFrame {
    frame: ProtocolFrame,
    _reservation: ProtocolReservation,
}

#[derive(Debug)]
struct DecodedProtocolFrame {
    frame: ProtocolFrame,
    encoded_bytes: usize,
}

#[derive(Debug)]
struct EncodedProtocolFrame {
    bytes: Vec<u8>,
    _reservation: ProtocolReservation,
}

#[derive(Debug)]
struct ProtocolOutputQueueState {
    ordinary: VecDeque<EncodedProtocolFrame>,
    progress: VecDeque<EncodedProtocolFrame>,
    rejection: VecDeque<EncodedProtocolFrame>,
    terminal: VecDeque<EncodedProtocolFrame>,
    observability: VecDeque<EncodedProtocolFrame>,
    next_control_class: u8,
    open: bool,
    terminal_error: Option<String>,
}

impl ProtocolOutputQueueState {
    fn control_len(&self) -> usize {
        self.progress
            .len()
            .saturating_add(self.rejection.len())
            .saturating_add(self.terminal.len())
            .saturating_add(self.observability.len())
    }

    fn pop_control(&mut self) -> Option<EncodedProtocolFrame> {
        for offset in 0..4 {
            let class = (self.next_control_class + offset) % 4;
            let frame = match class {
                0 => self.progress.pop_front(),
                1 => self.rejection.pop_front(),
                2 => self.terminal.pop_front(),
                3 => self.observability.pop_front(),
                _ => unreachable!("control class is reduced modulo four"),
            };
            if frame.is_some() {
                self.next_control_class = (class + 1) % 4;
                return frame;
            }
        }
        None
    }
}

#[derive(Debug)]
struct ProtocolOutputQueue {
    ordinary_capacity: usize,
    control_capacity: usize,
    state: Mutex<ProtocolOutputQueueState>,
    available: Condvar,
    control_available: Notify,
    closed: Notify,
}

impl ProtocolOutputQueue {
    fn new(ordinary_capacity: usize, control_capacity: usize) -> Self {
        Self {
            ordinary_capacity,
            control_capacity,
            state: Mutex::new(ProtocolOutputQueueState {
                ordinary: VecDeque::new(),
                progress: VecDeque::new(),
                rejection: VecDeque::new(),
                terminal: VecDeque::new(),
                observability: VecDeque::new(),
                next_control_class: 0,
                open: true,
                terminal_error: None,
            }),
            available: Condvar::new(),
            control_available: Notify::new(),
            closed: Notify::new(),
        }
    }

    fn enqueue(
        &self,
        class: ProtocolOutputClass,
        control: bool,
        frame: EncodedProtocolFrame,
    ) -> Result<(), ProtocolTrySendError> {
        self.enqueue_after_admission(class, control, frame, || Ok(()))
    }

    fn enqueue_retained<F>(
        &self,
        class: ProtocolOutputClass,
        control: bool,
        frame: EncodedProtocolFrame,
        retain: F,
        taken_over_error: &'static str,
    ) -> Result<(), ProtocolTrySendError>
    where
        F: FnOnce() -> bool,
    {
        self.enqueue_after_admission(class, control, frame, || {
            if retain() {
                Ok(())
            } else {
                Err(ProtocolTrySendError::Rejected(io::Error::other(
                    taken_over_error,
                )))
            }
        })
    }

    fn enqueue_after_admission<F>(
        &self,
        class: ProtocolOutputClass,
        control: bool,
        frame: EncodedProtocolFrame,
        retain: F,
    ) -> Result<(), ProtocolTrySendError>
    where
        F: FnOnce() -> Result<(), ProtocolTrySendError>,
    {
        let mut state = self.state.lock().unwrap_or_else(|poisoned| {
            eprintln!("ERR_AGENTOS_PROTOCOL_OUTPUT_QUEUE_POISONED: recovering output queue");
            poisoned.into_inner()
        });
        if !state.open {
            return Err(ProtocolTrySendError::Disconnected(
                state.terminal_error.clone().unwrap_or_else(|| {
                    String::from("ERR_AGENTOS_PROTOCOL_OUTPUT_CLOSED: output broker closed")
                }),
            ));
        }
        if !matches!(
            (control, class),
            (
                false,
                ProtocolOutputClass::Ordinary | ProtocolOutputClass::Observability
            ) | (
                true,
                ProtocolOutputClass::Progress
                    | ProtocolOutputClass::Rejection
                    | ProtocolOutputClass::Terminal
                    | ProtocolOutputClass::Observability
            )
        ) {
            return Err(ProtocolTrySendError::Rejected(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "ERR_AGENTOS_PROTOCOL_OUTPUT_CLASS: {class:?} is invalid for {} lane",
                    if control { "control" } else { "ordinary" }
                ),
            )));
        }
        let lane_len = if control {
            state.control_len()
        } else {
            state.ordinary.len()
        };
        let capacity = if control {
            self.control_capacity
        } else {
            self.ordinary_capacity
        };
        if lane_len >= capacity {
            return Err(ProtocolTrySendError::Rejected(io::Error::new(
                io::ErrorKind::Other,
                "ERR_AGENTOS_PROTOCOL_OUTPUT_ACCOUNTING: physical output queue filled despite logical reservation",
            )));
        }
        // This callback is the publication linearization point. Shutdown may
        // take over a claimed-but-unretained response at any time, so retain it
        // only after every enqueue failure has been ruled out and while the
        // queue lock prevents the frame from becoming visible first.
        retain()?;
        match (control, class) {
            (false, ProtocolOutputClass::Ordinary | ProtocolOutputClass::Observability) => {
                state.ordinary.push_back(frame);
            }
            (true, ProtocolOutputClass::Progress) => state.progress.push_back(frame),
            (true, ProtocolOutputClass::Rejection) => state.rejection.push_back(frame),
            (true, ProtocolOutputClass::Terminal) => state.terminal.push_back(frame),
            (true, ProtocolOutputClass::Observability) => state.observability.push_back(frame),
            _ => unreachable!("output lane was validated before publication retention"),
        }
        drop(state);
        self.available.notify_one();
        if control {
            self.control_available.notify_one();
        }
        Ok(())
    }

    fn recv_ordinary(&self) -> Option<EncodedProtocolFrame> {
        let mut state = self.state.lock().unwrap_or_else(|poisoned| {
            eprintln!("ERR_AGENTOS_PROTOCOL_OUTPUT_QUEUE_POISONED: recovering output queue");
            poisoned.into_inner()
        });
        loop {
            if let Some(frame) = state.ordinary.pop_front() {
                return Some(frame);
            }
            if !state.open {
                return None;
            }
            state = self.available.wait(state).unwrap_or_else(|poisoned| {
                eprintln!(
                    "ERR_AGENTOS_PROTOCOL_OUTPUT_QUEUE_POISONED: recovering output queue after wait"
                );
                poisoned.into_inner()
            });
        }
    }

    fn recv_combined(&self) -> Option<EncodedProtocolFrame> {
        let mut state = self.state.lock().unwrap_or_else(|poisoned| {
            eprintln!("ERR_AGENTOS_PROTOCOL_OUTPUT_QUEUE_POISONED: recovering output queue");
            poisoned.into_inner()
        });
        loop {
            if let Some(frame) = state.pop_control().or_else(|| state.ordinary.pop_front()) {
                return Some(frame);
            }
            if !state.open {
                return None;
            }
            state = self.available.wait(state).unwrap_or_else(|poisoned| {
                eprintln!(
                    "ERR_AGENTOS_PROTOCOL_OUTPUT_QUEUE_POISONED: recovering output queue after wait"
                );
                poisoned.into_inner()
            });
        }
    }

    async fn recv_control(&self) -> Option<EncodedProtocolFrame> {
        loop {
            let notified = self.control_available.notified();
            {
                let mut state = self.state.lock().unwrap_or_else(|poisoned| {
                    eprintln!(
                        "ERR_AGENTOS_PROTOCOL_OUTPUT_QUEUE_POISONED: recovering control output queue"
                    );
                    poisoned.into_inner()
                });
                if let Some(frame) = state.pop_control() {
                    return Some(frame);
                }
                if !state.open {
                    return None;
                }
            }
            notified.await;
        }
    }

    fn is_open(&self) -> bool {
        self.state
            .lock()
            .map(|state| state.open)
            .unwrap_or_else(|poisoned| {
                eprintln!("ERR_AGENTOS_PROTOCOL_OUTPUT_QUEUE_POISONED: recovering output queue while reading open state");
                poisoned.into_inner().open
            })
    }

    #[cfg(test)]
    fn close(&self) {
        self.close_with_error("ERR_AGENTOS_PROTOCOL_OUTPUT_CLOSED: output broker closed");
    }

    fn close_with_error(&self, error: impl Into<String>) {
        let mut state = self.state.lock().unwrap_or_else(|poisoned| {
            eprintln!("ERR_AGENTOS_PROTOCOL_OUTPUT_QUEUE_POISONED: recovering output queue");
            poisoned.into_inner()
        });
        if !state.open {
            return;
        }
        state.open = false;
        state.terminal_error = Some(error.into());
        // A writer failure means queued frames can no longer be delivered. Drop
        // them while closing so their count/byte reservations are released and
        // every async publisher can observe the terminal transport state.
        let ordinary = std::mem::take(&mut state.ordinary);
        let progress = std::mem::take(&mut state.progress);
        let rejection = std::mem::take(&mut state.rejection);
        let terminal = std::mem::take(&mut state.terminal);
        let observability = std::mem::take(&mut state.observability);
        drop(state);
        drop(ordinary);
        drop(progress);
        drop(rejection);
        drop(terminal);
        drop(observability);
        self.available.notify_all();
        self.control_available.notify_waiters();
        self.closed.notify_waiters();
    }

    fn terminal_error(&self) -> Option<String> {
        self.state
            .lock()
            .map(|state| state.terminal_error.clone())
            .unwrap_or_else(|poisoned| {
                eprintln!(
                    "ERR_AGENTOS_PROTOCOL_OUTPUT_QUEUE_POISONED: recovering output queue while reading terminal error"
                );
                poisoned.into_inner().terminal_error.clone()
            })
    }
}

#[derive(Clone)]
struct ProtocolFrameWriter {
    output: Arc<ProtocolOutputQueue>,
    codec: WireFrameCodec,
    ordinary_budget: ProtocolBudget,
    terminal_budget: ProtocolBudget,
    progress_budget: ProtocolBudget,
    rejection_budget: ProtocolBudget,
    control_observability_budget: ProtocolBudget,
}

#[derive(Debug)]
enum ProtocolTrySendError {
    Full(ProtocolLimitError),
    Disconnected(String),
    Rejected(io::Error),
}

impl fmt::Display for ProtocolTrySendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full(error) => error.fmt(formatter),
            Self::Disconnected(error) => formatter.write_str(error),
            Self::Rejected(error) => error.fmt(formatter),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProtocolOutputClass {
    Ordinary,
    Terminal,
    Progress,
    Rejection,
    Observability,
}

impl ProtocolFrameWriter {
    fn disconnected_error(&self) -> ProtocolTrySendError {
        ProtocolTrySendError::Disconnected(self.output.terminal_error().unwrap_or_else(|| {
            String::from("ERR_AGENTOS_PROTOCOL_OUTPUT_CLOSED: output broker closed")
        }))
    }

    fn new(
        output: Arc<ProtocolOutputQueue>,
        codec: WireFrameCodec,
        protocol: &agentos_runtime::RuntimeProtocolConfig,
        metrics: agentos_runtime::metrics::RuntimeMetrics,
    ) -> Result<Self, io::Error> {
        let control_observability_frames = protocol
            .max_control_frames
            .checked_sub(protocol.max_terminal_frames)
            .and_then(|value| value.checked_sub(protocol.max_progress_frames))
            .and_then(|value| value.checked_sub(protocol.max_rejection_frames))
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "ERR_AGENTOS_PROTOCOL_CONFIG: logical control frame capacities exceed runtime.protocol.maxControlFrames",
                )
            })?;
        let control_observability_bytes = protocol
            .max_control_bytes
            .checked_sub(protocol.max_terminal_bytes)
            .and_then(|value| value.checked_sub(protocol.max_progress_bytes))
            .and_then(|value| value.checked_sub(protocol.max_rejection_bytes))
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "ERR_AGENTOS_PROTOCOL_CONFIG: logical control byte capacities exceed runtime.protocol.maxControlBytes",
                )
            })?;
        let budget = |max_frames, max_bytes, frame_path, byte_path, label, metric| {
            ProtocolBudget::new(
                ProtocolBudgetConfig {
                    max_frames,
                    max_bytes,
                    frame_path,
                    byte_path,
                    label,
                    metric,
                },
                metrics.clone(),
            )
        };
        let ordinary_metric = agentos_runtime::metrics::ChannelMetricClass::StdioEgress;
        let control_metric = agentos_runtime::metrics::ChannelMetricClass::StdioEgress;
        Ok(Self {
            output,
            codec,
            ordinary_budget: budget(
                protocol.max_egress_frames,
                protocol.max_egress_bytes,
                "runtime.protocol.maxEgressFrames",
                "runtime.protocol.maxEgressBytes",
                "stdio ordinary egress",
                ordinary_metric,
            ),
            terminal_budget: budget(
                protocol.max_terminal_frames,
                protocol.max_terminal_bytes,
                "runtime.protocol.maxTerminalFrames",
                "runtime.protocol.maxTerminalBytes",
                "stdio terminal response egress",
                control_metric,
            ),
            progress_budget: budget(
                protocol.max_progress_frames,
                protocol.max_progress_bytes,
                "runtime.protocol.maxProgressFrames",
                "runtime.protocol.maxProgressBytes",
                "stdio progress egress",
                control_metric,
            ),
            rejection_budget: budget(
                protocol.max_rejection_frames,
                protocol.max_rejection_bytes,
                "runtime.protocol.maxRejectionFrames",
                "runtime.protocol.maxRejectionBytes",
                "stdio rejection egress",
                control_metric,
            ),
            control_observability_budget: budget(
                control_observability_frames,
                control_observability_bytes,
                "runtime.protocol.maxControlFrames",
                "runtime.protocol.maxControlBytes",
                "stdio control observability egress",
                control_metric,
            ),
        })
    }

    fn encoded_bytes(&self, frame: &ProtocolFrame) -> Result<Vec<u8>, io::Error> {
        self.codec
            .encode(frame)
            .map_err(wire_protocol_error)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))
    }

    fn is_control(frame: &ProtocolFrame) -> Result<bool, io::Error> {
        match frame {
            ProtocolFrame::EventFrame(event) => Ok(matches!(
                &event.payload,
                wire::EventPayload::StructuredEvent(event) if event.name == "heartbeat"
            )),
            ProtocolFrame::ResponseFrame(_) | ProtocolFrame::SidecarRequestFrame(_) => Ok(true),
            ProtocolFrame::RequestFrame(_)
            | ProtocolFrame::SidecarResponseFrame(_)
            | ProtocolFrame::ControlFrame(_) => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "ERR_AGENTOS_PROTOCOL_WRONG_LANE: sidecar cannot write {} frame",
                    frame_kind(frame)
                ),
            )),
        }
    }

    #[cfg(test)]
    fn default_class(frame: &ProtocolFrame) -> Result<ProtocolOutputClass, io::Error> {
        match frame {
            ProtocolFrame::ResponseFrame(_) => Ok(ProtocolOutputClass::Terminal),
            ProtocolFrame::SidecarRequestFrame(_) => Ok(ProtocolOutputClass::Progress),
            ProtocolFrame::EventFrame(event)
                if matches!(
                    &event.payload,
                    wire::EventPayload::StructuredEvent(event)
                        if event.name == "heartbeat" || event.name == "limit_warning"
                ) =>
            {
                Ok(ProtocolOutputClass::Observability)
            }
            ProtocolFrame::EventFrame(_) => Ok(ProtocolOutputClass::Ordinary),
            ProtocolFrame::RequestFrame(_)
            | ProtocolFrame::SidecarResponseFrame(_)
            | ProtocolFrame::ControlFrame(_) => Self::is_control(frame).map(|_| unreachable!()),
        }
    }

    fn budget_for(
        &self,
        class: ProtocolOutputClass,
        control: bool,
    ) -> Result<&ProtocolBudget, io::Error> {
        match (class, control) {
            (ProtocolOutputClass::Ordinary, false) => Ok(&self.ordinary_budget),
            (ProtocolOutputClass::Terminal, true) => Ok(&self.terminal_budget),
            (ProtocolOutputClass::Progress, true) => Ok(&self.progress_budget),
            (ProtocolOutputClass::Rejection, true) => Ok(&self.rejection_budget),
            (ProtocolOutputClass::Observability, false) => Ok(&self.ordinary_budget),
            (ProtocolOutputClass::Observability, true) => Ok(&self.control_observability_budget),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "ERR_AGENTOS_PROTOCOL_OUTPUT_CLASS: {class:?} is invalid for {} lane",
                    if control { "control" } else { "ordinary" }
                ),
            )),
        }
    }

    fn prepare(
        &self,
        class: ProtocolOutputClass,
        frame: ProtocolFrame,
    ) -> Result<(bool, EncodedProtocolFrame), ProtocolTrySendError> {
        let control = Self::is_control(&frame).map_err(ProtocolTrySendError::Rejected)?;
        let bytes = self
            .encoded_bytes(&frame)
            .map_err(ProtocolTrySendError::Rejected)?;
        let budget = self
            .budget_for(class, control)
            .map_err(ProtocolTrySendError::Rejected)?;
        let reservation = budget
            .reserve(bytes.len())
            .map_err(ProtocolTrySendError::Full)?;
        Ok((
            control,
            EncodedProtocolFrame {
                bytes,
                _reservation: reservation,
            },
        ))
    }

    fn enqueue(
        &self,
        class: ProtocolOutputClass,
        control: bool,
        encoded: EncodedProtocolFrame,
    ) -> Result<(), ProtocolTrySendError> {
        self.output.enqueue(class, control, encoded)
    }

    fn try_publish(
        &self,
        class: ProtocolOutputClass,
        frame: ProtocolFrame,
    ) -> Result<(), ProtocolTrySendError> {
        if !self.output.is_open() {
            return Err(self.disconnected_error());
        }
        let (control, encoded) = self.prepare(class, frame)?;
        self.enqueue(class, control, encoded)
    }

    async fn publish(
        &self,
        class: ProtocolOutputClass,
        frame: ProtocolFrame,
    ) -> Result<(), ProtocolTrySendError> {
        let control = Self::is_control(&frame).map_err(ProtocolTrySendError::Rejected)?;
        let bytes = self
            .encoded_bytes(&frame)
            .map_err(ProtocolTrySendError::Rejected)?;
        let budget = self
            .budget_for(class, control)
            .map_err(ProtocolTrySendError::Rejected)?;
        let reservation = loop {
            let changed = budget.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let closed = self.output.closed.notified();
            tokio::pin!(closed);
            closed.as_mut().enable();
            if !self.output.is_open() {
                return Err(self.disconnected_error());
            }
            match budget.reserve(bytes.len()) {
                Ok(reservation) => break reservation,
                Err(_) => {
                    tokio::select! {
                        _ = closed.as_mut() => return Err(self.disconnected_error()),
                        _ = changed.as_mut() => {}
                    }
                }
            }
        };
        self.enqueue(
            class,
            control,
            EncodedProtocolFrame {
                bytes,
                _reservation: reservation,
            },
        )
    }

    fn try_reserve_terminal(
        &self,
        maximum_encoded_bytes: usize,
    ) -> Result<ProtocolReservation, ProtocolTrySendError> {
        if !self.output.is_open() {
            return Err(self.disconnected_error());
        }
        self.terminal_budget
            .reserve(maximum_encoded_bytes)
            .map_err(ProtocolTrySendError::Full)
    }

    fn try_reserve_progress(
        &self,
        maximum_encoded_bytes: usize,
    ) -> Result<ProtocolReservation, ProtocolTrySendError> {
        if !self.output.is_open() {
            return Err(self.disconnected_error());
        }
        self.progress_budget
            .reserve(maximum_encoded_bytes)
            .map_err(ProtocolTrySendError::Full)
    }

    async fn publish_reserved_terminal(
        &self,
        reservation: ProtocolReservation,
        frame: ProtocolFrame,
    ) -> Result<(), ProtocolTrySendError> {
        self.publish_reserved_control(
            ProtocolOutputClass::Terminal,
            &self.terminal_budget,
            reservation,
            frame,
        )
        .await
    }

    async fn publish_reserved_terminal_for_operation(
        &self,
        reservation: ProtocolReservation,
        frame: ProtocolFrame,
        operation: &RequestOperation,
    ) -> Result<(), ProtocolTrySendError> {
        let encoded = self
            .prepare_reserved_control(
                ProtocolOutputClass::Terminal,
                &self.terminal_budget,
                reservation,
                frame,
            )
            .await?;
        self.output.enqueue_retained(
            ProtocolOutputClass::Terminal,
            true,
            encoded,
            || operation.mark_terminal_retained(),
            "ERR_AGENTOS_TERMINAL_PUBLICATION_TAKEN_OVER: shutdown took over the terminal response before broker retention",
        )
    }

    async fn publish_reserved_progress(
        &self,
        reservation: ProtocolReservation,
        frame: ProtocolFrame,
    ) -> Result<(), ProtocolTrySendError> {
        self.publish_reserved_control(
            ProtocolOutputClass::Progress,
            &self.progress_budget,
            reservation,
            frame,
        )
        .await
    }

    async fn publish_reserved_progress_for_request(
        &self,
        reservation: ProtocolReservation,
        frame: ProtocolFrame,
        progress_request: &ProgressRequest,
    ) -> Result<(), ProtocolTrySendError> {
        let encoded = self
            .prepare_reserved_control(
                ProtocolOutputClass::Progress,
                &self.progress_budget,
                reservation,
                frame,
            )
            .await?;
        self.output.enqueue_retained(
            ProtocolOutputClass::Progress,
            true,
            encoded,
            || progress_request.mark_acknowledgement_retained(),
            "ERR_AGENTOS_PROGRESS_PUBLICATION_TAKEN_OVER: shutdown took over the progress acknowledgement before broker retention",
        )
    }

    async fn publish_reserved_control(
        &self,
        class: ProtocolOutputClass,
        expected_budget: &ProtocolBudget,
        reservation: ProtocolReservation,
        frame: ProtocolFrame,
    ) -> Result<(), ProtocolTrySendError> {
        let encoded = self
            .prepare_reserved_control(class, expected_budget, reservation, frame)
            .await?;
        self.enqueue(class, true, encoded)
    }

    async fn prepare_reserved_control(
        &self,
        class: ProtocolOutputClass,
        expected_budget: &ProtocolBudget,
        mut reservation: ProtocolReservation,
        frame: ProtocolFrame,
    ) -> Result<EncodedProtocolFrame, ProtocolTrySendError> {
        let control = Self::is_control(&frame).map_err(ProtocolTrySendError::Rejected)?;
        if !control || !Arc::ptr_eq(&reservation.budget.state, &expected_budget.state) {
            return Err(ProtocolTrySendError::Rejected(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "ERR_AGENTOS_PROTOCOL_OUTPUT_RESERVATION: {class:?} output requires its matching reservation"
                ),
            )));
        }
        let bytes = match self.encoded_bytes(&frame) {
            Ok(bytes) if bytes.len() <= reservation.budget.config.max_bytes => bytes,
            Ok(bytes) => self.encode_reserved_response_limit_fallback(
                class,
                &frame,
                &reservation,
                Some(bytes.len()),
            )?,
            Err(error) => {
                eprintln!(
                    "ERR_AGENTOS_PROTOCOL_OUTPUT_ENCODE: reserved {class:?} response could not be encoded; emitting typed limit fallback: {error}"
                );
                self.encode_reserved_response_limit_fallback(class, &frame, &reservation, None)?
            }
        };
        while bytes.len() > reservation.bytes {
            let budget_changed = Arc::clone(&reservation.budget.changed);
            let changed = budget_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let closed = self.output.closed.notified();
            tokio::pin!(closed);
            closed.as_mut().enable();
            if !self.output.is_open() {
                return Err(self.disconnected_error());
            }
            match reservation.try_grow_bytes(bytes.len()) {
                Ok(()) => break,
                Err(_) => {
                    tokio::select! {
                        _ = closed.as_mut() => return Err(self.disconnected_error()),
                        _ = changed.as_mut() => {}
                    }
                }
            }
        }
        reservation.shrink_bytes(bytes.len());
        Ok(EncodedProtocolFrame {
            bytes,
            _reservation: reservation,
        })
    }

    fn encode_reserved_response_limit_fallback(
        &self,
        class: ProtocolOutputClass,
        frame: &ProtocolFrame,
        reservation: &ProtocolReservation,
        requested_bytes: Option<usize>,
    ) -> Result<Vec<u8>, ProtocolTrySendError> {
        let ProtocolFrame::ResponseFrame(response) = frame else {
            return Err(ProtocolTrySendError::Rejected(io::Error::new(
                io::ErrorKind::InvalidData,
                "ERR_AGENTOS_PROTOCOL_OUTPUT_LIMIT: only a response can use reserved fallback output",
            )));
        };
        let (code, operation) = match class {
            ProtocolOutputClass::Terminal => (
                "ERR_AGENTOS_TERMINAL_RESPONSE_LIMIT",
                "stdio.terminalResponse",
            ),
            ProtocolOutputClass::Progress => (
                "ERR_AGENTOS_PROGRESS_RESPONSE_LIMIT",
                "stdio.progressResponse",
            ),
            _ => {
                return Err(ProtocolTrySendError::Rejected(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "ERR_AGENTOS_PROTOCOL_OUTPUT_CLASS: reserved response fallback requires terminal or progress class",
                )));
            }
        };
        let fallback = ProtocolFrame::ResponseFrame(response_frame(
            response.request_id,
            response.ownership.clone(),
            ResponsePayload::RejectedResponse(wire::RejectedResponse {
                code: code.to_owned(),
                message: format!(
                    "response exceeded {}; raise {}",
                    reservation.budget.config.label, reservation.budget.config.byte_path
                ),
                limit_name: Some(reservation.budget.config.label.to_owned()),
                configured_limit: Some(
                    u64::try_from(reservation.budget.config.max_bytes).unwrap_or(u64::MAX),
                ),
                current_usage: Some(u64::try_from(reservation.bytes).unwrap_or(u64::MAX)),
                requested: requested_bytes.map(|bytes| u64::try_from(bytes).unwrap_or(u64::MAX)),
                unit: Some(String::from("bytes")),
                scope: Some(String::from("request")),
                vm_id: None,
                session_generation: None,
                capability_id: None,
                operation: Some(operation.to_owned()),
                configuration_path: Some(reservation.budget.config.byte_path.to_owned()),
                retryable: Some(false),
                errno: Some(String::from("EFBIG")),
            }),
        ));
        let bytes = self
            .encoded_bytes(&fallback)
            .map_err(ProtocolTrySendError::Rejected)?;
        if bytes.len() > reservation.budget.config.max_bytes {
            return Err(ProtocolTrySendError::Full(ProtocolLimitError {
                code: "ERR_AGENTOS_PROTOCOL_BYTE_LIMIT",
                path: reservation.budget.config.byte_path,
                label: reservation.budget.config.label,
                used: reservation.bytes,
                requested: bytes.len().saturating_sub(reservation.bytes),
                limit: reservation.budget.config.max_bytes,
                unit: "bytes",
            }));
        }
        Ok(bytes)
    }

    #[cfg(test)]
    fn try_send(&self, frame: ProtocolFrame) -> Result<(), ProtocolTrySendError> {
        let class = Self::default_class(&frame).map_err(ProtocolTrySendError::Rejected)?;
        self.try_publish(class, frame)
    }

    fn try_send_rejection(&self, frame: ProtocolFrame) -> Result<(), ProtocolTrySendError> {
        self.try_publish(ProtocolOutputClass::Rejection, frame)
    }

    fn try_send_progress(&self, frame: ProtocolFrame) -> Result<(), ProtocolTrySendError> {
        self.try_publish(ProtocolOutputClass::Progress, frame)
    }

    fn try_send_observability(&self, frame: ProtocolFrame) -> Result<(), ProtocolTrySendError> {
        self.try_publish(ProtocolOutputClass::Observability, frame)
    }
}

fn validate_protocol_transport_config(
    protocol: &agentos_runtime::RuntimeProtocolConfig,
    max_frame_bytes: usize,
) -> Result<(), io::Error> {
    for (path, bytes) in [
        (
            "runtime.protocol.maxIngressBytes",
            protocol.max_ingress_bytes,
        ),
        (
            "runtime.protocol.maxControlBytes",
            protocol.max_control_bytes,
        ),
        ("runtime.protocol.maxEgressBytes", protocol.max_egress_bytes),
        (
            "runtime.protocol.maxPendingResponseBytes",
            protocol.max_pending_response_bytes,
        ),
    ] {
        let required = max_frame_bytes.saturating_add(4);
        if bytes < required {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "ERR_AGENTOS_PROTOCOL_CONFIG: {path}={bytes} must be at least max_encoded_frame_bytes={required} so one legal frame remains admissible"
                ),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
fn request_frame(
    request_id: RequestId,
    ownership: OwnershipScope,
    payload: RequestPayload,
) -> RequestFrame {
    RequestFrame {
        schema: wire::protocol_schema(),
        request_id,
        ownership,
        payload,
    }
}

fn response_frame(
    request_id: RequestId,
    ownership: OwnershipScope,
    payload: ResponsePayload,
) -> ResponseFrame {
    ResponseFrame {
        schema: wire::protocol_schema(),
        request_id,
        ownership,
        payload,
    }
}

#[cfg(test)]
fn connection_ownership(connection_id: &str) -> OwnershipScope {
    OwnershipScope::ConnectionOwnership(wire::ConnectionOwnership {
        connection_id: connection_id.to_owned(),
    })
}

fn session_ownership(connection_id: &str, session_id: &str) -> OwnershipScope {
    OwnershipScope::SessionOwnership(wire::SessionOwnership {
        connection_id: connection_id.to_owned(),
        session_id: session_id.to_owned(),
    })
}

#[cfg(test)]
fn vm_ownership(connection_id: &str, session_id: &str, vm_id: &str) -> OwnershipScope {
    OwnershipScope::VmOwnership(wire::VmOwnership {
        connection_id: connection_id.to_owned(),
        session_id: session_id.to_owned(),
        vm_id: vm_id.to_owned(),
    })
}

fn wire_protocol_error(error: ProtocolCodecError) -> SidecarError {
    SidecarError::InvalidState(format!("invalid generated wire protocol frame: {error}"))
}

pub fn run(control_fd: OwnedFd) -> Result<(), Box<dyn Error>> {
    run_with_extensions(Vec::new(), control_fd)
}

pub fn run_with_runtime_config(
    control_fd: OwnedFd,
    runtime: agentos_runtime::RuntimeConfig,
) -> Result<(), Box<dyn Error>> {
    run_with_optional_control_and_runtime(Vec::new(), Some(control_fd), Some(runtime))
}

pub fn run_combined() -> Result<(), Box<dyn Error>> {
    run_combined_with_extensions(Vec::new())
}

pub fn run_with_extensions(
    extensions: Vec<Box<dyn Extension>>,
    control_fd: OwnedFd,
) -> Result<(), Box<dyn Error>> {
    run_with_optional_control(extensions, Some(control_fd))
}

pub fn run_combined_with_extensions(
    extensions: Vec<Box<dyn Extension>>,
) -> Result<(), Box<dyn Error>> {
    run_with_optional_control(extensions, None)
}

fn run_with_optional_control(
    extensions: Vec<Box<dyn Extension>>,
    control_fd: Option<OwnedFd>,
) -> Result<(), Box<dyn Error>> {
    run_with_optional_control_and_runtime(extensions, control_fd, None)
}

fn run_with_optional_control_and_runtime(
    extensions: Vec<Box<dyn Extension>>,
    control_fd: Option<OwnedFd>,
    runtime: Option<agentos_runtime::RuntimeConfig>,
) -> Result<(), Box<dyn Error>> {
    let mut config = NativeSidecarConfig {
        compile_cache_root: Some(default_compile_cache_root()),
        ..NativeSidecarConfig::default()
    };
    if let Some(runtime) = runtime {
        config.runtime = runtime;
    }
    let runtime = agentos_runtime::SidecarRuntime::process(&config.runtime)?;
    let runtime_context = runtime.context();
    // Initialize the embedded V8 runtime + platform now, on the long-lived main
    // thread, so it is never first-initialized on a transient worker thread (e.g. a
    // VM-create snapshot pre-warm thread that then exits — which corrupts V8's
    // platform and wedges later isolate creation). Best-effort.
    if let Err(error) = agentos_execution::v8_host::ensure_runtime_initialized(&runtime_context) {
        eprintln!("embedded V8 runtime init failed at startup: {error}");
    }
    // Extension request futures may use thread-affine VM services and are
    // therefore supervised as local tasks. The LocalSet lets multiple owned
    // extension requests make progress concurrently without making the full
    // NativeSidecar state Send or wrapping it in a global mutex.
    let local = tokio::task::LocalSet::new();
    runtime.block_on(local.run_until(run_async(extensions, config, runtime_context, control_fd)))
}

async fn run_async(
    extensions: Vec<Box<dyn Extension>>,
    config: NativeSidecarConfig,
    runtime_context: agentos_runtime::RuntimeContext,
    control_fd: Option<OwnedFd>,
) -> Result<(), Box<dyn Error>> {
    let callback_limits = FrameSidecarRequestLimits::from_config(&config);
    let protocol = config.runtime.protocol.clone();
    let max_frame_bytes = config.max_frame_bytes;
    validate_protocol_transport_config(&protocol, max_frame_bytes)?;
    let codec = WireFrameCodec::new(max_frame_bytes);
    let control_stream = control_fd.map(inherited_control_stream).transpose()?;
    let (mut control_reader, mut control_writer) = match control_stream {
        Some(stream) => {
            let (reader, writer) = stream.into_split();
            (Some(reader), Some(writer))
        }
        None => (None, None),
    };
    let metrics = runtime_context.metrics().clone();
    let ingress_budget = ProtocolBudget::new(
        ProtocolBudgetConfig {
            max_frames: protocol.max_ingress_frames,
            max_bytes: protocol.max_ingress_bytes,
            frame_path: "runtime.protocol.maxIngressFrames",
            byte_path: "runtime.protocol.maxIngressBytes",
            label: "stdio ordinary ingress",
            metric: agentos_runtime::metrics::ChannelMetricClass::StdioIngress,
        },
        metrics.clone(),
    );
    let control_ingress_budget = ProtocolBudget::new(
        ProtocolBudgetConfig {
            max_frames: protocol.max_control_frames,
            max_bytes: protocol.max_control_bytes,
            frame_path: "runtime.protocol.maxControlFrames",
            byte_path: "runtime.protocol.maxControlBytes",
            label: "stdio response/control ingress",
            metric: agentos_runtime::metrics::ChannelMetricClass::StdioIngress,
        },
        metrics,
    );
    let ownership_coordinator = OwnershipCoordinator::from_runtime_config(&config.runtime);
    let mut sidecar = NativeSidecar::with_config_extensions_and_runtime(
        LocalBridge::default(),
        config,
        extensions,
        runtime_context.clone(),
    )?;
    // The reader may classify only the extension's opaque payload; it never
    // interprets ACP or another extension protocol. Arc-backed extensions make
    // this immutable route table safe to share with the dedicated reader.
    let extension_routes = Arc::new(sidecar.extensions.clone());
    let (stdin_tx, stdin_rx) =
        channel::<Result<Option<AccountedProtocolFrame>, String>>(protocol.max_ingress_frames);
    let (stdin_control_tx, stdin_control_rx) =
        channel::<AccountedProtocolFrame>(protocol.max_control_frames);
    let (shutdown_tx, shutdown_rx) = channel::<wire::ControlFrame>(MAX_SHUTDOWN_QUEUE);
    let stdin_gauge = agentos_bridge::queue_tracker::register_queue(
        TrackedLimit::SidecarStdinFrames,
        protocol.max_ingress_frames,
    );
    let (event_ready_tx, event_ready_rx) = channel::<()>(MAX_EVENT_READY_QUEUE);
    let output_queue = Arc::new(ProtocolOutputQueue::new(
        protocol.max_egress_frames,
        protocol.max_control_frames,
    ));
    let frame_writer = ProtocolFrameWriter::new(
        Arc::clone(&output_queue),
        codec.clone(),
        &protocol,
        runtime_context.metrics().clone(),
    )?;
    let (write_error_tx, write_error_rx) = channel::<String>(MAX_TRANSPORT_ERROR_QUEUE);

    // Forward limit-registry near-capacity warnings to the host: the global sink
    // fires (edge-triggered, from arbitrary threads) into this channel, and the
    // event loop below drains it and emits a `StructuredEvent` (name
    // "limit_warning"). Keep the host-visible warning path bounded too: a
    // broken consumer must not turn observability into an unbounded heap sink.
    // The callback must never block an arbitrary producer, so it uses bounded
    // nonblocking admission and logs an explicit host-visible drop.
    let (limit_warning_tx, limit_warning_rx) =
        channel::<agentos_bridge::queue_tracker::LimitWarning>(MAX_LIMIT_WARNING_QUEUE);
    agentos_bridge::queue_tracker::set_limit_warning_handler(Box::new(move |warning| {
        if let Err(error) = limit_warning_tx.try_send(warning.clone()) {
            eprintln!(
                "ERR_AGENTOS_LIMIT_WARNING_QUEUE: could not enqueue limit warning {}: {error}",
                warning.name.as_str()
            );
        }
    }));
    let callback_transport = Arc::new(FrameSidecarRequestTransport::new(
        frame_writer.clone(),
        callback_limits,
    ));
    sidecar.set_sidecar_request_transport(callback_transport.clone());
    // Live event producers make a bounded, nonblocking handoff. A single async
    // drainer owns any wait for ordinary stdout capacity, so neither extension
    // execution nor the ingress router can be parked by a non-reading host.
    let (event_transport, mut live_event_rx) =
        FrameEventTransport::new(codec.clone(), &protocol, runtime_context.metrics().clone());
    let live_event_writer = frame_writer.clone();
    let live_event_error_tx = write_error_tx.clone();
    runtime_context.spawn(agentos_runtime::TaskClass::Runtime, async move {
        while let Some(pending) = live_event_rx.recv().await {
            let publish = live_event_writer
                .publish(
                    ProtocolOutputClass::Ordinary,
                    ProtocolFrame::EventFrame(pending.event),
                )
                .await;
            // `pending` owns the decoded handoff reservation until the broker
            // has retained the encoded frame or failed terminally.
            drop(pending._reservation);
            if let Err(error) = publish {
                if let Err(send_error) = live_event_error_tx.try_send(error.to_string()) {
                    eprintln!(
                        "ERR_AGENTOS_TRANSPORT_ERROR_QUEUE: could not enqueue live-event output error: {send_error}"
                    );
                }
                break;
            }
        }
    })?;
    let event_transport = Arc::new(event_transport);
    sidecar.set_event_transport(event_transport);
    // Every execution backend and deferred sidecar producer shares this
    // process-level edge. Durable bounded queues retain the data; the notify is
    // only a coalesced prompt to drain them, so no recurring session poll is
    // needed.
    let process_event_notify = Arc::clone(&sidecar.process_event_notify);
    // Extension waiters observe only post-pump state transitions. Keeping this
    // separate from the runtime producer edge makes the protocol coordinator
    // the sole consumer that can drain execution-engine queues.
    let routed_process_event_notify = Arc::new(Notify::new());
    let request_operations = OperationTable::from_protocol_config(&protocol);
    let progress_requests = request_operations.progress_requests();
    let extension_service_capacity = protocol
        .max_in_flight_requests
        .saturating_add(protocol.max_control_frames)
        .max(1);
    let (extension_service_tx, extension_service_rx) =
        channel::<ExtensionServiceCommand>(extension_service_capacity);
    let progress_service_capacity = protocol.max_progress_frames.max(1);
    let (progress_service_tx, progress_service_rx) =
        channel::<ExtensionServiceCommand>(progress_service_capacity);
    let (extension_service_completion_tx, extension_service_completion_rx) =
        channel::<CompletedExtensionServiceCommand>(extension_service_capacity);
    let extension_services: Arc<dyn ExtensionServices> = Arc::new(
        RoutedExtensionServices::new_with_process_event_broker_and_progress(
            extension_service_tx,
            progress_service_tx,
            Arc::clone(&routed_process_event_notify),
            sidecar.process_event_broker(),
        ),
    );
    sidecar.set_extension_services(Arc::clone(&extension_services));
    let (extension_completion_tx, extension_completion_rx) =
        channel::<DetachedExtensionCompletion>(extension_service_capacity);
    let (request_completion_tx, request_completion_rx) =
        channel::<DetachedRequestCompletion>(protocol.max_in_flight_requests.max(1));
    let reader_codec = codec.clone();
    let reader_frame_writer = frame_writer.clone();
    let writer_error_tx = write_error_tx.clone();
    let combined_stdio = control_writer.is_none();
    // AGENTOS_THREAD_SITE: constant-stdio-writer
    thread::spawn(move || {
        let mut writer = io::BufWriter::new(io::stdout());
        while let Some(frame) = if combined_stdio {
            output_queue.recv_combined()
        } else {
            output_queue.recv_ordinary()
        } {
            if let Err(error) = write_encoded_frame(&mut writer, &frame.bytes) {
                if let Err(send_error) = writer_error_tx.try_send(error.to_string()) {
                    eprintln!(
                        "ERR_AGENTOS_TRANSPORT_ERROR_QUEUE: could not enqueue stdout writer error: {send_error}"
                    );
                }
                output_queue.close_with_error(format!(
                    "ERR_AGENTOS_PROTOCOL_OUTPUT_WRITE: stdout writer failed: {error}"
                ));
                break;
            }
        }
    });
    if let Some(mut control_writer) = control_writer.take() {
        let control_output_queue = Arc::clone(&frame_writer.output);
        let control_write_error_tx = write_error_tx.clone();
        runtime_context.spawn(agentos_runtime::TaskClass::Runtime, async move {
            while let Some(frame) = control_output_queue.recv_control().await {
                let result = async {
                    control_writer.write_all(&frame.bytes).await?;
                    control_writer.flush().await
                }
                .await;
                if let Err(error) = result {
                    if let Err(send_error) = control_write_error_tx.try_send(error.to_string()) {
                        eprintln!(
                            "ERR_AGENTOS_TRANSPORT_ERROR_QUEUE: could not enqueue control writer error: {send_error}"
                        );
                    }
                    control_output_queue.close_with_error(format!(
                        "ERR_AGENTOS_PROTOCOL_OUTPUT_WRITE: control writer failed: {error}"
                    ));
                    break;
                }
            }
        })?;
    }
    let heartbeat_thread = spawn_heartbeat_thread(frame_writer.clone(), HEARTBEAT_INTERVAL);

    if let Some(mut control_reader) = control_reader.take() {
        // AGENTOS_THREAD_SITE: constant-stdio-reader
        thread::spawn({
            let read_error_tx = write_error_tx.clone();
            let progress_tx = stdin_control_tx.clone();
            let progress_budget = control_ingress_budget.clone();
            let extension_routes = Arc::clone(&extension_routes);
            move || {
                let mut stdin = io::stdin();
                loop {
                    let frame = match read_frame(&reader_codec, &mut stdin) {
                        Ok(Some(frame)) => frame,
                        Ok(None) => break,
                        Err(error) => {
                            if let Err(send_error) = read_error_tx.try_send(error.to_string()) {
                                eprintln!(
                                    "ERR_AGENTOS_TRANSPORT_ERROR_QUEUE: could not enqueue stdin reader error: {send_error}"
                                );
                            }
                            break;
                        }
                    };
                    if matches!(
                        route_decoded_stdin_frame(
                            frame,
                            &stdin_tx,
                            &progress_tx,
                            &reader_frame_writer,
                            &ingress_budget,
                            &progress_budget,
                            &extension_routes,
                        ),
                        StdinReaderFlow::Stop
                    ) {
                        break;
                    }
                    stdin_gauge
                        .observe_depth(stdin_tx.max_capacity().saturating_sub(stdin_tx.capacity()));
                }
            }
        });
        let control_reader_codec = codec.clone();
        let control_reader_transport = callback_transport.clone();
        let control_read_error_tx = write_error_tx.clone();
        runtime_context.spawn(agentos_runtime::TaskClass::Runtime, async move {
            loop {
                let frame = match read_frame_async(&control_reader_codec, &mut control_reader).await
                {
                    Ok(Some(frame)) => frame,
                    Ok(None) => {
                        let _ = control_read_error_tx
                            .try_send(String::from("response/control stream closed"));
                        break;
                    }
                    Err(error) => {
                        let _ = control_read_error_tx.try_send(error.to_string());
                        break;
                    }
                };
                if matches!(
                    route_decoded_control_frame(
                        frame,
                        &control_reader_transport,
                        &stdin_control_tx,
                        &shutdown_tx,
                        &control_ingress_budget,
                    ),
                    StdinReaderFlow::Stop
                ) {
                    break;
                }
            }
        })?;
    } else {
        // Rivet's V8 child-process bridge cannot currently inherit fd 3. Keep
        // the logical lane priorities, but multiplex both lanes over stdio.
        let combined_callback_transport = callback_transport.clone();
        // AGENTOS_THREAD_SITE: constant-stdio-reader
        thread::spawn({
            let read_error_tx = write_error_tx.clone();
            let extension_routes = Arc::clone(&extension_routes);
            move || {
                let mut stdin = io::stdin();
                loop {
                    let frame = match read_frame(&reader_codec, &mut stdin) {
                        Ok(Some(frame)) => frame,
                        Ok(None) => break,
                        Err(error) => {
                            let _ = read_error_tx.try_send(error.to_string());
                            break;
                        }
                    };
                    if matches!(
                        route_decoded_combined_frame(
                            frame,
                            &stdin_tx,
                            &combined_callback_transport,
                            &stdin_control_tx,
                            &shutdown_tx,
                            &reader_frame_writer,
                            &ingress_budget,
                            &control_ingress_budget,
                            &extension_routes,
                        ),
                        StdinReaderFlow::Stop
                    ) {
                        break;
                    }
                    stdin_gauge
                        .observe_depth(stdin_tx.max_capacity().saturating_sub(stdin_tx.capacity()));
                }
            }
        });
    }

    let result = run_protocol_engine(ProtocolEngine {
        protocol,
        sidecar,
        extension_services,
        ownership_coordinator,
        request_operations,
        progress_requests,
        callback_transport,
        frame_writer,
        stdin_rx,
        stdin_control_rx,
        shutdown_rx,
        extension_service_rx,
        progress_service_rx,
        extension_service_completion_tx,
        extension_service_completion_rx,
        extension_completion_tx,
        extension_completion_rx,
        request_completion_tx,
        request_completion_rx,
        limit_warning_rx,
        event_ready_tx,
        event_ready_rx,
        process_event_notify,
        routed_process_event_notify,
        write_error_rx,
    })
    .await;
    heartbeat_thread.stop();
    result
}

/// Production routing engine shared by real stdio and deterministic protocol
/// loop tests. Physical readers and writers own transport I/O; this engine owns
/// request routing, supervision, completion, event pumping, and bounded drain.
struct ProtocolEngine {
    protocol: agentos_runtime::RuntimeProtocolConfig,
    sidecar: NativeSidecar<LocalBridge>,
    extension_services: Arc<dyn ExtensionServices>,
    ownership_coordinator: OwnershipCoordinator,
    request_operations: OperationTable,
    progress_requests: ProgressOperationView,
    callback_transport: Arc<FrameSidecarRequestTransport>,
    frame_writer: ProtocolFrameWriter,
    stdin_rx: Receiver<Result<Option<AccountedProtocolFrame>, String>>,
    stdin_control_rx: Receiver<AccountedProtocolFrame>,
    shutdown_rx: Receiver<wire::ControlFrame>,
    extension_service_rx: Receiver<ExtensionServiceCommand>,
    progress_service_rx: Receiver<ExtensionServiceCommand>,
    extension_service_completion_tx: Sender<CompletedExtensionServiceCommand>,
    extension_service_completion_rx: Receiver<CompletedExtensionServiceCommand>,
    extension_completion_tx: Sender<DetachedExtensionCompletion>,
    extension_completion_rx: Receiver<DetachedExtensionCompletion>,
    request_completion_tx: Sender<DetachedRequestCompletion>,
    request_completion_rx: Receiver<DetachedRequestCompletion>,
    limit_warning_rx: Receiver<agentos_bridge::queue_tracker::LimitWarning>,
    event_ready_tx: Sender<()>,
    event_ready_rx: Receiver<()>,
    process_event_notify: Arc<Notify>,
    routed_process_event_notify: Arc<Notify>,
    write_error_rx: Receiver<String>,
}

async fn run_protocol_engine(engine: ProtocolEngine) -> Result<(), Box<dyn Error>> {
    let ProtocolEngine {
        protocol,
        mut sidecar,
        extension_services,
        ownership_coordinator,
        request_operations,
        progress_requests,
        callback_transport,
        frame_writer,
        mut stdin_rx,
        mut stdin_control_rx,
        mut shutdown_rx,
        mut extension_service_rx,
        mut progress_service_rx,
        extension_service_completion_tx,
        mut extension_service_completion_rx,
        extension_completion_tx,
        mut extension_completion_rx,
        request_completion_tx,
        mut request_completion_rx,
        mut limit_warning_rx,
        event_ready_tx,
        mut event_ready_rx,
        process_event_notify,
        routed_process_event_notify,
        mut write_error_rx,
    } = engine;
    // A caller may construct the protocol engine around a sidecar that already
    // owns restored or embedded connection/session state. Seed the loop-local
    // pump membership from that authoritative state; lifecycle responses add
    // newly committed memberships below as usual.
    let mut active_sessions = sidecar
        .sessions
        .iter()
        .map(|(session_id, session)| SessionScope {
            connection_id: session.connection_id.clone(),
            session_id: session_id.clone(),
        })
        .collect::<BTreeSet<_>>();
    let mut active_connections = sidecar.connections.keys().cloned().collect::<BTreeSet<_>>();
    let mut extension_service_tasks = JoinSet::<()>::new();
    let mut progress_service_tasks = JoinSet::<()>::new();
    let mut extension_tasks = JoinSet::<()>::new();
    let mut request_tasks = JoinSet::<()>::new();
    let mut output_tasks = JoinSet::<Result<(), String>>::new();
    // Exactly one durable process event may wait for ordinary broker capacity.
    // The source queue remains the durable backlog; this set owns only the
    // single frame that has been removed from it for publication.
    let mut ordinary_event_tasks = JoinSet::<Result<(), String>>::new();
    let extension_service_capacity = protocol
        .max_in_flight_requests
        .saturating_add(protocol.max_control_frames)
        .max(1);
    let progress_service_capacity = protocol.max_progress_frames.max(1);
    let owned_process_event_capacity = extension_service_capacity
        .min(protocol.max_process_events)
        .max(1);
    // Claimed internal runtime events have left their producer queue, so they
    // remain durably owned here until bounded service admission is available.
    // Each pending command owns exactly one coordinator registration. Because
    // the pump claims only the remaining slots in this queue and this capacity
    // is no greater than maxProcessEvents, the separate deferred-registration
    // bound cannot reject an event after production has claimed it. Later
    // events stay in their durable source queue instead of becoming untracked
    // overflow.
    let mut pending_owned_process_events = VecDeque::<PreparedExtensionServiceCommand>::new();

    let mut limit_warning_closed = false;
    let mut stdin_closed = false;
    let mut control_ingress_closed = false;
    let mut shutdown_ingress_closed = false;
    let mut write_error_closed = false;
    let mut drain_state = None::<ProtocolDrainState>;
    let shutdown_grace = Duration::from_millis(protocol.shutdown_grace_ms);

    macro_rules! protocol_try {
        ($label:lifetime, $result:expr, $context:literal) => {
            match $result {
                Ok(value) => value,
                Err(error) => {
                    begin_protocol_transport_failure(
                        format!(concat!($context, ": {}"), error),
                        &mut drain_state,
                        shutdown_grace,
                        &request_operations,
                        &progress_requests,
                        &callback_transport,
                        &frame_writer,
                        false,
                        &mut stdin_closed,
                        &mut control_ingress_closed,
                        &mut shutdown_ingress_closed,
                        &mut stdin_rx,
                        &mut stdin_control_rx,
                        &mut shutdown_rx,
                    );
                    continue $label;
                }
            }
        };
    }

    if let Err(error) = flush_sidecar_requests(&mut sidecar, &frame_writer, &mut output_tasks) {
        begin_protocol_transport_failure(
            format!("ERR_AGENTOS_SIDECAR_REQUEST_FLUSH: {error}"),
            &mut drain_state,
            shutdown_grace,
            &request_operations,
            &progress_requests,
            &callback_transport,
            &frame_writer,
            false,
            &mut stdin_closed,
            &mut control_ingress_closed,
            &mut shutdown_ingress_closed,
            &mut stdin_rx,
            &mut stdin_control_rx,
            &mut shutdown_rx,
        );
    }
    'protocol: loop {
        // Tokio's JoinSet retains completed entries until they are reaped. Do
        // this before biased ingress selection so continuously-ready input
        // cannot grow bookkeeping without bound or starve progress-service
        // admission behind already-completed tasks.
        if let Err(error) = reap_protocol_tasks_nowait(
            &mut extension_service_tasks,
            &mut progress_service_tasks,
            &mut extension_tasks,
            &mut request_tasks,
            &mut output_tasks,
            &mut ordinary_event_tasks,
            &event_ready_tx,
        ) {
            let message = format!(
                "ERR_AGENTOS_PROTOCOL_SUPERVISOR: task failed while routing protocol work: {error}"
            );
            begin_protocol_transport_failure(
                message,
                &mut drain_state,
                shutdown_grace,
                &request_operations,
                &progress_requests,
                &callback_transport,
                &frame_writer,
                false,
                &mut stdin_closed,
                &mut control_ingress_closed,
                &mut shutdown_ingress_closed,
                &mut stdin_rx,
                &mut stdin_control_rx,
                &mut shutdown_rx,
            );
            continue;
        }
        let pending_admission_turn = pending_owned_process_events.len();
        for _ in 0..pending_admission_turn {
            if extension_service_tasks.len() >= extension_service_capacity {
                break;
            }
            let Some(target) = pending_owned_process_events.pop_front() else {
                break;
            };
            match target.admit_vm_event_nowait() {
                Ok(VmEventAdmissionResult::Admitted(target)) => {
                    schedule_extension_service_command(
                        target,
                        &extension_service_completion_tx,
                        &mut extension_service_tasks,
                    );
                }
                Ok(VmEventAdmissionResult::Deferred(target)) => {
                    pending_owned_process_events.push_back(target);
                }
                Err(error) => tracing::debug!(
                    %error,
                    "claimed process event ended before VM service admission"
                ),
            }
        }
        if drain_state.is_some()
            && request_operations.snapshot().in_flight_requests == 0
            && progress_requests.snapshot().in_flight_requests == 0
            && extension_service_tasks.is_empty()
            && progress_service_tasks.is_empty()
            && extension_service_completion_rx.is_empty()
            && extension_tasks.is_empty()
            && extension_completion_rx.is_empty()
            && request_tasks.is_empty()
            && request_completion_rx.is_empty()
            && output_tasks.is_empty()
            && ordinary_event_tasks.is_empty()
            && pending_owned_process_events.is_empty()
            && stdin_rx.is_empty()
            && stdin_control_rx.is_empty()
        {
            break;
        }
        if drain_state.is_none()
            && stdin_closed
            && extension_service_tasks.is_empty()
            && progress_service_tasks.is_empty()
            && extension_service_completion_rx.is_empty()
            && extension_tasks.is_empty()
            && extension_completion_rx.is_empty()
            && request_tasks.is_empty()
            && request_completion_rx.is_empty()
            && output_tasks.is_empty()
            && ordinary_event_tasks.is_empty()
            && pending_owned_process_events.is_empty()
        {
            break;
        }

        let drain_deadline = drain_state
            .as_ref()
            .map(|state| state.deadline)
            .unwrap_or_else(|| tokio::time::Instant::now() + Duration::from_secs(86_400));

        tokio::select! {
            biased;
            _ = tokio::time::sleep_until(drain_deadline), if drain_state.is_some() => {
                break 'protocol;
            }
            maybe_shutdown = shutdown_rx.recv(), if !shutdown_ingress_closed => {
                let Some(control) = maybe_shutdown else {
                    begin_protocol_transport_failure(
                        String::from("shutdown/control ingress closed"),
                        &mut drain_state,
                        shutdown_grace,
                        &request_operations,
                        &progress_requests,
                        &callback_transport,
                        &frame_writer,
                        false,
                        &mut stdin_closed,
                        &mut control_ingress_closed,
                        &mut shutdown_ingress_closed,
                        &mut stdin_rx,
                        &mut stdin_control_rx,
                        &mut shutdown_rx,
                    );
                    continue;
                };
                match control.payload {
                    wire::ControlPayload::ShutdownControl(shutdown) => {
                        tracing::debug!(reason = %shutdown.reason, "host requested sidecar shutdown");
                        begin_protocol_drain(
                            &mut drain_state,
                            OperationCancellationReason::Shutdown,
                            shutdown_grace,
                            None,
                            &request_operations,
                            &progress_requests,
                            false,
                        );
                    }
                }
            }
            // The select is biased so progress service admission must precede
            // ordinary service work. Separate capacity alone would not help if
            // a continuously-ready ordinary queue could win every router turn.
            maybe_progress_service = progress_service_rx.recv(), if progress_service_tasks.len() < progress_service_capacity => {
                let Some(command) = maybe_progress_service else {
                    begin_protocol_transport_failure(
                        String::from("progress extension service command channel closed while router was active"),
                        &mut drain_state,
                        shutdown_grace,
                        &request_operations,
                        &progress_requests,
                        &callback_transport,
                        &frame_writer,
                        false,
                        &mut stdin_closed,
                        &mut control_ingress_closed,
                        &mut shutdown_ingress_closed,
                        &mut stdin_rx,
                        &mut stdin_control_rx,
                        &mut shutdown_rx,
                    );
                    continue 'protocol;
                };
                let prepared = prepare_extension_service_command(
                    &mut sidecar,
                    &ownership_coordinator,
                    command,
                );
                schedule_extension_service_command(
                    prepared,
                    &extension_service_completion_tx,
                    &mut progress_service_tasks,
                );
            }
            maybe_service = extension_service_rx.recv(), if extension_service_tasks.len() < extension_service_capacity => {
                let Some(command) = maybe_service else {
                    begin_protocol_transport_failure(
                        String::from("extension service command channel closed while router was active"),
                        &mut drain_state,
                        shutdown_grace,
                        &request_operations,
                        &progress_requests,
                        &callback_transport,
                        &frame_writer,
                        false,
                        &mut stdin_closed,
                        &mut control_ingress_closed,
                        &mut shutdown_ingress_closed,
                        &mut stdin_rx,
                        &mut stdin_control_rx,
                        &mut shutdown_rx,
                    );
                    continue 'protocol;
                };
                let prepared = prepare_extension_service_command(
                    &mut sidecar,
                    &ownership_coordinator,
                    command,
                );
                schedule_extension_service_command(
                    prepared,
                    &extension_service_completion_tx,
                    &mut extension_service_tasks,
                );
            }
            maybe_service_completion = extension_service_completion_rx.recv() => {
                let Some(completion) = maybe_service_completion else {
                    begin_protocol_transport_failure(
                        String::from("extension service completion channel closed while router was active"),
                        &mut drain_state,
                        shutdown_grace,
                        &request_operations,
                        &progress_requests,
                        &callback_transport,
                        &frame_writer,
                        false,
                        &mut stdin_closed,
                        &mut control_ingress_closed,
                        &mut shutdown_ingress_closed,
                        &mut stdin_rx,
                        &mut stdin_control_rx,
                        &mut shutdown_rx,
                    );
                    continue 'protocol;
                };
                if let Some(next) = completion.complete(&mut sidecar) {
                    schedule_extension_service_command(
                        next,
                        &extension_service_completion_tx,
                        &mut extension_service_tasks,
                    );
                }
                untrack_disposed_sessions(
                    &sidecar.take_disposed_sessions(),
                    &mut active_sessions,
                );
                protocol_try!('protocol,
                    flush_sidecar_requests(&mut sidecar, &frame_writer, &mut output_tasks),
                    "ERR_AGENTOS_SIDECAR_REQUEST_FLUSH"
                );
            }
            maybe_completion = extension_completion_rx.recv() => {
                let Some(completion) = maybe_completion else {
                    begin_protocol_transport_failure(
                        String::from("extension completion channel closed while router was active"),
                        &mut drain_state,
                        shutdown_grace,
                        &request_operations,
                        &progress_requests,
                        &callback_transport,
                        &frame_writer,
                        false,
                        &mut stdin_closed,
                        &mut control_ingress_closed,
                        &mut shutdown_ingress_closed,
                        &mut stdin_rx,
                        &mut stdin_control_rx,
                        &mut shutdown_rx,
                    );
                    continue 'protocol;
                };
                protocol_try!('protocol, finish_extension_request(
                    completion,
                    &sidecar,
                    &frame_writer,
                    &mut output_tasks,
                    &mut active_sessions,
                    &mut active_connections,
                ), "ERR_AGENTOS_EXTENSION_COMPLETION");
                protocol_try!('protocol,
                    flush_sidecar_requests(&mut sidecar, &frame_writer, &mut output_tasks),
                    "ERR_AGENTOS_SIDECAR_REQUEST_FLUSH"
                );
            }
            maybe_completion = request_completion_rx.recv() => {
                let Some(completion) = maybe_completion else {
                    begin_protocol_transport_failure(
                        String::from("request completion channel closed while router was active"),
                        &mut drain_state,
                        shutdown_grace,
                        &request_operations,
                        &progress_requests,
                        &callback_transport,
                        &frame_writer,
                        false,
                        &mut stdin_closed,
                        &mut control_ingress_closed,
                        &mut shutdown_ingress_closed,
                        &mut stdin_rx,
                        &mut stdin_control_rx,
                        &mut shutdown_rx,
                    );
                    continue 'protocol;
                };
                protocol_try!('protocol, finish_request(
                    completion,
                    &mut sidecar,
                    &ownership_coordinator,
                    &request_completion_tx,
                    &mut request_tasks,
                    &frame_writer,
                    &mut output_tasks,
                    &mut active_sessions,
                    &mut active_connections,
                ), "ERR_AGENTOS_REQUEST_COMPLETION");
                protocol_try!('protocol,
                    flush_sidecar_requests(&mut sidecar, &frame_writer, &mut output_tasks),
                    "ERR_AGENTOS_SIDECAR_REQUEST_FLUSH"
                );
            }
            maybe_control = stdin_control_rx.recv(), if !control_ingress_closed => {
                match maybe_control {
                    Some(frame) => {
                        protocol_try!('protocol, route_protocol_frame(
                            frame,
                            &mut sidecar,
                            &extension_services,
                            &request_operations,
                            &progress_requests,
                            &ownership_coordinator,
                            &extension_completion_tx,
                            &request_completion_tx,
                            &mut extension_tasks,
                            &mut request_tasks,
                            &mut output_tasks,
                            &frame_writer,
                            protocol.terminal_fallback_bytes,
                            &mut active_sessions,
                            &mut active_connections,
                        ), "ERR_AGENTOS_PROTOCOL_CONTROL_ROUTE");
                    }
                    None => {
                        begin_protocol_transport_failure(
                            String::from("response/control stream closed"),
                            &mut drain_state,
                            shutdown_grace,
                            &request_operations,
                            &progress_requests,
                            &callback_transport,
                            &frame_writer,
                            false,
                            &mut stdin_closed,
                            &mut control_ingress_closed,
                            &mut shutdown_ingress_closed,
                            &mut stdin_rx,
                            &mut stdin_control_rx,
                            &mut shutdown_rx,
                        );
                    }
                }
            }
            maybe_frame = stdin_rx.recv(), if !stdin_closed => {
                match maybe_frame {
                    Some(frame) => {
                        let Some(frame) = protocol_try!('protocol,
                            frame.map_err(io::Error::other),
                            "ERR_AGENTOS_PROTOCOL_INGRESS"
                        ) else {
                            stdin_closed = true;
                            continue;
                        };
                        protocol_try!('protocol, route_protocol_frame(
                            frame,
                            &mut sidecar,
                            &extension_services,
                            &request_operations,
                            &progress_requests,
                            &ownership_coordinator,
                            &extension_completion_tx,
                            &request_completion_tx,
                            &mut extension_tasks,
                            &mut request_tasks,
                            &mut output_tasks,
                            &frame_writer,
                            protocol.terminal_fallback_bytes,
                            &mut active_sessions,
                            &mut active_connections,
                        ), "ERR_AGENTOS_PROTOCOL_REQUEST_ROUTE");
                    }
                    None => {
                        begin_protocol_transport_failure(
                            String::from("ordinary request ingress closed"),
                            &mut drain_state,
                            shutdown_grace,
                            &request_operations,
                            &progress_requests,
                            &callback_transport,
                            &frame_writer,
                            false,
                            &mut stdin_closed,
                            &mut control_ingress_closed,
                            &mut shutdown_ingress_closed,
                            &mut stdin_rx,
                            &mut stdin_control_rx,
                            &mut shutdown_rx,
                        );
                    },
                }
            }
            maybe_warning = limit_warning_rx.recv(), if !limit_warning_closed => {
                match maybe_warning {
                    Some(warning) => {
                        // A limit warning is process-global; deliver it ONCE. The
                        // stdio transport is single-client, so emit it to the first
                        // active connection (if any) rather than fanning out a copy
                        // per connection. Dropped if no client has authenticated yet
                        // (only the tracing log survives, which is acceptable).
                        if let Some(connection_id) = active_connections.iter().next() {
                            let mut detail = std::collections::HashMap::new();
                            detail.insert(String::from("limit"), warning.name.as_str().to_string());
                            detail.insert(
                                String::from("category"),
                                warning.category.as_str().to_string(),
                            );
                            detail.insert(String::from("observed"), warning.observed.to_string());
                            detail.insert(String::from("capacity"), warning.capacity.to_string());
                            detail.insert(
                                String::from("fillPercent"),
                                warning.fill_percent.to_string(),
                            );
                            let frame = protocol_try!('protocol,
                                crate::service::structured_event_frame(
                                    connection_id,
                                    "limit_warning",
                                    detail,
                                ),
                                "ERR_AGENTOS_LIMIT_WARNING_FRAME"
                            );
                            match frame_writer.try_send_observability(ProtocolFrame::EventFrame(frame)) {
                                Ok(()) => {}
                                Err(ProtocolTrySendError::Full(error)) => {
                                    eprintln!(
                                        "ERR_AGENTOS_LIMIT_WARNING_OUTPUT: limit warning could not be retained: {error}"
                                    );
                                }
                                Err(ProtocolTrySendError::Disconnected(error)) => {
                                    begin_protocol_transport_failure(
                                        format!("ERR_AGENTOS_LIMIT_WARNING_OUTPUT: {error}"),
                                        &mut drain_state,
                                        shutdown_grace,
                                        &request_operations,
                                        &progress_requests,
                                        &callback_transport,
                                        &frame_writer,
                                        true,
                                        &mut stdin_closed,
                                        &mut control_ingress_closed,
                                        &mut shutdown_ingress_closed,
                                        &mut stdin_rx,
                                        &mut stdin_control_rx,
                                        &mut shutdown_rx,
                                    );
                                    continue 'protocol;
                                }
                                Err(ProtocolTrySendError::Rejected(error)) => {
                                    begin_protocol_transport_failure(
                                        format!("ERR_AGENTOS_LIMIT_WARNING_OUTPUT: {error}"),
                                        &mut drain_state,
                                        shutdown_grace,
                                        &request_operations,
                                        &progress_requests,
                                        &callback_transport,
                                        &frame_writer,
                                        false,
                                        &mut stdin_closed,
                                        &mut control_ingress_closed,
                                        &mut shutdown_ingress_closed,
                                        &mut stdin_rx,
                                        &mut stdin_control_rx,
                                        &mut shutdown_rx,
                                    );
                                    continue 'protocol;
                                }
                            }
                        }
                    }
                    None => {
                        // Sender dropped (only possible if another sidecar replaced
                        // the global handler in-process). Disarm this branch so the
                        // select! does not hot-spin on an always-ready closed
                        // receiver; do NOT break — that would tear down the sidecar.
                        limit_warning_closed = true;
                    }
                }
            }
            maybe_ready = event_ready_rx.recv(), if ordinary_event_tasks.is_empty() => {
                let Some(()) = maybe_ready else {
                    begin_protocol_transport_failure(
                        String::from("event-ready channel closed while router was active"),
                        &mut drain_state,
                        shutdown_grace,
                        &request_operations,
                        &progress_requests,
                        &callback_transport,
                        &frame_writer,
                        false,
                        &mut stdin_closed,
                        &mut control_ingress_closed,
                        &mut shutdown_ingress_closed,
                        &mut stdin_rx,
                        &mut stdin_control_rx,
                        &mut shutdown_rx,
                    );
                    continue 'protocol;
                };
                for session in active_sessions.iter().cloned().collect::<Vec<_>>() {
                    let frame = protocol_try!('protocol,
                        sidecar
                            .poll_event_nowait(&session.compat_ownership_scope())
                            .and_then(|event| {
                                event
                                    .map(wire::event_frame_from_compat)
                                    .transpose()
                                    .map_err(wire_protocol_error)
                            }),
                        "ERR_AGENTOS_EVENT_PUMP"
                    );
                    if let Some(frame) = frame {
                        let scheduled = schedule_durable_event_frame(
                            &frame_writer,
                            &mut ordinary_event_tasks,
                            frame,
                        );
                        debug_assert!(
                            scheduled,
                            "event output task must be empty while the event-ready branch is enabled"
                        );
                        break;
                    }
                }
                protocol_try!('protocol,
                    flush_sidecar_requests(&mut sidecar, &frame_writer, &mut output_tasks),
                    "ERR_AGENTOS_SIDECAR_REQUEST_FLUSH"
                );
            }
            _ = process_event_notify.notified(), if pending_owned_process_events.len() < owned_process_event_capacity => {
                let mut routed_process_event_progress = false;
                for session in active_sessions.iter().cloned().collect::<Vec<_>>() {
                    let remaining_claims = owned_process_event_capacity
                        .saturating_sub(pending_owned_process_events.len());
                    if remaining_claims == 0 {
                        break;
                    }
                    let turn = protocol_try!('protocol,
                        sidecar.pump_process_events_nowait(
                            &session.compat_ownership_scope(),
                            remaining_claims,
                        ),
                        "ERR_AGENTOS_PROCESS_EVENT_PUMP"
                    );
                    if turn.emitted_any {
                        routed_process_event_progress = true;
                        protocol_try!('protocol,
                            rearm_event_ready(&event_ready_tx),
                            "ERR_AGENTOS_EVENT_READY_WAKE"
                        );
                    }
                    for target in turn.javascript_services {
                        match prepare_owned_process_event_service(
                            &mut sidecar,
                            &ownership_coordinator,
                            OwnedProcessEventService::Javascript(target),
                        )
                        .admit_vm_event_nowait()
                        {
                            Ok(VmEventAdmissionResult::Admitted(prepared)
                            | VmEventAdmissionResult::Deferred(prepared)) => {
                                pending_owned_process_events.push_back(prepared);
                            }
                            Err(error) => tracing::debug!(
                                %error,
                                "claimed JavaScript event ended before VM service admission"
                            ),
                        }
                    }
                    for target in turn.python_services {
                        match prepare_owned_process_event_service(
                            &mut sidecar,
                            &ownership_coordinator,
                            OwnedProcessEventService::Python(target),
                        )
                        .admit_vm_event_nowait()
                        {
                            Ok(VmEventAdmissionResult::Admitted(prepared)
                            | VmEventAdmissionResult::Deferred(prepared)) => {
                                pending_owned_process_events.push_back(prepared);
                            }
                            Err(error) => tracing::debug!(
                                %error,
                                "claimed Python event ended before VM service admission"
                            ),
                        }
                    }
                    for target in turn.python_socket_completions {
                        match prepare_owned_process_event_service(
                            &mut sidecar,
                            &ownership_coordinator,
                            OwnedProcessEventService::PythonSocketCompletion(target),
                        )
                        .admit_vm_event_nowait()
                        {
                            Ok(VmEventAdmissionResult::Admitted(prepared)
                            | VmEventAdmissionResult::Deferred(prepared)) => {
                                pending_owned_process_events.push_back(prepared);
                            }
                            Err(error) => tracing::debug!(
                                %error,
                                "claimed Python socket completion ended before VM service admission"
                            ),
                        }
                    }
                    for target in turn.child_bridge_services {
                        match prepare_owned_process_event_service(
                            &mut sidecar,
                            &ownership_coordinator,
                            OwnedProcessEventService::ChildBridge(target),
                        )
                        .admit_vm_event_nowait()
                        {
                            Ok(VmEventAdmissionResult::Admitted(prepared)
                            | VmEventAdmissionResult::Deferred(prepared)) => {
                                pending_owned_process_events.push_back(prepared);
                            }
                            Err(error) => tracing::debug!(
                                %error,
                                "claimed child bridge event ended before VM service admission"
                            ),
                        }
                    }
                    debug_assert!(
                        pending_owned_process_events.len() <= owned_process_event_capacity,
                        "the process-event pump must not claim beyond tracked local capacity"
                    );
                }
                if routed_process_event_progress {
                    // These waiters registered before their durable probe.
                    // Broadcast only after the central pump has claimed
                    // internal work and handed claimed public events to the
                    // broker; this edge is never used to drive the pump itself.
                    routed_process_event_notify.notify_waiters();
                }
                protocol_try!('protocol,
                    flush_sidecar_requests(&mut sidecar, &frame_writer, &mut output_tasks),
                    "ERR_AGENTOS_SIDECAR_REQUEST_FLUSH"
                );
            }
            maybe_write_error = write_error_rx.recv(), if !write_error_closed => {
                if let Some(error) = maybe_write_error {
                    let message = format!("ERR_AGENTOS_PROTOCOL_TRANSPORT: {error}");
                    begin_protocol_transport_failure(
                        message,
                        &mut drain_state,
                        shutdown_grace,
                        &request_operations,
                        &progress_requests,
                        &callback_transport,
                        &frame_writer,
                        true,
                        &mut stdin_closed,
                        &mut control_ingress_closed,
                        &mut shutdown_ingress_closed,
                        &mut stdin_rx,
                        &mut stdin_control_rx,
                        &mut shutdown_rx,
                    );
                } else {
                    write_error_closed = true;
                }
            }
            maybe_task = extension_service_tasks.join_next(), if !extension_service_tasks.is_empty() => {
                if let Some(Err(error)) = maybe_task {
                    begin_protocol_transport_failure(
                        format!(
                        "ERR_AGENTOS_EXTENSION_SERVICE_SUPERVISOR_TASK: completion monitor failed: {error}"
                        ),
                        &mut drain_state,
                        shutdown_grace,
                        &request_operations,
                        &progress_requests,
                        &callback_transport,
                        &frame_writer,
                        false,
                        &mut stdin_closed,
                        &mut control_ingress_closed,
                        &mut shutdown_ingress_closed,
                        &mut stdin_rx,
                        &mut stdin_control_rx,
                        &mut shutdown_rx,
                    );
                }
            }
            maybe_task = progress_service_tasks.join_next(), if !progress_service_tasks.is_empty() => {
                if let Some(Err(error)) = maybe_task {
                    begin_protocol_transport_failure(
                        format!(
                        "ERR_AGENTOS_PROGRESS_SERVICE_SUPERVISOR_TASK: completion monitor failed: {error}"
                        ),
                        &mut drain_state,
                        shutdown_grace,
                        &request_operations,
                        &progress_requests,
                        &callback_transport,
                        &frame_writer,
                        false,
                        &mut stdin_closed,
                        &mut control_ingress_closed,
                        &mut shutdown_ingress_closed,
                        &mut stdin_rx,
                        &mut stdin_control_rx,
                        &mut shutdown_rx,
                    );
                }
            }
            maybe_task = extension_tasks.join_next(), if !extension_tasks.is_empty() => {
                if let Some(Err(error)) = maybe_task {
                    begin_protocol_transport_failure(
                        format!(
                        "ERR_AGENTOS_REQUEST_SUPERVISOR_TASK: extension completion monitor failed: {error}"
                        ),
                        &mut drain_state,
                        shutdown_grace,
                        &request_operations,
                        &progress_requests,
                        &callback_transport,
                        &frame_writer,
                        false,
                        &mut stdin_closed,
                        &mut control_ingress_closed,
                        &mut shutdown_ingress_closed,
                        &mut stdin_rx,
                        &mut stdin_control_rx,
                        &mut shutdown_rx,
                    );
                }
            }
            maybe_task = request_tasks.join_next(), if !request_tasks.is_empty() => {
                if let Some(Err(error)) = maybe_task {
                    begin_protocol_transport_failure(
                        format!(
                        "ERR_AGENTOS_REQUEST_SUPERVISOR_TASK: generic completion monitor failed: {error}"
                        ),
                        &mut drain_state,
                        shutdown_grace,
                        &request_operations,
                        &progress_requests,
                        &callback_transport,
                        &frame_writer,
                        false,
                        &mut stdin_closed,
                        &mut control_ingress_closed,
                        &mut shutdown_ingress_closed,
                        &mut stdin_rx,
                        &mut stdin_control_rx,
                        &mut shutdown_rx,
                    );
                }
            }
            maybe_output = output_tasks.join_next(), if !output_tasks.is_empty() => {
                match maybe_output {
                    Some(Ok(Ok(()))) => {}
                    Some(Ok(Err(error))) => {
                        begin_protocol_transport_failure(
                            format!("ERR_AGENTOS_OUTPUT_TASK: output publisher failed: {error}"),
                            &mut drain_state,
                            shutdown_grace,
                            &request_operations,
                            &progress_requests,
                            &callback_transport,
                            &frame_writer,
                            false,
                            &mut stdin_closed,
                            &mut control_ingress_closed,
                            &mut shutdown_ingress_closed,
                            &mut stdin_rx,
                            &mut stdin_control_rx,
                            &mut shutdown_rx,
                        );
                    }
                    Some(Err(error)) => {
                        begin_protocol_transport_failure(
                            format!("ERR_AGENTOS_OUTPUT_TASK: output publisher failed: {error}"),
                            &mut drain_state,
                            shutdown_grace,
                            &request_operations,
                            &progress_requests,
                            &callback_transport,
                            &frame_writer,
                            false,
                            &mut stdin_closed,
                            &mut control_ingress_closed,
                            &mut shutdown_ingress_closed,
                            &mut stdin_rx,
                            &mut stdin_control_rx,
                            &mut shutdown_rx,
                        );
                    }
                    None => {}
                }
            }
            maybe_event_output = ordinary_event_tasks.join_next(), if !ordinary_event_tasks.is_empty() => {
                match maybe_event_output {
                    Some(Ok(Ok(()))) => {
                        // Re-arm after the broker retained the prior frame. The
                        // capacity-one wake coalesces with producer-side wakes.
                        protocol_try!('protocol,
                            rearm_event_ready(&event_ready_tx),
                            "ERR_AGENTOS_EVENT_READY_WAKE"
                        );
                    }
                    Some(Ok(Err(error))) => {
                        begin_protocol_transport_failure(
                            format!("ERR_AGENTOS_OUTPUT_TASK: durable event publisher failed: {error}"),
                            &mut drain_state,
                            shutdown_grace,
                            &request_operations,
                            &progress_requests,
                            &callback_transport,
                            &frame_writer,
                            false,
                            &mut stdin_closed,
                            &mut control_ingress_closed,
                            &mut shutdown_ingress_closed,
                            &mut stdin_rx,
                            &mut stdin_control_rx,
                            &mut shutdown_rx,
                        );
                    }
                    Some(Err(error)) => {
                        begin_protocol_transport_failure(
                            format!("ERR_AGENTOS_OUTPUT_TASK: durable event publisher failed: {error}"),
                            &mut drain_state,
                            shutdown_grace,
                            &request_operations,
                            &progress_requests,
                            &callback_transport,
                            &frame_writer,
                            false,
                            &mut stdin_closed,
                            &mut control_ingress_closed,
                            &mut shutdown_ingress_closed,
                            &mut stdin_rx,
                            &mut stdin_control_rx,
                            &mut shutdown_rx,
                        );
                    }
                    None => {}
                }
            }
        }
    }

    let drain = match drain_state {
        Some(drain) => drain,
        None => {
            request_operations.close(OperationCancellationReason::ConnectionClosed);
            progress_requests.close(OperationCancellationReason::ConnectionClosed);
            ProtocolDrainState {
                reason: OperationCancellationReason::ConnectionClosed,
                deadline: tokio::time::Instant::now() + shutdown_grace,
                terminal_error: None,
            }
        }
    };
    while let Some(pending) = pending_owned_process_events.pop_front() {
        pending.cancel_before_schedule(drain.reason);
    }
    let report = finalize_protocol_drain(
        drain.reason,
        shutdown_grace,
        protocol.terminal_fallback_bytes,
        &request_operations,
        &progress_requests,
        &callback_transport,
        &frame_writer,
        &mut extension_service_tasks,
        &mut progress_service_tasks,
        &mut extension_service_completion_rx,
        &mut extension_tasks,
        &mut extension_completion_rx,
        &mut request_tasks,
        &mut request_completion_rx,
        &mut output_tasks,
        &mut ordinary_event_tasks,
    )
    .await;
    tracing::debug!(
        ?report,
        reason = ?drain.reason,
        "protocol drain completed"
    );
    cleanup_connections(
        &mut sidecar,
        &ownership_coordinator,
        &active_connections,
        &mut active_sessions,
    )
    .await;
    if let Some(error) = drain.terminal_error {
        Err(io::Error::new(io::ErrorKind::BrokenPipe, error).into())
    } else {
        Ok(())
    }
}

fn begin_protocol_drain(
    drain_state: &mut Option<ProtocolDrainState>,
    reason: OperationCancellationReason,
    grace: Duration,
    terminal_error: Option<String>,
    operations: &OperationTable,
    progress_requests: &ProgressOperationView,
    close_progress_admission: bool,
) -> bool {
    if let Some(state) = drain_state {
        if close_progress_admission {
            progress_requests.close(reason);
        }
        if state.terminal_error.is_none() {
            state.terminal_error = terminal_error;
        }
        return false;
    }
    let signalled = operations.close(reason);
    let progress_signalled = if close_progress_admission {
        progress_requests.close(reason)
    } else {
        progress_requests.signal_all(reason)
    };
    tracing::debug!(
        ?reason,
        signalled,
        progress_signalled,
        grace_ms = grace.as_millis(),
        close_progress_admission,
        "protocol supervisor entered draining state"
    );
    *drain_state = Some(ProtocolDrainState {
        reason,
        deadline: tokio::time::Instant::now() + grace,
        terminal_error,
    });
    true
}

#[allow(clippy::too_many_arguments)]
fn begin_protocol_transport_failure(
    message: String,
    drain_state: &mut Option<ProtocolDrainState>,
    grace: Duration,
    operations: &OperationTable,
    progress_requests: &ProgressOperationView,
    callback_transport: &FrameSidecarRequestTransport,
    writer: &ProtocolFrameWriter,
    output_failed: bool,
    stdin_closed: &mut bool,
    control_ingress_closed: &mut bool,
    shutdown_ingress_closed: &mut bool,
    stdin_rx: &mut Receiver<Result<Option<AccountedProtocolFrame>, String>>,
    stdin_control_rx: &mut Receiver<AccountedProtocolFrame>,
    shutdown_rx: &mut Receiver<wire::ControlFrame>,
) {
    // Preserve a healthy control lane for forced terminal outcomes when the
    // failure came from routing or task supervision. Only an observed broker
    // disconnect or physical writer failure proves output itself is unusable.
    if output_failed {
        writer.output.close_with_error(message.clone());
    }
    begin_protocol_drain(
        drain_state,
        OperationCancellationReason::TransportClosed,
        grace,
        Some(message.clone()),
        operations,
        progress_requests,
        true,
    );
    if let Err(error) = callback_transport.fail_all(&message) {
        eprintln!(
            "ERR_AGENTOS_PROTOCOL_CALLBACK_DRAIN: failed to release callback waiters after transport failure: {error}"
        );
    }
    *stdin_closed = true;
    *control_ingress_closed = true;
    *shutdown_ingress_closed = true;
    stdin_rx.close();
    stdin_control_rx.close();
    shutdown_rx.close();
}

#[allow(clippy::too_many_arguments)]
async fn finalize_protocol_drain(
    reason: OperationCancellationReason,
    output_grace: Duration,
    terminal_fallback_bytes: usize,
    operations: &OperationTable,
    progress_requests: &ProgressOperationView,
    callback_transport: &FrameSidecarRequestTransport,
    writer: &ProtocolFrameWriter,
    extension_service_tasks: &mut JoinSet<()>,
    progress_service_tasks: &mut JoinSet<()>,
    extension_service_completion_rx: &mut Receiver<CompletedExtensionServiceCommand>,
    extension_tasks: &mut JoinSet<()>,
    extension_completion_rx: &mut Receiver<DetachedExtensionCompletion>,
    request_tasks: &mut JoinSet<()>,
    request_completion_rx: &mut Receiver<DetachedRequestCompletion>,
    output_tasks: &mut JoinSet<Result<(), String>>,
    ordinary_event_tasks: &mut JoinSet<Result<(), String>>,
) -> ProtocolFinalizeReport {
    let forced_terminals = operations.force_terminalize(reason);
    let forced_progress = progress_requests.force_acknowledge(reason);
    let mut report = ProtocolFinalizeReport {
        forced_terminal_responses: forced_terminals.len(),
        forced_progress_acknowledgements: forced_progress.len(),
        ..ProtocolFinalizeReport::default()
    };

    extension_service_tasks.abort_all();
    while extension_service_tasks.join_next().await.is_some() {}
    progress_service_tasks.abort_all();
    while progress_service_tasks.join_next().await.is_some() {}
    while let Ok(completion) = extension_service_completion_rx.try_recv() {
        drop(completion);
    }
    extension_tasks.abort_all();
    while extension_tasks.join_next().await.is_some() {}
    while let Ok(completion) = extension_completion_rx.try_recv() {
        drop(completion);
    }
    request_tasks.abort_all();
    while request_tasks.join_next().await.is_some() {}
    while let Ok(completion) = request_completion_rx.try_recv() {
        drop(completion);
    }
    output_tasks.abort_all();
    while output_tasks.join_next().await.is_some() {}
    ordinary_event_tasks.abort_all();
    while ordinary_event_tasks.join_next().await.is_some() {}

    if let Err(error) = callback_transport.fail_all(&format!(
        "ERR_AGENTOS_PROTOCOL_DRAINED: protocol closed while waiting for a registered sidecar response ({reason:?})"
    )) {
        report.failed_deliveries = report.failed_deliveries.saturating_add(1);
        eprintln!(
            "ERR_AGENTOS_PROTOCOL_CALLBACK_DRAIN: failed to release callback waiters: {error}"
        );
    }

    for outcome in forced_terminals {
        let frame = forced_shutdown_response(&outcome, reason, false);
        let result = writer
            .try_reserve_terminal(terminal_fallback_bytes)
            .map_err(|error| error.to_string());
        match result {
            Ok(reservation) => {
                if let Err(error) = writer.publish_reserved_terminal(reservation, frame).await {
                    report.failed_deliveries = report.failed_deliveries.saturating_add(1);
                    eprintln!(
                        "ERR_AGENTOS_FORCED_TERMINAL_DELIVERY: could not deliver shutdown terminal response for {}:{}: {error}",
                        outcome.key.connection_id, outcome.key.request_id
                    );
                }
            }
            Err(error) => {
                report.failed_deliveries = report.failed_deliveries.saturating_add(1);
                eprintln!(
                    "ERR_AGENTOS_FORCED_TERMINAL_RESERVATION: could not reserve shutdown terminal response for {}:{}: {error}",
                    outcome.key.connection_id, outcome.key.request_id
                );
            }
        }
    }

    let progress_fallback_bytes =
        terminal_fallback_bytes.min(writer.progress_budget.config.max_bytes);
    for outcome in forced_progress {
        let frame = forced_shutdown_response(&outcome, reason, true);
        let result = writer
            .try_reserve_progress(progress_fallback_bytes)
            .map_err(|error| error.to_string());
        match result {
            Ok(reservation) => {
                if let Err(error) = writer.publish_reserved_progress(reservation, frame).await {
                    report.failed_deliveries = report.failed_deliveries.saturating_add(1);
                    eprintln!(
                        "ERR_AGENTOS_FORCED_PROGRESS_DELIVERY: could not deliver shutdown progress acknowledgement for {}:{}: {error}",
                        outcome.key.connection_id, outcome.key.request_id
                    );
                }
            }
            Err(error) => {
                report.failed_deliveries = report.failed_deliveries.saturating_add(1);
                eprintln!(
                    "ERR_AGENTOS_FORCED_PROGRESS_RESERVATION: could not reserve shutdown progress acknowledgement for {}:{}: {error}",
                    outcome.key.connection_id, outcome.key.request_id
                );
            }
        }
    }

    report.control_drained = wait_for_control_output_drain(writer, output_grace).await;
    if !report.control_drained {
        eprintln!(
            "ERR_AGENTOS_PROTOCOL_CONTROL_DRAIN_TIMEOUT: control output did not drain within {}ms; terminal={:?} progress={:?} rejection={:?} observability={:?}",
            output_grace.as_millis(),
            writer.terminal_budget.usage(),
            writer.progress_budget.usage(),
            writer.rejection_budget.usage(),
            writer.control_observability_budget.usage(),
        );
    }
    writer.output.close_with_error(format!(
        "ERR_AGENTOS_PROTOCOL_DRAINED: protocol output closed after {reason:?}"
    ));

    let operation_snapshot = operations.snapshot();
    let progress_snapshot = progress_requests.snapshot();
    if operation_snapshot.in_flight_requests != 0
        || operation_snapshot.in_flight_request_bytes != 0
        || progress_snapshot.in_flight_requests != 0
        || progress_snapshot.in_flight_request_bytes != 0
    {
        report.failed_deliveries = report.failed_deliveries.saturating_add(1);
        eprintln!(
            "ERR_AGENTOS_PROTOCOL_DRAIN_ACCOUNTING: operation registry was not empty after forced drain: ordinary={operation_snapshot:?} progress={progress_snapshot:?}"
        );
    }

    report
}

fn forced_shutdown_response(
    outcome: &ForcedRequestOutcome,
    reason: OperationCancellationReason,
    progress: bool,
) -> ProtocolFrame {
    let (code, operation) = if progress {
        ("ERR_AGENTOS_PROGRESS_SHUTDOWN", "stdio.progressDrain")
    } else {
        ("ERR_AGENTOS_REQUEST_SHUTDOWN", "stdio.requestDrain")
    };
    ProtocolFrame::ResponseFrame(response_frame(
        outcome.key.request_id,
        outcome.ownership.clone(),
        ResponsePayload::RejectedResponse(wire::RejectedResponse {
            code: code.to_owned(),
            message: format!(
                "request did not complete before the bounded protocol drain ended ({reason:?})"
            ),
            limit_name: None,
            configured_limit: None,
            current_usage: None,
            requested: Some(1),
            unit: Some(String::from("requests")),
            scope: Some(String::from("connection")),
            vm_id: None,
            session_generation: None,
            capability_id: None,
            operation: Some(operation.to_owned()),
            configuration_path: Some(String::from("runtime.protocol.shutdownGraceMs")),
            retryable: Some(false),
            errno: Some(String::from("ESHUTDOWN")),
        }),
    ))
}

async fn wait_for_control_output_drain(writer: &ProtocolFrameWriter, grace: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + grace;
    loop {
        let terminal_changed = writer.terminal_budget.changed.notified();
        let progress_changed = writer.progress_budget.changed.notified();
        let rejection_changed = writer.rejection_budget.changed.notified();
        let observability_changed = writer.control_observability_budget.changed.notified();
        tokio::pin!(terminal_changed);
        tokio::pin!(progress_changed);
        tokio::pin!(rejection_changed);
        tokio::pin!(observability_changed);
        terminal_changed.as_mut().enable();
        progress_changed.as_mut().enable();
        rejection_changed.as_mut().enable();
        observability_changed.as_mut().enable();

        if !writer.output.is_open() {
            return false;
        }
        if writer.terminal_budget.usage().0 == 0
            && writer.progress_budget.usage().0 == 0
            && writer.rejection_budget.usage().0 == 0
            && writer.control_observability_budget.usage().0 == 0
        {
            return true;
        }

        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => return false,
            _ = terminal_changed.as_mut() => {}
            _ = progress_changed.as_mut() => {}
            _ = rejection_changed.as_mut() => {}
            _ = observability_changed.as_mut() => {}
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn route_protocol_frame(
    accounted_frame: AccountedProtocolFrame,
    sidecar: &mut NativeSidecar<LocalBridge>,
    services: &Arc<dyn ExtensionServices>,
    operations: &OperationTable,
    progress_requests: &ProgressOperationView,
    ownership_coordinator: &OwnershipCoordinator,
    completion_tx: &Sender<DetachedExtensionCompletion>,
    request_completion_tx: &Sender<DetachedRequestCompletion>,
    extension_tasks: &mut JoinSet<()>,
    request_tasks: &mut JoinSet<()>,
    output_tasks: &mut JoinSet<Result<(), String>>,
    write_tx: &ProtocolFrameWriter,
    terminal_fallback_bytes: usize,
    active_sessions: &mut BTreeSet<SessionScope>,
    active_connections: &mut BTreeSet<String>,
) -> Result<(), Box<dyn Error>> {
    let AccountedProtocolFrame {
        frame,
        _reservation: ingress_reservation,
    } = accounted_frame;
    match frame {
        ProtocolFrame::RequestFrame(request) => {
            let class = extension_request_class(
                &ProtocolFrame::RequestFrame(request.clone()),
                &sidecar.extensions,
            );
            let request_bytes = ingress_reservation.bytes;
            let (operation, progress_request, output_reservation) = if class
                == ExtensionRequestClass::Progress
            {
                let key = RequestOperationKey::new(
                    ownership_connection_id(&request.ownership),
                    request.request_id,
                );
                let progress_request = match progress_requests.admit_owned(
                    key,
                    request.ownership.clone(),
                    request_bytes,
                ) {
                    Ok(progress_request) => progress_request,
                    Err(error) => {
                        publish_progress_admission_rejection(write_tx, request, &error)?;
                        return Ok(());
                    }
                };
                let progress_fallback_bytes =
                    terminal_fallback_bytes.min(write_tx.progress_budget.config.max_bytes);
                let reservation = match write_tx.try_reserve_progress(progress_fallback_bytes) {
                    Ok(reservation) => reservation,
                    Err(error) => {
                        drop(progress_request);
                        publish_request_rejection(
                            write_tx,
                            request,
                            "ERR_AGENTOS_PROGRESS_RESERVATION_LIMIT",
                            &format!(
                                "progress response reservation unavailable: {error}; raise runtime.protocol.maxProgressFrames or runtime.protocol.maxProgressBytes"
                            ),
                            Some("runtime.protocol.maxProgressFrames"),
                        )?;
                        return Ok(());
                    }
                };
                (None, Some(progress_request), reservation)
            } else {
                let metadata = request_operation_metadata(&request, &sidecar.extensions);
                let key = RequestOperationKey::new(
                    ownership_connection_id(&request.ownership),
                    request.request_id,
                );
                let operation = match operations.admit(key, metadata, request_bytes) {
                    Ok(operation) => operation,
                    Err(error) => {
                        publish_request_admission_rejection(write_tx, request, &error)?;
                        return Ok(());
                    }
                };
                let terminal_reservation = match write_tx
                    .try_reserve_terminal(terminal_fallback_bytes)
                {
                    Ok(reservation) => reservation,
                    Err(error) => {
                        drop(operation);
                        publish_request_rejection(
                            write_tx,
                            request,
                            "ERR_AGENTOS_TERMINAL_RESERVATION_LIMIT",
                            &format!(
                                "terminal response reservation unavailable: {error}; raise runtime.protocol.maxTerminalFrames or runtime.protocol.maxTerminalBytes"
                            ),
                            Some("runtime.protocol.maxTerminalFrames"),
                        )?;
                        return Ok(());
                    }
                };
                (Some(operation), None, terminal_reservation)
            };

            let prepared = match sidecar
                .prepare_extension_request_wire(request.clone(), Arc::clone(services))
            {
                Ok(prepared) => prepared,
                Err(error) => {
                    let dispatch = sidecar.reject_wire_request_error(request, &error)?;
                    schedule_dispatch_output(
                        dispatch,
                        class,
                        operation,
                        progress_request,
                        output_reservation,
                        true,
                        write_tx,
                        output_tasks,
                        active_sessions,
                        active_connections,
                    )?;
                    flush_sidecar_requests(sidecar, write_tx, output_tasks)?;
                    return Ok(());
                }
            };

            if let Some(prepared) = prepared {
                let completion_tx = completion_tx.clone();
                let ownership_coordinator = ownership_coordinator.clone();
                extension_tasks.spawn_local(async move {
                    let coordinator_admission = match operation.as_ref() {
                        Some(operation) => ownership_coordinator
                            .admit(operation.metadata(), operation.cancellation())
                            .await
                            .map(Some),
                        None => Ok(None),
                    };
                    let coordinator_permit = match coordinator_admission {
                        Ok(permit) => permit,
                        Err(error) => {
                            let completion = DetachedExtensionCompletion {
                                request,
                                class,
                                operation,
                                progress_request,
                                coordinator_permit: None,
                                output_reservation,
                                result: Err(coordinator_admission_error(error)),
                            };
                            if completion_tx.send(completion).await.is_err() {
                                tracing::error!(
                                    "ERR_AGENTOS_REQUEST_COMPLETION_CLOSED: protocol router stopped before extension admission failure could be reported"
                                );
                            }
                            return;
                        }
                    };
                    let task = tokio::task::spawn_local(prepared.execute());
                    let result = match task.await {
                        Ok(completed) => Ok(completed),
                        Err(error) => Err(SidecarError::Execution(format!(
                            "ERR_AGENTOS_REQUEST_TASK_PANIC: extension request task failed: {error}"
                        ))),
                    };
                    let completion = DetachedExtensionCompletion {
                        request,
                        class,
                        operation,
                        progress_request,
                        coordinator_permit,
                        output_reservation,
                        result,
                    };
                    if completion_tx.send(completion).await.is_err() {
                        tracing::error!(
                            "ERR_AGENTOS_REQUEST_COMPLETION_CLOSED: protocol router stopped before extension completion could be reported"
                        );
                    }
                });
            } else {
                if matches!(request.payload, RequestPayload::CreateVmRequest(_)) {
                    let compat_request =
                        wire::request_frame_to_compat(request.clone()).map_err(|error| {
                            SidecarError::InvalidState(format!(
                                "invalid generated CreateVm request: {error}"
                            ))
                        })?;
                    let crate::protocol::RequestPayload::CreateVm(payload) =
                        compat_request.payload.clone()
                    else {
                        unreachable!("wire CreateVm converted to a different compatibility route")
                    };
                    let operation = operation.expect("CreateVm request has operation admission");
                    let prepared = match sidecar.prepare_create_vm(&compat_request, payload) {
                        Ok(prepared) => prepared,
                        Err(error) => {
                            let dispatch =
                                sidecar.reject_wire_request_error(request.clone(), &error)?;
                            schedule_dispatch_output(
                                dispatch,
                                class,
                                Some(operation),
                                progress_request,
                                output_reservation,
                                true,
                                write_tx,
                                output_tasks,
                                active_sessions,
                                active_connections,
                            )?;
                            flush_sidecar_requests(sidecar, write_tx, output_tasks)?;
                            return Ok(());
                        }
                    };
                    schedule_prepared_create_vm(
                        prepared,
                        request,
                        operation,
                        ownership_coordinator.clone(),
                        output_reservation,
                        request_completion_tx,
                        request_tasks,
                    );
                    flush_sidecar_requests(sidecar, write_tx, output_tasks)?;
                    return Ok(());
                }

                if matches!(request.payload, RequestPayload::DisposeVmRequest(_)) {
                    let compat_request =
                        wire::request_frame_to_compat(request.clone()).map_err(|error| {
                            SidecarError::InvalidState(format!(
                                "invalid generated DisposeVm request: {error}"
                            ))
                        })?;
                    let crate::protocol::RequestPayload::DisposeVm(payload) =
                        compat_request.payload.clone()
                    else {
                        unreachable!("wire DisposeVm converted to a different compatibility route")
                    };
                    let cancellation_reason = match payload.reason {
                        crate::protocol::DisposeReason::Requested => {
                            OperationCancellationReason::Explicit
                        }
                        crate::protocol::DisposeReason::ConnectionClosed => {
                            OperationCancellationReason::ConnectionClosed
                        }
                        crate::protocol::DisposeReason::HostShutdown => {
                            OperationCancellationReason::Shutdown
                        }
                    };
                    let operation = operation.expect("DisposeVm request has operation admission");
                    let plan = match sidecar.prepare_dispose_vm(&compat_request, payload) {
                        Ok(plan) => plan,
                        Err(error) => {
                            let dispatch =
                                sidecar.reject_wire_request_error(request.clone(), &error)?;
                            schedule_dispatch_output(
                                dispatch,
                                class,
                                Some(operation),
                                progress_request,
                                output_reservation,
                                true,
                                write_tx,
                                output_tasks,
                                active_sessions,
                                active_connections,
                            )?;
                            flush_sidecar_requests(sidecar, write_tx, output_tasks)?;
                            return Ok(());
                        }
                    };
                    let disposal = match ownership_coordinator.begin_vm_disposal_with_operations(
                        &request.ownership,
                        cancellation_reason,
                        operations,
                        operation.key(),
                    ) {
                        Ok(disposal) => disposal,
                        Err(error) => {
                            let dispatch = sidecar.reject_wire_request_error(
                                request.clone(),
                                &coordinator_admission_error(error),
                            )?;
                            schedule_dispatch_output(
                                dispatch,
                                class,
                                Some(operation),
                                progress_request,
                                output_reservation,
                                true,
                                write_tx,
                                output_tasks,
                                active_sessions,
                                active_connections,
                            )?;
                            flush_sidecar_requests(sidecar, write_tx, output_tasks)?;
                            return Ok(());
                        }
                    };
                    schedule_prepared_dispose_vm(
                        plan,
                        disposal,
                        request,
                        operation,
                        output_reservation,
                        request_completion_tx,
                        request_tasks,
                    );
                    flush_sidecar_requests(sidecar, write_tx, output_tasks)?;
                    return Ok(());
                }

                if let Some(prepared) = sidecar.prepare_request_wire(request.clone())? {
                    if let Some(membership) = prepared.committed_membership() {
                        sidecar.commit_prepared_membership(membership)?;
                        if let Err(error) = commit_prepared_membership(
                            ownership_coordinator,
                            membership,
                            active_sessions,
                            active_connections,
                        ) {
                            let dispatch = sidecar.reject_wire_request_error(
                                request.clone(),
                                &SidecarError::InvalidState(error.to_string()),
                            )?;
                            schedule_dispatch_output(
                                dispatch,
                                class,
                                operation,
                                progress_request,
                                output_reservation,
                                true,
                                write_tx,
                                output_tasks,
                                active_sessions,
                                active_connections,
                            )?;
                            flush_sidecar_requests(sidecar, write_tx, output_tasks)?;
                            return Err(error.into());
                        }
                    }
                    let operation =
                        operation.expect("ordinary prepared request has operation admission");
                    let coordinate =
                        !matches!(request.payload, RequestPayload::AuthenticateRequest(_));
                    schedule_prepared_request(
                        prepared,
                        request,
                        operation,
                        coordinate,
                        ownership_coordinator.clone(),
                        output_reservation,
                        request_completion_tx,
                        request_tasks,
                    );
                    flush_sidecar_requests(sidecar, write_tx, output_tasks)?;
                    return Ok(());
                }
                let error = SidecarError::InvalidState(String::from(
                    "ERR_AGENTOS_UNPREPARED_REQUEST_ROUTE: non-extension request did not produce owned request work",
                ));
                let dispatch = sidecar.reject_wire_request_error(request.clone(), &error)?;
                schedule_dispatch_output(
                    dispatch,
                    class,
                    operation,
                    progress_request,
                    output_reservation,
                    true,
                    write_tx,
                    output_tasks,
                    active_sessions,
                    active_connections,
                )?;
            }
            flush_sidecar_requests(sidecar, write_tx, output_tasks)?;
        }
        ProtocolFrame::SidecarResponseFrame(response) => {
            sidecar.accept_wire_sidecar_response(response)?;
            flush_sidecar_requests(sidecar, write_tx, output_tasks)?;
        }
        other => {
            return Err(format!(
                "expected request or sidecar_response frame on stdin, received {}",
                frame_kind(&other)
            )
            .into());
        }
    }
    // Drop any sessions the sidecar disposed while handling this frame from the
    // active-session set so the event pump stops iterating dead sessions (M5).
    untrack_disposed_sessions(&sidecar.take_disposed_sessions(), active_sessions);
    Ok(())
}

fn reap_protocol_tasks_nowait(
    extension_service_tasks: &mut JoinSet<()>,
    progress_service_tasks: &mut JoinSet<()>,
    extension_tasks: &mut JoinSet<()>,
    request_tasks: &mut JoinSet<()>,
    output_tasks: &mut JoinSet<Result<(), String>>,
    ordinary_event_tasks: &mut JoinSet<Result<(), String>>,
    event_ready_tx: &Sender<()>,
) -> Result<(), Box<dyn Error>> {
    reap_unit_tasks_nowait(
        extension_service_tasks,
        "ERR_AGENTOS_EXTENSION_SERVICE_SUPERVISOR_TASK",
    )?;
    reap_unit_tasks_nowait(
        progress_service_tasks,
        "ERR_AGENTOS_PROGRESS_SERVICE_SUPERVISOR_TASK",
    )?;
    reap_unit_tasks_nowait(
        extension_tasks,
        "ERR_AGENTOS_REQUEST_SUPERVISOR_TASK: extension",
    )?;
    reap_unit_tasks_nowait(
        request_tasks,
        "ERR_AGENTOS_REQUEST_SUPERVISOR_TASK: generic",
    )?;
    reap_output_tasks_nowait(output_tasks, false, event_ready_tx)?;
    reap_output_tasks_nowait(ordinary_event_tasks, true, event_ready_tx)?;
    Ok(())
}

fn reap_unit_tasks_nowait(
    tasks: &mut JoinSet<()>,
    error_code: &'static str,
) -> Result<(), Box<dyn Error>> {
    while let Some(result) = tasks.try_join_next() {
        if let Err(error) = result {
            return Err(io::Error::other(format!(
                "{error_code}: completion monitor failed: {error}"
            ))
            .into());
        }
    }
    Ok(())
}

fn reap_output_tasks_nowait(
    tasks: &mut JoinSet<Result<(), String>>,
    rearm_durable_events: bool,
    event_ready_tx: &Sender<()>,
) -> Result<(), Box<dyn Error>> {
    while let Some(result) = tasks.try_join_next() {
        match result {
            Ok(Ok(())) => {
                if rearm_durable_events {
                    rearm_event_ready(event_ready_tx)?;
                }
            }
            Ok(Err(error)) => {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, error).into());
            }
            Err(error) => {
                return Err(io::Error::other(format!(
                    "ERR_AGENTOS_OUTPUT_TASK: output publisher failed: {error}"
                ))
                .into());
            }
        }
    }
    Ok(())
}

fn schedule_prepared_request(
    prepared: PreparedRequest,
    request: RequestFrame,
    operation: RequestOperation,
    coordinate: bool,
    ownership_coordinator: OwnershipCoordinator,
    output_reservation: ProtocolReservation,
    request_completion_tx: &Sender<DetachedRequestCompletion>,
    request_tasks: &mut JoinSet<()>,
) {
    let request_completion_tx = request_completion_tx.clone();
    request_tasks.spawn_local(async move {
        let coordinator_admission = if coordinate {
            ownership_coordinator
                .admit(operation.metadata(), operation.cancellation())
                .await
                .map(Some)
        } else {
            Ok(None)
        };
        let coordinator_permit = match coordinator_admission {
            Ok(permit) => permit,
            Err(error) => {
                let completion = DetachedRequestCompletion {
                    request,
                    operation,
                    coordinator_permit: None,
                    output_reservation,
                    result: DetachedRequestResult::Generic(Err(coordinator_admission_error(
                        error,
                    ))),
                };
                if request_completion_tx.send(completion).await.is_err() {
                    tracing::error!(
                        "ERR_AGENTOS_REQUEST_COMPLETION_CLOSED: protocol router stopped before generic admission failure could be reported"
                    );
                }
                return;
            }
        };
        // This is a lightweight ownership/lifecycle registration, not a
        // mutable state guard. Retaining it through terminal completion lets
        // teardown cancel and drain the operation without serializing ordinary
        // requests or holding `VmState` across an external wait.
        let cancellation = operation.cancellation();
        let mut task = tokio::task::spawn_local(prepared.execute());
        let result = tokio::select! {
            result = &mut task => match result {
                Ok(completed) => Ok(completed),
                Err(error) => Err(SidecarError::Execution(format!(
                    "ERR_AGENTOS_REQUEST_TASK_PANIC: generic request task failed: {error}"
                ))),
            },
            reason = cancellation.cancelled() => {
                task.abort();
                match task.await {
                    Ok(_) => {}
                    Err(error) if error.is_cancelled() => {}
                    Err(error) => {
                        tracing::error!(
                            "ERR_AGENTOS_REQUEST_ABORT: cancelled generic request task failed while stopping: {error}"
                        );
                    }
                }
                Err(SidecarError::InvalidState(format!(
                    "ERR_AGENTOS_REQUEST_CANCELLED: generic request was cancelled: {reason:?}"
                )))
            }
        };
        let completion = DetachedRequestCompletion {
            request,
            operation,
            coordinator_permit,
            output_reservation,
            result: DetachedRequestResult::Generic(result),
        };
        if request_completion_tx.send(completion).await.is_err() {
            tracing::error!(
                "ERR_AGENTOS_REQUEST_COMPLETION_CLOSED: protocol router stopped before generic completion could be reported"
            );
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn schedule_prepared_create_vm(
    prepared: PreparedCreateVm<LocalBridge>,
    request: RequestFrame,
    operation: RequestOperation,
    ownership_coordinator: OwnershipCoordinator,
    output_reservation: ProtocolReservation,
    request_completion_tx: &Sender<DetachedRequestCompletion>,
    request_tasks: &mut JoinSet<()>,
) {
    let request_completion_tx = request_completion_tx.clone();
    request_tasks.spawn_local(async move {
        let coordinator_permit = match ownership_coordinator
            .admit(operation.metadata(), operation.cancellation())
            .await
        {
            Ok(permit) => Some(permit),
            Err(error) => {
                let completion = DetachedRequestCompletion {
                    request,
                    operation,
                    coordinator_permit: None,
                    output_reservation,
                    result: DetachedRequestResult::Create(Err(coordinator_admission_error(error))),
                };
                if request_completion_tx.send(completion).await.is_err() {
                    tracing::error!(
                        "ERR_AGENTOS_REQUEST_COMPLETION_CLOSED: protocol router stopped before CreateVm admission failure could be reported"
                    );
                }
                return;
            }
        };
        let task = tokio::task::spawn_local(prepared.execute());
        let result = match task.await {
            Ok(result) => result,
            Err(error) => Err(SidecarError::Execution(format!(
                "ERR_AGENTOS_REQUEST_TASK_PANIC: CreateVm task failed: {error}"
            ))),
        };
        let completion = DetachedRequestCompletion {
            request,
            operation,
            coordinator_permit,
            output_reservation,
            result: DetachedRequestResult::Create(result),
        };
        if request_completion_tx.send(completion).await.is_err() {
            tracing::error!(
                "ERR_AGENTOS_REQUEST_COMPLETION_CLOSED: protocol router stopped before CreateVm completion could be reported"
            );
        }
    });
}

fn schedule_prepared_dispose_vm(
    plan: DisposeVmPlan<LocalBridge>,
    disposal: VmDisposal,
    request: RequestFrame,
    operation: RequestOperation,
    output_reservation: ProtocolReservation,
    request_completion_tx: &Sender<DetachedRequestCompletion>,
    request_tasks: &mut JoinSet<()>,
) {
    let request_completion_tx = request_completion_tx.clone();
    request_tasks.spawn_local(async move {
        disposal.wait_drained().await;
        let completion = DetachedRequestCompletion {
            request,
            operation,
            coordinator_permit: None,
            output_reservation,
            result: DetachedRequestResult::DisposeDrained { plan, disposal },
        };
        if request_completion_tx.send(completion).await.is_err() {
            tracing::error!(
                "ERR_AGENTOS_REQUEST_COMPLETION_CLOSED: protocol router stopped before DisposeVm drain completion could be reported"
            );
        }
    });
}

fn schedule_prepared_dispose_execution(
    prepared: PreparedDisposeVm<LocalBridge>,
    disposal: VmDisposal,
    request: RequestFrame,
    operation: RequestOperation,
    output_reservation: ProtocolReservation,
    request_completion_tx: &Sender<DetachedRequestCompletion>,
    request_tasks: &mut JoinSet<()>,
) {
    let request_completion_tx = request_completion_tx.clone();
    request_tasks.spawn_local(async move {
        let task = tokio::task::spawn_local(prepared.execute());
        let result = match task.await {
            Ok(completed) => Ok(completed),
            Err(error) => Err(SidecarError::Execution(format!(
                "ERR_AGENTOS_REQUEST_TASK_PANIC: DisposeVm task failed: {error}"
            ))),
        };
        let completion = DetachedRequestCompletion {
            request,
            operation,
            coordinator_permit: None,
            output_reservation,
            result: DetachedRequestResult::DisposeExecuted { result, disposal },
        };
        if request_completion_tx.send(completion).await.is_err() {
            tracing::error!(
                "ERR_AGENTOS_REQUEST_COMPLETION_CLOSED: protocol router stopped before DisposeVm teardown completion could be reported"
            );
        }
    });
}

fn schedule_extension_service_command(
    prepared: PreparedExtensionServiceCommand,
    completion_tx: &Sender<CompletedExtensionServiceCommand>,
    tasks: &mut JoinSet<()>,
) {
    let completion_tx = completion_tx.clone();
    tasks.spawn_local(async move {
        let operation = prepared.operation();
        let Some(completion) = prepared.execute_supervised().await else {
            return;
        };
        if completion_tx.send(completion).await.is_err() {
            tracing::error!(
                operation,
                "ERR_AGENTOS_EXTENSION_SERVICE_COMPLETION_CLOSED: protocol router stopped before extension service completion could be applied"
            );
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn finish_extension_request(
    completion: DetachedExtensionCompletion,
    sidecar: &NativeSidecar<LocalBridge>,
    write_tx: &ProtocolFrameWriter,
    output_tasks: &mut JoinSet<Result<(), String>>,
    active_sessions: &mut BTreeSet<SessionScope>,
    active_connections: &mut BTreeSet<String>,
) -> Result<(), Box<dyn Error>> {
    let DetachedExtensionCompletion {
        request,
        class,
        operation,
        progress_request,
        coordinator_permit,
        output_reservation,
        result,
    } = completion;
    let (dispatch, failed) = match result {
        Ok(completed) => {
            let failed = completed.failed();
            match sidecar.complete_extension_request(completed) {
                Ok(dispatch) => (dispatch, failed),
                Err(error) => (
                    sidecar.reject_wire_request_error(request.clone(), &error)?,
                    true,
                ),
            }
        }
        Err(error) => (
            sidecar.reject_wire_request_error(request.clone(), &error)?,
            true,
        ),
    };
    drop(coordinator_permit);
    schedule_dispatch_output(
        dispatch,
        class,
        operation,
        progress_request,
        output_reservation,
        failed,
        write_tx,
        output_tasks,
        active_sessions,
        active_connections,
    )
}

#[allow(clippy::too_many_arguments)]
fn finish_request(
    completion: DetachedRequestCompletion,
    sidecar: &mut NativeSidecar<LocalBridge>,
    ownership_coordinator: &OwnershipCoordinator,
    request_completion_tx: &Sender<DetachedRequestCompletion>,
    request_tasks: &mut JoinSet<()>,
    write_tx: &ProtocolFrameWriter,
    output_tasks: &mut JoinSet<Result<(), String>>,
    active_sessions: &mut BTreeSet<SessionScope>,
    active_connections: &mut BTreeSet<String>,
) -> Result<(), Box<dyn Error>> {
    let DetachedRequestCompletion {
        request,
        operation,
        coordinator_permit,
        output_reservation,
        result,
    } = completion;
    let (dispatch, failed, update_membership) = match result {
        DetachedRequestResult::Generic(Ok(completed)) => {
            let failed = completed.failed();
            match sidecar.complete_request(completed) {
                Ok(dispatch) => (dispatch, failed, !failed),
                Err(error) => (
                    sidecar.reject_wire_request_error(request.clone(), &error)?,
                    true,
                    false,
                ),
            }
        }
        DetachedRequestResult::Generic(Err(error)) => (
            sidecar.reject_wire_request_error(request.clone(), &error)?,
            true,
            false,
        ),
        DetachedRequestResult::Create(result) => match result
            .and_then(|completed| sidecar.complete_create_vm(completed))
            .and_then(crate::service::wire_dispatch_result)
        {
            Ok(dispatch) => (dispatch, false, true),
            Err(error) => (
                sidecar.reject_wire_request_error(request.clone(), &error)?,
                true,
                false,
            ),
        },
        DetachedRequestResult::DisposeDrained { plan, disposal } => {
            match sidecar.detach_vm_for_disposal(plan) {
                Ok(prepared) => {
                    schedule_prepared_dispose_execution(
                        prepared,
                        disposal,
                        request,
                        operation,
                        output_reservation,
                        request_completion_tx,
                        request_tasks,
                    );
                    return Ok(());
                }
                Err(error) => {
                    // Detachment is the boundary after which it is safe to
                    // close coordinator membership. Keep the VM Closing on a
                    // failed detach so it cannot be admitted concurrently.
                    drop(disposal);
                    (
                        sidecar.reject_wire_request_error(request.clone(), &error)?,
                        true,
                        false,
                    )
                }
            }
        }
        DetachedRequestResult::DisposeExecuted { result, disposal } => {
            let central_result = match result {
                Ok(completed) => sidecar
                    .complete_dispose_vm(completed)
                    .and_then(crate::service::wire_dispatch_result),
                Err(error) => {
                    if let OwnershipScope::VmOwnership(scope) = &request.ownership {
                        sidecar.reclaim_vm_tracking(&scope.session_id, &scope.vm_id);
                        if let Err(cleanup_error) =
                            sidecar.bridge.clear_vm_permissions(&scope.vm_id)
                        {
                            eprintln!(
                                "ERR_AGENTOS_VM_DISPOSE_PANIC_CLEANUP: vm_id={} error={cleanup_error}",
                                scope.vm_id
                            );
                        }
                    }
                    Err(error)
                }
            };
            // Central reclamation has run (or the detached task panicked and
            // its fail-closed cleanup above ran). Coordinator closure must not
            // be skipped merely because teardown reported an error.
            let coordinator_result = disposal.complete().map_err(|error| {
                SidecarError::InvalidState(format!("ERR_AGENTOS_VM_COORDINATOR_DISPOSAL: {error}"))
            });
            let result = match (central_result, coordinator_result) {
                (Ok(dispatch), Ok(())) => Ok(dispatch),
                (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
                (Err(error), Err(coordinator_error)) => {
                    eprintln!(
                        "ERR_AGENTOS_VM_COORDINATOR_DISPOSAL_AFTER_TEARDOWN: {coordinator_error}"
                    );
                    Err(error)
                }
            };
            match result {
                Ok(dispatch) => (dispatch, false, false),
                Err(error) => (
                    sidecar.reject_wire_request_error(request.clone(), &error)?,
                    true,
                    false,
                ),
            }
        }
    };
    if update_membership {
        update_ownership_membership(
            ownership_coordinator,
            &operation,
            &request,
            &dispatch.response.payload,
        )?;
    }
    drop(coordinator_permit);
    schedule_dispatch_output(
        dispatch,
        ExtensionRequestClass::Ordinary,
        Some(operation),
        None,
        output_reservation,
        failed,
        write_tx,
        output_tasks,
        active_sessions,
        active_connections,
    )
}

#[allow(clippy::too_many_arguments)]
fn schedule_dispatch_output(
    dispatch: WireDispatchResult,
    class: ExtensionRequestClass,
    operation: Option<RequestOperation>,
    progress_request: Option<ProgressRequest>,
    output_reservation: ProtocolReservation,
    _failed: bool,
    write_tx: &ProtocolFrameWriter,
    output_tasks: &mut JoinSet<Result<(), String>>,
    active_sessions: &mut BTreeSet<SessionScope>,
    active_connections: &mut BTreeSet<String>,
) -> Result<(), Box<dyn Error>> {
    track_session_state(
        &dispatch.response.payload,
        active_sessions,
        active_connections,
    );
    let writer = write_tx.clone();
    output_tasks.spawn_local(async move {
        let mut operation = operation;
        let mut progress_request = progress_request;
        if class == ExtensionRequestClass::Progress {
            let progress_request = progress_request.as_ref().ok_or_else(|| {
                String::from("progress request completion lost its admission reservation")
            })?;
            if !progress_request.try_acknowledge() {
                return Err(String::from(
                    "ERR_AGENTOS_DUPLICATE_PROGRESS_ACKNOWLEDGEMENT: progress request acknowledgement was already claimed",
                ));
            }
            writer
                .publish_reserved_progress_for_request(
                    output_reservation,
                    ProtocolFrame::ResponseFrame(dispatch.response),
                    progress_request,
                )
                .await
                .map_err(|error| error.to_string())?;
        } else {
            let operation = operation.as_ref().ok_or_else(|| {
                String::from("ordinary request completion lost its operation reservation")
            })?;
            if !operation.try_mark_terminal() {
                return Err(String::from(
                    "ERR_AGENTOS_DUPLICATE_TERMINAL_RESPONSE: request terminal response was already claimed",
                ));
            }
            writer
                .publish_reserved_terminal_for_operation(
                    output_reservation,
                    ProtocolFrame::ResponseFrame(dispatch.response),
                    operation,
                )
                .await
                .map_err(|error| error.to_string())?;
        }
        for event in dispatch.events {
            writer
                .publish(
                    ProtocolOutputClass::Ordinary,
                    ProtocolFrame::EventFrame(event),
                )
                .await
                .map_err(|error| error.to_string())?;
        }
        if class == ExtensionRequestClass::Progress {
            progress_request
                .take()
                .expect("progress request validated before output publication")
                .release();
        } else {
            operation
                .take()
                .expect("ordinary request validated before output publication")
                .release();
        }
        Ok(())
    });
    Ok(())
}

fn schedule_output_frame(
    writer: &ProtocolFrameWriter,
    output_tasks: &mut JoinSet<Result<(), String>>,
    class: ProtocolOutputClass,
    frame: ProtocolFrame,
) {
    let writer = writer.clone();
    output_tasks.spawn_local(async move {
        writer
            .publish(class, frame)
            .await
            .map_err(|error| error.to_string())
    });
}

fn schedule_durable_event_frame(
    writer: &ProtocolFrameWriter,
    output_tasks: &mut JoinSet<Result<(), String>>,
    frame: wire::EventFrame,
) -> bool {
    if !output_tasks.is_empty() {
        return false;
    }
    schedule_output_frame(
        writer,
        output_tasks,
        ProtocolOutputClass::Ordinary,
        ProtocolFrame::EventFrame(frame),
    );
    true
}

fn rearm_event_ready(sender: &Sender<()>) -> Result<(), io::Error> {
    match sender.try_send(()) {
        Ok(()) | Err(tokio::sync::mpsc::error::TrySendError::Full(())) => Ok(()),
        Err(tokio::sync::mpsc::error::TrySendError::Closed(())) => Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "event-ready wake receiver closed",
        )),
    }
}

fn request_operation_metadata(
    request: &RequestFrame,
    _extensions: &BTreeMap<String, Arc<dyn Extension>>,
) -> RequestOperationMetadata {
    // Lifecycle requests mutate or require a stable view of VM-wide state, so
    // they use the exclusive gate. Every other core VM request is shared by
    // default; this is deliberately not a per-request ordering matrix.
    let vm_lifecycle = matches!(
        &request.payload,
        RequestPayload::DisposeVmRequest(_)
            | RequestPayload::BootstrapRootFilesystemRequest(_)
            | RequestPayload::ConfigureVmRequest(_)
            | RequestPayload::CreateLayerRequest
            | RequestPayload::SealLayerRequest(_)
            | RequestPayload::ImportSnapshotRequest(_)
            | RequestPayload::ExportSnapshotRequest(_)
            | RequestPayload::CreateOverlayRequest(_)
            | RequestPayload::SnapshotRootFilesystemRequest(_)
            | RequestPayload::LinkPackageRequest(_)
    );
    let vm_concurrency = match &request.payload {
        // Extension payloads are opaque to the core. ACP and other extensions
        // own any protocol-specific conflict state in their route handlers.
        RequestPayload::ExtEnvelope(_) => VmConcurrencyClass::OwnershipOnly,
        _ => match &request.ownership {
            OwnershipScope::ConnectionOwnership(_) | OwnershipScope::SessionOwnership(_) => {
                VmConcurrencyClass::OwnershipOnly
            }
            OwnershipScope::VmOwnership(_) if vm_lifecycle => {
                VmConcurrencyClass::ExclusiveVmLifecycle
            }
            OwnershipScope::VmOwnership(_) => VmConcurrencyClass::SharedVm,
        },
    };
    let operation = match &request.payload {
        RequestPayload::ExtEnvelope(envelope) => format!("extension:{}", envelope.namespace),
        _ => String::from("sidecar_request"),
    };
    RequestOperationMetadata::new(request.ownership.clone(), operation, vm_concurrency)
}

fn ownership_connection_id(ownership: &OwnershipScope) -> &str {
    match ownership {
        OwnershipScope::ConnectionOwnership(scope) => &scope.connection_id,
        OwnershipScope::SessionOwnership(scope) => &scope.connection_id,
        OwnershipScope::VmOwnership(scope) => &scope.connection_id,
    }
}

fn coordinator_admission_error(error: OwnershipCoordinatorError) -> SidecarError {
    SidecarError::RequestAdmission {
        code: error.code(),
        message: error.to_string(),
        configuration_path: error.configuration_path(),
        retryable: error.retryable(),
        errno: error.errno(),
    }
}

fn publish_request_admission_rejection(
    writer: &ProtocolFrameWriter,
    request: RequestFrame,
    error: &RequestAdmissionError,
) -> Result<(), io::Error> {
    let (limit_name, configured_limit, current_usage, requested, unit, retryable, errno) =
        match error {
            RequestAdmissionError::CountLimit {
                current,
                requested,
                limit,
            } => (
                Some(String::from("in-flight requests")),
                Some(u64::try_from(*limit).unwrap_or(u64::MAX)),
                Some(u64::try_from(*current).unwrap_or(u64::MAX)),
                Some(u64::try_from(*requested).unwrap_or(u64::MAX)),
                Some(String::from("requests")),
                Some(true),
                Some(String::from("EAGAIN")),
            ),
            RequestAdmissionError::ByteLimit {
                current,
                requested,
                limit,
            } => (
                Some(String::from("in-flight request bytes")),
                Some(u64::try_from(*limit).unwrap_or(u64::MAX)),
                Some(u64::try_from(*current).unwrap_or(u64::MAX)),
                Some(u64::try_from(*requested).unwrap_or(u64::MAX)),
                Some(String::from("bytes")),
                Some(true),
                Some(String::from("EAGAIN")),
            ),
            RequestAdmissionError::DuplicateRequest { .. } => (
                None,
                None,
                None,
                Some(1),
                Some(String::from("requests")),
                Some(false),
                Some(String::from("EEXIST")),
            ),
            RequestAdmissionError::RegistryClosed { .. }
            | RequestAdmissionError::ConnectionClosed { .. }
            | RequestAdmissionError::OwnershipClosed { .. } => (
                None,
                None,
                None,
                Some(1),
                Some(String::from("requests")),
                Some(false),
                Some(String::from("ESHUTDOWN")),
            ),
            RequestAdmissionError::OwnershipMismatch { .. } => (
                None,
                None,
                None,
                Some(1),
                Some(String::from("requests")),
                Some(false),
                Some(String::from("EACCES")),
            ),
        };
    let rejection = ProtocolFrame::ResponseFrame(response_frame(
        request.request_id,
        request.ownership,
        ResponsePayload::RejectedResponse(wire::RejectedResponse {
            code: error.code().to_owned(),
            message: error.to_string(),
            limit_name,
            configured_limit,
            current_usage,
            requested,
            unit,
            scope: Some(String::from("connection")),
            vm_id: None,
            session_generation: None,
            capability_id: None,
            operation: Some(String::from("stdio.requestAdmission")),
            configuration_path: error.configuration_path().map(str::to_owned),
            retryable,
            errno,
        }),
    ));
    publish_rejection_frame(writer, rejection)
}

fn publish_progress_admission_rejection(
    writer: &ProtocolFrameWriter,
    request: RequestFrame,
    error: &ProgressRequestAdmissionError,
) -> Result<(), io::Error> {
    let (limit_name, configured_limit, current_usage, requested, unit, retryable, errno) =
        match error {
            ProgressRequestAdmissionError::CountLimit {
                current,
                requested,
                limit,
            } => (
                Some(String::from("in-flight progress requests")),
                Some(u64::try_from(*limit).unwrap_or(u64::MAX)),
                Some(u64::try_from(*current).unwrap_or(u64::MAX)),
                Some(u64::try_from(*requested).unwrap_or(u64::MAX)),
                Some(String::from("requests")),
                Some(true),
                Some(String::from("EAGAIN")),
            ),
            ProgressRequestAdmissionError::ByteLimit {
                current,
                requested,
                limit,
            } => (
                Some(String::from("in-flight progress request bytes")),
                Some(u64::try_from(*limit).unwrap_or(u64::MAX)),
                Some(u64::try_from(*current).unwrap_or(u64::MAX)),
                Some(u64::try_from(*requested).unwrap_or(u64::MAX)),
                Some(String::from("bytes")),
                Some(true),
                Some(String::from("EAGAIN")),
            ),
            ProgressRequestAdmissionError::DuplicateRequest { .. } => (
                None,
                None,
                None,
                Some(1),
                Some(String::from("requests")),
                Some(false),
                Some(String::from("EEXIST")),
            ),
            ProgressRequestAdmissionError::RegistryClosed { .. }
            | ProgressRequestAdmissionError::ConnectionClosed { .. }
            | ProgressRequestAdmissionError::OwnershipClosed { .. } => (
                None,
                None,
                None,
                Some(1),
                Some(String::from("requests")),
                Some(false),
                Some(String::from("ESHUTDOWN")),
            ),
        };
    let rejection = ProtocolFrame::ResponseFrame(response_frame(
        request.request_id,
        request.ownership,
        ResponsePayload::RejectedResponse(wire::RejectedResponse {
            code: error.code().to_owned(),
            message: error.to_string(),
            limit_name,
            configured_limit,
            current_usage,
            requested,
            unit,
            scope: Some(String::from("connection")),
            vm_id: None,
            session_generation: None,
            capability_id: None,
            operation: Some(String::from("stdio.progressAdmission")),
            configuration_path: error.configuration_path().map(str::to_owned),
            retryable,
            errno,
        }),
    ));
    publish_rejection_frame(writer, rejection)
}

fn publish_request_rejection(
    writer: &ProtocolFrameWriter,
    request: RequestFrame,
    code: &str,
    message: &str,
    configuration_path: Option<&str>,
) -> Result<(), io::Error> {
    let rejection = ProtocolFrame::ResponseFrame(response_frame(
        request.request_id,
        request.ownership,
        ResponsePayload::RejectedResponse(wire::RejectedResponse {
            code: code.to_owned(),
            message: message.to_owned(),
            limit_name: None,
            configured_limit: None,
            current_usage: None,
            requested: Some(1),
            unit: Some(String::from("requests")),
            scope: Some(String::from("connection")),
            vm_id: None,
            session_generation: None,
            capability_id: None,
            operation: Some(String::from("stdio.requestAdmission")),
            configuration_path: configuration_path.map(str::to_owned),
            retryable: Some(true),
            errno: Some(String::from("EAGAIN")),
        }),
    ));
    publish_rejection_frame(writer, rejection)
}

fn publish_rejection_frame(
    writer: &ProtocolFrameWriter,
    rejection: ProtocolFrame,
) -> Result<(), io::Error> {
    writer.try_send_rejection(rejection).map_err(|error| {
        io::Error::new(
            io::ErrorKind::BrokenPipe,
            format!(
                "ERR_AGENTOS_REJECTION_RESERVE_EXHAUSTED: could not retain request rejection: {error}"
            ),
        )
    })
}

/// Remove every disposed session scope from the stdio transport's active-session
/// set. Without this the set is insert-only (`track_session_state` adds on
/// `SessionOpenedResponse` but nothing ever removed), so it grew per session for
/// the process lifetime and the ~250us event pump iterated every dead entry (M5).
fn untrack_disposed_sessions(
    disposed: &[(String, String)],
    active_sessions: &mut BTreeSet<SessionScope>,
) {
    for (connection_id, session_id) in disposed {
        active_sessions.remove(&SessionScope {
            connection_id: connection_id.clone(),
            session_id: session_id.clone(),
        });
    }
}

async fn cleanup_connections(
    sidecar: &mut NativeSidecar<LocalBridge>,
    ownership_coordinator: &OwnershipCoordinator,
    active_connections: &BTreeSet<String>,
    active_sessions: &mut BTreeSet<SessionScope>,
) {
    for connection_id in active_connections {
        match ownership_coordinator
            .begin_connection_disposal(connection_id, OperationCancellationReason::ConnectionClosed)
        {
            Ok(disposal) => {
                disposal.wait_drained().await;
                if let Err(error) = disposal.complete() {
                    eprintln!(
                        "ERR_AGENTOS_CONNECTION_COORDINATOR_DISPOSAL: {connection_id}: {error}"
                    );
                }
            }
            Err(error) => {
                eprintln!("ERR_AGENTOS_CONNECTION_COORDINATOR_DISPOSAL: {connection_id}: {error}");
            }
        }
        let _ = sidecar.remove_connection(connection_id).await;
    }
    untrack_disposed_sessions(&sidecar.take_disposed_sessions(), active_sessions);
}

fn update_ownership_membership(
    coordinator: &OwnershipCoordinator,
    operation: &RequestOperation,
    request: &RequestFrame,
    response: &ResponsePayload,
) -> Result<(), io::Error> {
    let result = match response {
        ResponsePayload::AuthenticatedResponse(response) => {
            let scope = OwnershipScope::connection(&response.connection_id);
            operation.reopen_scope(&scope);
            ensure_connection_membership(coordinator, &response.connection_id)
        }
        ResponsePayload::SessionOpenedResponse(response) => {
            let scope =
                OwnershipScope::session(&response.owner_connection_id, &response.session_id);
            operation.reopen_scope(&scope);
            ensure_session_membership(
                coordinator,
                &response.owner_connection_id,
                &response.session_id,
            )
        }
        ResponsePayload::VmCreatedResponse(response) => match &request.ownership {
            OwnershipScope::SessionOwnership(scope) => {
                operation.reopen_scope(&OwnershipScope::vm(
                    &scope.connection_id,
                    &scope.session_id,
                    &response.vm_id,
                ));
                coordinator
                    .connection(&scope.connection_id)
                    .and_then(|connection| connection.session(&scope.session_id))
                    .and_then(|session| session.open_vm(response.vm_id.clone()))
                    .map(|_| ())
            }
            ownership => Err(
                crate::ownership_coordinator::OwnershipCoordinatorError::OwnershipMismatch {
                    expected: String::from("session ownership for CreateVmRequest"),
                    actual: format!("{ownership:?}"),
                },
            ),
        },
        ResponsePayload::VmDisposedResponse(_) => coordinator
            .begin_vm_disposal(&request.ownership, OperationCancellationReason::Explicit)
            .and_then(|disposal| disposal.complete()),
        _ => return Ok(()),
    };
    result.map_err(|error| {
        io::Error::other(format!(
            "ERR_AGENTOS_OWNERSHIP_MEMBERSHIP: successful lifecycle response could not update coordinator membership: {error}"
        ))
    })
}

fn ensure_connection_membership(
    coordinator: &OwnershipCoordinator,
    connection_id: &str,
) -> Result<(), crate::ownership_coordinator::OwnershipCoordinatorError> {
    coordinator
        .register_connection(connection_id.to_owned())
        .map(|_| ())
        .or_else(|error| match error {
            crate::ownership_coordinator::OwnershipCoordinatorError::Duplicate {
                scope: "connection",
                ..
            } => Ok(()),
            error => Err(error),
        })
}

fn ensure_session_membership(
    coordinator: &OwnershipCoordinator,
    connection_id: &str,
    session_id: &str,
) -> Result<(), crate::ownership_coordinator::OwnershipCoordinatorError> {
    coordinator
        .connection(connection_id)
        .and_then(|connection| connection.open_session(session_id.to_owned()))
        .map(|_| ())
        .or_else(|error| match error {
            crate::ownership_coordinator::OwnershipCoordinatorError::Duplicate {
                scope: "session",
                ..
            } => Ok(()),
            error => Err(error),
        })
}

fn commit_prepared_membership(
    coordinator: &OwnershipCoordinator,
    membership: &PreparedMembershipCommit,
    active_sessions: &mut BTreeSet<SessionScope>,
    active_connections: &mut BTreeSet<String>,
) -> Result<(), io::Error> {
    let result = match membership {
        PreparedMembershipCommit::Connection { connection_id, .. } => {
            // Track central sidecar ownership before the coordinator update so
            // a coordinator failure still leaves shutdown cleanup with the
            // authoritative connection to dispose.
            active_connections.insert(connection_id.clone());
            ensure_connection_membership(coordinator, connection_id)
        }
        PreparedMembershipCommit::Session {
            connection_id,
            session_id,
            ..
        } => {
            active_sessions.insert(SessionScope {
                connection_id: connection_id.clone(),
                session_id: session_id.clone(),
            });
            ensure_session_membership(coordinator, connection_id, session_id)
        }
    };
    result.map_err(|error| {
        io::Error::other(format!(
            "ERR_AGENTOS_OWNERSHIP_MEMBERSHIP: prepared lifecycle commit could not update coordinator membership: {error}"
        ))
    })
}

fn track_session_state(
    payload: &ResponsePayload,
    active_sessions: &mut BTreeSet<SessionScope>,
    active_connections: &mut BTreeSet<String>,
) {
    match payload {
        ResponsePayload::AuthenticatedResponse(AuthenticatedResponse { connection_id, .. }) => {
            active_connections.insert(connection_id.clone());
        }
        ResponsePayload::SessionOpenedResponse(SessionOpenedResponse {
            session_id,
            owner_connection_id,
        }) => {
            active_sessions.insert(SessionScope {
                connection_id: owner_connection_id.clone(),
                session_id: session_id.clone(),
            });
        }
        _ => {}
    }
}

fn read_frame(
    codec: &WireFrameCodec,
    reader: &mut impl Read,
) -> Result<Option<DecodedProtocolFrame>, Box<dyn Error>> {
    let mut prefix = [0u8; 4];
    match reader.read_exact(&mut prefix) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
            return Ok(None);
        }
        Err(error) => return Err(error.into()),
    }

    let declared_len = u32::from_be_bytes(prefix) as usize;
    if declared_len > codec.max_frame_bytes() {
        return Err(ProtocolCodecError::FrameTooLarge {
            size: declared_len,
            max: codec.max_frame_bytes(),
        }
        .into());
    }
    let total_len = prefix.len().saturating_add(declared_len);
    let mut bytes = Vec::with_capacity(total_len);
    bytes.extend_from_slice(&prefix);
    bytes.resize(total_len, 0);
    reader.read_exact(&mut bytes[prefix.len()..])?;

    Ok(Some(DecodedProtocolFrame {
        frame: codec.decode(&bytes)?,
        encoded_bytes: total_len,
    }))
}

fn inherited_control_stream(fd: OwnedFd) -> Result<tokio::net::UnixStream, io::Error> {
    let stream = StdUnixStream::from(fd);
    stream.set_nonblocking(true)?;
    tokio::net::UnixStream::from_std(stream)
}

async fn read_frame_async(
    codec: &WireFrameCodec,
    reader: &mut (impl AsyncRead + Unpin),
) -> Result<Option<DecodedProtocolFrame>, Box<dyn Error + Send + Sync>> {
    let mut prefix = [0u8; 4];
    match reader.read_exact(&mut prefix).await {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error.into()),
    }

    let declared_len = u32::from_be_bytes(prefix) as usize;
    if declared_len > codec.max_frame_bytes() {
        return Err(ProtocolCodecError::FrameTooLarge {
            size: declared_len,
            max: codec.max_frame_bytes(),
        }
        .into());
    }
    let total_len = prefix.len().saturating_add(declared_len);
    let mut bytes = Vec::with_capacity(total_len);
    bytes.extend_from_slice(&prefix);
    bytes.resize(total_len, 0);
    reader.read_exact(&mut bytes[prefix.len()..]).await?;

    Ok(Some(DecodedProtocolFrame {
        frame: codec.decode(&bytes)?,
        encoded_bytes: total_len,
    }))
}

fn write_encoded_frame(writer: &mut impl Write, bytes: &[u8]) -> Result<(), io::Error> {
    writer.write_all(bytes)?;
    writer.flush()
}

fn frame_kind(frame: &ProtocolFrame) -> &'static str {
    match frame {
        ProtocolFrame::RequestFrame(_) => "request",
        ProtocolFrame::ResponseFrame(_) => "response",
        ProtocolFrame::EventFrame(_) => "event",
        ProtocolFrame::SidecarRequestFrame(_) => "sidecar_request",
        ProtocolFrame::SidecarResponseFrame(_) => "sidecar_response",
        ProtocolFrame::ControlFrame(_) => "control",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StdinReaderFlow {
    Continue,
    Stop,
}

fn route_decoded_stdin_frame(
    decoded: DecodedProtocolFrame,
    ordinary_sender: &Sender<Result<Option<AccountedProtocolFrame>, String>>,
    progress_sender: &Sender<AccountedProtocolFrame>,
    overload_writer: &ProtocolFrameWriter,
    ingress_budget: &ProtocolBudget,
    progress_budget: &ProtocolBudget,
    extensions: &BTreeMap<String, Arc<dyn Extension>>,
) -> StdinReaderFlow {
    let DecodedProtocolFrame {
        frame,
        encoded_bytes,
    } = decoded;
    if !matches!(frame, ProtocolFrame::RequestFrame(_)) {
        eprintln!(
            "ERR_AGENTOS_PROTOCOL_WRONG_LANE: expected request on ordinary stdin, received {}",
            frame_kind(&frame)
        );
        return StdinReaderFlow::Stop;
    }

    if extension_request_class(&frame, extensions) == ExtensionRequestClass::Progress {
        return route_progress_request(
            frame,
            encoded_bytes,
            progress_sender,
            overload_writer,
            progress_budget,
        );
    }

    let reservation = match ingress_budget.reserve(encoded_bytes) {
        Ok(reservation) => reservation,
        Err(error) => {
            return reject_stdin_ingress_frame(frame, error, overload_writer);
        }
    };
    match enqueue_stdin_frame(
        ordinary_sender,
        Ok(Some(AccountedProtocolFrame {
            frame,
            _reservation: reservation,
        })),
    ) {
        Ok(()) => StdinReaderFlow::Continue,
        Err(StdinFrameQueueError::Closed) => StdinReaderFlow::Stop,
        Err(StdinFrameQueueError::Full(frame)) => {
            let Ok(Some(frame)) = *frame else {
                eprintln!(
                    "{STDIO_INGRESS_LIMIT_ERROR_CODE}: stdin request queue exceeded \
                     {} frames; raise {}",
                    ingress_budget.config.max_frames, ingress_budget.config.frame_path,
                );
                return StdinReaderFlow::Continue;
            };
            let error = ProtocolLimitError {
                code: "ERR_AGENTOS_PROTOCOL_FRAME_LIMIT",
                path: ingress_budget.config.frame_path,
                label: ingress_budget.config.label,
                used: ingress_budget.config.max_frames,
                requested: 1,
                limit: ingress_budget.config.max_frames,
                unit: "frames",
            };
            reject_stdin_ingress_frame(frame.frame, error, overload_writer)
        }
    }
}

fn extension_request_class(
    frame: &ProtocolFrame,
    extensions: &BTreeMap<String, Arc<dyn Extension>>,
) -> ExtensionRequestClass {
    let ProtocolFrame::RequestFrame(request) = frame else {
        return ExtensionRequestClass::Ordinary;
    };
    let RequestPayload::ExtEnvelope(envelope) = &request.payload else {
        return ExtensionRequestClass::Ordinary;
    };
    extensions
        .get(&envelope.namespace)
        .map_or(ExtensionRequestClass::Ordinary, |extension| {
            extension.request_class(&envelope.payload)
        })
}

fn route_progress_request(
    frame: ProtocolFrame,
    encoded_bytes: usize,
    progress_sender: &Sender<AccountedProtocolFrame>,
    overload_writer: &ProtocolFrameWriter,
    progress_budget: &ProtocolBudget,
) -> StdinReaderFlow {
    let reservation = match progress_budget.reserve(encoded_bytes) {
        Ok(reservation) => reservation,
        Err(error) => return reject_stdin_ingress_frame(frame, error, overload_writer),
    };
    match progress_sender.try_send(AccountedProtocolFrame {
        frame,
        _reservation: reservation,
    }) {
        Ok(()) => StdinReaderFlow::Continue,
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => StdinReaderFlow::Stop,
        Err(tokio::sync::mpsc::error::TrySendError::Full(frame)) => {
            let error = ProtocolLimitError {
                code: "ERR_AGENTOS_PROTOCOL_FRAME_LIMIT",
                path: progress_budget.config.frame_path,
                label: "extension progress ingress",
                used: progress_budget.config.max_frames,
                requested: 1,
                limit: progress_budget.config.max_frames,
                unit: "frames",
            };
            reject_stdin_ingress_frame(frame.frame, error, overload_writer)
        }
    }
}

fn route_decoded_control_frame(
    decoded: DecodedProtocolFrame,
    callback_transport: &FrameSidecarRequestTransport,
    control_sender: &Sender<AccountedProtocolFrame>,
    shutdown_sender: &Sender<wire::ControlFrame>,
    control_budget: &ProtocolBudget,
) -> StdinReaderFlow {
    let DecodedProtocolFrame {
        frame,
        encoded_bytes,
    } = decoded;
    let ProtocolFrame::SidecarResponseFrame(response) = frame else {
        if let ProtocolFrame::ControlFrame(control) = frame {
            return match shutdown_sender.try_send(control) {
                Ok(()) => StdinReaderFlow::Continue,
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                    // Shutdown is durable once queued. Coalesce duplicates
                    // rather than allowing them to consume the response budget.
                    StdinReaderFlow::Continue
                }
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => StdinReaderFlow::Stop,
            };
        }
        eprintln!(
            "ERR_AGENTOS_PROTOCOL_WRONG_LANE: expected sidecar_response or control on response/control stream, received {}",
            frame_kind(&frame)
        );
        return StdinReaderFlow::Stop;
    };
    let response = match callback_transport.accept_response(response) {
        Ok(()) => return StdinReaderFlow::Continue,
        Err(response) => *response,
    };
    let request_id = response.request_id;
    let reservation = match control_budget.reserve(encoded_bytes) {
        Ok(reservation) => reservation,
        Err(error) => {
            eprintln!(
                "{STDIO_CONTROL_LIMIT_ERROR_CODE}: {error}; dropping unmatched response request_id={request_id}"
            );
            return StdinReaderFlow::Continue;
        }
    };
    match control_sender.try_send(AccountedProtocolFrame {
        frame: ProtocolFrame::SidecarResponseFrame(response),
        _reservation: reservation,
    }) {
        Ok(()) => StdinReaderFlow::Continue,
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
            eprintln!(
                "{STDIO_CONTROL_LIMIT_ERROR_CODE}: sidecar response control queue exceeded \
                 {} frames; raise {}; dropping unmatched response request_id={request_id}",
                control_budget.config.max_frames, control_budget.config.frame_path,
            );
            StdinReaderFlow::Continue
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => StdinReaderFlow::Stop,
    }
}

#[allow(clippy::too_many_arguments)]
fn route_decoded_combined_frame(
    decoded: DecodedProtocolFrame,
    ordinary_sender: &Sender<Result<Option<AccountedProtocolFrame>, String>>,
    callback_transport: &FrameSidecarRequestTransport,
    control_sender: &Sender<AccountedProtocolFrame>,
    shutdown_sender: &Sender<wire::ControlFrame>,
    overload_writer: &ProtocolFrameWriter,
    ingress_budget: &ProtocolBudget,
    control_budget: &ProtocolBudget,
    extensions: &BTreeMap<String, Arc<dyn Extension>>,
) -> StdinReaderFlow {
    match &decoded.frame {
        ProtocolFrame::RequestFrame(_) => route_decoded_stdin_frame(
            decoded,
            ordinary_sender,
            control_sender,
            overload_writer,
            ingress_budget,
            control_budget,
            extensions,
        ),
        ProtocolFrame::SidecarResponseFrame(_) | ProtocolFrame::ControlFrame(_) => {
            route_decoded_control_frame(
                decoded,
                callback_transport,
                control_sender,
                shutdown_sender,
                control_budget,
            )
        }
        frame => {
            eprintln!(
                "ERR_AGENTOS_PROTOCOL_WRONG_LANE: host cannot write {} frame",
                frame_kind(frame)
            );
            StdinReaderFlow::Stop
        }
    }
}

fn reject_stdin_ingress_frame(
    frame: ProtocolFrame,
    error: ProtocolLimitError,
    overload_writer: &ProtocolFrameWriter,
) -> StdinReaderFlow {
    let ProtocolFrame::RequestFrame(request) = frame else {
        eprintln!(
            "{STDIO_INGRESS_LIMIT_ERROR_CODE}: {error}; dropping unexpected {} frame",
            frame_kind(&frame)
        );
        return StdinReaderFlow::Continue;
    };
    let rejection_request_id = request.request_id;
    let rejection = ProtocolFrame::ResponseFrame(response_frame(
        rejection_request_id,
        request.ownership,
        ResponsePayload::RejectedResponse(wire::RejectedResponse {
            code: error.code.to_owned(),
            message: format!("{error}; retry after the current request backlog drains"),
            limit_name: Some(error.label.to_owned()),
            configured_limit: Some(u64::try_from(error.limit).unwrap_or(u64::MAX)),
            current_usage: Some(u64::try_from(error.used).unwrap_or(u64::MAX)),
            requested: Some(u64::try_from(error.requested).unwrap_or(u64::MAX)),
            unit: Some(error.unit.to_owned()),
            scope: Some(String::from("process")),
            vm_id: None,
            session_generation: None,
            capability_id: None,
            operation: Some(String::from("stdio.requestAdmission")),
            configuration_path: Some(error.path.to_owned()),
            retryable: Some(true),
            errno: Some(String::from("EAGAIN")),
        }),
    ));
    match overload_writer.try_send_rejection(rejection) {
        Ok(()) => StdinReaderFlow::Continue,
        Err(ProtocolTrySendError::Full(output_error)) => {
            eprintln!(
                "{STDIO_INGRESS_LIMIT_ERROR_CODE}: reserved rejection egress is full ({output_error}); closing request ingress instead of silently dropping request_id={} without a terminal outcome",
                rejection_request_id,
            );
            StdinReaderFlow::Stop
        }
        Err(ProtocolTrySendError::Disconnected(_)) => StdinReaderFlow::Stop,
        Err(ProtocolTrySendError::Rejected(error)) => {
            eprintln!(
                "{STDIO_INGRESS_LIMIT_ERROR_CODE}: could not encode/admit overload rejection: {error}"
            );
            StdinReaderFlow::Stop
        }
    }
}

#[derive(Debug)]
enum StdinFrameQueueError {
    Full(Box<Result<Option<AccountedProtocolFrame>, String>>),
    Closed,
}

fn enqueue_stdin_frame(
    sender: &tokio::sync::mpsc::Sender<Result<Option<AccountedProtocolFrame>, String>>,
    frame: Result<Option<AccountedProtocolFrame>, String>,
) -> Result<(), StdinFrameQueueError> {
    sender.try_send(frame).map_err(|error| match error {
        tokio::sync::mpsc::error::TrySendError::Full(frame) => {
            StdinFrameQueueError::Full(Box::new(frame))
        }
        tokio::sync::mpsc::error::TrySendError::Closed(_) => StdinFrameQueueError::Closed,
    })
}

fn flush_sidecar_requests(
    sidecar: &mut NativeSidecar<LocalBridge>,
    writer: &ProtocolFrameWriter,
    output_tasks: &mut JoinSet<Result<(), String>>,
) -> Result<(), Box<dyn Error>> {
    while let Some(request) = sidecar.pop_wire_sidecar_request()? {
        schedule_output_frame(
            writer,
            output_tasks,
            ProtocolOutputClass::Progress,
            ProtocolFrame::SidecarRequestFrame(request),
        );
    }
    Ok(())
}

/// Emit a connection-scoped `StructuredEvent { name: "heartbeat" }` frame every
/// `interval` for as long as the stdout writer is alive. This is the host's
/// liveness signal: it resets the host's silence watchdog, so a host that sees
/// no frames at all for several intervals can conclude the sidecar process is
/// dead or wedged rather than merely busy. Runs on a dedicated thread so a
/// long synchronous dispatch cannot starve the heartbeat and trigger a false
/// host-side silence timeout.
fn spawn_heartbeat_thread(write_tx: ProtocolFrameWriter, interval: Duration) -> HeartbeatThread {
    let (stop, stop_rx) = mpsc::sync_channel(1);
    // AGENTOS_THREAD_SITE: constant-heartbeat
    let join = thread::spawn(move || {
        loop {
            match stop_rx.recv_timeout(interval) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            let frame = match crate::service::structured_event_frame(
                HEARTBEAT_CONNECTION_ID,
                "heartbeat",
                std::collections::HashMap::new(),
            ) {
                Ok(frame) => frame,
                Err(error) => {
                    // Unreachable for a fixed name/empty detail; if it ever fires,
                    // stop loudly instead of spinning on a broken encoder.
                    tracing::error!(
                        target: "agentos_native_sidecar::stdio",
                        %error,
                        "failed to encode heartbeat frame; stopping heartbeat task",
                    );
                    return;
                }
            };
            match write_tx.try_send_observability(ProtocolFrame::EventFrame(frame)) {
                Ok(()) => {}
                Err(ProtocolTrySendError::Full(_)) => {
                    // A full outbound lane means the host already has pending
                    // sidecar traffic, which itself satisfies liveness.
                }
                Err(ProtocolTrySendError::Disconnected(_)) => return,
                Err(ProtocolTrySendError::Rejected(error)) => {
                    tracing::error!(
                        target: "agentos_native_sidecar::stdio",
                        %error,
                        "failed to admit heartbeat frame; stopping heartbeat task",
                    );
                    return;
                }
            }
        }
    });
    HeartbeatThread { stop, join }
}

fn default_compile_cache_root() -> PathBuf {
    // Stable across sidecar processes so V8 compile-cache (cachedData) survives a
    // fresh sidecar/VM and benefits cold starts. Previously keyed by PID, which
    // gave every process an empty cache — cold module imports never reused
    // compiled bytecode. Entries are namespaced+validated downstream by
    // `stable_compile_cache_namespace_hash` + V8's source/version checks, so a
    // shared root is safe; stale or mismatched entries are simply ignored.
    std::env::temp_dir().join("agentos-native-sidecar-compile-cache")
}

#[cfg(test)]
#[path = "stdio/request_concurrency_tests.rs"]
mod request_concurrency_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{AuthenticateRequest, ExtEnvelope};
    use std::io::Cursor;

    fn test_protocol_budget(
        max_frames: usize,
        max_bytes: usize,
        label: &'static str,
    ) -> ProtocolBudget {
        ProtocolBudget::new(
            ProtocolBudgetConfig {
                max_frames,
                max_bytes,
                frame_path: "runtime.protocol.maxIngressFrames",
                byte_path: "runtime.protocol.maxIngressBytes",
                label,
                metric: agentos_runtime::metrics::ChannelMetricClass::StdioIngress,
            },
            agentos_runtime::metrics::RuntimeMetrics::new(),
        )
    }

    fn test_decoded_frame(frame: ProtocolFrame) -> DecodedProtocolFrame {
        DecodedProtocolFrame {
            frame,
            encoded_bytes: 1,
        }
    }

    fn test_accounted_frame(
        frame: ProtocolFrame,
        budget: &ProtocolBudget,
    ) -> AccountedProtocolFrame {
        AccountedProtocolFrame {
            frame,
            _reservation: budget.reserve(1).expect("test frame reservation"),
        }
    }

    fn test_frame_writer(capacity: usize) -> (ProtocolFrameWriter, Arc<ProtocolOutputQueue>) {
        test_frame_writer_with_inflight(capacity, 1)
    }

    fn test_frame_writer_with_inflight(
        capacity: usize,
        max_in_flight: usize,
    ) -> (ProtocolFrameWriter, Arc<ProtocolOutputQueue>) {
        let codec = WireFrameCodec::new(4096);
        let ordinary_capacity = capacity.max(2);
        let control_capacity = capacity.max(max_in_flight.saturating_add(3));
        let output = Arc::new(ProtocolOutputQueue::new(
            ordinary_capacity,
            control_capacity,
        ));
        let maximum_encoded_bytes = codec.max_frame_bytes().saturating_add(4);
        let mut protocol = agentos_runtime::RuntimeProtocolConfig::default();
        protocol.max_egress_frames = ordinary_capacity;
        protocol.max_egress_bytes = ordinary_capacity.saturating_mul(maximum_encoded_bytes);
        protocol.max_control_frames = control_capacity;
        protocol.max_control_bytes = control_capacity.saturating_mul(maximum_encoded_bytes);
        protocol.max_in_flight_requests = max_in_flight;
        protocol.max_terminal_frames = max_in_flight;
        protocol.max_terminal_bytes = max_in_flight.saturating_mul(maximum_encoded_bytes);
        protocol.terminal_fallback_bytes = maximum_encoded_bytes;
        protocol.max_progress_frames = 1;
        protocol.max_progress_bytes = maximum_encoded_bytes;
        protocol.max_rejection_frames = 1;
        protocol.max_rejection_bytes = maximum_encoded_bytes;
        (
            ProtocolFrameWriter::new(
                Arc::clone(&output),
                codec,
                &protocol,
                agentos_runtime::metrics::RuntimeMetrics::new(),
            )
            .expect("test output partitions"),
            output,
        )
    }

    struct GatedExtension {
        started: Arc<AtomicUsize>,
        started_notify: Arc<Notify>,
        release: Arc<Notify>,
    }

    impl Extension for GatedExtension {
        fn namespace(&self) -> &str {
            "dev.rivet.agentos.test.gated"
        }

        fn handle_request<'a>(
            &'a self,
            _ctx: crate::ExtensionContext,
            payload: Vec<u8>,
        ) -> crate::ExtensionFuture<'a, crate::ExtensionResponse> {
            Box::pin(async move {
                self.started.fetch_add(1, Ordering::AcqRel);
                self.started_notify.notify_waiters();
                self.release.notified().await;
                Ok(crate::ExtensionResponse::new(payload))
            })
        }

        fn request_class(&self, payload: &[u8]) -> ExtensionRequestClass {
            if payload == b"progress" {
                ExtensionRequestClass::Progress
            } else {
                ExtensionRequestClass::Ordinary
            }
        }
    }

    struct PanickingExtension;

    impl Extension for PanickingExtension {
        fn namespace(&self) -> &str {
            "dev.rivet.agentos.test.panicking"
        }

        fn handle_request<'a>(
            &'a self,
            _ctx: crate::ExtensionContext,
            _payload: Vec<u8>,
        ) -> crate::ExtensionFuture<'a, crate::ExtensionResponse> {
            Box::pin(async move {
                panic!("deterministic extension panic");
            })
        }
    }

    async fn wait_for_started(started: &AtomicUsize, started_notify: &Notify, expected: usize) {
        loop {
            let notified = started_notify.notified();
            if started.load(Ordering::Acquire) >= expected {
                return;
            }
            notified.await;
        }
    }

    async fn drain_claimed_process_event_services_one_at_a_time(
        pending: &mut VecDeque<PreparedExtensionServiceCommand>,
        expected: usize,
        completion_tx: &Sender<CompletedExtensionServiceCommand>,
        completion_rx: &mut Receiver<CompletedExtensionServiceCommand>,
        tasks: &mut JoinSet<()>,
        sidecar: &mut NativeSidecar<LocalBridge>,
    ) {
        for _ in 0..expected {
            let admission_turn = pending.len();
            let mut admitted = None;
            for _ in 0..admission_turn {
                let target = pending.pop_front().expect("pending claimed process event");
                match target
                    .admit_vm_event_nowait()
                    .expect("retry claimed process-event admission")
                {
                    VmEventAdmissionResult::Admitted(target) => {
                        admitted = Some(target);
                        break;
                    }
                    VmEventAdmissionResult::Deferred(target) => pending.push_back(target),
                }
            }
            let admitted = admitted.expect(
                "one pre-admitted internal event must run and release capacity for the next",
            );
            schedule_extension_service_command(admitted, completion_tx, tasks);
            let completion = tokio::time::timeout(Duration::from_secs(1), completion_rx.recv())
                .await
                .expect("process-event service deadline")
                .expect("process-event service completion");
            assert!(completion.complete(sidecar).is_none());
            tokio::time::timeout(Duration::from_secs(1), tasks.join_next())
                .await
                .expect("process-event supervisor deadline")
                .expect("process-event supervisor task")
                .expect("process-event supervisor task succeeds");
        }
        assert!(
            pending.is_empty(),
            "every claimed event must run exactly once"
        );
        assert!(tasks.is_empty());
        assert!(completion_rx.try_recv().is_err());
    }

    #[test]
    fn request_metadata_maps_ownership_and_vm_concurrency_independently() {
        let extensions = BTreeMap::new();
        let lifecycle = request_frame(
            1,
            vm_ownership("conn", "session", "vm"),
            RequestPayload::DisposeVmRequest(wire::DisposeVmRequest {
                reason: wire::DisposeReason::Requested,
            }),
        );
        assert!(matches!(
            request_operation_metadata(&lifecycle, &extensions).vm_concurrency,
            VmConcurrencyClass::ExclusiveVmLifecycle
        ));

        let operation = request_frame(
            2,
            vm_ownership("conn", "session", "vm"),
            RequestPayload::GetProcessSnapshotRequest,
        );
        assert!(matches!(
            request_operation_metadata(&operation, &extensions).vm_concurrency,
            VmConcurrencyClass::SharedVm
        ));

        let session = request_frame(
            3,
            session_ownership("conn", "session"),
            RequestPayload::CreateVmRequest(wire::CreateVmRequest::legacy_test_config(
                wire::GuestRuntimeKind::JavaScript,
                Default::default(),
                Default::default(),
                None,
            )),
        );
        let metadata = request_operation_metadata(&session, &extensions);
        assert_eq!(metadata.ownership, session_ownership("conn", "session"));
        assert_eq!(metadata.vm_concurrency, VmConcurrencyClass::OwnershipOnly);
    }

    #[test]
    fn protocol_router_never_awaits_extension_service_or_event_work_inline() {
        let source = include_str!("stdio.rs");
        let legacy_service_handler = ["handle_extension_service", "_command("].concat();
        let legacy_event_poll = ["poll_event", "_wire("].concat();
        let legacy_event_pump = ["pump_process", "_events("].concat();
        let bounded_service_guard = [
            "extension_service_tasks.len() < ",
            "extension_service_capacity",
        ]
        .concat();
        assert!(
            !source.contains(&legacy_service_handler),
            "the legacy whole-sidecar async extension service handler must stay deleted"
        );
        assert!(
            !source.contains(&legacy_event_poll) && !source.contains(&legacy_event_pump),
            "the protocol router must only perform bounded non-suspending event turns"
        );
        assert!(
            source.contains(&bounded_service_guard),
            "extension service admission must remain bounded by its tracked task capacity"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn request_concurrency_pre_reap_reuses_service_capacity_across_repeated_waves() {
        tokio::task::LocalSet::new()
            .run_until(async {
                const SERVICE_CAPACITY: usize = 4;
                const WAVES: usize = 32;

                let mut extension_service_tasks = JoinSet::new();
                let mut progress_service_tasks = JoinSet::new();
                let mut extension_tasks = JoinSet::new();
                let mut request_tasks = JoinSet::new();
                let mut output_tasks = JoinSet::new();
                let mut ordinary_event_tasks = JoinSet::new();
                let (event_ready_tx, mut event_ready_rx) = channel(1);
                let completed = Arc::new(AtomicUsize::new(0));
                let completed_notify = Arc::new(Notify::new());
                let mut expected_completed = 0usize;

                for wave in 0..WAVES {
                    assert_eq!(
                        extension_service_tasks.len(),
                        0,
                        "wave {wave} must begin with all service-task capacity available"
                    );

                    for _ in 0..SERVICE_CAPACITY {
                        assert!(
                            extension_service_tasks.len() < SERVICE_CAPACITY,
                            "completed JoinSet entries must not starve service admission"
                        );

                        let service_completed = Arc::clone(&completed);
                        let service_completed_notify = Arc::clone(&completed_notify);
                        extension_service_tasks.spawn_local(async move {
                            service_completed.fetch_add(1, Ordering::AcqRel);
                            service_completed_notify.notify_waiters();
                        });

                        let extension_completed = Arc::clone(&completed);
                        let extension_completed_notify = Arc::clone(&completed_notify);
                        extension_tasks.spawn_local(async move {
                            extension_completed.fetch_add(1, Ordering::AcqRel);
                            extension_completed_notify.notify_waiters();
                        });

                        let request_completed = Arc::clone(&completed);
                        let request_completed_notify = Arc::clone(&completed_notify);
                        request_tasks.spawn_local(async move {
                            request_completed.fetch_add(1, Ordering::AcqRel);
                            request_completed_notify.notify_waiters();
                        });

                        let output_completed = Arc::clone(&completed);
                        let output_completed_notify = Arc::clone(&completed_notify);
                        output_tasks.spawn_local(async move {
                            output_completed.fetch_add(1, Ordering::AcqRel);
                            output_completed_notify.notify_waiters();
                            Ok(())
                        });
                    }

                    // Production permits at most one durable event publisher at
                    // a time; include it so the common pre-reap path and wake
                    // re-arm are exercised on every router iteration.
                    let completed_for_event = Arc::clone(&completed);
                    let completed_notify_for_event = Arc::clone(&completed_notify);
                    ordinary_event_tasks.spawn_local(async move {
                        completed_for_event.fetch_add(1, Ordering::AcqRel);
                        completed_notify_for_event.notify_waiters();
                        Ok(())
                    });

                    expected_completed = expected_completed
                        .saturating_add(SERVICE_CAPACITY.saturating_mul(4).saturating_add(1));
                    wait_for_started(
                        completed.as_ref(),
                        completed_notify.as_ref(),
                        expected_completed,
                    )
                    .await;

                    // JoinSet retains completed entries. This is the state seen
                    // at the top of the next biased protocol-router iteration.
                    assert_eq!(extension_service_tasks.len(), SERVICE_CAPACITY);
                    assert_eq!(extension_tasks.len(), SERVICE_CAPACITY);
                    assert_eq!(request_tasks.len(), SERVICE_CAPACITY);
                    assert_eq!(output_tasks.len(), SERVICE_CAPACITY);
                    assert_eq!(ordinary_event_tasks.len(), 1);

                    reap_protocol_tasks_nowait(
                        &mut extension_service_tasks,
                        &mut progress_service_tasks,
                        &mut extension_tasks,
                        &mut request_tasks,
                        &mut output_tasks,
                        &mut ordinary_event_tasks,
                        &event_ready_tx,
                    )
                    .expect("pre-select reap succeeds");

                    assert!(extension_service_tasks.is_empty());
                    assert!(progress_service_tasks.is_empty());
                    assert!(extension_tasks.is_empty());
                    assert!(request_tasks.is_empty());
                    assert!(output_tasks.is_empty());
                    assert!(ordinary_event_tasks.is_empty());
                    event_ready_rx
                        .try_recv()
                        .expect("durable-event completion re-arms its coalesced wake");
                    assert!(event_ready_rx.try_recv().is_err());
                }

                assert_eq!(
                    completed.load(Ordering::Acquire),
                    WAVES * (SERVICE_CAPACITY * 4 + 1)
                );
                assert!(WAVES * SERVICE_CAPACITY > SERVICE_CAPACITY);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn generic_supervisor_completes_out_of_order_and_turns_panic_into_one_terminal() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let config = NativeSidecarConfig::default();
                let runtime = agentos_runtime::SidecarRuntime::process(&config.runtime)
                    .expect("test runtime");
                let mut sidecar = NativeSidecar::with_config_extensions_and_runtime(
                    LocalBridge::default(),
                    config.clone(),
                    Vec::new(),
                    runtime.context(),
                )
                .expect("test sidecar");
                let operations = OperationTable::from_protocol_config(&config.runtime.protocol);
                let ownership_coordinator =
                    OwnershipCoordinator::from_runtime_config(&config.runtime);
                ownership_coordinator
                    .register_connection("generic-a")
                    .expect("register generic connection");
                let (writer, output) = test_frame_writer_with_inflight(8, 3);
                let (completion_tx, mut completion_rx) = channel(3);
                let mut request_tasks = JoinSet::new();
                let mut output_tasks = JoinSet::new();
                let mut active_sessions = BTreeSet::new();
                let mut active_connections = BTreeSet::new();
                let release_first = Arc::new(Notify::new());

                for request_id in [10, 11] {
                    let request = request_frame(
                        request_id,
                        connection_ownership("generic-a"),
                        RequestPayload::GetProcessSnapshotRequest,
                    );
                    let compat_request =
                        wire::request_frame_to_compat(request.clone()).expect("compat request");
                    let response_request = compat_request.clone();
                    let release = Arc::clone(&release_first);
                    let prepared = PreparedRequest::from_future(compat_request, async move {
                        if request_id == 10 {
                            release.notified().await;
                        }
                        Ok(agentos_native_sidecar_core::DispatchResult {
                            response: agentos_native_sidecar_core::reject(
                                &response_request,
                                "TEST_GENERIC_COMPLETE",
                                "generic completion",
                            ),
                            events: Vec::new(),
                        })
                    });
                    let operation = operations
                        .admit(
                            RequestOperationKey::new("generic-a", request_id),
                            request_operation_metadata(&request, &BTreeMap::new()),
                            1,
                        )
                        .expect("admit generic request");
                    schedule_prepared_request(
                        prepared,
                        request,
                        operation,
                        true,
                        ownership_coordinator.clone(),
                        writer.try_reserve_terminal(1).expect("terminal reserve"),
                        &completion_tx,
                        &mut request_tasks,
                    );
                }

                let second = tokio::time::timeout(Duration::from_secs(1), completion_rx.recv())
                    .await
                    .expect("independent generic completion deadline")
                    .expect("independent generic completion");
                assert_eq!(second.request.request_id, 11);
                finish_request(
                    second,
                    &mut sidecar,
                    &ownership_coordinator,
                    &completion_tx,
                    &mut request_tasks,
                    &writer,
                    &mut output_tasks,
                    &mut active_sessions,
                    &mut active_connections,
                )
                .expect("finish independent generic request");
                output_tasks
                    .join_next()
                    .await
                    .expect("independent terminal task")
                    .expect("independent terminal join")
                    .expect("independent terminal output");
                let ProtocolFrame::ResponseFrame(response) =
                    decode_test_output(output.recv_control().await.expect("request 11 response"))
                else {
                    panic!("expected generic response");
                };
                assert_eq!(response.request_id, 11);

                release_first.notify_waiters();
                let first = completion_rx
                    .recv()
                    .await
                    .expect("first generic completion");
                assert_eq!(first.request.request_id, 10);
                finish_request(
                    first,
                    &mut sidecar,
                    &ownership_coordinator,
                    &completion_tx,
                    &mut request_tasks,
                    &writer,
                    &mut output_tasks,
                    &mut active_sessions,
                    &mut active_connections,
                )
                .expect("finish first generic request");
                output_tasks
                    .join_next()
                    .await
                    .expect("first terminal task")
                    .expect("first terminal join")
                    .expect("first terminal output");
                let ProtocolFrame::ResponseFrame(response) =
                    decode_test_output(output.recv_control().await.expect("request 10 response"))
                else {
                    panic!("expected generic response");
                };
                assert_eq!(response.request_id, 10);

                let request = request_frame(
                    12,
                    connection_ownership("generic-a"),
                    RequestPayload::GetProcessSnapshotRequest,
                );
                let compat_request =
                    wire::request_frame_to_compat(request.clone()).expect("compat panic request");
                let prepared = PreparedRequest::from_future(compat_request, async move {
                    panic!("deterministic generic panic");
                    #[allow(unreachable_code)]
                    Ok(agentos_native_sidecar_core::DispatchResult {
                        response: unreachable!(),
                        events: Vec::new(),
                    })
                });
                let operation = operations
                    .admit(
                        RequestOperationKey::new("generic-a", 12),
                        request_operation_metadata(&request, &BTreeMap::new()),
                        1,
                    )
                    .expect("admit panicking request");
                schedule_prepared_request(
                    prepared,
                    request,
                    operation,
                    true,
                    ownership_coordinator.clone(),
                    writer
                        .try_reserve_terminal(1)
                        .expect("panic terminal reserve"),
                    &completion_tx,
                    &mut request_tasks,
                );
                let panic_completion = completion_rx.recv().await.expect("panic completion");
                assert!(matches!(
                    &panic_completion.result,
                    DetachedRequestResult::Generic(Err(SidecarError::Execution(message)))
                        if message.contains("ERR_AGENTOS_REQUEST_TASK_PANIC")
                ));
                finish_request(
                    panic_completion,
                    &mut sidecar,
                    &ownership_coordinator,
                    &completion_tx,
                    &mut request_tasks,
                    &writer,
                    &mut output_tasks,
                    &mut active_sessions,
                    &mut active_connections,
                )
                .expect("finish panicking generic request");
                output_tasks
                    .join_next()
                    .await
                    .expect("panic terminal task")
                    .expect("panic terminal join")
                    .expect("panic terminal output");
                let ProtocolFrame::ResponseFrame(response) =
                    decode_test_output(output.recv_control().await.expect("panic response"))
                else {
                    panic!("expected panic response");
                };
                assert_eq!(response.request_id, 12);
                let ResponsePayload::RejectedResponse(rejection) = response.payload else {
                    panic!("generic panic must be typed rejection");
                };
                assert!(rejection.message.contains("ERR_AGENTOS_REQUEST_TASK_PANIC"));
                assert_eq!(operations.snapshot().in_flight_requests, 0);
                while request_tasks.join_next().await.is_some() {}
                output.close();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn staged_dispose_cancels_and_drains_a_gated_vm_operation_before_detach() {
        tokio::task::LocalSet::new()
            .run_until(async {
                struct DropSignal {
                    dropped: Arc<std::sync::atomic::AtomicBool>,
                    notify: Arc<Notify>,
                }

                impl Drop for DropSignal {
                    fn drop(&mut self) {
                        self.dropped.store(true, Ordering::Release);
                        self.notify.notify_waiters();
                    }
                }

                let connection_id = "dispose-route-connection";
                let session_id = "dispose-route-session";
                let config = NativeSidecarConfig::default();
                let runtime = agentos_runtime::SidecarRuntime::process(&config.runtime)
                    .expect("test runtime");
                let mut sidecar = NativeSidecar::with_config_extensions_and_runtime(
                    LocalBridge::default(),
                    config.clone(),
                    Vec::new(),
                    runtime.context(),
                )
                .expect("test sidecar");
                sidecar.connections.insert(
                    connection_id.to_owned(),
                    crate::state::ConnectionState {
                        auth_token: String::new(),
                        sessions: BTreeSet::from([session_id.to_owned()]),
                    },
                );
                sidecar.sessions.insert(
                    session_id.to_owned(),
                    crate::state::SessionState {
                        connection_id: connection_id.to_owned(),
                        placement: crate::protocol::SidecarPlacement::SidecarPlacementShared(
                            crate::protocol::SidecarPlacementShared { pool: None },
                        ),
                        metadata: BTreeMap::new(),
                        vm_ids: BTreeSet::new(),
                    },
                );
                let create_payload = crate::protocol::CreateVmRequest::legacy_test_config(
                    crate::protocol::GuestRuntimeKind::JavaScript,
                    Default::default(),
                    Default::default(),
                    None,
                );
                let create_request = crate::protocol::RequestFrame::new(
                    1,
                    crate::protocol::OwnershipScope::session(connection_id, session_id),
                    crate::protocol::RequestPayload::CreateVm(create_payload.clone()),
                );
                let prepared_create = sidecar
                    .prepare_create_vm(&create_request, create_payload)
                    .expect("prepare test VM");
                let completed_create = prepared_create.execute().await.expect("build test VM");
                sidecar
                    .complete_create_vm(completed_create)
                    .expect("publish test VM");
                let vm_id = sidecar
                    .sessions
                    .get(session_id)
                    .and_then(|session| session.vm_ids.iter().next())
                    .cloned()
                    .expect("test VM membership");

                let ownership_coordinator =
                    OwnershipCoordinator::from_runtime_config(&config.runtime);
                let coordinator_session = ownership_coordinator
                    .register_connection(connection_id)
                    .and_then(|connection| connection.open_session(session_id))
                    .expect("test coordinator session");
                coordinator_session
                    .open_vm(vm_id.clone())
                    .expect("test coordinator VM");

                let operations = OperationTable::from_protocol_config(&config.runtime.protocol);
                let progress_requests =
                    ProgressOperationView::from_protocol_config(&config.runtime.protocol);
                let (writer, output) = test_frame_writer_with_inflight(8, 2);
                let ingress_budget = test_protocol_budget(4, 4096, "dispose route ingress");
                let (service_tx, _service_rx) = channel(4);
                let services: Arc<dyn ExtensionServices> = Arc::new(RoutedExtensionServices::new(
                    service_tx,
                    Arc::clone(&sidecar.process_event_notify),
                ));
                let (extension_completion_tx, _extension_completion_rx) = channel(2);
                let (request_completion_tx, mut request_completion_rx) = channel(4);
                let mut extension_tasks = JoinSet::new();
                let mut request_tasks = JoinSet::new();
                let mut output_tasks = JoinSet::new();
                let mut active_sessions = BTreeSet::new();
                let mut active_connections = BTreeSet::new();

                let operation_request = request_frame(
                    70,
                    vm_ownership(connection_id, session_id, &vm_id),
                    RequestPayload::GetProcessSnapshotRequest,
                );
                let compat_operation = wire::request_frame_to_compat(operation_request.clone())
                    .expect("compat gated operation");
                let response_request = compat_operation.clone();
                let started = Arc::new(std::sync::atomic::AtomicBool::new(false));
                let started_notify = Arc::new(Notify::new());
                let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
                let dropped_notify = Arc::new(Notify::new());
                let prepared_operation = PreparedRequest::from_future(compat_operation, {
                    let started = Arc::clone(&started);
                    let started_notify = Arc::clone(&started_notify);
                    let dropped = Arc::clone(&dropped);
                    let dropped_notify = Arc::clone(&dropped_notify);
                    async move {
                        let _drop_signal = DropSignal {
                            dropped,
                            notify: dropped_notify,
                        };
                        started.store(true, Ordering::Release);
                        started_notify.notify_waiters();
                        std::future::pending::<()>().await;
                        #[allow(unreachable_code)]
                        Ok(agentos_native_sidecar_core::DispatchResult {
                            response: agentos_native_sidecar_core::reject(
                                &response_request,
                                "UNREACHABLE",
                                "gated operation should be cancelled",
                            ),
                            events: Vec::new(),
                        })
                    }
                });
                let operation = operations
                    .admit(
                        RequestOperationKey::new(connection_id, 70),
                        request_operation_metadata(&operation_request, &BTreeMap::new()),
                        1,
                    )
                    .expect("admit gated VM operation");
                let operation_cancellation = operation.cancellation();
                schedule_prepared_request(
                    prepared_operation,
                    operation_request,
                    operation,
                    true,
                    ownership_coordinator.clone(),
                    writer.try_reserve_terminal(1).expect("operation terminal"),
                    &request_completion_tx,
                    &mut request_tasks,
                );
                tokio::time::timeout(Duration::from_secs(1), async {
                    loop {
                        let notified = started_notify.notified();
                        if started.load(Ordering::Acquire) {
                            break;
                        }
                        notified.await;
                    }
                })
                .await
                .expect("gated VM operation started");

                let dispose_request = request_frame(
                    71,
                    vm_ownership(connection_id, session_id, &vm_id),
                    RequestPayload::DisposeVmRequest(wire::DisposeVmRequest {
                        reason: wire::DisposeReason::Requested,
                    }),
                );
                route_protocol_frame(
                    test_accounted_frame(
                        ProtocolFrame::RequestFrame(dispose_request),
                        &ingress_budget,
                    ),
                    &mut sidecar,
                    &services,
                    &operations,
                    &progress_requests,
                    &ownership_coordinator,
                    &extension_completion_tx,
                    &request_completion_tx,
                    &mut extension_tasks,
                    &mut request_tasks,
                    &mut output_tasks,
                    &writer,
                    1,
                    &mut active_sessions,
                    &mut active_connections,
                )
                .expect("route staged DisposeVm");
                assert_eq!(
                    operation_cancellation.cancelled().await,
                    OperationCancellationReason::Explicit
                );

                let mut response_ids = Vec::new();
                while response_ids.len() < 2 {
                    let completion =
                        tokio::time::timeout(Duration::from_secs(1), request_completion_rx.recv())
                            .await
                            .expect("staged disposal completion deadline")
                            .expect("staged disposal completion");
                    finish_request(
                        completion,
                        &mut sidecar,
                        &ownership_coordinator,
                        &request_completion_tx,
                        &mut request_tasks,
                        &writer,
                        &mut output_tasks,
                        &mut active_sessions,
                        &mut active_connections,
                    )
                    .expect("advance staged disposal");
                    while !output_tasks.is_empty() {
                        output_tasks
                            .join_next()
                            .await
                            .expect("terminal task")
                            .expect("terminal join")
                            .expect("terminal output");
                        let ProtocolFrame::ResponseFrame(response) = decode_test_output(
                            output.recv_control().await.expect("terminal response"),
                        ) else {
                            panic!("expected terminal response");
                        };
                        response_ids.push(response.request_id);
                    }
                }

                response_ids.sort_unstable();
                assert_eq!(response_ids, vec![70, 71]);
                assert!(dropped.load(Ordering::Acquire));
                assert!(!sidecar.vms.contains_key(&vm_id));
                assert_eq!(operations.snapshot().in_flight_requests, 0);
                assert!(ownership_coordinator
                    .begin_vm_disposal(
                        &vm_ownership(connection_id, session_id, &vm_id),
                        OperationCancellationReason::Explicit,
                    )
                    .is_err());
                assert!(
                    tokio::time::timeout(Duration::from_millis(25), output.recv_control(),)
                        .await
                        .is_err()
                );
                while request_tasks.join_next().await.is_some() {}
                output.close();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn blocked_extension_service_does_not_delay_another_service_command() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let config = NativeSidecarConfig::default();
                let runtime = agentos_runtime::SidecarRuntime::process(&config.runtime)
                    .expect("test runtime");
                let mut sidecar = NativeSidecar::with_config_extensions_and_runtime(
                    LocalBridge::default(),
                    config,
                    Vec::new(),
                    runtime.context(),
                )
                .expect("test sidecar");
                let started = Arc::new(AtomicUsize::new(0));
                let started_notify = Arc::new(Notify::new());
                let release = Arc::new(Notify::new());
                let gated_started = Arc::clone(&started);
                let gated_started_notify = Arc::clone(&started_notify);
                let gated_release = Arc::clone(&release);
                let (gated, mut gated_reply) =
                    crate::extension_services::prepared_test_service_command(
                        "test_gated_service",
                        async move {
                            gated_started.fetch_add(1, Ordering::AcqRel);
                            gated_started_notify.notify_waiters();
                            gated_release.notified().await;
                        },
                    );
                let (ready, ready_reply) = crate::extension_services::prepared_test_service_command(
                    "test_ready_service",
                    async {},
                );
                let (completion_tx, mut completion_rx) = channel(2);
                let mut tasks = JoinSet::new();

                schedule_extension_service_command(gated, &completion_tx, &mut tasks);
                wait_for_started(&started, &started_notify, 1).await;
                schedule_extension_service_command(ready, &completion_tx, &mut tasks);

                let ready_completion =
                    tokio::time::timeout(Duration::from_secs(1), completion_rx.recv())
                        .await
                        .expect("ready service completion deadline")
                        .expect("ready service completion");
                assert!(ready_completion.complete(&mut sidecar).is_none());
                ready_reply
                    .await
                    .expect("ready reply channel")
                    .expect("ready service reply");
                assert!(gated_reply.try_recv().is_err());

                release.notify_waiters();
                let gated_completion =
                    tokio::time::timeout(Duration::from_secs(1), completion_rx.recv())
                        .await
                        .expect("gated service completion deadline")
                        .expect("gated service completion");
                assert!(gated_completion.complete(&mut sidecar).is_none());
                gated_reply
                    .await
                    .expect("gated reply channel")
                    .expect("gated service reply");
                while tasks.join_next().await.is_some() {}
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn blocked_python_event_service_does_not_delay_an_independent_request() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let config = NativeSidecarConfig::default();
                let runtime = agentos_runtime::SidecarRuntime::process(&config.runtime)
                    .expect("test runtime");
                let mut sidecar = NativeSidecar::with_config_extensions_and_runtime(
                    LocalBridge::default(),
                    config.clone(),
                    Vec::new(),
                    runtime.context(),
                )
                .expect("test sidecar");
                let ownership_coordinator =
                    OwnershipCoordinator::from_runtime_config(&config.runtime);
                ownership_coordinator
                    .register_connection("conn-independent-service")
                    .expect("register independent connection");
                let operations = OperationTable::from_protocol_config(&config.runtime.protocol);
                let progress_requests =
                    ProgressOperationView::from_protocol_config(&config.runtime.protocol);
                let ingress_budget = test_protocol_budget(4, 4096, "test request ingress");
                let (writer, output) = test_frame_writer_with_inflight(8, 2);
                let (service_tx, _service_rx) = channel(4);
                let services: Arc<dyn ExtensionServices> = Arc::new(RoutedExtensionServices::new(
                    service_tx,
                    Arc::clone(&sidecar.process_event_notify),
                ));
                let (extension_completion_tx, _extension_completion_rx) = channel(2);
                let (request_completion_tx, mut request_completion_rx) = channel(2);
                let (service_completion_tx, mut service_completion_rx) = channel(2);
                let mut extension_tasks = JoinSet::new();
                let mut request_tasks = JoinSet::new();
                let mut service_tasks = JoinSet::new();
                let mut output_tasks = JoinSet::new();
                let mut active_sessions = BTreeSet::new();
                let mut active_connections = BTreeSet::new();
                let started = Arc::new(AtomicUsize::new(0));
                let started_notify = Arc::new(Notify::new());
                let release = Arc::new(Notify::new());
                let gated_started = Arc::clone(&started);
                let gated_started_notify = Arc::clone(&started_notify);
                let gated_release = Arc::clone(&release);
                let (gated, mut gated_reply) =
                    crate::extension_services::prepared_test_service_command(
                        "service_python_process_event",
                        async move {
                            gated_started.fetch_add(1, Ordering::AcqRel);
                            gated_started_notify.notify_waiters();
                            gated_release.notified().await;
                        },
                    );
                schedule_extension_service_command(
                    gated,
                    &service_completion_tx,
                    &mut service_tasks,
                );
                wait_for_started(&started, &started_notify, 1).await;

                route_protocol_frame(
                    test_accounted_frame(
                        ProtocolFrame::RequestFrame(request_frame(
                            21,
                            connection_ownership("conn-independent-service"),
                            RequestPayload::GetProcessSnapshotRequest,
                        )),
                        &ingress_budget,
                    ),
                    &mut sidecar,
                    &services,
                    &operations,
                    &progress_requests,
                    &ownership_coordinator,
                    &extension_completion_tx,
                    &request_completion_tx,
                    &mut extension_tasks,
                    &mut request_tasks,
                    &mut output_tasks,
                    &writer,
                    2,
                    &mut active_sessions,
                    &mut active_connections,
                )
                .expect("route independent request");

                let request_completion =
                    tokio::time::timeout(Duration::from_secs(1), request_completion_rx.recv())
                        .await
                        .expect("independent request completion deadline")
                        .expect("independent request completion");
                assert_eq!(request_completion.request.request_id, 21);
                finish_request(
                    request_completion,
                    &mut sidecar,
                    &ownership_coordinator,
                    &request_completion_tx,
                    &mut request_tasks,
                    &writer,
                    &mut output_tasks,
                    &mut active_sessions,
                    &mut active_connections,
                )
                .expect("finish independent request");
                output_tasks
                    .join_next()
                    .await
                    .expect("independent output task")
                    .expect("output task join")
                    .expect("independent output publish");
                let ProtocolFrame::ResponseFrame(response) =
                    decode_test_output(output.recv_control().await.expect("independent response"))
                else {
                    panic!("expected response frame");
                };
                assert_eq!(response.request_id, 21);
                assert!(gated_reply.try_recv().is_err());

                release.notify_waiters();
                let completion =
                    tokio::time::timeout(Duration::from_secs(1), service_completion_rx.recv())
                        .await
                        .expect("gated service completion deadline")
                        .expect("gated service completion");
                assert!(completion.complete(&mut sidecar).is_none());
                gated_reply
                    .await
                    .expect("gated reply channel")
                    .expect("gated service reply");
                while service_tasks.join_next().await.is_some() {}
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn root_and_child_python_vfs_and_socket_work_runs_while_independent_request_is_blocked() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let started = Arc::new(AtomicUsize::new(0));
                let started_notify = Arc::new(Notify::new());
                let release = Arc::new(Notify::new());
                let extension = GatedExtension {
                    started: Arc::clone(&started),
                    started_notify: Arc::clone(&started_notify),
                    release: Arc::clone(&release),
                };
                let config = NativeSidecarConfig::default();
                let runtime = agentos_runtime::SidecarRuntime::process(&config.runtime)
                    .expect("test process runtime");
                let mut sidecar = NativeSidecar::with_config_extensions_and_runtime(
                    LocalBridge::default(),
                    config.clone(),
                    vec![Box::new(extension)],
                    runtime.context(),
                )
                .expect("test sidecar");

                let connection_id = "conn-root-python-vfs";
                let session_id = "session-root-python-vfs";
                sidecar.connections.insert(
                    connection_id.to_owned(),
                    crate::state::ConnectionState {
                        auth_token: String::new(),
                        sessions: BTreeSet::from([session_id.to_owned()]),
                    },
                );
                sidecar.sessions.insert(
                    session_id.to_owned(),
                    crate::state::SessionState {
                        connection_id: connection_id.to_owned(),
                        placement: crate::protocol::SidecarPlacement::SidecarPlacementShared(
                            crate::protocol::SidecarPlacementShared { pool: None },
                        ),
                        metadata: BTreeMap::new(),
                        vm_ids: BTreeSet::new(),
                    },
                );
                let create_payload = crate::protocol::CreateVmRequest::legacy_test_config(
                    crate::protocol::GuestRuntimeKind::Python,
                    Default::default(),
                    Default::default(),
                    Some(crate::protocol::PermissionsPolicy::allow_all()),
                );
                let create_request = crate::protocol::RequestFrame::new(
                    1,
                    crate::protocol::OwnershipScope::session(connection_id, session_id),
                    crate::protocol::RequestPayload::CreateVm(create_payload.clone()),
                );
                let prepared_create = sidecar
                    .prepare_create_vm(&create_request, create_payload)
                    .expect("prepare root Python test VM");
                let completed_create = prepared_create
                    .execute()
                    .await
                    .expect("build root Python test VM");
                sidecar
                    .complete_create_vm(completed_create)
                    .expect("publish root Python test VM");
                let vm_id = sidecar
                    .sessions
                    .get(session_id)
                    .and_then(|session| session.vm_ids.iter().next())
                    .cloned()
                    .expect("root Python test VM membership");

                // The trusted executor is process-global and may already have
                // been initialized by another test. Saturate only the bounded
                // ownership domains exercised here instead of attempting to
                // reconfigure that executor.
                let mut coordinator_runtime_config = config.runtime.clone();
                coordinator_runtime_config.protocol.max_in_flight_requests = 1;
                coordinator_runtime_config.protocol.max_process_events = 2;
                let coordinator =
                    OwnershipCoordinator::from_runtime_config(&coordinator_runtime_config);
                coordinator
                    .register_connection("conn-gated-root-python-vfs")
                    .expect("register gated request connection");
                coordinator
                    .register_connection(connection_id)
                    .and_then(|connection| connection.open_session(session_id))
                    .and_then(|session| session.open_vm(vm_id.clone()))
                    .expect("register root Python test VM");
                let _ordinary_saturation = coordinator
                    .admit(
                        &RequestOperationMetadata::new(
                            crate::protocol::OwnershipScope::vm(connection_id, session_id, &vm_id),
                            "saturate ordinary VM admission",
                            VmConcurrencyClass::SharedVm,
                        ),
                        crate::request_operations::OperationCancellation::new(),
                    )
                    .await
                    .expect("fill ordinary VM admission independently of internal events");

                let suffix = SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("test clock")
                    .as_nanos();
                let pyodide_dir =
                    std::env::temp_dir().join(format!("agentos-root-python-vfs-{suffix}"));
                fs::create_dir_all(&pyodide_dir).expect("create fake Pyodide directory");
                fs::write(
                    pyodide_dir.join("pyodide.mjs"),
                    r#"
export async function loadPyodide() {
  return {
    setStdin(_stdin) {},
    async runPythonAsync(_code) {
      await new Promise(() => setInterval(() => {}, 1_000));
    },
  };
}
"#,
                )
                .expect("write fake Pyodide module");
                fs::write(pyodide_dir.join("pyodide-lock.json"), "{\"packages\":[]}\n")
                    .expect("write fake Pyodide lock");
                for fixture in ["python_stdlib.zip", "pyodide.asm.js", "pyodide.asm.wasm"] {
                    fs::write(pyodide_dir.join(fixture), []).expect("write fake Pyodide asset");
                }
                let host_cwd = pyodide_dir.join("cwd");
                fs::create_dir_all(&host_cwd).expect("create fake Python cwd");
                let engines = sidecar
                    .vms
                    .get(&vm_id)
                    .expect("root Python test VM")
                    .execution_engines
                    .clone();
                let python_limits = {
                    let vm = sidecar.vms.get(&vm_id).expect("root Python test VM");
                    agentos_execution::PythonExecutionLimits {
                        output_buffer_max_bytes: Some(vm.limits.python.output_buffer_max_bytes),
                        execution_timeout_ms: Some(vm.limits.python.execution_timeout_ms),
                        max_old_space_mb: Some(vm.limits.python.max_old_space_mb),
                        vfs_rpc_timeout_ms: Some(vm.limits.python.vfs_rpc_timeout_ms),
                        reactor_work_quantum: Some(vm.limits.reactor.work_quantum),
                        bridge_call_timeout_ms: Some(
                            vm.limits
                                .reactor
                                .operation_deadline_ms
                                .saturating_add(1_000),
                        ),
                        max_open_fds: vm.kernel.resource_limits().max_open_fds,
                    }
                };
                let start_python = |label: &'static str| {
                    let context = engines
                        .python(label)
                        .expect("borrow Python engine")
                        .create_context(agentos_execution::CreatePythonContextRequest {
                            vm_id: vm_id.clone(),
                            pyodide_dist_path: pyodide_dir.clone(),
                        });
                    engines
                        .python(label)
                        .expect("borrow Python engine")
                        .start_execution(agentos_execution::StartPythonExecutionRequest {
                            guest_runtime: Default::default(),
                            limits: python_limits.clone(),
                            vm_id: vm_id.clone(),
                            context_id: context.context_id,
                            code: String::from("print('hold-open')"),
                            file_path: None,
                            env: BTreeMap::new(),
                            cwd: host_cwd.clone(),
                        })
                        .expect("start fake Python execution")
                };
                let execution = start_python("start root Python VFS test execution");
                let attached_execution = start_python("start attached Python VFS test execution");
                let detached_execution = start_python("start detached Python VFS test execution");
                let (
                    kernel_handle,
                    attached_kernel_handle,
                    detached_kernel_handle,
                    runtime_context,
                    limits,
                ) = {
                    let mut vm = sidecar.vms.get_mut(&vm_id).expect("root Python test VM");
                    let kernel_handle = vm
                        .kernel
                        .spawn_process(
                            crate::state::PYTHON_COMMAND,
                            vec![String::from("print('hold-open')")],
                            agentos_kernel::kernel::SpawnOptions {
                                requester_driver: Some(String::from(
                                    crate::state::EXECUTION_DRIVER_NAME,
                                )),
                                cwd: Some(String::from("/")),
                                ..agentos_kernel::kernel::SpawnOptions::default()
                            },
                        )
                        .expect("spawn root Python kernel process");
                    let attached_kernel_handle = vm
                        .kernel
                        .spawn_process(
                            crate::state::PYTHON_COMMAND,
                            vec![String::from("print('hold-open')")],
                            agentos_kernel::kernel::SpawnOptions {
                                requester_driver: Some(String::from(
                                    crate::state::EXECUTION_DRIVER_NAME,
                                )),
                                parent_pid: Some(kernel_handle.pid()),
                                cwd: Some(String::from("/")),
                                ..agentos_kernel::kernel::SpawnOptions::default()
                            },
                        )
                        .expect("spawn attached Python kernel process");
                    let detached_kernel_handle = vm
                        .kernel
                        .spawn_process(
                            crate::state::PYTHON_COMMAND,
                            vec![String::from("print('hold-open')")],
                            agentos_kernel::kernel::SpawnOptions {
                                requester_driver: Some(String::from(
                                    crate::state::EXECUTION_DRIVER_NAME,
                                )),
                                cwd: Some(String::from("/")),
                                ..agentos_kernel::kernel::SpawnOptions::default()
                            },
                        )
                        .expect("spawn detached Python kernel process");
                    (
                        kernel_handle,
                        attached_kernel_handle,
                        detached_kernel_handle,
                        vm.runtime_context.clone(),
                        vm.limits.clone(),
                    )
                };
                let process_id = String::from("proc-root-python-vfs");
                let attached_process_id = String::from("proc-attached-python-vfs");
                let detached_process_id = String::from("proc-detached-python-vfs");
                let write_request =
                    |id, path: &str, content_base64: &str| agentos_execution::PythonVfsRpcRequest {
                        id,
                        method: agentos_execution::PythonVfsRpcMethod::Write,
                        path: path.to_owned(),
                        destination: None,
                        target: None,
                        mode: None,
                        uid: None,
                        gid: None,
                        atime_ms: None,
                        mtime_ms: None,
                        content_base64: Some(content_base64.to_owned()),
                        recursive: false,
                        url: None,
                        http_method: None,
                        headers: BTreeMap::new(),
                        body_base64: None,
                        hostname: None,
                        family: None,
                        port: None,
                        socket_id: None,
                        command: None,
                        args: Vec::new(),
                        argv0: None,
                        cwd: None,
                        env: BTreeMap::new(),
                        shell: false,
                        max_buffer: None,
                        timeout_ms: None,
                    };
                let process_event_capacity = 1;
                let mut process = crate::state::ActiveProcess::new(
                    kernel_handle.pid(),
                    kernel_handle,
                    runtime_context.clone(),
                    limits.clone(),
                    process_event_capacity,
                    crate::protocol::GuestRuntimeKind::Python,
                    crate::state::ActiveExecution::Python(execution),
                );
                process
                    .queue_pending_execution_event(
                        crate::state::ActiveExecutionEvent::PythonVfsRpcRequest(Box::new(
                            write_request(
                                900,
                                "/root-python-vfs-progress.txt",
                                "cm9vdCBweXRob24gdmZzIHByb2dyZXNz",
                            ),
                        )),
                    )
                    .expect("queue root Python VFS write event");
                let mut attached = crate::state::ActiveProcess::new(
                    attached_kernel_handle.pid(),
                    attached_kernel_handle,
                    runtime_context.clone(),
                    limits.clone(),
                    process_event_capacity,
                    crate::protocol::GuestRuntimeKind::Python,
                    crate::state::ActiveExecution::Python(attached_execution),
                );
                attached
                    .queue_pending_execution_event(
                        crate::state::ActiveExecutionEvent::PythonVfsRpcRequest(Box::new(
                            write_request(
                                901,
                                "/attached-python-vfs-progress.txt",
                                "YXR0YWNoZWQgcHl0aG9uIHZmcyBwcm9ncmVzcw==",
                            ),
                        )),
                    )
                    .expect("queue attached Python VFS write event");
                process
                    .child_processes
                    .insert(attached_process_id.clone(), attached);

                let mut detached = crate::state::ActiveProcess::new(
                    detached_kernel_handle.pid(),
                    detached_kernel_handle,
                    runtime_context,
                    limits,
                    process_event_capacity,
                    crate::protocol::GuestRuntimeKind::Python,
                    crate::state::ActiveExecution::Python(detached_execution),
                )
                .with_detached(true);
                detached
                    .queue_pending_execution_event(
                        crate::state::ActiveExecutionEvent::PythonVfsRpcRequest(Box::new(
                            write_request(
                                902,
                                "/detached-python-vfs-progress.txt",
                                "ZGV0YWNoZWQgcHl0aG9uIHZmcyBwcm9ncmVzcw==",
                            ),
                        )),
                    )
                    .expect("queue detached Python VFS write event");
                {
                    let mut vm = sidecar.vms.get_mut(&vm_id).expect("root Python test VM");
                    vm.active_processes.insert(process_id.clone(), process);
                    vm.detached_child_processes
                        .insert(detached_process_id.clone());
                    vm.active_processes
                        .insert(detached_process_id.clone(), detached);
                }

                let protocol = agentos_runtime::RuntimeProtocolConfig::default();
                let operations = OperationTable::from_protocol_config(&protocol);
                let progress_requests = ProgressOperationView::from_protocol_config(&protocol);
                let ingress_budget = test_protocol_budget(4, 4096, "root Python VFS ingress");
                let (writer, output) = test_frame_writer_with_inflight(8, 2);
                let (service_tx, _service_rx) = channel(2);
                let services: Arc<dyn ExtensionServices> = Arc::new(RoutedExtensionServices::new(
                    service_tx,
                    Arc::clone(&sidecar.process_event_notify),
                ));
                let (extension_completion_tx, mut extension_completion_rx) = channel(4);
                let (request_completion_tx, _request_completion_rx) = channel(4);
                let (service_completion_tx, mut service_completion_rx) = channel(4);
                let mut extension_tasks = JoinSet::new();
                let mut request_tasks = JoinSet::new();
                let mut service_tasks = JoinSet::new();
                let mut output_tasks = JoinSet::new();
                let mut active_sessions = BTreeSet::new();
                let mut active_connections = BTreeSet::new();

                route_protocol_frame(
                    test_accounted_frame(
                        ProtocolFrame::RequestFrame(request_frame(
                            901,
                            connection_ownership("conn-gated-root-python-vfs"),
                            RequestPayload::ExtEnvelope(ExtEnvelope {
                                namespace: String::from("dev.rivet.agentos.test.gated"),
                                payload: b"gated".to_vec(),
                            }),
                        )),
                        &ingress_budget,
                    ),
                    &mut sidecar,
                    &services,
                    &operations,
                    &progress_requests,
                    &coordinator,
                    &extension_completion_tx,
                    &request_completion_tx,
                    &mut extension_tasks,
                    &mut request_tasks,
                    &mut output_tasks,
                    &writer,
                    1,
                    &mut active_sessions,
                    &mut active_connections,
                )
                .expect("route gated independent request");
                wait_for_started(&started, &started_notify, 1).await;

                let ownership =
                    crate::protocol::OwnershipScope::vm(connection_id, session_id, &vm_id);
                let mut turn = sidecar
                    .pump_process_events_nowait(&ownership, 3)
                    .expect("claim root and child Python VFS events");
                assert_eq!(turn.python_services.len(), 3);
                assert!(turn.javascript_services.is_empty());
                assert!(turn.child_bridge_services.is_empty());
                turn.python_services.sort_by_key(|target| target.request.id);
                let claimed_targets = turn
                    .python_services
                    .iter()
                    .map(|target| {
                        (
                            target.request.id,
                            target.process_id.clone(),
                            target.child_path.clone(),
                        )
                    })
                    .collect::<Vec<_>>();
                assert_eq!(
                    claimed_targets,
                    vec![
                        (900, process_id.clone(), Vec::new()),
                        (901, process_id.clone(), vec![attached_process_id.clone()]),
                        (902, detached_process_id.clone(), Vec::new()),
                    ],
                    "Python event claims must retain the exact root/child locator"
                );
                let mut pending_python_services = VecDeque::new();
                let mut deferred_python_services = 0usize;
                for target in turn.python_services {
                    let admission = prepare_owned_process_event_service(
                        &mut sidecar,
                        &coordinator,
                        OwnedProcessEventService::Python(target),
                    )
                    .admit_vm_event_nowait()
                    .expect("admit or durably defer Python VFS service");
                    let prepared = match admission {
                        VmEventAdmissionResult::Admitted(prepared) => prepared,
                        VmEventAdmissionResult::Deferred(prepared) => {
                            deferred_python_services += 1;
                            prepared
                        }
                    };
                    pending_python_services.push_back(prepared);
                }
                assert_eq!(deferred_python_services, 1);
                drain_claimed_process_event_services_one_at_a_time(
                    &mut pending_python_services,
                    3,
                    &service_completion_tx,
                    &mut service_completion_rx,
                    &mut service_tasks,
                    &mut sidecar,
                )
                .await;
                assert!(extension_completion_rx.try_recv().is_err());

                let mut vm = sidecar.vms.get_mut(&vm_id).expect("root Python test VM");
                for (path, expected) in [
                    (
                        "/root-python-vfs-progress.txt",
                        b"root python vfs progress".as_slice(),
                    ),
                    (
                        "/attached-python-vfs-progress.txt",
                        b"attached python vfs progress".as_slice(),
                    ),
                    (
                        "/detached-python-vfs-progress.txt",
                        b"detached python vfs progress".as_slice(),
                    ),
                ] {
                    assert_eq!(
                        vm.kernel.read_file(path).expect("read Python VFS write"),
                        expected
                    );
                }
                drop(vm);
                let second_turn = sidecar
                    .pump_process_events_nowait(&ownership, 3)
                    .expect("second Python event turn");
                assert!(second_turn.python_services.is_empty());

                let socket_completion = |request_id| {
                    crate::state::ActiveExecutionEvent::PythonSocketConnectCompletion(Box::new(
                        crate::state::PythonSocketConnectCompletion {
                            request_id,
                            result: Err(crate::state::DeferredRpcError {
                                code: String::from("ECONNREFUSED"),
                                message: String::from("deterministic socket completion"),
                            }),
                        },
                    ))
                };
                {
                    let mut vm = sidecar.vms.get_mut(&vm_id).expect("root Python test VM");
                    vm.active_processes
                        .get_mut(&process_id)
                        .expect("root Python process")
                        .queue_pending_execution_event(socket_completion(910))
                        .expect("queue root Python socket completion");
                    vm.active_processes
                        .get_mut(&process_id)
                        .and_then(|root| root.child_processes.get_mut(&attached_process_id))
                        .expect("attached Python process")
                        .queue_pending_execution_event(socket_completion(911))
                        .expect("queue attached Python socket completion");
                    vm.active_processes
                        .get_mut(&detached_process_id)
                        .expect("detached Python process")
                        .queue_pending_execution_event(socket_completion(912))
                        .expect("queue detached Python socket completion");
                }
                let mut socket_turn = sidecar
                    .pump_process_events_nowait(&ownership, 3)
                    .expect("claim root and child Python socket completions");
                socket_turn
                    .python_socket_completions
                    .sort_by_key(|target| target.completion.request_id);
                assert_eq!(
                    socket_turn
                        .python_socket_completions
                        .iter()
                        .map(|target| (
                            target.completion.request_id,
                            target.process_id.clone(),
                            target.child_path.clone(),
                        ))
                        .collect::<Vec<_>>(),
                    vec![
                        (910, process_id.clone(), Vec::new()),
                        (911, process_id.clone(), vec![attached_process_id.clone()]),
                        (912, detached_process_id.clone(), Vec::new()),
                    ],
                    "Python socket completions must retain the exact responder path"
                );
                let mut pending_socket_completions = VecDeque::new();
                let mut deferred_socket_completions = 0usize;
                for target in socket_turn.python_socket_completions {
                    let admission = prepare_owned_process_event_service(
                        &mut sidecar,
                        &coordinator,
                        OwnedProcessEventService::PythonSocketCompletion(target),
                    )
                    .admit_vm_event_nowait()
                    .expect("admit or durably defer Python socket completion");
                    let prepared = match admission {
                        VmEventAdmissionResult::Admitted(prepared) => prepared,
                        VmEventAdmissionResult::Deferred(prepared) => {
                            deferred_socket_completions += 1;
                            prepared
                        }
                    };
                    pending_socket_completions.push_back(prepared);
                }
                assert_eq!(deferred_socket_completions, 1);
                drain_claimed_process_event_services_one_at_a_time(
                    &mut pending_socket_completions,
                    3,
                    &service_completion_tx,
                    &mut service_completion_rx,
                    &mut service_tasks,
                    &mut sidecar,
                )
                .await;
                let empty_socket_turn = sidecar
                    .pump_process_events_nowait(&ownership, 3)
                    .expect("second Python socket completion turn");
                assert!(empty_socket_turn.python_socket_completions.is_empty());

                release.notify_waiters();
                let gated_completion =
                    tokio::time::timeout(Duration::from_secs(1), extension_completion_rx.recv())
                        .await
                        .expect("gated independent request deadline")
                        .expect("gated independent request completion");
                finish_extension_request(
                    gated_completion,
                    &sidecar,
                    &writer,
                    &mut output_tasks,
                    &mut active_sessions,
                    &mut active_connections,
                )
                .expect("finish gated independent request");
                output_tasks
                    .join_next()
                    .await
                    .expect("gated output task")
                    .expect("gated output join")
                    .expect("gated output publish");
                let ProtocolFrame::ResponseFrame(response) =
                    decode_test_output(output.recv_control().await.expect("gated response"))
                else {
                    panic!("expected gated response frame");
                };
                assert_eq!(response.request_id, 901);

                let (mut process, mut detached) = {
                    let mut vm = sidecar.vms.get_mut(&vm_id).expect("root Python test VM");
                    let process = vm
                        .active_processes
                        .remove(&process_id)
                        .expect("remove fake root Python process");
                    let detached = vm
                        .active_processes
                        .remove(&detached_process_id)
                        .expect("remove fake detached Python process");
                    (process, detached)
                };
                let mut attached = process
                    .child_processes
                    .remove(&attached_process_id)
                    .expect("remove fake attached Python process");
                attached
                    .execution
                    .terminate()
                    .expect("terminate attached Python execution");
                detached
                    .execution
                    .terminate()
                    .expect("terminate detached Python execution");
                process
                    .execution
                    .terminate()
                    .expect("terminate fake root Python execution");
                let _ = fs::remove_dir_all(&pyodide_dir);
                while service_tasks.join_next().await.is_some() {}
                while extension_tasks.join_next().await.is_some() {}
                while request_tasks.join_next().await.is_some() {}
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn python_event_service_panic_is_typed_and_releases_vm_admission() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let config = NativeSidecarConfig::default();
                let runtime = agentos_runtime::SidecarRuntime::process(&config.runtime)
                    .expect("test runtime");
                let mut sidecar = NativeSidecar::with_config_extensions_and_runtime(
                    LocalBridge::default(),
                    config.clone(),
                    Vec::new(),
                    runtime.context(),
                )
                .expect("test sidecar");
                let coordinator = OwnershipCoordinator::from_runtime_config(&config.runtime);
                let connection = coordinator
                    .register_connection("conn-python-service-panic")
                    .expect("register connection");
                let session = connection
                    .open_session("session-python-service-panic")
                    .expect("open session");
                session.open_vm("vm-python-service-panic").expect("open VM");
                let ownership = vm_ownership(
                    "conn-python-service-panic",
                    "session-python-service-panic",
                    "vm-python-service-panic",
                );
                let (panicking, panic_reply) =
                    crate::extension_services::prepared_test_vm_service_command(
                        "service_python_process_event",
                        &coordinator,
                        &ownership,
                        async { panic!("deterministic Python event service panic") },
                    );
                let (completion_tx, mut completion_rx) = channel(2);
                let mut tasks = JoinSet::new();
                schedule_extension_service_command(panicking, &completion_tx, &mut tasks);
                tasks
                    .join_next()
                    .await
                    .expect("panic service supervisor task")
                    .expect("panic service join monitor");
                let error = panic_reply
                    .await
                    .expect("panic reply channel")
                    .expect_err("panic must be observable");
                assert!(error
                    .to_string()
                    .contains("ERR_AGENTOS_EXTENSION_SERVICE_TASK_PANIC"));
                assert!(completion_rx.try_recv().is_err());

                let (ready, ready_reply) =
                    crate::extension_services::prepared_test_vm_service_command(
                        "service_python_process_event",
                        &coordinator,
                        &ownership,
                        async {},
                    );
                schedule_extension_service_command(ready, &completion_tx, &mut tasks);
                let completion = tokio::time::timeout(Duration::from_secs(1), completion_rx.recv())
                    .await
                    .expect("post-panic service completion deadline")
                    .expect("post-panic service completion");
                assert!(completion.complete(&mut sidecar).is_none());
                ready_reply
                    .await
                    .expect("ready reply channel")
                    .expect("VM admission released after panic");
                while tasks.join_next().await.is_some() {}
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn vm_disposal_cancels_and_drains_blocked_python_event_service() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let config = NativeSidecarConfig::default();
                let coordinator = OwnershipCoordinator::from_runtime_config(&config.runtime);
                let connection = coordinator
                    .register_connection("conn-python-service-dispose")
                    .expect("register connection");
                let session = connection
                    .open_session("session-python-service-dispose")
                    .expect("open session");
                session
                    .open_vm("vm-python-service-dispose")
                    .expect("open VM");
                let ownership = vm_ownership(
                    "conn-python-service-dispose",
                    "session-python-service-dispose",
                    "vm-python-service-dispose",
                );
                let started = Arc::new(Notify::new());
                let started_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
                let task_started = Arc::clone(&started);
                let task_started_flag = Arc::clone(&started_flag);
                let (blocked, blocked_reply) =
                    crate::extension_services::prepared_test_vm_service_command(
                        "service_python_process_event",
                        &coordinator,
                        &ownership,
                        async move {
                            task_started_flag.store(true, Ordering::Release);
                            task_started.notify_waiters();
                            std::future::pending::<()>().await;
                        },
                    );
                let (completion_tx, mut completion_rx) = channel(1);
                let mut tasks = JoinSet::new();
                schedule_extension_service_command(blocked, &completion_tx, &mut tasks);
                tokio::time::timeout(Duration::from_secs(1), async {
                    loop {
                        let notified = started.notified();
                        if started_flag.load(Ordering::Acquire) {
                            break;
                        }
                        notified.await;
                    }
                })
                .await
                .expect("blocked Python service start");

                let disposal = coordinator
                    .begin_vm_disposal(&ownership, OperationCancellationReason::Explicit)
                    .expect("begin VM disposal");
                let error = tokio::time::timeout(Duration::from_secs(1), blocked_reply)
                    .await
                    .expect("blocked Python service cancellation deadline")
                    .expect("blocked Python service reply")
                    .expect_err("disposal must cancel the service");
                assert!(error
                    .to_string()
                    .contains("ERR_AGENTOS_EXTENSION_SERVICE_CANCELLED"));
                tasks
                    .join_next()
                    .await
                    .expect("cancelled service supervisor task")
                    .expect("cancelled service join monitor");
                assert!(completion_rx.try_recv().is_err());
                tokio::time::timeout(Duration::from_secs(1), disposal.wait_drained())
                    .await
                    .expect("VM service drain deadline");
                disposal.complete().expect("complete VM disposal");
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn supervised_extension_wait_does_not_block_an_independent_request() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let started = Arc::new(AtomicUsize::new(0));
                let started_notify = Arc::new(Notify::new());
                let release = Arc::new(Notify::new());
                let extension = GatedExtension {
                    started: Arc::clone(&started),
                    started_notify: Arc::clone(&started_notify),
                    release: Arc::clone(&release),
                };
                let config = NativeSidecarConfig::default();
                let runtime = agentos_runtime::SidecarRuntime::process(&config.runtime)
                    .expect("test runtime");
                let runtime_context = runtime.context();
                let mut sidecar = NativeSidecar::with_config_extensions_and_runtime(
                    LocalBridge::default(),
                    config,
                    vec![Box::new(extension)],
                    runtime_context,
                )
                .expect("test sidecar");
                let protocol = agentos_runtime::RuntimeProtocolConfig::default();
                let operations = OperationTable::from_protocol_config(&protocol);
                let progress_requests = ProgressOperationView::from_protocol_config(&protocol);
                let ownership_coordinator = OwnershipCoordinator::from_runtime_config(
                    &NativeSidecarConfig::default().runtime,
                );
                ownership_coordinator
                    .register_connection("conn-gated")
                    .expect("register gated connection");
                ownership_coordinator
                    .register_connection("conn-independent")
                    .expect("register independent connection");
                let ingress_budget = test_protocol_budget(8, 4096, "test request ingress");
                let (writer, output) = test_frame_writer_with_inflight(8, 4);
                let (service_tx, _service_rx) = channel(8);
                let services: Arc<dyn ExtensionServices> = Arc::new(RoutedExtensionServices::new(
                    service_tx,
                    Arc::clone(&sidecar.process_event_notify),
                ));
                let (completion_tx, mut completion_rx) = channel(8);
                let (request_completion_tx, mut request_completion_rx) = channel(8);
                let mut extension_tasks = JoinSet::new();
                let mut request_tasks = JoinSet::new();
                let mut output_tasks = JoinSet::new();
                let mut active_sessions = BTreeSet::new();
                let mut active_connections = BTreeSet::new();

                let prompt = ProtocolFrame::RequestFrame(request_frame(
                    10,
                    connection_ownership("conn-gated"),
                    RequestPayload::ExtEnvelope(ExtEnvelope {
                        namespace: String::from("dev.rivet.agentos.test.gated"),
                        payload: b"prompt-result".to_vec(),
                    }),
                ));
                route_protocol_frame(
                    test_accounted_frame(prompt, &ingress_budget),
                    &mut sidecar,
                    &services,
                    &operations,
                    &progress_requests,
                    &ownership_coordinator,
                    &completion_tx,
                    &request_completion_tx,
                    &mut extension_tasks,
                    &mut request_tasks,
                    &mut output_tasks,
                    &writer,
                    1,
                    &mut active_sessions,
                    &mut active_connections,
                )
                .expect("route prompt");
                wait_for_started(&started, &started_notify, 1).await;

                let independent = ProtocolFrame::RequestFrame(request_frame(
                    11,
                    connection_ownership("conn-independent"),
                    RequestPayload::ExtEnvelope(ExtEnvelope {
                        namespace: String::from("dev.rivet.agentos.test.unknown"),
                        payload: Vec::new(),
                    }),
                ));
                route_protocol_frame(
                    test_accounted_frame(independent, &ingress_budget),
                    &mut sidecar,
                    &services,
                    &operations,
                    &progress_requests,
                    &ownership_coordinator,
                    &completion_tx,
                    &request_completion_tx,
                    &mut extension_tasks,
                    &mut request_tasks,
                    &mut output_tasks,
                    &writer,
                    1,
                    &mut active_sessions,
                    &mut active_connections,
                )
                .expect("route independent request");

                let completion =
                    tokio::time::timeout(Duration::from_secs(1), request_completion_rx.recv())
                        .await
                        .expect("independent request completion deadline")
                        .expect("independent request completion");
                finish_request(
                    completion,
                    &mut sidecar,
                    &ownership_coordinator,
                    &request_completion_tx,
                    &mut request_tasks,
                    &writer,
                    &mut output_tasks,
                    &mut active_sessions,
                    &mut active_connections,
                )
                .expect("finish independent request");
                let output_result = output_tasks
                    .join_next()
                    .await
                    .expect("independent output task")
                    .expect("output task join");
                output_result.expect("independent output publish");
                let ProtocolFrame::ResponseFrame(response) =
                    decode_test_output(output.recv_control().await.expect("independent response"))
                else {
                    panic!("expected response frame");
                };
                assert_eq!(response.request_id, 11);
                assert_eq!(
                    ownership_connection_id(&response.ownership),
                    "conn-independent"
                );
                assert_eq!(operations.snapshot().in_flight_requests, 1);

                release.notify_waiters();
                let completion = tokio::time::timeout(Duration::from_secs(1), completion_rx.recv())
                    .await
                    .expect("prompt completion deadline")
                    .expect("prompt completion");
                finish_extension_request(
                    completion,
                    &sidecar,
                    &writer,
                    &mut output_tasks,
                    &mut active_sessions,
                    &mut active_connections,
                )
                .expect("finish prompt");
                let prompt_output = output_tasks
                    .join_next()
                    .await
                    .expect("prompt output task")
                    .expect("prompt output join");
                prompt_output.expect("prompt output publish");
                let ProtocolFrame::ResponseFrame(response) =
                    decode_test_output(output.recv_control().await.expect("prompt response"))
                else {
                    panic!("expected prompt response frame");
                };
                assert_eq!(response.request_id, 10);
                assert_eq!(ownership_connection_id(&response.ownership), "conn-gated");
                assert_eq!(operations.snapshot().in_flight_requests, 0);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn duplicate_live_progress_request_is_rejected_and_original_acknowledges_once() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let started = Arc::new(AtomicUsize::new(0));
                let started_notify = Arc::new(Notify::new());
                let release = Arc::new(Notify::new());
                let extension = GatedExtension {
                    started: Arc::clone(&started),
                    started_notify: Arc::clone(&started_notify),
                    release: Arc::clone(&release),
                };
                let config = NativeSidecarConfig::default();
                let runtime = agentos_runtime::SidecarRuntime::process(&config.runtime)
                    .expect("test runtime");
                let mut sidecar = NativeSidecar::with_config_extensions_and_runtime(
                    LocalBridge::default(),
                    config,
                    vec![Box::new(extension)],
                    runtime.context(),
                )
                .expect("test sidecar");
                let protocol = agentos_runtime::RuntimeProtocolConfig::default();
                let operations = OperationTable::from_protocol_config(&protocol);
                let progress_requests = ProgressOperationView::from_protocol_config(&protocol);
                let ownership_coordinator = OwnershipCoordinator::from_runtime_config(
                    &NativeSidecarConfig::default().runtime,
                );
                let ingress_budget = test_protocol_budget(4, 4096, "test progress ingress");
                let (writer, output) = test_frame_writer_with_inflight(8, 1);
                let (service_tx, _service_rx) = channel(4);
                let services: Arc<dyn ExtensionServices> = Arc::new(RoutedExtensionServices::new(
                    service_tx,
                    Arc::clone(&sidecar.process_event_notify),
                ));
                let (completion_tx, mut completion_rx) = channel(4);
                let (request_completion_tx, _request_completion_rx) = channel(4);
                let mut extension_tasks = JoinSet::new();
                let mut request_tasks = JoinSet::new();
                let mut output_tasks = JoinSet::new();
                let mut active_sessions = BTreeSet::new();
                let mut active_connections = BTreeSet::new();

                for attempt in 0..2 {
                    let request = ProtocolFrame::RequestFrame(request_frame(
                        70,
                        connection_ownership("conn-progress"),
                        RequestPayload::ExtEnvelope(ExtEnvelope {
                            namespace: String::from("dev.rivet.agentos.test.gated"),
                            payload: b"progress".to_vec(),
                        }),
                    ));
                    route_protocol_frame(
                        test_accounted_frame(request, &ingress_budget),
                        &mut sidecar,
                        &services,
                        &operations,
                        &progress_requests,
                        &ownership_coordinator,
                        &completion_tx,
                        &request_completion_tx,
                        &mut extension_tasks,
                        &mut request_tasks,
                        &mut output_tasks,
                        &writer,
                        1,
                        &mut active_sessions,
                        &mut active_connections,
                    )
                    .expect("route progress request");
                    if attempt == 0 {
                        wait_for_started(&started, &started_notify, 1).await;
                    }
                }

                let ProtocolFrame::ResponseFrame(duplicate) =
                    decode_test_output(output.recv_control().await.expect("duplicate rejection"))
                else {
                    panic!("expected duplicate progress rejection");
                };
                assert_eq!(duplicate.request_id, 70);
                let ResponsePayload::RejectedResponse(rejection) = duplicate.payload else {
                    panic!("expected typed duplicate progress rejection");
                };
                assert_eq!(rejection.code, "ERR_AGENTOS_DUPLICATE_PROGRESS_REQUEST_ID");
                assert_eq!(progress_requests.snapshot().in_flight_requests, 1);
                assert_eq!(operations.snapshot().in_flight_requests, 0);

                release.notify_waiters();
                let completion = completion_rx.recv().await.expect("progress completion");
                finish_extension_request(
                    completion,
                    &sidecar,
                    &writer,
                    &mut output_tasks,
                    &mut active_sessions,
                    &mut active_connections,
                )
                .expect("finish progress request");
                output_tasks
                    .join_next()
                    .await
                    .expect("progress output task")
                    .expect("progress output join")
                    .expect("progress acknowledgement publish");
                let ProtocolFrame::ResponseFrame(acknowledgement) = decode_test_output(
                    output
                        .recv_control()
                        .await
                        .expect("progress acknowledgement"),
                ) else {
                    panic!("expected progress acknowledgement");
                };
                assert_eq!(acknowledgement.request_id, 70);
                assert_eq!(progress_requests.snapshot().in_flight_requests, 0);
            })
            .await;
    }

    #[test]
    fn progress_class_bypasses_saturated_ordinary_ingress() {
        let started = Arc::new(AtomicUsize::new(0));
        let started_notify = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let extension: Arc<dyn Extension> = Arc::new(GatedExtension {
            started,
            started_notify,
            release,
        });
        let extensions = BTreeMap::from([(extension.namespace().to_owned(), extension)]);
        let ordinary_budget = test_protocol_budget(1, 4096, "test ordinary ingress");
        let progress_budget = test_protocol_budget(1, 4096, "test progress ingress");
        let (ordinary_tx, mut ordinary_rx) = channel(1);
        let (progress_tx, mut progress_rx) = channel(1);
        ordinary_tx
            .try_send(Ok(Some(test_accounted_frame(
                ProtocolFrame::RequestFrame(request_frame(
                    1,
                    connection_ownership("conn"),
                    RequestPayload::AuthenticateRequest(AuthenticateRequest {
                        client_name: String::from("queued"),
                        auth_token: String::new(),
                        protocol_version: wire::PROTOCOL_VERSION,
                        bridge_version: agentos_bridge::bridge_contract().version,
                    }),
                )),
                &ordinary_budget,
            ))))
            .expect("fill ordinary ingress");
        let (writer, output) = test_frame_writer(4);
        let progress = ProtocolFrame::RequestFrame(request_frame(
            2,
            connection_ownership("conn"),
            RequestPayload::ExtEnvelope(ExtEnvelope {
                namespace: String::from("dev.rivet.agentos.test.gated"),
                payload: b"progress".to_vec(),
            }),
        ));
        assert_eq!(
            route_decoded_stdin_frame(
                test_decoded_frame(progress),
                &ordinary_tx,
                &progress_tx,
                &writer,
                &ordinary_budget,
                &progress_budget,
                &extensions,
            ),
            StdinReaderFlow::Continue,
        );
        assert!(progress_rx.try_recv().is_ok());
        assert!(ordinary_rx.try_recv().is_ok());
        assert!(output
            .state
            .lock()
            .expect("output state")
            .rejection
            .is_empty());
        output.close();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn duplicate_and_cross_connection_ids_preserve_independent_operations() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let started = Arc::new(AtomicUsize::new(0));
                let started_notify = Arc::new(Notify::new());
                let release = Arc::new(Notify::new());
                let extension = GatedExtension {
                    started: Arc::clone(&started),
                    started_notify: Arc::clone(&started_notify),
                    release: Arc::clone(&release),
                };
                let config = NativeSidecarConfig::default();
                let runtime = agentos_runtime::SidecarRuntime::process(&config.runtime)
                    .expect("test runtime");
                let runtime_context = runtime.context();
                let mut sidecar = NativeSidecar::with_config_extensions_and_runtime(
                    LocalBridge::default(),
                    config,
                    vec![Box::new(extension)],
                    runtime_context,
                )
                .expect("test sidecar");
                let protocol = agentos_runtime::RuntimeProtocolConfig::default();
                let operations = OperationTable::from_protocol_config(&protocol);
                let progress_requests = ProgressOperationView::from_protocol_config(&protocol);
                let ownership_coordinator = OwnershipCoordinator::from_runtime_config(
                    &NativeSidecarConfig::default().runtime,
                );
                for (connection_id, session_id) in [
                    ("conn-a", "session-a"),
                    ("conn-a", "session-b"),
                    ("conn-b", "session-c"),
                ] {
                    let connection = match ownership_coordinator.connection(connection_id) {
                        Ok(connection) => connection,
                        Err(_) => ownership_coordinator
                            .register_connection(connection_id)
                            .expect("register test connection"),
                    };
                    connection
                        .open_session(session_id)
                        .expect("register test session");
                }
                let ingress_budget = test_protocol_budget(8, 4096, "test request ingress");
                let (writer, output) = test_frame_writer_with_inflight(8, 4);
                let (service_tx, _service_rx) = channel(8);
                let services: Arc<dyn ExtensionServices> = Arc::new(RoutedExtensionServices::new(
                    service_tx,
                    Arc::clone(&sidecar.process_event_notify),
                ));
                let (completion_tx, mut completion_rx) = channel(8);
                let (request_completion_tx, _request_completion_rx) = channel(8);
                let mut extension_tasks = JoinSet::new();
                let mut request_tasks = JoinSet::new();
                let mut output_tasks = JoinSet::new();
                let mut active_sessions = BTreeSet::new();
                let mut active_connections = BTreeSet::new();

                let requests = [
                    (20, "conn-a", "session-a"),
                    (21, "conn-a", "session-b"),
                    (20, "conn-b", "session-c"),
                ];
                for (request_id, connection_id, session_id) in requests {
                    let prompt = ProtocolFrame::RequestFrame(request_frame(
                        request_id,
                        session_ownership(connection_id, session_id),
                        RequestPayload::ExtEnvelope(ExtEnvelope {
                            namespace: String::from("dev.rivet.agentos.test.gated"),
                            payload: format!("result-{connection_id}-{request_id}").into_bytes(),
                        }),
                    ));
                    route_protocol_frame(
                        test_accounted_frame(prompt, &ingress_budget),
                        &mut sidecar,
                        &services,
                        &operations,
                        &progress_requests,
                        &ownership_coordinator,
                        &completion_tx,
                        &request_completion_tx,
                        &mut extension_tasks,
                        &mut request_tasks,
                        &mut output_tasks,
                        &writer,
                        1,
                        &mut active_sessions,
                        &mut active_connections,
                    )
                    .expect("route concurrent prompt");
                }

                let duplicate = ProtocolFrame::RequestFrame(request_frame(
                    20,
                    session_ownership("conn-a", "session-a"),
                    RequestPayload::ExtEnvelope(ExtEnvelope {
                        namespace: String::from("dev.rivet.agentos.test.gated"),
                        payload: b"duplicate".to_vec(),
                    }),
                ));
                route_protocol_frame(
                    test_accounted_frame(duplicate, &ingress_budget),
                    &mut sidecar,
                    &services,
                    &operations,
                    &progress_requests,
                    &ownership_coordinator,
                    &completion_tx,
                    &request_completion_tx,
                    &mut extension_tasks,
                    &mut request_tasks,
                    &mut output_tasks,
                    &writer,
                    1,
                    &mut active_sessions,
                    &mut active_connections,
                )
                .expect("reject duplicate prompt");
                let ProtocolFrame::ResponseFrame(duplicate_response) =
                    decode_test_output(output.recv_control().await.expect("duplicate rejection"))
                else {
                    panic!("expected duplicate rejection response");
                };
                assert_eq!(duplicate_response.request_id, 20);
                assert_eq!(
                    ownership_connection_id(&duplicate_response.ownership),
                    "conn-a"
                );
                let ResponsePayload::RejectedResponse(rejection) = duplicate_response.payload
                else {
                    panic!("expected typed duplicate rejection");
                };
                assert_eq!(rejection.code, "ERR_AGENTOS_DUPLICATE_REQUEST_ID");
                assert_eq!(operations.snapshot().in_flight_requests, 3);

                tokio::time::timeout(
                    Duration::from_secs(1),
                    wait_for_started(&started, &started_notify, 3),
                )
                .await
                .expect("different sessions and connections must all start before release");
                assert_eq!(started.load(Ordering::Acquire), 3);

                release.notify_waiters();
                for _ in 0..3 {
                    let completion = completion_rx.recv().await.expect("extension completion");
                    finish_extension_request(
                        completion,
                        &sidecar,
                        &writer,
                        &mut output_tasks,
                        &mut active_sessions,
                        &mut active_connections,
                    )
                    .expect("finish extension request");
                }
                let mut completed = BTreeSet::new();
                for _ in 0..3 {
                    output_tasks
                        .join_next()
                        .await
                        .expect("terminal output task")
                        .expect("terminal output join")
                        .expect("terminal output publish");
                    let ProtocolFrame::ResponseFrame(response) =
                        decode_test_output(output.recv_control().await.expect("terminal response"))
                    else {
                        panic!("expected terminal response");
                    };
                    let OwnershipScope::SessionOwnership(ownership) = response.ownership else {
                        panic!("expected session ownership");
                    };
                    completed.insert((
                        response.request_id,
                        ownership.connection_id,
                        ownership.session_id,
                    ));
                }
                assert_eq!(
                    completed,
                    BTreeSet::from([
                        (20, String::from("conn-a"), String::from("session-a")),
                        (21, String::from("conn-a"), String::from("session-b")),
                        (20, String::from("conn-b"), String::from("session-c")),
                    ])
                );
                assert_eq!(operations.snapshot().in_flight_requests, 0);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn router_reports_count_and_byte_admission_limits_before_terminal_capacity() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let started = Arc::new(AtomicUsize::new(0));
                let started_notify = Arc::new(Notify::new());
                let release = Arc::new(Notify::new());
                let extension = GatedExtension {
                    started: Arc::clone(&started),
                    started_notify: Arc::clone(&started_notify),
                    release: Arc::clone(&release),
                };
                let config = NativeSidecarConfig::default();
                let runtime = agentos_runtime::SidecarRuntime::process(&config.runtime)
                    .expect("test runtime");
                let mut sidecar = NativeSidecar::with_config_extensions_and_runtime(
                    LocalBridge::default(),
                    config,
                    vec![Box::new(extension)],
                    runtime.context(),
                )
                .expect("test sidecar");
                let ingress_budget = test_protocol_budget(8, 4096, "test request ingress");
                let (writer, output) = test_frame_writer_with_inflight(8, 1);
                let (service_tx, _service_rx) = channel(8);
                let services: Arc<dyn ExtensionServices> = Arc::new(RoutedExtensionServices::new(
                    service_tx,
                    Arc::clone(&sidecar.process_event_notify),
                ));
                let ownership_coordinator = OwnershipCoordinator::from_runtime_config(
                    &NativeSidecarConfig::default().runtime,
                );
                ownership_coordinator
                    .register_connection("conn-count")
                    .expect("register count connection");
                ownership_coordinator
                    .register_connection("conn-bytes")
                    .expect("register byte connection");
                let (completion_tx, mut completion_rx) = channel(8);
                let (request_completion_tx, _request_completion_rx) = channel(8);
                let mut extension_tasks = JoinSet::new();
                let mut request_tasks = JoinSet::new();
                let mut output_tasks = JoinSet::new();
                let mut active_sessions = BTreeSet::new();
                let mut active_connections = BTreeSet::new();

                let mut count_protocol = agentos_runtime::RuntimeProtocolConfig::default();
                count_protocol.max_in_flight_requests = 1;
                count_protocol.max_in_flight_request_bytes = 8;
                let count_operations = OperationTable::from_protocol_config(&count_protocol);
                let count_progress_requests =
                    ProgressOperationView::from_protocol_config(&count_protocol);
                for request_id in [30, 31] {
                    let request = ProtocolFrame::RequestFrame(request_frame(
                        request_id,
                        connection_ownership("conn-count"),
                        RequestPayload::ExtEnvelope(ExtEnvelope {
                            namespace: String::from("dev.rivet.agentos.test.gated"),
                            payload: format!("count-{request_id}").into_bytes(),
                        }),
                    ));
                    route_protocol_frame(
                        test_accounted_frame(request, &ingress_budget),
                        &mut sidecar,
                        &services,
                        &count_operations,
                        &count_progress_requests,
                        &ownership_coordinator,
                        &completion_tx,
                        &request_completion_tx,
                        &mut extension_tasks,
                        &mut request_tasks,
                        &mut output_tasks,
                        &writer,
                        1,
                        &mut active_sessions,
                        &mut active_connections,
                    )
                    .expect("route count-limited request");
                    if request_id == 30 {
                        wait_for_started(&started, &started_notify, 1).await;
                    }
                }
                assert_admission_rejection(
                    output.recv_control().await.expect("count rejection"),
                    31,
                    "ERR_AGENTOS_IN_FLIGHT_REQUEST_LIMIT",
                    "runtime.protocol.maxInFlightRequests",
                    1,
                    1,
                    "requests",
                );
                assert_eq!(count_operations.snapshot().in_flight_requests, 1);
                release.notify_waiters();
                let completion = completion_rx.recv().await.expect("count completion");
                finish_extension_request(
                    completion,
                    &sidecar,
                    &writer,
                    &mut output_tasks,
                    &mut active_sessions,
                    &mut active_connections,
                )
                .expect("finish count operation");
                output_tasks
                    .join_next()
                    .await
                    .expect("count terminal task")
                    .expect("count terminal join")
                    .expect("count terminal publish");
                drop(
                    output
                        .recv_control()
                        .await
                        .expect("count terminal response"),
                );
                assert_eq!(count_operations.snapshot().in_flight_requests, 0);

                let mut byte_protocol = agentos_runtime::RuntimeProtocolConfig::default();
                byte_protocol.max_in_flight_requests = 2;
                byte_protocol.max_in_flight_request_bytes = 1;
                let byte_operations = OperationTable::from_protocol_config(&byte_protocol);
                let byte_progress_requests =
                    ProgressOperationView::from_protocol_config(&byte_protocol);
                for request_id in [40, 41] {
                    let request = ProtocolFrame::RequestFrame(request_frame(
                        request_id,
                        connection_ownership("conn-bytes"),
                        RequestPayload::ExtEnvelope(ExtEnvelope {
                            namespace: String::from("dev.rivet.agentos.test.gated"),
                            payload: format!("bytes-{request_id}").into_bytes(),
                        }),
                    ));
                    route_protocol_frame(
                        test_accounted_frame(request, &ingress_budget),
                        &mut sidecar,
                        &services,
                        &byte_operations,
                        &byte_progress_requests,
                        &ownership_coordinator,
                        &completion_tx,
                        &request_completion_tx,
                        &mut extension_tasks,
                        &mut request_tasks,
                        &mut output_tasks,
                        &writer,
                        1,
                        &mut active_sessions,
                        &mut active_connections,
                    )
                    .expect("route byte-limited request");
                    if request_id == 40 {
                        wait_for_started(&started, &started_notify, 2).await;
                    }
                }
                assert_admission_rejection(
                    output.recv_control().await.expect("byte rejection"),
                    41,
                    "ERR_AGENTOS_IN_FLIGHT_REQUEST_BYTE_LIMIT",
                    "runtime.protocol.maxInFlightRequestBytes",
                    1,
                    1,
                    "bytes",
                );
                assert_eq!(byte_operations.snapshot().in_flight_request_bytes, 1);
                release.notify_waiters();
                let completion = completion_rx.recv().await.expect("byte completion");
                finish_extension_request(
                    completion,
                    &sidecar,
                    &writer,
                    &mut output_tasks,
                    &mut active_sessions,
                    &mut active_connections,
                )
                .expect("finish byte operation");
                output_tasks
                    .join_next()
                    .await
                    .expect("byte terminal task")
                    .expect("byte terminal join")
                    .expect("byte terminal publish");
                drop(output.recv_control().await.expect("byte terminal response"));
                assert_eq!(byte_operations.snapshot().in_flight_request_bytes, 0);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn admitted_terminal_response_survives_full_ordinary_output() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let config = NativeSidecarConfig::default();
                let runtime = agentos_runtime::SidecarRuntime::process(&config.runtime)
                    .expect("test runtime");
                let mut sidecar = NativeSidecar::with_config_extensions_and_runtime(
                    LocalBridge::default(),
                    config,
                    vec![Box::new(PanickingExtension)],
                    runtime.context(),
                )
                .expect("test sidecar");
                let protocol = agentos_runtime::RuntimeProtocolConfig::default();
                let operations = OperationTable::from_protocol_config(&protocol);
                let progress_requests = ProgressOperationView::from_protocol_config(&protocol);
                let ownership_coordinator = OwnershipCoordinator::from_runtime_config(
                    &NativeSidecarConfig::default().runtime,
                );
                ownership_coordinator
                    .register_connection("conn-panic")
                    .expect("register panic connection")
                    .open_session("session-panic")
                    .expect("register panic session");
                let ingress_budget = test_protocol_budget(4, 4096, "test request ingress");
                let (writer, output) = test_frame_writer_with_inflight(8, 1);
                assert!(
                    fill_ordinary_output(&writer) > 0,
                    "ordinary output must be saturated before request admission"
                );
                let (service_tx, _service_rx) = channel(4);
                let services: Arc<dyn ExtensionServices> = Arc::new(RoutedExtensionServices::new(
                    service_tx,
                    Arc::clone(&sidecar.process_event_notify),
                ));
                let (completion_tx, mut completion_rx) = channel(4);
                let (request_completion_tx, _request_completion_rx) = channel(4);
                let mut extension_tasks = JoinSet::new();
                let mut request_tasks = JoinSet::new();
                let mut output_tasks = JoinSet::new();
                let mut active_sessions = BTreeSet::new();
                let mut active_connections = BTreeSet::new();

                let request = ProtocolFrame::RequestFrame(request_frame(
                    50,
                    session_ownership("conn-panic", "session-panic"),
                    RequestPayload::ExtEnvelope(ExtEnvelope {
                        namespace: String::from("dev.rivet.agentos.test.panicking"),
                        payload: Vec::new(),
                    }),
                ));
                route_protocol_frame(
                    test_accounted_frame(request, &ingress_budget),
                    &mut sidecar,
                    &services,
                    &operations,
                    &progress_requests,
                    &ownership_coordinator,
                    &completion_tx,
                    &request_completion_tx,
                    &mut extension_tasks,
                    &mut request_tasks,
                    &mut output_tasks,
                    &writer,
                    1,
                    &mut active_sessions,
                    &mut active_connections,
                )
                .expect("route panicking extension");
                assert_eq!(operations.snapshot().in_flight_requests, 1);

                let completion = tokio::time::timeout(Duration::from_secs(1), completion_rx.recv())
                    .await
                    .expect("panic completion deadline")
                    .expect("panic completion");
                finish_extension_request(
                    completion,
                    &sidecar,
                    &writer,
                    &mut output_tasks,
                    &mut active_sessions,
                    &mut active_connections,
                )
                .expect("finish panic response");
                output_tasks
                    .join_next()
                    .await
                    .expect("panic terminal task")
                    .expect("panic terminal join")
                    .expect("panic terminal publish");

                let ProtocolFrame::ResponseFrame(response) = decode_test_output(
                    output
                        .recv_control()
                        .await
                        .expect("panic terminal response"),
                ) else {
                    panic!("expected panic response");
                };
                assert_eq!(response.request_id, 50);
                assert_eq!(ownership_connection_id(&response.ownership), "conn-panic");
                let ResponsePayload::RejectedResponse(rejection) = response.payload else {
                    panic!("expected panic rejection");
                };
                assert!(rejection.message.contains("ERR_AGENTOS_REQUEST_TASK_PANIC"));
                assert_eq!(operations.snapshot().in_flight_requests, 0);
                {
                    let output_state = output.state.lock().expect("output state");
                    assert!(output_state.terminal.is_empty());
                    assert!(output_state.rejection.is_empty());
                }
                output.close();
            })
            .await;
    }

    fn decode_test_output(frame: EncodedProtocolFrame) -> ProtocolFrame {
        WireFrameCodec::new(4096)
            .decode(&frame.bytes)
            .expect("decode test output frame")
    }

    fn assert_admission_rejection(
        frame: EncodedProtocolFrame,
        request_id: RequestId,
        code: &str,
        configuration_path: &str,
        current_usage: u64,
        configured_limit: u64,
        unit: &str,
    ) {
        let ProtocolFrame::ResponseFrame(response) = decode_test_output(frame) else {
            panic!("expected rejection response");
        };
        assert_eq!(response.request_id, request_id);
        let ResponsePayload::RejectedResponse(rejection) = response.payload else {
            panic!("expected typed rejection payload");
        };
        assert_eq!(rejection.code, code);
        assert_eq!(
            rejection.configuration_path.as_deref(),
            Some(configuration_path)
        );
        assert_eq!(rejection.current_usage, Some(current_usage));
        assert_eq!(rejection.configured_limit, Some(configured_limit));
        assert_eq!(rejection.requested, Some(1));
        assert_eq!(rejection.unit.as_deref(), Some(unit));
        assert!(rejection.message.contains("raise runtime.protocol"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn heartbeat_task_emits_periodic_structured_heartbeat_frames() {
        let (write_tx, write_rx) = test_frame_writer(16);
        let _heartbeat_thread = spawn_heartbeat_thread(write_tx, Duration::from_millis(5));

        // Two beats prove the emitter is periodic, not one-shot.
        for beat in 0..2 {
            let frame = decode_test_output(
                write_rx
                    .recv_control()
                    .await
                    .expect("heartbeat control frame"),
            );
            let ProtocolFrame::EventFrame(event) = frame else {
                panic!("expected event frame for beat {beat}, got {frame:?}");
            };
            let event = crate::wire::event_frame_to_compat(event).expect("decode heartbeat frame");
            let crate::protocol::EventPayload::Structured(structured) = event.payload else {
                panic!("expected structured payload for beat {beat}");
            };
            assert_eq!(structured.name, "heartbeat");
        }
        // Dropping the receiver disconnects the channel; the emitter thread
        // observes the send failure and exits cleanly.
    }

    #[test]
    fn read_frame_rejects_oversized_prefix_before_allocating_payload() {
        let codec = WireFrameCodec::new(16);
        let mut reader = Cursor::new((32_u32).to_be_bytes().to_vec());

        let error = read_frame(&codec, &mut reader).expect_err("oversized frame should fail");
        let error = error
            .downcast::<ProtocolCodecError>()
            .expect("protocol codec error");
        assert!(matches!(
            *error,
            ProtocolCodecError::FrameTooLarge { size: 32, max: 16 }
        ));
    }

    #[tokio::test]
    async fn partial_control_frame_is_rejected_from_its_prefix_before_body_allocation() {
        let codec = WireFrameCodec::new(16);
        let (mut host, mut sidecar) = tokio::io::duplex(8);
        host.write_all(&32_u32.to_be_bytes())
            .await
            .expect("write oversized control prefix");

        let error = tokio::time::timeout(
            Duration::from_secs(1),
            read_frame_async(&codec, &mut sidecar),
        )
        .await
        .expect("prefix classification must not wait for an oversized body")
        .expect_err("oversized control frame should fail");
        assert!(error.to_string().contains("limit is 16"));
    }

    #[test]
    fn protocol_lanes_reject_misrouted_frames() {
        let transport = test_callback_transport(FrameSidecarRequestLimits {
            max_pending_responses: 4,
            max_pending_response_bytes: 4096,
            max_frame_bytes: 4096,
        });
        let ingress_budget = test_protocol_budget(4, 4096, "test ordinary ingress");
        let control_budget = test_protocol_budget(4, 4096, "test control ingress");
        let (ordinary_tx, _ordinary_rx) =
            channel::<Result<Option<AccountedProtocolFrame>, String>>(4);
        let (control_tx, _control_rx) = channel::<AccountedProtocolFrame>(4);
        let (shutdown_tx, _shutdown_rx) = channel::<wire::ControlFrame>(1);
        let (overload_tx, _overload_rx) = test_frame_writer(4);

        assert_eq!(
            route_decoded_stdin_frame(
                test_decoded_frame(ProtocolFrame::SidecarResponseFrame(test_sidecar_response(
                    -1,
                    b"wrong-lane",
                ))),
                &ordinary_tx,
                &control_tx,
                &overload_tx,
                &ingress_budget,
                &control_budget,
                &BTreeMap::new(),
            ),
            StdinReaderFlow::Stop,
        );

        let request = ProtocolFrame::RequestFrame(request_frame(
            1,
            connection_ownership("wrong-control-lane"),
            RequestPayload::AuthenticateRequest(AuthenticateRequest {
                client_name: String::from("wrong-lane"),
                auth_token: String::from("token"),
                protocol_version: wire::PROTOCOL_VERSION,
                bridge_version: agentos_bridge::bridge_contract().version,
            }),
        ));
        assert_eq!(
            route_decoded_control_frame(
                test_decoded_frame(request),
                &transport,
                &control_tx,
                &shutdown_tx,
                &control_budget,
            ),
            StdinReaderFlow::Stop,
        );
    }

    #[test]
    fn stdio_work_queues_are_bounded() {
        let capacity = agentos_runtime::DEFAULT_PROTOCOL_MAX_INGRESS_FRAMES;
        let (stdin_tx, _stdin_rx) =
            channel::<Result<Option<AccountedProtocolFrame>, String>>(capacity);
        for _ in 0..capacity {
            enqueue_stdin_frame(&stdin_tx, Ok(None))
                .expect("stdin frame queue should accept capacity");
        }
        assert!(matches!(
            enqueue_stdin_frame(&stdin_tx, Ok(None)),
            Err(StdinFrameQueueError::Full(_))
        ));

        let (event_ready_tx, _event_ready_rx) = channel::<()>(MAX_EVENT_READY_QUEUE);
        event_ready_tx
            .try_send(())
            .expect("event-ready queue should accept capacity");
        assert!(matches!(
            event_ready_tx.try_send(()),
            Err(tokio::sync::mpsc::error::TrySendError::Full(_))
        ));
    }

    #[test]
    fn protocol_budget_enforces_count_and_bytes_and_releases_exactly() {
        let count_budget = test_protocol_budget(1, 8, "test count budget");
        let first = count_budget.reserve(4).expect("first frame fits");
        let error = count_budget
            .reserve(1)
            .expect_err("second frame exceeds count capacity");
        assert_eq!(error.code, "ERR_AGENTOS_PROTOCOL_FRAME_LIMIT");
        assert_eq!(error.path, "runtime.protocol.maxIngressFrames");
        drop(first);
        drop(count_budget.reserve(8).expect("released slot is reusable"));

        let byte_budget = test_protocol_budget(2, 8, "test byte budget");
        let first = byte_budget.reserve(5).expect("first byte charge fits");
        let error = byte_budget
            .reserve(4)
            .expect_err("aggregate byte capacity must be enforced");
        assert_eq!(error.code, "ERR_AGENTOS_PROTOCOL_BYTE_LIMIT");
        assert_eq!(error.path, "runtime.protocol.maxIngressBytes");
        drop(first);
        drop(byte_budget.reserve(8).expect("released bytes are reusable"));
    }

    #[tokio::test]
    async fn protocol_output_queue_physically_separates_control_and_events() {
        let (writer, output) = test_frame_writer(4);
        let event = crate::service::structured_event_frame(
            "conn-priority",
            "ordinary",
            std::collections::HashMap::new(),
        )
        .expect("event frame");
        writer
            .try_send(ProtocolFrame::EventFrame(event))
            .expect("queue ordinary event");
        writer
            .try_send(ProtocolFrame::ResponseFrame(response_frame(
                77,
                connection_ownership("conn-priority"),
                ResponsePayload::RejectedResponse(wire::RejectedResponse {
                    code: String::from("TEST"),
                    message: String::from("control"),
                    limit_name: None,
                    configured_limit: None,
                    current_usage: None,
                    requested: None,
                    unit: None,
                    scope: None,
                    vm_id: None,
                    session_generation: None,
                    capability_id: None,
                    operation: None,
                    configuration_path: None,
                    retryable: None,
                    errno: None,
                }),
            )))
            .expect("queue control response");

        let first = decode_test_output(
            output
                .recv_control()
                .await
                .expect("response/control output"),
        );
        assert!(matches!(first, ProtocolFrame::ResponseFrame(_)));
        let second = decode_test_output(output.recv_ordinary().expect("ordinary event output"));
        assert!(matches!(second, ProtocolFrame::EventFrame(_)));
    }

    fn test_callback_transport(limits: FrameSidecarRequestLimits) -> FrameSidecarRequestTransport {
        let (write_tx, _write_rx) = test_frame_writer(4);
        FrameSidecarRequestTransport::new(write_tx, limits)
    }

    fn test_sidecar_response(request_id: RequestId, payload: &[u8]) -> SidecarResponseFrame {
        SidecarResponseFrame {
            schema: wire::protocol_schema(),
            request_id,
            ownership: connection_ownership("conn-callback"),
            payload: wire::SidecarResponsePayload::ExtEnvelope(ExtEnvelope {
                namespace: String::from("dev.agentos.test.callback"),
                payload: payload.to_vec(),
            }),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn async_callback_waiter_does_not_park_router_and_cleans_up_on_drop() {
        let (writer, output) = test_frame_writer(4);
        let transport = FrameSidecarRequestTransport::new(
            writer,
            FrameSidecarRequestLimits {
                max_pending_responses: 4,
                max_pending_response_bytes: 4096,
                max_frame_bytes: 4096,
            },
        );
        let ownership = wire::ownership_scope_to_compat(connection_ownership("conn-callback"));
        let request = crate::protocol::SidecarRequestFrame::new(
            -8,
            ownership.clone(),
            crate::protocol::SidecarRequestPayload::Ext(crate::protocol::ExtEnvelope {
                namespace: String::from("dev.agentos.test.callback"),
                payload: b"request".to_vec(),
            }),
        );
        let mut response = Box::pin(SidecarRequestTransport::send_request_async(
            &transport,
            request,
            Duration::from_secs(1),
        ));

        let outbound = tokio::select! {
            result = &mut response => panic!("callback completed before a response: {result:?}"),
            outbound = output.recv_control() => outbound.expect("callback request frame"),
        };
        let ProtocolFrame::SidecarRequestFrame(outbound) = decode_test_output(outbound) else {
            panic!("expected sidecar callback request");
        };
        assert_eq!(outbound.request_id, -8);
        assert_eq!(transport.pending_usage().0, 1);

        transport
            .accept_response(test_sidecar_response(-8, b"settled"))
            .expect("registered async response routes directly");
        let response = response.await.expect("async callback response");
        assert_eq!(response.request_id, -8);
        assert_eq!(transport.pending_usage(), (0, 0));

        let dropped = crate::protocol::SidecarRequestFrame::new(
            -9,
            ownership,
            crate::protocol::SidecarRequestPayload::Ext(crate::protocol::ExtEnvelope {
                namespace: String::from("dev.agentos.test.callback"),
                payload: b"drop".to_vec(),
            }),
        );
        let mut dropped = Box::pin(SidecarRequestTransport::send_request_async(
            &transport,
            dropped,
            Duration::from_secs(1),
        ));
        let _outbound = tokio::select! {
            result = &mut dropped => panic!("callback completed before drop: {result:?}"),
            outbound = output.recv_control() => outbound.expect("second callback request frame"),
        };
        assert_eq!(transport.pending_usage().0, 1);
        drop(dropped);
        assert_eq!(transport.pending_usage(), (0, 0));
        output.close();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn async_callback_progress_publication_is_timed_and_cleans_waiter() {
        let (writer, output) = test_frame_writer(4);
        writer
            .try_send_progress(queue_test_sidecar_request(-70))
            .expect("fill progress output reserve");
        let transport = FrameSidecarRequestTransport::new(
            writer,
            FrameSidecarRequestLimits {
                max_pending_responses: 4,
                max_pending_response_bytes: 4096,
                max_frame_bytes: 4096,
            },
        );
        let request = crate::protocol::SidecarRequestFrame::new(
            -71,
            wire::ownership_scope_to_compat(connection_ownership("conn-callback")),
            crate::protocol::SidecarRequestPayload::Ext(crate::protocol::ExtEnvelope {
                namespace: String::from("dev.agentos.test.callback"),
                payload: b"request".to_vec(),
            }),
        );

        let error = SidecarRequestTransport::send_request_async(
            &transport,
            request,
            Duration::from_millis(10),
        )
        .await
        .expect_err("full progress output must meet the callback deadline");
        assert!(error.to_string().contains("timed out publishing"));
        assert_eq!(transport.pending_usage(), (0, 0));
        output.close();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn writer_failure_wakes_all_callback_waiters_with_same_error() {
        let transport = test_callback_transport(FrameSidecarRequestLimits {
            max_pending_responses: 4,
            max_pending_response_bytes: 4096,
            max_frame_bytes: 4096,
        });
        let sync_waiter = transport
            .register_waiter(-80)
            .expect("register synchronous waiter");
        let async_waiter = transport
            .register_async_waiter(-81)
            .expect("register asynchronous waiter");
        assert_eq!(transport.pending_usage(), (2, 0));

        transport
            .fail_all("TEST_WRITER_FAILURE: fd3 closed")
            .expect("fail callback waiters");
        let sync_error = sync_waiter
            .recv()
            .expect("sync waiter wake")
            .expect_err("sync waiter receives failure");
        let async_error = async_waiter
            .await
            .expect("async waiter wake")
            .expect_err("async waiter receives failure");
        assert_eq!(sync_error.to_string(), async_error.to_string());
        assert!(sync_error.to_string().contains("TEST_WRITER_FAILURE"));
        assert_eq!(transport.pending_usage(), (0, 0));
    }

    #[tokio::test]
    async fn ordinary_ingress_saturation_preserves_later_direct_response_progress() {
        let transport = test_callback_transport(FrameSidecarRequestLimits {
            max_pending_responses: 4,
            max_pending_response_bytes: 4096,
            max_frame_bytes: 4096,
        });
        let callback_rx = transport
            .register_waiter(-7)
            .expect("callback waiter should be admitted");
        assert_eq!(transport.pending_usage(), (1, 0));
        let ingress_budget = test_protocol_budget(4, 4096, "test ordinary ingress");
        let control_budget = test_protocol_budget(4, 4096, "test control ingress");
        let (ordinary_tx, mut ordinary_rx) =
            channel::<Result<Option<AccountedProtocolFrame>, String>>(1);
        let (control_tx, mut control_rx) = channel::<AccountedProtocolFrame>(1);
        let (shutdown_tx, mut shutdown_rx) = channel::<wire::ControlFrame>(1);
        let (overload_tx, overload_rx) = test_frame_writer(4);

        let queued = ProtocolFrame::RequestFrame(request_frame(
            1,
            connection_ownership("conn-callback"),
            RequestPayload::AuthenticateRequest(AuthenticateRequest {
                client_name: String::from("queued"),
                auth_token: String::from("token"),
                protocol_version: wire::PROTOCOL_VERSION,
                bridge_version: agentos_bridge::bridge_contract().version,
            }),
        ));
        ordinary_tx
            .try_send(Ok(Some(test_accounted_frame(queued, &ingress_budget))))
            .expect("fill ordinary request lane");

        let overflow = ProtocolFrame::RequestFrame(request_frame(
            2,
            connection_ownership("conn-callback"),
            RequestPayload::AuthenticateRequest(AuthenticateRequest {
                client_name: String::from("overflow"),
                auth_token: String::from("token"),
                protocol_version: wire::PROTOCOL_VERSION,
                bridge_version: agentos_bridge::bridge_contract().version,
            }),
        ));
        assert_eq!(
            route_decoded_stdin_frame(
                test_decoded_frame(overflow),
                &ordinary_tx,
                &control_tx,
                &overload_tx,
                &ingress_budget,
                &control_budget,
                &BTreeMap::new(),
            ),
            StdinReaderFlow::Continue,
            "ordinary saturation must not terminate the reader"
        );
        let ProtocolFrame::ResponseFrame(rejection) = decode_test_output(
            overload_rx
                .recv_control()
                .await
                .expect("overload request should receive an isolated rejection"),
        ) else {
            panic!("expected overload rejection response");
        };
        let ResponsePayload::RejectedResponse(rejection) = rejection.payload else {
            panic!("expected typed rejected response");
        };
        assert_eq!(rejection.code, "ERR_AGENTOS_PROTOCOL_FRAME_LIMIT");
        assert_eq!(
            rejection.configuration_path.as_deref(),
            Some("runtime.protocol.maxIngressFrames")
        );

        assert_eq!(
            route_decoded_control_frame(
                test_decoded_frame(ProtocolFrame::SidecarResponseFrame(test_sidecar_response(
                    -7, b"settled"
                ))),
                &transport,
                &control_tx,
                &shutdown_tx,
                &control_budget,
            ),
            StdinReaderFlow::Continue,
        );
        let delivery = callback_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("direct response should bypass the full ordinary lane")
            .expect("direct response should fit the byte budget");
        assert_eq!(delivery.response.request_id, -7);
        let (pending_count, pending_bytes) = transport.pending_usage();
        assert_eq!(pending_count, 1);
        assert!(pending_bytes > 0 && pending_bytes <= 4096);
        assert!(control_rx.try_recv().is_err());

        assert_eq!(
            route_decoded_control_frame(
                test_decoded_frame(ProtocolFrame::SidecarResponseFrame(test_sidecar_response(
                    99, b"legacy"
                ))),
                &transport,
                &control_tx,
                &shutdown_tx,
                &control_budget,
            ),
            StdinReaderFlow::Continue,
        );
        assert_eq!(
            route_decoded_control_frame(
                test_decoded_frame(ProtocolFrame::ControlFrame(wire::ControlFrame {
                    schema: wire::protocol_schema(),
                    payload: wire::ControlPayload::ShutdownControl(wire::ShutdownControl {
                        reason: String::from("saturated control lane"),
                    }),
                })),
                &transport,
                &control_tx,
                &shutdown_tx,
                &control_budget,
            ),
            StdinReaderFlow::Continue,
            "shutdown must bypass the full unmatched-response lane",
        );
        assert!(matches!(
            shutdown_rx.try_recv(),
            Ok(wire::ControlFrame {
                payload: wire::ControlPayload::ShutdownControl(wire::ShutdownControl { reason }),
                ..
            }) if reason == "saturated control lane"
        ));
        let AccountedProtocolFrame {
            frame: ProtocolFrame::SidecarResponseFrame(control_response),
            ..
        } = control_rx
            .try_recv()
            .expect("unmatched response should enter the control lane")
        else {
            panic!("expected sidecar response on the control lane");
        };
        assert_eq!(control_response.request_id, 99);
        assert!(
            ordinary_rx.try_recv().is_ok(),
            "queued request remains intact"
        );
        drop(delivery);
        assert_eq!(transport.pending_usage(), (0, 0));
    }

    #[test]
    fn callback_waiter_count_limit_is_typed_and_releases_on_cancel() {
        let transport = test_callback_transport(FrameSidecarRequestLimits {
            max_pending_responses: 1,
            max_pending_response_bytes: 4096,
            max_frame_bytes: 4096,
        });
        let _first = transport
            .register_waiter(-1)
            .expect("first waiter should fit");
        let error = transport
            .register_waiter(-2)
            .expect_err("second waiter should exceed the count limit");
        let message = error.to_string();
        assert!(message.contains(PENDING_RESPONSE_COUNT_ERROR_CODE));
        assert!(message.contains(PENDING_RESPONSE_COUNT_CONFIG_PATH));
        assert_eq!(transport.pending_usage(), (1, 0));

        transport.cancel_waiter(-1).expect("cancel first waiter");
        assert_eq!(transport.pending_usage(), (0, 0));
        let _second = transport
            .register_waiter(-2)
            .expect("released count reservation should be reusable");
    }

    #[test]
    fn callback_response_byte_limit_settles_waiter_with_typed_error_and_releases() {
        let transport = test_callback_transport(FrameSidecarRequestLimits {
            max_pending_responses: 2,
            max_pending_response_bytes: 1,
            max_frame_bytes: 4096,
        });
        let receiver = transport
            .register_waiter(-3)
            .expect("waiter count should fit without pre-reserving maximum response bytes");
        assert_eq!(transport.pending_usage(), (1, 0));

        transport
            .accept_response(test_sidecar_response(-3, b"larger than one byte"))
            .expect("registered response should route directly");
        let error = receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("waiter should be settled")
            .expect_err("actual response bytes must enforce the aggregate limit");
        let message = error.to_string();
        assert!(message.contains(PENDING_RESPONSE_BYTES_ERROR_CODE));
        assert!(message.contains(PENDING_RESPONSE_BYTES_CONFIG_PATH));
        assert_eq!(transport.pending_usage(), (0, 0));
        let _second_receiver = transport
            .register_waiter(-4)
            .expect("released count reservation should be reusable");
        assert_eq!(transport.pending_usage(), (1, 0));
    }

    fn queue_test_event(request_id: RequestId) -> ProtocolFrame {
        let mut detail = std::collections::HashMap::new();
        detail.insert(String::from("request_id"), request_id.to_string());
        ProtocolFrame::EventFrame(
            crate::service::structured_event_frame("conn-queue", "queue-test", detail)
                .expect("queue event"),
        )
    }

    fn queue_test_response(request_id: RequestId) -> ProtocolFrame {
        ProtocolFrame::ResponseFrame(response_frame(
            request_id,
            connection_ownership("conn-queue"),
            ResponsePayload::RejectedResponse(wire::RejectedResponse {
                code: String::from("TEST"),
                message: String::from("test response"),
                limit_name: None,
                configured_limit: None,
                current_usage: None,
                requested: None,
                unit: None,
                scope: None,
                vm_id: None,
                session_generation: None,
                capability_id: None,
                operation: None,
                configuration_path: None,
                retryable: None,
                errno: None,
            }),
        ))
    }

    fn queue_test_sidecar_request(request_id: RequestId) -> ProtocolFrame {
        ProtocolFrame::SidecarRequestFrame(crate::wire::SidecarRequestFrame {
            schema: wire::protocol_schema(),
            request_id,
            ownership: connection_ownership("conn-queue"),
            payload: wire::SidecarRequestPayload::ExtEnvelope(ExtEnvelope {
                namespace: String::from("dev.agentos.test.callback"),
                payload: b"callback".to_vec(),
            }),
        })
    }

    fn fill_ordinary_output(writer: &ProtocolFrameWriter) -> usize {
        let mut admitted = 0usize;
        loop {
            match writer.try_send(queue_test_event(admitted as RequestId)) {
                Ok(()) => admitted += 1,
                Err(ProtocolTrySendError::Full(_)) => return admitted,
                Err(error) => panic!("unexpected ordinary output error: {error}"),
            }
        }
    }

    #[tokio::test]
    async fn protocol_output_backpressure_suspends_only_async_producer() {
        let (writer, output) = test_frame_writer(8);
        let admitted = fill_ordinary_output(&writer);
        assert!(admitted > 0, "test must saturate ordinary output");

        // Ordinary saturation does not consume the pre-admission terminal or
        // progress reservations used by production request completion.
        let terminal = queue_test_response(100);
        let terminal_reservation = writer
            .try_reserve_terminal(1)
            .expect("terminal fallback reservation is independent");
        writer
            .publish_reserved_terminal(terminal_reservation, terminal)
            .await
            .expect("terminal response has independent admission");
        let progress = queue_test_response(101);
        let progress_reservation = writer
            .try_reserve_progress(1)
            .expect("progress fallback reservation is independent");
        writer
            .publish_reserved_progress(progress_reservation, progress)
            .await
            .expect("progress acknowledgement has independent admission");

        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let publisher = writer.clone();
        let mut task = tokio::spawn(async move {
            let _ = started_tx.send(());
            publisher
                .publish(ProtocolOutputClass::Ordinary, queue_test_event(999))
                .await
        });
        started_rx.await.expect("publisher started");
        assert!(
            tokio::time::timeout(Duration::from_millis(10), &mut task)
                .await
                .is_err(),
            "publisher should await capacity without parking the test/router task"
        );

        drop(output.recv_ordinary().expect("drain one ordinary frame"));
        tokio::time::timeout(Duration::from_secs(1), &mut task)
            .await
            .expect("publisher woke after capacity release")
            .expect("publisher task joined")
            .expect("publisher admitted frame");
        output.close();
        assert_eq!(writer.ordinary_budget.usage(), (0, 0));
    }

    #[tokio::test]
    async fn full_ordinary_output_does_not_block_cancel_ack_or_shutdown() {
        let (writer, output) = test_frame_writer(8);
        assert!(fill_ordinary_output(&writer) > 0);

        // ACP cancel is classified as a progress request; its exactly-once
        // acknowledgement therefore uses the reserved progress response path.
        let cancel_reservation = writer
            .try_reserve_progress(1)
            .expect("cancel acknowledgement reserve");
        writer
            .publish_reserved_progress(cancel_reservation, queue_test_response(501))
            .await
            .expect("cancel acknowledgement bypasses ordinary output");
        assert!(matches!(
            decode_test_output(output.recv_control().await.expect("cancel acknowledgement")),
            ProtocolFrame::ResponseFrame(frame) if frame.request_id == 501
        ));

        // Shutdown bypasses both request queues and output admission entirely.
        let transport = FrameSidecarRequestTransport::new(
            writer,
            FrameSidecarRequestLimits {
                max_pending_responses: 1,
                max_pending_response_bytes: 4096,
                max_frame_bytes: 4096,
            },
        );
        let control_budget = test_protocol_budget(1, 4096, "test control ingress");
        let (control_tx, _control_rx) = channel::<AccountedProtocolFrame>(1);
        let (shutdown_tx, mut shutdown_rx) = channel::<wire::ControlFrame>(1);
        assert_eq!(
            route_decoded_control_frame(
                test_decoded_frame(ProtocolFrame::ControlFrame(wire::ControlFrame {
                    schema: wire::protocol_schema(),
                    payload: wire::ControlPayload::ShutdownControl(wire::ShutdownControl {
                        reason: String::from("ordinary output saturated"),
                    }),
                })),
                &transport,
                &control_tx,
                &shutdown_tx,
                &control_budget,
            ),
            StdinReaderFlow::Continue
        );
        assert!(matches!(
            shutdown_rx.try_recv(),
            Ok(wire::ControlFrame {
                payload: wire::ControlPayload::ShutdownControl(wire::ShutdownControl { reason }),
                ..
            }) if reason == "ordinary output saturated"
        ));
        output.close();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shutdown_takeover_before_retention_publishes_one_terminal_and_progress_frame() {
        let (writer, output) = test_frame_writer_with_inflight(8, 1);
        let protocol = agentos_runtime::RuntimeProtocolConfig::default();
        let operations = OperationTable::from_protocol_config(&protocol);
        let progress_requests = ProgressOperationView::from_protocol_config(&protocol);
        let ownership = connection_ownership("takeover-before-retention");

        let operation = operations
            .admit(
                RequestOperationKey::new("takeover-before-retention", 610),
                RequestOperationMetadata::new(
                    ownership.clone(),
                    "terminal-race",
                    VmConcurrencyClass::OwnershipOnly,
                ),
                11,
            )
            .expect("admit terminal race");
        assert!(operation.try_mark_terminal());
        let terminal_reservation = writer
            .try_reserve_terminal(writer.terminal_budget.config.max_bytes)
            .expect("reserve original terminal");
        let terminal = writer
            .prepare_reserved_control(
                ProtocolOutputClass::Terminal,
                &writer.terminal_budget,
                terminal_reservation,
                queue_test_response(610),
            )
            .await
            .expect("prepare original terminal");
        let mut forced = Vec::new();
        let error = output
            .enqueue_retained(
                ProtocolOutputClass::Terminal,
                true,
                terminal,
                || {
                    // The queue invokes this only after open/lane/capacity
                    // validation while holding its mutex: this is the exact
                    // boundary immediately before the retention CAS.
                    forced =
                        operations.force_terminalize(OperationCancellationReason::Shutdown);
                    operation.mark_terminal_retained()
                },
                "ERR_AGENTOS_TERMINAL_PUBLICATION_TAKEN_OVER: shutdown took over the terminal response before broker retention",
            )
            .expect_err("taken-over terminal must not enter the broker");
        assert_eq!(forced.len(), 1, "takeover wins before broker retention");
        assert!(error
            .to_string()
            .contains("ERR_AGENTOS_TERMINAL_PUBLICATION_TAKEN_OVER"));
        assert_eq!(output.state.lock().expect("output queue").control_len(), 0);
        assert_eq!(writer.terminal_budget.usage(), (0, 0));
        operation.release();
        assert_eq!(operations.snapshot().in_flight_requests, 0);

        let terminal_outcome = forced.into_iter().next().expect("forced terminal outcome");
        let reservation = writer
            .try_reserve_terminal(writer.terminal_budget.config.max_bytes)
            .expect("reserve forced terminal");
        writer
            .publish_reserved_terminal(
                reservation,
                forced_shutdown_response(
                    &terminal_outcome,
                    OperationCancellationReason::Shutdown,
                    false,
                ),
            )
            .await
            .expect("publish forced terminal");
        let terminal = output.recv_control().await.expect("one terminal frame");
        assert!(matches!(
            decode_test_output(terminal),
            ProtocolFrame::ResponseFrame(response) if response.request_id == 610
        ));
        assert_eq!(output.state.lock().expect("output queue").control_len(), 0);
        assert_eq!(writer.terminal_budget.usage(), (0, 0));

        let progress_request = progress_requests
            .admit_owned(
                RequestOperationKey::new("takeover-before-retention", 611),
                ownership,
                7,
            )
            .expect("admit progress race");
        assert!(progress_request.try_acknowledge());
        let progress_reservation = writer
            .try_reserve_progress(writer.progress_budget.config.max_bytes)
            .expect("reserve original progress acknowledgement");
        let progress = writer
            .prepare_reserved_control(
                ProtocolOutputClass::Progress,
                &writer.progress_budget,
                progress_reservation,
                queue_test_response(611),
            )
            .await
            .expect("prepare original progress acknowledgement");
        let mut forced = Vec::new();
        let error = output
            .enqueue_retained(
                ProtocolOutputClass::Progress,
                true,
                progress,
                || {
                    forced = progress_requests
                        .force_acknowledge(OperationCancellationReason::Shutdown);
                    progress_request.mark_acknowledgement_retained()
                },
                "ERR_AGENTOS_PROGRESS_PUBLICATION_TAKEN_OVER: shutdown took over the progress acknowledgement before broker retention",
            )
            .expect_err("taken-over progress acknowledgement must not enter the broker");
        assert_eq!(forced.len(), 1, "takeover wins before broker retention");
        assert!(error
            .to_string()
            .contains("ERR_AGENTOS_PROGRESS_PUBLICATION_TAKEN_OVER"));
        assert_eq!(output.state.lock().expect("output queue").control_len(), 0);
        assert_eq!(writer.progress_budget.usage(), (0, 0));
        progress_request.release();
        assert_eq!(progress_requests.snapshot().in_flight_requests, 0);

        let progress_outcome = forced.into_iter().next().expect("forced progress outcome");
        let reservation = writer
            .try_reserve_progress(writer.progress_budget.config.max_bytes)
            .expect("reserve forced progress acknowledgement");
        writer
            .publish_reserved_progress(
                reservation,
                forced_shutdown_response(
                    &progress_outcome,
                    OperationCancellationReason::Shutdown,
                    true,
                ),
            )
            .await
            .expect("publish forced progress acknowledgement");
        let progress = output.recv_control().await.expect("one progress frame");
        assert!(matches!(
            decode_test_output(progress),
            ProtocolFrame::ResponseFrame(response) if response.request_id == 611
        ));
        assert_eq!(output.state.lock().expect("output queue").control_len(), 0);
        assert_eq!(writer.progress_budget.usage(), (0, 0));
        output.close();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn broker_retention_before_shutdown_prevents_terminal_and_progress_takeover() {
        let (writer, output) = test_frame_writer_with_inflight(8, 1);
        let protocol = agentos_runtime::RuntimeProtocolConfig::default();
        let operations = OperationTable::from_protocol_config(&protocol);
        let progress_requests = ProgressOperationView::from_protocol_config(&protocol);
        let ownership = connection_ownership("retention-before-takeover");

        let operation = operations
            .admit(
                RequestOperationKey::new("retention-before-takeover", 620),
                RequestOperationMetadata::new(
                    ownership.clone(),
                    "terminal-race",
                    VmConcurrencyClass::OwnershipOnly,
                ),
                11,
            )
            .expect("admit terminal race");
        assert!(operation.try_mark_terminal());
        let terminal_reservation = writer
            .try_reserve_terminal(writer.terminal_budget.config.max_bytes)
            .expect("reserve terminal");
        writer
            .publish_reserved_terminal_for_operation(
                terminal_reservation,
                queue_test_response(620),
                &operation,
            )
            .await
            .expect("retain and publish terminal atomically");
        assert!(operations
            .force_terminalize(OperationCancellationReason::Shutdown)
            .is_empty());
        operation.release();
        assert_eq!(operations.snapshot().in_flight_requests, 0);
        let terminal = output
            .recv_control()
            .await
            .expect("retained terminal frame");
        assert!(matches!(
            decode_test_output(terminal),
            ProtocolFrame::ResponseFrame(response) if response.request_id == 620
        ));
        assert_eq!(output.state.lock().expect("output queue").control_len(), 0);
        assert_eq!(writer.terminal_budget.usage(), (0, 0));

        let progress_request = progress_requests
            .admit_owned(
                RequestOperationKey::new("retention-before-takeover", 621),
                ownership,
                7,
            )
            .expect("admit progress race");
        assert!(progress_request.try_acknowledge());
        let progress_reservation = writer
            .try_reserve_progress(writer.progress_budget.config.max_bytes)
            .expect("reserve progress acknowledgement");
        writer
            .publish_reserved_progress_for_request(
                progress_reservation,
                queue_test_response(621),
                &progress_request,
            )
            .await
            .expect("retain and publish progress acknowledgement atomically");
        assert!(progress_requests
            .force_acknowledge(OperationCancellationReason::Shutdown)
            .is_empty());
        progress_request.release();
        assert_eq!(progress_requests.snapshot().in_flight_requests, 0);
        let progress = output
            .recv_control()
            .await
            .expect("retained progress frame");
        assert!(matches!(
            decode_test_output(progress),
            ProtocolFrame::ResponseFrame(response) if response.request_id == 621
        ));
        assert_eq!(output.state.lock().expect("output queue").control_len(), 0);
        assert_eq!(writer.progress_budget.usage(), (0, 0));
        output.close();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn bounded_shutdown_forces_exactly_one_terminal_and_progress_outcome() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (writer, output) = test_frame_writer_with_inflight(8, 1);
                let operations = OperationTable::from_protocol_config(
                    &agentos_runtime::RuntimeProtocolConfig::default(),
                );
                let progress_requests = ProgressOperationView::from_protocol_config(
                    &agentos_runtime::RuntimeProtocolConfig::default(),
                );
                let ordinary_ownership = session_ownership("shutdown-conn", "session-a");
                let operation = operations
                    .admit(
                        RequestOperationKey::new("shutdown-conn", 700),
                        RequestOperationMetadata::new(
                            ordinary_ownership.clone(),
                            "gated-shutdown",
                            VmConcurrencyClass::OwnershipOnly,
                        ),
                        17,
                    )
                    .expect("admit gated ordinary request");
                let cancellation = operation.cancellation();
                let terminal_reservation = writer
                    .try_reserve_terminal(writer.terminal_budget.config.max_bytes)
                    .expect("reserve original terminal outcome");
                let progress_ownership = vm_ownership("shutdown-conn", "session-a", "vm-a");
                let progress_request = progress_requests
                    .admit_owned(
                        RequestOperationKey::new("shutdown-conn", 702),
                        progress_ownership,
                        13,
                    )
                    .expect("admit active progress request before shutdown");
                let progress_cancellation = progress_request.cancellation();
                let progress_reservation = writer
                    .try_reserve_progress(writer.progress_budget.config.max_bytes)
                    .expect("reserve original progress acknowledgement");

                let callback_transport = FrameSidecarRequestTransport::new(
                    writer.clone(),
                    FrameSidecarRequestLimits {
                        max_pending_responses: 2,
                        max_pending_response_bytes: 4096,
                        max_frame_bytes: 4096,
                    },
                );
                let callback_waiter = callback_transport
                    .register_async_waiter(-700)
                    .expect("register callback waiter");

                let mut drain_state = None;
                assert!(begin_protocol_drain(
                    &mut drain_state,
                    OperationCancellationReason::Shutdown,
                    Duration::from_millis(25),
                    None,
                    &operations,
                    &progress_requests,
                    false,
                ));
                assert_eq!(
                    cancellation.cancelled().await,
                    OperationCancellationReason::Shutdown
                );
                assert_eq!(
                    progress_cancellation.cancelled().await,
                    OperationCancellationReason::Shutdown
                );
                assert!(matches!(
                    operations.check_admission(
                        &RequestOperationKey::new("shutdown-conn", 701),
                        &RequestOperationMetadata::new(
                            ordinary_ownership.clone(),
                            "late",
                            VmConcurrencyClass::OwnershipOnly,
                        ),
                        1,
                    ),
                    Err(RequestAdmissionError::RegistryClosed { .. })
                ));

                progress_requests
                    .check_admission(&RequestOperationKey::new("shutdown-conn", 703), 1)
                    .expect("new progress remains admissible during cooperative drain");

                let mut extension_tasks = JoinSet::new();
                extension_tasks.spawn_local(async move {
                    let held = (
                        operation,
                        terminal_reservation,
                        progress_request,
                        progress_reservation,
                    );
                    std::future::pending::<()>().await;
                    drop(held);
                });
                tokio::task::yield_now().await;

                let output_reader = Arc::clone(&output);
                let reader = thread::spawn(move || {
                    let mut frames = Vec::new();
                    while let Some(frame) = output_reader.recv_combined() {
                        frames.push(frame.bytes.clone());
                    }
                    frames
                });
                let (_completion_tx, mut completion_rx) = channel(2);
                let (_service_completion_tx, mut service_completion_rx) = channel(2);
                let mut service_tasks = JoinSet::new();
                let mut progress_service_tasks = JoinSet::new();
                let (_request_completion_tx, mut request_completion_rx) = channel(2);
                let mut request_tasks = JoinSet::new();
                let mut output_tasks = JoinSet::new();
                let mut ordinary_event_tasks = JoinSet::new();
                let report = finalize_protocol_drain(
                    OperationCancellationReason::Shutdown,
                    Duration::from_millis(250),
                    writer.terminal_budget.config.max_bytes,
                    &operations,
                    &progress_requests,
                    &callback_transport,
                    &writer,
                    &mut service_tasks,
                    &mut progress_service_tasks,
                    &mut service_completion_rx,
                    &mut extension_tasks,
                    &mut completion_rx,
                    &mut request_tasks,
                    &mut request_completion_rx,
                    &mut output_tasks,
                    &mut ordinary_event_tasks,
                )
                .await;

                assert_eq!(report.forced_terminal_responses, 1);
                assert_eq!(report.forced_progress_acknowledgements, 1);
                assert_eq!(report.failed_deliveries, 0);
                assert!(report.control_drained);
                assert_eq!(operations.snapshot().in_flight_requests, 0);
                assert_eq!(progress_requests.snapshot().in_flight_requests, 0);
                assert_eq!(callback_transport.pending_usage(), (0, 0));
                let callback_error = callback_waiter
                    .await
                    .expect("callback waiter was settled")
                    .expect_err("callback waiter receives shutdown failure");
                assert!(callback_error.to_string().contains("PROTOCOL_DRAINED"));

                let frames = reader.join().expect("join output reader");
                assert_eq!(frames.len(), 2);
                let responses = frames
                    .iter()
                    .map(|bytes| writer.codec.decode(bytes).expect("decode forced response"))
                    .collect::<Vec<_>>();
                assert!(responses.iter().any(|frame| matches!(
                    frame,
                    ProtocolFrame::ResponseFrame(response)
                        if response.request_id == 700
                            && matches!(
                                &response.payload,
                                ResponsePayload::RejectedResponse(rejection)
                                    if rejection.code == "ERR_AGENTOS_REQUEST_SHUTDOWN"
                            )
                )));
                assert!(responses.iter().any(|frame| matches!(
                    frame,
                    ProtocolFrame::ResponseFrame(response)
                        if response.request_id == 702
                            && matches!(
                                &response.payload,
                                ResponsePayload::RejectedResponse(rejection)
                                    if rejection.code == "ERR_AGENTOS_PROGRESS_SHUTDOWN"
                            )
                )));
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn disconnected_transport_cancels_and_releases_without_waiting_on_output() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (writer, output) = test_frame_writer_with_inflight(8, 1);
                let operations = OperationTable::from_protocol_config(
                    &agentos_runtime::RuntimeProtocolConfig::default(),
                );
                let progress_requests = ProgressOperationView::from_protocol_config(
                    &agentos_runtime::RuntimeProtocolConfig::default(),
                );
                let ownership = connection_ownership("disconnect-conn");
                let operation = operations
                    .admit(
                        RequestOperationKey::new("disconnect-conn", 800),
                        RequestOperationMetadata::new(
                            ownership.clone(),
                            "gated-disconnect",
                            VmConcurrencyClass::OwnershipOnly,
                        ),
                        17,
                    )
                    .expect("admit disconnected ordinary request");
                let terminal_reservation = writer
                    .try_reserve_terminal(writer.terminal_budget.config.max_bytes)
                    .expect("reserve original terminal outcome");
                let progress_request = progress_requests
                    .admit_owned(
                        RequestOperationKey::new("disconnect-conn", 801),
                        ownership,
                        13,
                    )
                    .expect("admit disconnected progress request");
                let progress_reservation = writer
                    .try_reserve_progress(writer.progress_budget.config.max_bytes)
                    .expect("reserve original progress acknowledgement");

                let callback_transport = FrameSidecarRequestTransport::new(
                    writer.clone(),
                    FrameSidecarRequestLimits {
                        max_pending_responses: 2,
                        max_pending_response_bytes: 4096,
                        max_frame_bytes: 4096,
                    },
                );
                let callback_waiter = callback_transport
                    .register_async_waiter(-800)
                    .expect("register callback waiter");
                output.close_with_error("TEST_TRANSPORT_DISCONNECTED");

                let mut drain_state = None;
                assert!(begin_protocol_drain(
                    &mut drain_state,
                    OperationCancellationReason::TransportClosed,
                    Duration::from_millis(25),
                    Some(String::from("TEST_TRANSPORT_DISCONNECTED")),
                    &operations,
                    &progress_requests,
                    true,
                ));
                assert!(matches!(
                    progress_requests
                        .check_admission(&RequestOperationKey::new("disconnect-conn", 802), 1,),
                    Err(ProgressRequestAdmissionError::RegistryClosed { .. })
                ));

                let mut extension_tasks = JoinSet::new();
                extension_tasks.spawn_local(async move {
                    let held = (
                        operation,
                        terminal_reservation,
                        progress_request,
                        progress_reservation,
                    );
                    std::future::pending::<()>().await;
                    drop(held);
                });
                tokio::task::yield_now().await;
                let (_completion_tx, mut completion_rx) = channel(2);
                let (_service_completion_tx, mut service_completion_rx) = channel(2);
                let mut service_tasks = JoinSet::new();
                let mut progress_service_tasks = JoinSet::new();
                let (_request_completion_tx, mut request_completion_rx) = channel(2);
                let mut request_tasks = JoinSet::new();
                let mut output_tasks = JoinSet::new();
                let mut ordinary_event_tasks = JoinSet::new();
                let report = tokio::time::timeout(
                    Duration::from_millis(100),
                    finalize_protocol_drain(
                        OperationCancellationReason::TransportClosed,
                        Duration::from_millis(25),
                        writer.terminal_budget.config.max_bytes,
                        &operations,
                        &progress_requests,
                        &callback_transport,
                        &writer,
                        &mut service_tasks,
                        &mut progress_service_tasks,
                        &mut service_completion_rx,
                        &mut extension_tasks,
                        &mut completion_rx,
                        &mut request_tasks,
                        &mut request_completion_rx,
                        &mut output_tasks,
                        &mut ordinary_event_tasks,
                    ),
                )
                .await
                .expect("disconnected drain is bounded");

                assert_eq!(report.forced_terminal_responses, 1);
                assert_eq!(report.forced_progress_acknowledgements, 1);
                assert_eq!(report.failed_deliveries, 2);
                assert!(!report.control_drained);
                assert_eq!(operations.snapshot().in_flight_requests, 0);
                assert_eq!(progress_requests.snapshot().in_flight_requests, 0);
                assert_eq!(callback_transport.pending_usage(), (0, 0));
                let callback_error = callback_waiter
                    .await
                    .expect("callback waiter was settled")
                    .expect_err("callback waiter receives disconnect failure");
                assert!(callback_error.to_string().contains("PROTOCOL_DRAINED"));
            })
            .await;
    }

    #[tokio::test]
    async fn protocol_output_close_drains_reservations_and_wakes_publishers() {
        let (writer, output) = test_frame_writer(8);
        fill_ordinary_output(&writer);
        assert!(writer.ordinary_budget.usage().0 > 0);

        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let publisher = writer.clone();
        let task = tokio::spawn(async move {
            let _ = started_tx.send(());
            publisher
                .publish(ProtocolOutputClass::Ordinary, queue_test_event(1000))
                .await
        });
        started_rx.await.expect("publisher started");
        output.close_with_error("TEST_WRITER_FAILURE: stdout closed");

        let error = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("close woke publisher")
            .expect("publisher task joined")
            .expect_err("closed output rejects publisher");
        assert!(matches!(
            error,
            ProtocolTrySendError::Disconnected(ref reason)
                if reason == "TEST_WRITER_FAILURE: stdout closed"
        ));
        let later = writer
            .try_publish(ProtocolOutputClass::Ordinary, queue_test_event(1001))
            .expect_err("future publishers observe the recorded writer failure");
        assert!(matches!(
            later,
            ProtocolTrySendError::Disconnected(ref reason)
                if reason == "TEST_WRITER_FAILURE: stdout closed"
        ));
        assert_eq!(writer.ordinary_budget.usage(), (0, 0));
        assert_eq!(writer.terminal_budget.usage(), (0, 0));
        assert_eq!(writer.progress_budget.usage(), (0, 0));
        assert_eq!(writer.rejection_budget.usage(), (0, 0));
        assert_eq!(writer.control_observability_budget.usage(), (0, 0));
    }

    #[test]
    fn protocol_output_logical_control_reserves_are_independent() {
        let (writer, output) = test_frame_writer(16);
        let mut next_terminal_id = 1;
        let mut terminal_count = 0;
        loop {
            match writer.try_send(queue_test_response(next_terminal_id)) {
                Ok(()) => {
                    terminal_count += 1;
                    next_terminal_id += 1;
                }
                Err(ProtocolTrySendError::Full(_)) => break,
                Err(error) => panic!("unexpected terminal output error: {error}"),
            }
        }
        assert!(terminal_count > 0);

        writer
            .try_send_progress(queue_test_sidecar_request(-1))
            .expect("terminal saturation cannot consume progress reserve");
        writer
            .try_send_rejection(queue_test_response(10_001))
            .expect("terminal saturation cannot consume rejection reserve");

        let heartbeat = crate::service::structured_event_frame(
            HEARTBEAT_CONNECTION_ID,
            "heartbeat",
            std::collections::HashMap::new(),
        )
        .expect("heartbeat frame");
        writer
            .try_send_observability(ProtocolFrame::EventFrame(heartbeat))
            .expect("terminal saturation cannot consume observability reserve");

        output.close();
    }

    #[test]
    fn non_reading_ordinary_output_does_not_park_ingress_router() {
        let (writer, output) = test_frame_writer(8);
        assert!(fill_ordinary_output(&writer) > 0);
        let ingress_budget = test_protocol_budget(2, 4096, "test ordinary ingress");
        let control_budget = test_protocol_budget(2, 4096, "test control ingress");
        let (ordinary_tx, mut ordinary_rx) =
            channel::<Result<Option<AccountedProtocolFrame>, String>>(1);
        let (control_tx, _control_rx) = channel::<AccountedProtocolFrame>(1);
        let request = ProtocolFrame::RequestFrame(request_frame(
            401,
            connection_ownership("conn-non-reading"),
            RequestPayload::AuthenticateRequest(AuthenticateRequest {
                client_name: String::from("non-reading-host"),
                auth_token: String::from("token"),
                protocol_version: wire::PROTOCOL_VERSION,
                bridge_version: agentos_bridge::bridge_contract().version,
            }),
        ));

        assert_eq!(
            route_decoded_stdin_frame(
                test_decoded_frame(request),
                &ordinary_tx,
                &control_tx,
                &writer,
                &ingress_budget,
                &control_budget,
                &BTreeMap::new(),
            ),
            StdinReaderFlow::Continue
        );
        assert!(ordinary_rx
            .try_recv()
            .expect("ingress remains independently admissible")
            .expect("decoded frame")
            .is_some());
        output.close();
    }

    #[tokio::test]
    async fn protocol_output_control_dequeue_uses_deterministic_priority() {
        let (writer, output) = test_frame_writer(16);
        writer
            .try_send(queue_test_response(101))
            .expect("queue terminal response first");
        let heartbeat = crate::service::structured_event_frame(
            HEARTBEAT_CONNECTION_ID,
            "heartbeat",
            std::collections::HashMap::new(),
        )
        .expect("heartbeat frame");
        writer
            .try_send_observability(ProtocolFrame::EventFrame(heartbeat))
            .expect("queue observability second");
        writer
            .try_send_rejection(queue_test_response(202))
            .expect("queue rejection third");
        writer
            .try_send_progress(queue_test_sidecar_request(-303))
            .expect("queue progress last");

        let progress = decode_test_output(output.recv_control().await.expect("progress output"));
        let rejection = decode_test_output(output.recv_control().await.expect("rejection output"));
        let terminal = decode_test_output(output.recv_control().await.expect("terminal output"));
        let observability =
            decode_test_output(output.recv_control().await.expect("observability output"));

        assert!(matches!(
            progress,
            ProtocolFrame::SidecarRequestFrame(frame) if frame.request_id == -303
        ));
        assert!(matches!(
            rejection,
            ProtocolFrame::ResponseFrame(frame) if frame.request_id == 202
        ));
        assert!(matches!(
            terminal,
            ProtocolFrame::ResponseFrame(frame) if frame.request_id == 101
        ));
        assert!(matches!(
            observability,
            ProtocolFrame::EventFrame(wire::EventFrame {
                payload: wire::EventPayload::StructuredEvent(wire::StructuredEvent { name, .. }),
                ..
            }) if name == "heartbeat"
        ));
        output.close();
    }

    #[tokio::test]
    async fn protocol_output_progress_burst_cannot_starve_terminal_response() {
        let (writer, output) = test_frame_writer(16);
        writer
            .try_send_progress(queue_test_sidecar_request(-1))
            .expect("queue first progress frame");
        writer
            .try_send_progress(queue_test_sidecar_request(-2))
            .expect("queue second progress frame");
        writer
            .try_send(queue_test_response(3))
            .expect("queue terminal response");

        let first = decode_test_output(output.recv_control().await.expect("first output"));
        let second = decode_test_output(output.recv_control().await.expect("second output"));
        let third = decode_test_output(output.recv_control().await.expect("third output"));
        assert!(matches!(
            first,
            ProtocolFrame::SidecarRequestFrame(frame) if frame.request_id == -1
        ));
        assert!(matches!(
            second,
            ProtocolFrame::ResponseFrame(frame) if frame.request_id == 3
        ));
        assert!(matches!(
            third,
            ProtocolFrame::SidecarRequestFrame(frame) if frame.request_id == -2
        ));
        output.close();
    }

    #[test]
    fn combined_stdio_preserves_control_priority_ahead_of_ordinary() {
        let (writer, output) = test_frame_writer(16);
        writer
            .try_publish(ProtocolOutputClass::Ordinary, queue_test_event(1))
            .expect("queue ordinary first");
        writer
            .try_publish(ProtocolOutputClass::Terminal, queue_test_response(102))
            .expect("queue terminal second");
        let heartbeat = crate::service::structured_event_frame(
            HEARTBEAT_CONNECTION_ID,
            "heartbeat",
            std::collections::HashMap::new(),
        )
        .expect("heartbeat frame");
        writer
            .try_send_observability(ProtocolFrame::EventFrame(heartbeat))
            .expect("queue observability third");
        writer
            .try_send_rejection(queue_test_response(203))
            .expect("queue rejection fourth");
        writer
            .try_send_progress(queue_test_sidecar_request(-304))
            .expect("queue progress last");

        assert!(matches!(
            decode_test_output(output.recv_combined().expect("progress")),
            ProtocolFrame::SidecarRequestFrame(frame) if frame.request_id == -304
        ));
        assert!(matches!(
            decode_test_output(output.recv_combined().expect("rejection")),
            ProtocolFrame::ResponseFrame(frame) if frame.request_id == 203
        ));
        assert!(matches!(
            decode_test_output(output.recv_combined().expect("terminal")),
            ProtocolFrame::ResponseFrame(frame) if frame.request_id == 102
        ));
        assert!(matches!(
            decode_test_output(output.recv_combined().expect("observability")),
            ProtocolFrame::EventFrame(_)
        ));
        assert!(matches!(
            decode_test_output(output.recv_combined().expect("ordinary")),
            ProtocolFrame::EventFrame(_)
        ));
        output.close();
    }

    #[test]
    fn output_classification_covers_every_producer_shape() {
        let terminal = queue_test_response(301);
        let sidecar_request = queue_test_sidecar_request(-302);
        let ordinary = queue_test_event(303);
        let warning = ProtocolFrame::EventFrame(
            crate::service::structured_event_frame(
                "conn-queue",
                "limit_warning",
                std::collections::HashMap::new(),
            )
            .expect("warning event"),
        );
        let heartbeat = ProtocolFrame::EventFrame(
            crate::service::structured_event_frame(
                HEARTBEAT_CONNECTION_ID,
                "heartbeat",
                std::collections::HashMap::new(),
            )
            .expect("heartbeat event"),
        );

        assert_eq!(
            ProtocolFrameWriter::default_class(&terminal).expect("terminal class"),
            ProtocolOutputClass::Terminal
        );
        assert_eq!(
            ProtocolFrameWriter::default_class(&sidecar_request).expect("callback class"),
            ProtocolOutputClass::Progress
        );
        assert_eq!(
            ProtocolFrameWriter::default_class(&ordinary).expect("event class"),
            ProtocolOutputClass::Ordinary
        );
        assert_eq!(
            ProtocolFrameWriter::default_class(&warning).expect("warning class"),
            ProtocolOutputClass::Observability
        );
        assert_eq!(
            ProtocolFrameWriter::default_class(&heartbeat).expect("heartbeat class"),
            ProtocolOutputClass::Observability
        );
        // Progress acknowledgements and admission errors are both response
        // frames, so their router call sites select these explicit classes.
        assert_ne!(ProtocolOutputClass::Progress, ProtocolOutputClass::Terminal);
        assert_ne!(
            ProtocolOutputClass::Rejection,
            ProtocolOutputClass::Terminal
        );
    }

    #[test]
    fn exhausted_rejection_reserve_stops_ingress_instead_of_dropping_outcome() {
        let (writer, output) = test_frame_writer(4);
        writer
            .try_send_rejection(queue_test_response(1))
            .expect("fill the one-frame rejection reserve");
        let request = ProtocolFrame::RequestFrame(request_frame(
            2,
            connection_ownership("conn-rejection"),
            RequestPayload::AuthenticateRequest(AuthenticateRequest {
                client_name: String::from("overload"),
                auth_token: String::from("token"),
                protocol_version: wire::PROTOCOL_VERSION,
                bridge_version: agentos_bridge::bridge_contract().version,
            }),
        ));
        let outcome = reject_stdin_ingress_frame(
            request,
            ProtocolLimitError {
                code: "ERR_AGENTOS_PROTOCOL_FRAME_LIMIT",
                path: "runtime.protocol.maxIngressFrames",
                label: "stdio ordinary ingress",
                used: 1,
                requested: 1,
                limit: 1,
                unit: "frames",
            },
            &writer,
        );
        assert_eq!(
            outcome,
            StdinReaderFlow::Stop,
            "the reader must close explicitly rather than continue after losing a response"
        );
        output.close();
    }

    #[tokio::test]
    async fn reserved_terminal_capacity_is_consumed_by_exactly_one_response() {
        let (writer, output) = test_frame_writer(8);
        let frame = queue_test_response(22);
        let encoded_bytes = writer.encoded_bytes(&frame).expect("encode response").len();
        let reservation = writer
            .try_reserve_terminal(1)
            .expect("reserve small terminal fallback");
        assert_eq!(writer.terminal_budget.usage().0, 1);
        writer
            .publish_reserved_terminal(reservation, frame)
            .await
            .expect("grow and publish through terminal reservation");
        assert_eq!(writer.terminal_budget.usage(), (1, encoded_bytes));
        drop(output.recv_control().await.expect("response queued"));
        assert_eq!(writer.terminal_budget.usage(), (0, 0));
        output.close();
    }

    #[tokio::test]
    async fn oversized_terminal_uses_reserved_typed_fallback() {
        let codec = WireFrameCodec::new(8192);
        let maximum_encoded_bytes = codec.max_frame_bytes().saturating_add(4);
        let output = Arc::new(ProtocolOutputQueue::new(4, 4));
        let mut protocol = agentos_runtime::RuntimeProtocolConfig::default();
        protocol.max_egress_frames = 4;
        protocol.max_egress_bytes = 4 * maximum_encoded_bytes;
        protocol.max_control_frames = 4;
        protocol.max_control_bytes = 4 * maximum_encoded_bytes;
        protocol.max_in_flight_requests = 1;
        protocol.max_terminal_frames = 1;
        protocol.max_terminal_bytes = 1024;
        protocol.terminal_fallback_bytes = 512;
        protocol.max_progress_frames = 1;
        protocol.max_progress_bytes = maximum_encoded_bytes;
        protocol.max_rejection_frames = 1;
        protocol.max_rejection_bytes = maximum_encoded_bytes;
        let writer = ProtocolFrameWriter::new(
            Arc::clone(&output),
            codec,
            &protocol,
            agentos_runtime::metrics::RuntimeMetrics::new(),
        )
        .expect("test output partitions");
        let oversized = ProtocolFrame::ResponseFrame(response_frame(
            24,
            connection_ownership("conn-queue"),
            ResponsePayload::RejectedResponse(wire::RejectedResponse {
                code: String::from("OVERSIZED"),
                message: "x".repeat(2048),
                limit_name: None,
                configured_limit: None,
                current_usage: None,
                requested: None,
                unit: None,
                scope: None,
                vm_id: None,
                session_generation: None,
                capability_id: None,
                operation: None,
                configuration_path: None,
                retryable: None,
                errno: None,
            }),
        ));
        let reservation = writer
            .try_reserve_terminal(protocol.terminal_fallback_bytes)
            .expect("reserve typed terminal fallback");

        writer
            .publish_reserved_terminal(reservation, oversized)
            .await
            .expect("oversized terminal is replaced by its reserved fallback");
        let ProtocolFrame::ResponseFrame(response) =
            decode_test_output(output.recv_control().await.expect("terminal fallback"))
        else {
            panic!("expected response fallback");
        };
        let ResponsePayload::RejectedResponse(rejection) = response.payload else {
            panic!("expected typed fallback rejection");
        };
        assert_eq!(rejection.code, "ERR_AGENTOS_TERMINAL_RESPONSE_LIMIT");
        assert_eq!(
            rejection.configuration_path.as_deref(),
            Some("runtime.protocol.maxTerminalBytes")
        );
        assert_eq!(writer.terminal_budget.usage(), (0, 0));
        output.close();
    }

    #[tokio::test]
    async fn reserved_progress_capacity_grows_without_consuming_terminal_capacity() {
        let (writer, output) = test_frame_writer(8);
        let frame = queue_test_response(23);
        let encoded_bytes = writer.encoded_bytes(&frame).expect("encode response").len();
        let reservation = writer
            .try_reserve_progress(1)
            .expect("reserve small progress fallback");
        assert_eq!(writer.progress_budget.usage(), (1, 1));
        assert_eq!(writer.terminal_budget.usage(), (0, 0));
        writer
            .publish_reserved_progress(reservation, frame)
            .await
            .expect("grow and publish through progress reservation");
        assert_eq!(writer.progress_budget.usage(), (1, encoded_bytes));
        assert_eq!(writer.terminal_budget.usage(), (0, 0));
        drop(
            output
                .recv_control()
                .await
                .expect("progress response queued"),
        );
        assert_eq!(writer.progress_budget.usage(), (0, 0));
        output.close();
    }

    #[test]
    fn live_event_handoff_is_bounded_nonblocking_and_releases_exactly() {
        let codec = WireFrameCodec::new(4096);
        let mut protocol = agentos_runtime::RuntimeProtocolConfig::default();
        protocol.max_egress_frames = 1;
        protocol.max_egress_bytes = codec.max_frame_bytes().saturating_add(4);
        let (transport, mut receiver) = FrameEventTransport::new(
            codec,
            &protocol,
            agentos_runtime::metrics::RuntimeMetrics::new(),
        );
        let ProtocolFrame::EventFrame(first) = queue_test_event(1) else {
            unreachable!()
        };
        let ProtocolFrame::EventFrame(second) = queue_test_event(2) else {
            unreachable!()
        };

        transport.emit_event(first).expect("first event admitted");
        let error = transport
            .emit_event(second.clone())
            .expect_err("bounded handoff rejects a second retained event");
        assert!(error
            .to_string()
            .contains("runtime.protocol.maxEgressFrames"));
        assert_eq!(transport.budget.usage().0, 1);

        drop(receiver.try_recv().expect("drain retained live event"));
        assert_eq!(transport.budget.usage(), (0, 0));
        transport
            .emit_event(second)
            .expect("released handoff capacity is reusable");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn durable_event_drain_stops_and_rearms_without_loss_or_duplication() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (writer, output) = test_frame_writer(2);
                assert_eq!(fill_ordinary_output(&writer), 2);
                let mut durable = VecDeque::from([1001, 1002, 1003].map(|request_id| {
                    match queue_test_event(request_id) {
                        ProtocolFrame::EventFrame(frame) => frame,
                        _ => unreachable!(),
                    }
                }));
                let mut publisher = JoinSet::<Result<(), String>>::new();
                let (ready_tx, mut ready_rx) = channel::<()>(MAX_EVENT_READY_QUEUE);
                let mut delivered = Vec::new();

                for _ in 0..3 {
                    let frame = durable.pop_front().expect("next durable event");
                    assert!(schedule_durable_event_frame(&writer, &mut publisher, frame,));
                    let retained = durable.len();
                    if let Some(next) = durable.front().cloned() {
                        assert!(
                            !schedule_durable_event_frame(&writer, &mut publisher, next),
                            "the pump must stop while one ordinary publisher is backpressured"
                        );
                        assert_eq!(
                            durable.len(),
                            retained,
                            "stopped drain retains source state"
                        );
                    }

                    let drained = decode_test_output(
                        output
                            .recv_ordinary()
                            .expect("free one ordinary output slot"),
                    );
                    if let ProtocolFrame::EventFrame(wire::EventFrame {
                        payload: wire::EventPayload::StructuredEvent(event),
                        ..
                    }) = drained
                    {
                        if let Some(request_id) = event
                            .detail
                            .get("request_id")
                            .and_then(|value| value.parse::<RequestId>().ok())
                            .filter(|request_id| *request_id >= 1000)
                        {
                            delivered.push(request_id);
                        }
                    }
                    publisher
                        .join_next()
                        .await
                        .expect("durable publisher")
                        .expect("durable publisher join")
                        .expect("durable event publish");
                    rearm_event_ready(&ready_tx).expect("re-arm durable event pump");
                    ready_rx.recv().await.expect("coalesced ready wake");
                }

                while delivered.len() < 3 {
                    let ProtocolFrame::EventFrame(wire::EventFrame {
                        payload: wire::EventPayload::StructuredEvent(event),
                        ..
                    }) = decode_test_output(
                        output
                            .recv_ordinary()
                            .expect("drain published durable event"),
                    )
                    else {
                        panic!("expected structured durable event");
                    };
                    let request_id = event
                        .detail
                        .get("request_id")
                        .expect("durable event id")
                        .parse::<RequestId>()
                        .expect("numeric durable event id");
                    if request_id >= 1000 {
                        delivered.push(request_id);
                    }
                }
                assert!(durable.is_empty());
                assert_eq!(delivered, vec![1001, 1002, 1003]);
                output.close();
            })
            .await;
    }

    // Regression (M5): the active-session set must shrink when a session is
    // disposed. `track_session_state` is insert-only, so the transport relies on
    // `untrack_disposed_sessions` draining the sidecar's disposed-session signal;
    // without it a long-lived connection's set grows per session forever and the
    // ~250us event pump iterates every dead entry.
    #[test]
    fn disposed_sessions_are_untracked_from_active_sessions() {
        let mut active_sessions = BTreeSet::<SessionScope>::new();
        let mut active_connections = BTreeSet::<String>::new();
        track_session_state(
            &ResponsePayload::SessionOpenedResponse(SessionOpenedResponse {
                session_id: String::from("session-1"),
                owner_connection_id: String::from("conn-1"),
            }),
            &mut active_sessions,
            &mut active_connections,
        );
        assert_eq!(
            active_sessions.len(),
            1,
            "opening a session should track it for the event pump"
        );

        untrack_disposed_sessions(
            &[(String::from("conn-1"), String::from("session-1"))],
            &mut active_sessions,
        );
        assert!(
            active_sessions.is_empty(),
            "a disposed session must be removed from the active-session set"
        );
    }

    #[test]
    fn read_frame_decodes_wire_authenticate_request() {
        let codec = WireFrameCodec::new(wire::DEFAULT_MAX_FRAME_BYTES);
        let frame = ProtocolFrame::RequestFrame(request_frame(
            1,
            connection_ownership("client-hint"),
            RequestPayload::AuthenticateRequest(AuthenticateRequest {
                client_name: "probe".to_string(),
                auth_token: "probe-token".to_string(),
                protocol_version: wire::PROTOCOL_VERSION,
                bridge_version: agentos_bridge::bridge_contract().version,
            }),
        ));
        let encoded = codec.encode(&frame).expect("encode wire frame");
        let mut reader = Cursor::new(encoded);

        let decoded = read_frame(&codec, &mut reader)
            .expect("decode bare frame")
            .expect("frame present");

        assert_eq!(decoded.frame, frame);
        assert!(decoded.encoded_bytes > 4);
    }
}

#[derive(Debug, Clone)]
pub(crate) struct LocalBridge {
    started_at: Instant,
    next_timer_id: usize,
    snapshots: BTreeMap<String, FilesystemSnapshot>,
}

impl Default for LocalBridge {
    fn default() -> Self {
        Self {
            started_at: Instant::now(),
            next_timer_id: 0,
            snapshots: BTreeMap::new(),
        }
    }
}

impl BridgeTypes for LocalBridge {
    type Error = LocalBridgeError;
}

impl FilesystemBridge for LocalBridge {
    fn read_file(&mut self, request: ReadFileRequest) -> Result<Vec<u8>, Self::Error> {
        fs::read(Self::host_path(&request.path))
            .map_err(|error| LocalBridgeError::io("read", &request.path, error))
    }

    fn write_file(&mut self, request: WriteFileRequest) -> Result<(), Self::Error> {
        let host_path = Self::host_path(&request.path);
        if let Some(parent) = host_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| LocalBridgeError::io("mkdir", &request.path, error))?;
        }
        fs::write(host_path, request.contents)
            .map_err(|error| LocalBridgeError::io("write", &request.path, error))
    }

    fn stat(&mut self, request: PathRequest) -> Result<FileMetadata, Self::Error> {
        fs::metadata(Self::host_path(&request.path))
            .map(Self::file_metadata)
            .map_err(|error| LocalBridgeError::io("stat", &request.path, error))
    }

    fn lstat(&mut self, request: PathRequest) -> Result<FileMetadata, Self::Error> {
        fs::symlink_metadata(Self::host_path(&request.path))
            .map(Self::file_metadata)
            .map_err(|error| LocalBridgeError::io("lstat", &request.path, error))
    }

    fn read_dir(&mut self, request: ReadDirRequest) -> Result<Vec<DirectoryEntry>, Self::Error> {
        let mut entries = fs::read_dir(Self::host_path(&request.path))
            .map_err(|error| LocalBridgeError::io("readdir", &request.path, error))?
            .map(|entry| {
                let entry =
                    entry.map_err(|error| LocalBridgeError::io("readdir", &request.path, error))?;
                let kind = entry
                    .file_type()
                    .map(Self::file_kind)
                    .map_err(|error| LocalBridgeError::io("readdir", &request.path, error))?;
                Ok(DirectoryEntry {
                    name: entry.file_name().to_string_lossy().into_owned(),
                    kind,
                })
            })
            .collect::<Result<Vec<_>, LocalBridgeError>>()?;
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(entries)
    }

    fn create_dir(&mut self, request: CreateDirRequest) -> Result<(), Self::Error> {
        let host_path = Self::host_path(&request.path);
        if request.recursive {
            fs::create_dir_all(host_path)
        } else {
            fs::create_dir(host_path)
        }
        .map_err(|error| LocalBridgeError::io("mkdir", &request.path, error))
    }

    fn remove_file(&mut self, request: PathRequest) -> Result<(), Self::Error> {
        fs::remove_file(Self::host_path(&request.path))
            .map_err(|error| LocalBridgeError::io("unlink", &request.path, error))
    }

    fn remove_dir(&mut self, request: PathRequest) -> Result<(), Self::Error> {
        fs::remove_dir(Self::host_path(&request.path))
            .map_err(|error| LocalBridgeError::io("rmdir", &request.path, error))
    }

    fn rename(&mut self, request: RenameRequest) -> Result<(), Self::Error> {
        let from_path = Self::host_path(&request.from_path);
        let to_path = Self::host_path(&request.to_path);
        if let Some(parent) = to_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| LocalBridgeError::io("mkdir", &request.to_path, error))?;
        }
        fs::rename(from_path, to_path).map_err(|error| {
            LocalBridgeError::unsupported(format!(
                "rename {} -> {}: {}",
                request.from_path, request.to_path, error
            ))
        })
    }

    fn symlink(&mut self, request: SymlinkRequest) -> Result<(), Self::Error> {
        let link_path = Self::host_path(&request.link_path);
        if let Some(parent) = link_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| LocalBridgeError::io("mkdir", &request.link_path, error))?;
        }
        create_symlink(&request.target_path, link_path)
            .map_err(|error| LocalBridgeError::io("symlink", &request.link_path, error))
    }

    fn read_link(&mut self, request: PathRequest) -> Result<String, Self::Error> {
        fs::read_link(Self::host_path(&request.path))
            .map(|target| target.to_string_lossy().into_owned())
            .map_err(|error| LocalBridgeError::io("readlink", &request.path, error))
    }

    fn chmod(&mut self, request: ChmodRequest) -> Result<(), Self::Error> {
        let permissions = fs::Permissions::from_mode(request.mode);
        fs::set_permissions(Self::host_path(&request.path), permissions)
            .map_err(|error| LocalBridgeError::io("chmod", &request.path, error))
    }

    fn truncate(&mut self, request: TruncateRequest) -> Result<(), Self::Error> {
        OpenOptions::new()
            .write(true)
            .create(false)
            .open(Self::host_path(&request.path))
            .and_then(|file| file.set_len(request.len))
            .map_err(|error| LocalBridgeError::io("truncate", &request.path, error))
    }

    fn exists(&mut self, request: PathRequest) -> Result<bool, Self::Error> {
        Ok(fs::symlink_metadata(Self::host_path(&request.path)).is_ok())
    }
}

impl PermissionBridge for LocalBridge {
    fn check_filesystem_access(
        &mut self,
        request: FilesystemPermissionRequest,
    ) -> Result<PermissionDecision, Self::Error> {
        Ok(PermissionDecision::deny(format!(
            "no static filesystem policy registered for {}:{}",
            request.vm_id, request.path
        )))
    }

    fn check_network_access(
        &mut self,
        request: NetworkPermissionRequest,
    ) -> Result<PermissionDecision, Self::Error> {
        Ok(PermissionDecision::deny(format!(
            "no static network policy registered for {}:{}",
            request.vm_id, request.resource
        )))
    }

    fn check_command_execution(
        &mut self,
        request: CommandPermissionRequest,
    ) -> Result<PermissionDecision, Self::Error> {
        Ok(PermissionDecision::deny(format!(
            "no static child_process policy registered for {}:{}",
            request.vm_id, request.command
        )))
    }

    fn check_environment_access(
        &mut self,
        request: EnvironmentPermissionRequest,
    ) -> Result<PermissionDecision, Self::Error> {
        Ok(PermissionDecision::deny(format!(
            "no static env policy registered for {}:{}",
            request.vm_id, request.key
        )))
    }
}

impl PersistenceBridge for LocalBridge {
    fn load_filesystem_state(
        &mut self,
        request: LoadFilesystemStateRequest,
    ) -> Result<Option<FilesystemSnapshot>, Self::Error> {
        Ok(self.snapshots.get(&request.vm_id).cloned())
    }

    fn flush_filesystem_state(
        &mut self,
        request: FlushFilesystemStateRequest,
    ) -> Result<(), Self::Error> {
        self.snapshots.insert(request.vm_id, request.snapshot);
        Ok(())
    }
}

impl ClockBridge for LocalBridge {
    fn wall_clock(&mut self, _request: ClockRequest) -> Result<SystemTime, Self::Error> {
        Ok(SystemTime::now())
    }

    fn monotonic_clock(&mut self, _request: ClockRequest) -> Result<Duration, Self::Error> {
        Ok(self.started_at.elapsed())
    }

    fn schedule_timer(
        &mut self,
        request: ScheduleTimerRequest,
    ) -> Result<ScheduledTimer, Self::Error> {
        self.next_timer_id += 1;
        Ok(ScheduledTimer {
            timer_id: format!("timer-{}", self.next_timer_id),
            delay: request.delay,
        })
    }
}

impl RandomBridge for LocalBridge {
    fn fill_random_bytes(&mut self, request: RandomBytesRequest) -> Result<Vec<u8>, Self::Error> {
        Ok(vec![0u8; request.len])
    }
}

impl EventBridge for LocalBridge {
    fn emit_structured_event(&mut self, _event: StructuredEventRecord) -> Result<(), Self::Error> {
        Ok(())
    }

    fn emit_diagnostic(&mut self, _event: DiagnosticRecord) -> Result<(), Self::Error> {
        Ok(())
    }

    fn emit_log(&mut self, _event: LogRecord) -> Result<(), Self::Error> {
        Ok(())
    }

    fn emit_lifecycle(&mut self, _event: LifecycleEventRecord) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl ExecutionBridge for LocalBridge {
    fn create_javascript_context(
        &mut self,
        _request: CreateJavascriptContextRequest,
    ) -> Result<GuestContextHandle, Self::Error> {
        Err(LocalBridgeError::unsupported(
            "execution bridge is handled internally by the native sidecar",
        ))
    }

    fn create_wasm_context(
        &mut self,
        _request: CreateWasmContextRequest,
    ) -> Result<GuestContextHandle, Self::Error> {
        Err(LocalBridgeError::unsupported(
            "execution bridge is handled internally by the native sidecar",
        ))
    }

    fn start_execution(
        &mut self,
        _request: StartExecutionRequest,
    ) -> Result<StartedExecution, Self::Error> {
        Err(LocalBridgeError::unsupported(
            "execution bridge is handled internally by the native sidecar",
        ))
    }

    fn write_stdin(&mut self, _request: WriteExecutionStdinRequest) -> Result<(), Self::Error> {
        Err(LocalBridgeError::unsupported(
            "execution bridge is handled internally by the native sidecar",
        ))
    }

    fn close_stdin(&mut self, _request: ExecutionHandleRequest) -> Result<(), Self::Error> {
        Err(LocalBridgeError::unsupported(
            "execution bridge is handled internally by the native sidecar",
        ))
    }

    fn kill_execution(&mut self, _request: KillExecutionRequest) -> Result<(), Self::Error> {
        Err(LocalBridgeError::unsupported(
            "execution bridge is handled internally by the native sidecar",
        ))
    }

    fn poll_execution_event(
        &mut self,
        _request: PollExecutionEventRequest,
    ) -> Result<Option<ExecutionEvent>, Self::Error> {
        Ok(None)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SessionScope {
    connection_id: String,
    session_id: String,
}

impl SessionScope {
    fn ownership_scope(&self) -> OwnershipScope {
        session_ownership(&self.connection_id, &self.session_id)
    }

    fn compat_ownership_scope(&self) -> crate::protocol::OwnershipScope {
        wire::ownership_scope_to_compat(self.ownership_scope())
    }
}

struct PendingLiveEvent {
    event: crate::wire::EventFrame,
    _reservation: ProtocolReservation,
}

/// Live event sink backed by a bounded handoff to one async output drainer.
/// `emit_event` never waits for stdout or broker capacity. Once admitted, the
/// decoded event remains charged until the drainer has transferred it into the
/// encoded output broker, and queue saturation is a typed producer-visible
/// failure rather than silent loss or an unbounded waiter task.
struct FrameEventTransport {
    sender: Sender<PendingLiveEvent>,
    budget: ProtocolBudget,
    codec: WireFrameCodec,
}

impl FrameEventTransport {
    fn new(
        codec: WireFrameCodec,
        protocol: &agentos_runtime::RuntimeProtocolConfig,
        metrics: agentos_runtime::metrics::RuntimeMetrics,
    ) -> (Self, Receiver<PendingLiveEvent>) {
        let (sender, receiver) = channel(protocol.max_egress_frames);
        (
            Self {
                sender,
                budget: ProtocolBudget::new(
                    ProtocolBudgetConfig {
                        max_frames: protocol.max_egress_frames,
                        max_bytes: protocol.max_egress_bytes,
                        frame_path: "runtime.protocol.maxEgressFrames",
                        byte_path: "runtime.protocol.maxEgressBytes",
                        label: "stdio live-event handoff",
                        metric: agentos_runtime::metrics::ChannelMetricClass::StdioEgress,
                    },
                    metrics,
                ),
                codec,
            },
            receiver,
        )
    }
}

impl EventSinkTransport for FrameEventTransport {
    fn emit_event(&self, event: crate::wire::EventFrame) -> Result<(), SidecarError> {
        let encoded_bytes = self
            .codec
            .encode(&ProtocolFrame::EventFrame(event.clone()))
            .map_err(wire_protocol_error)?
            .len();
        let reservation = self
            .budget
            .reserve(encoded_bytes)
            .map_err(|error| SidecarError::Bridge(error.to_string()))?;
        self.sender
            .try_send(PendingLiveEvent {
                event,
                _reservation: reservation,
            })
            .map_err(|error| match error {
                tokio::sync::mpsc::error::TrySendError::Full(_) => SidecarError::Bridge(
                    String::from(
                        "ERR_AGENTOS_LIVE_EVENT_QUEUE_LIMIT: live-event handoff is full; raise runtime.protocol.maxEgressFrames or runtime.protocol.maxEgressBytes",
                    ),
                ),
                tokio::sync::mpsc::error::TrySendError::Closed(_) => SidecarError::Bridge(
                    String::from("ERR_AGENTOS_PROTOCOL_OUTPUT_CLOSED: live-event output is closed"),
                ),
            })
    }
}

#[derive(Debug, Clone, Copy)]
struct FrameSidecarRequestLimits {
    max_pending_responses: usize,
    max_pending_response_bytes: usize,
    max_frame_bytes: usize,
}

impl FrameSidecarRequestLimits {
    fn from_config(config: &NativeSidecarConfig) -> Self {
        Self {
            max_pending_responses: config.runtime.protocol.max_pending_responses,
            max_pending_response_bytes: config.runtime.protocol.max_pending_response_bytes,
            max_frame_bytes: config.max_frame_bytes,
        }
    }
}

#[derive(Debug)]
struct PendingResponseReservation {
    counter: Arc<AtomicUsize>,
    amount: usize,
}

impl Drop for PendingResponseReservation {
    fn drop(&mut self) {
        self.counter.fetch_sub(self.amount, Ordering::AcqRel);
    }
}

#[derive(Debug)]
struct PendingSidecarResponse {
    response: SidecarResponseFrame,
    _count_reservation: PendingResponseReservation,
    _byte_reservation: PendingResponseReservation,
}

type PendingSidecarResponseResult = Result<PendingSidecarResponse, SidecarError>;

struct PendingSidecarResponseTarget {
    sender: PendingSidecarResponseSender,
    count_reservation: PendingResponseReservation,
}

enum PendingSidecarResponseSender {
    Sync(mpsc::SyncSender<PendingSidecarResponseResult>),
    Async(tokio::sync::oneshot::Sender<PendingSidecarResponseResult>),
}

struct FrameSidecarRequestTransport {
    writer: ProtocolFrameWriter,
    pending: Arc<Mutex<BTreeMap<RequestId, PendingSidecarResponseTarget>>>,
    pending_count: Arc<AtomicUsize>,
    pending_response_bytes: Arc<AtomicUsize>,
    limits: FrameSidecarRequestLimits,
}

struct AsyncSidecarWaiterGuard<'a> {
    transport: &'a FrameSidecarRequestTransport,
    request_id: RequestId,
}

impl Drop for AsyncSidecarWaiterGuard<'_> {
    fn drop(&mut self) {
        if let Err(error) = self.transport.cancel_waiter(self.request_id) {
            eprintln!(
                "failed to cancel dropped async sidecar response waiter {}: {error}",
                self.request_id
            );
        }
    }
}

impl FrameSidecarRequestTransport {
    fn new(writer: ProtocolFrameWriter, limits: FrameSidecarRequestLimits) -> Self {
        Self {
            writer,
            pending: Arc::new(Mutex::new(BTreeMap::new())),
            pending_count: Arc::new(AtomicUsize::new(0)),
            pending_response_bytes: Arc::new(AtomicUsize::new(0)),
            limits,
        }
    }

    fn reserve(
        counter: &Arc<AtomicUsize>,
        amount: usize,
        limit: usize,
        code: &'static str,
        config_path: &'static str,
        resource_name: &'static str,
    ) -> Result<PendingResponseReservation, SidecarError> {
        let mut observed = counter.load(Ordering::Acquire);
        loop {
            let Some(next) = observed.checked_add(amount) else {
                return Err(SidecarError::Bridge(format!(
                    "{code}: {resource_name} reservation overflowed usize; limit={limit}; raise {config_path}"
                )));
            };
            if next > limit {
                return Err(SidecarError::Bridge(format!(
                    "{code}: {resource_name} would reach {next}, exceeding limit {limit}; raise {config_path}"
                )));
            }
            match counter.compare_exchange_weak(observed, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => {
                    return Ok(PendingResponseReservation {
                        counter: Arc::clone(counter),
                        amount,
                    });
                }
                Err(current) => observed = current,
            }
        }
    }

    fn register_waiter(
        &self,
        request_id: RequestId,
    ) -> Result<mpsc::Receiver<PendingSidecarResponseResult>, SidecarError> {
        let mut pending = self.pending.lock().map_err(|_| {
            SidecarError::Bridge(String::from("sidecar callback waiter map lock poisoned"))
        })?;
        if pending.contains_key(&request_id) {
            return Err(SidecarError::Bridge(format!(
                "duplicate sidecar callback request id {request_id}"
            )));
        }
        let count_reservation = Self::reserve(
            &self.pending_count,
            1,
            self.limits.max_pending_responses,
            PENDING_RESPONSE_COUNT_ERROR_CODE,
            PENDING_RESPONSE_COUNT_CONFIG_PATH,
            "pending sidecar response count",
        )?;
        let (sender, receiver) = mpsc::sync_channel(1);
        pending.insert(
            request_id,
            PendingSidecarResponseTarget {
                sender: PendingSidecarResponseSender::Sync(sender),
                count_reservation,
            },
        );
        Ok(receiver)
    }

    fn register_async_waiter(
        &self,
        request_id: RequestId,
    ) -> Result<tokio::sync::oneshot::Receiver<PendingSidecarResponseResult>, SidecarError> {
        let mut pending = self.pending.lock().map_err(|_| {
            SidecarError::Bridge(String::from("sidecar callback waiter map lock poisoned"))
        })?;
        if pending.contains_key(&request_id) {
            return Err(SidecarError::Bridge(format!(
                "duplicate sidecar callback request id {request_id}"
            )));
        }
        let count_reservation = Self::reserve(
            &self.pending_count,
            1,
            self.limits.max_pending_responses,
            PENDING_RESPONSE_COUNT_ERROR_CODE,
            PENDING_RESPONSE_COUNT_CONFIG_PATH,
            "pending sidecar response count",
        )?;
        let (sender, receiver) = tokio::sync::oneshot::channel();
        pending.insert(
            request_id,
            PendingSidecarResponseTarget {
                sender: PendingSidecarResponseSender::Async(sender),
                count_reservation,
            },
        );
        Ok(receiver)
    }

    fn cancel_waiter(&self, request_id: RequestId) -> Result<(), SidecarError> {
        self.pending
            .lock()
            .map_err(|_| {
                SidecarError::Bridge(String::from("sidecar callback waiter map lock poisoned"))
            })?
            .remove(&request_id);
        Ok(())
    }

    fn fail_all(&self, message: &str) -> Result<(), SidecarError> {
        let pending = {
            let mut pending = self.pending.lock().map_err(|_| {
                SidecarError::Bridge(String::from("sidecar callback waiter map lock poisoned"))
            })?;
            std::mem::take(&mut *pending)
        };
        for (request_id, target) in pending {
            let PendingSidecarResponseTarget {
                sender,
                count_reservation: _,
            } = target;
            let error = Err(SidecarError::Io(message.to_owned()));
            match sender {
                PendingSidecarResponseSender::Sync(sender) => {
                    if let Err(send_error) = sender.try_send(error) {
                        tracing::debug!(
                            target: "agentos_native_sidecar::stdio",
                            request_id,
                            ?send_error,
                            "failed to wake synchronous callback waiter during transport failure",
                        );
                    }
                }
                PendingSidecarResponseSender::Async(sender) => {
                    if sender.send(error).is_err() {
                        tracing::debug!(
                            target: "agentos_native_sidecar::stdio",
                            request_id,
                            "async callback waiter was already gone during transport failure",
                        );
                    }
                }
            }
        }
        Ok(())
    }

    /// Settle a registered synchronous callback without touching either stdin
    /// dispatch lane. `Err(response)` means this is an unmatched legacy
    /// response and the reader must route it through the bounded control lane.
    fn accept_response(
        &self,
        response: SidecarResponseFrame,
    ) -> Result<(), Box<SidecarResponseFrame>> {
        let request_id = response.request_id;
        let target = {
            let mut pending = match self.pending.lock() {
                Ok(pending) => pending,
                Err(_) => {
                    eprintln!("sidecar callback waiter map lock poisoned");
                    return Err(Box::new(response));
                }
            };
            pending.remove(&response.request_id)
        };
        let Some(target) = target else {
            return Err(Box::new(response));
        };

        let PendingSidecarResponseTarget {
            sender,
            count_reservation,
        } = target;
        let response_bytes = WireFrameCodec::new(self.limits.max_frame_bytes)
            .encode(&ProtocolFrame::SidecarResponseFrame(response.clone()))
            // The four-byte framing prefix is consumed by the decoder and is
            // not part of retained decoded-response state.
            .map(|bytes| bytes.len().saturating_sub(4))
            .map_err(wire_protocol_error);
        let delivery = response_bytes.and_then(|response_bytes| {
            let byte_reservation = Self::reserve(
                &self.pending_response_bytes,
                response_bytes,
                self.limits.max_pending_response_bytes,
                PENDING_RESPONSE_BYTES_ERROR_CODE,
                PENDING_RESPONSE_BYTES_CONFIG_PATH,
                "pending sidecar response bytes",
            )?;
            Ok(PendingSidecarResponse {
                response,
                _count_reservation: count_reservation,
                _byte_reservation: byte_reservation,
            })
        });

        // Each registered waiter has exactly one producer. Both sender forms
        // settle without blocking the control reader; a timed-out receiver
        // simply drops the response and its reservations here.
        match sender {
            PendingSidecarResponseSender::Sync(sender) => match sender.try_send(delivery) {
                Ok(()) => {}
                Err(mpsc::TrySendError::Full(_)) => eprintln!(
                    "sidecar callback response channel unexpectedly full for request_id={request_id}"
                ),
                Err(mpsc::TrySendError::Disconnected(_)) => tracing::debug!(
                    target: "agentos_native_sidecar::stdio",
                    request_id,
                    "sidecar callback response arrived after its waiter disconnected",
                ),
            },
            PendingSidecarResponseSender::Async(sender) => {
                if sender.send(delivery).is_err() {
                    tracing::debug!(
                        target: "agentos_native_sidecar::stdio",
                        request_id,
                        "sidecar callback response arrived after its async waiter disconnected",
                    );
                }
            }
        }
        Ok(())
    }

    #[cfg(test)]
    fn pending_usage(&self) -> (usize, usize) {
        (
            self.pending_count.load(Ordering::Acquire),
            self.pending_response_bytes.load(Ordering::Acquire),
        )
    }
}

impl SidecarRequestTransport for FrameSidecarRequestTransport {
    fn send_request(
        &self,
        request: crate::protocol::SidecarRequestFrame,
        timeout: Duration,
    ) -> Result<crate::protocol::SidecarResponseFrame, SidecarError> {
        let request =
            wire::sidecar_request_frame_from_compat(request).map_err(wire_protocol_error)?;
        let receiver = self.register_waiter(request.request_id)?;
        // Synchronous compatibility callbacks may not park a trusted runtime
        // worker waiting for output capacity. Progress output has independent
        // reserved admission; saturation returns a typed, retryable failure.
        let write_deadline = Instant::now() + timeout;
        let write_result = self
            .writer
            .try_send_progress(ProtocolFrame::SidecarRequestFrame(request.clone()))
            .map_err(|error| error.to_string());
        if let Err(message) = write_result {
            if let Err(error) = self.cancel_waiter(request.request_id) {
                eprintln!("failed to cancel sidecar response waiter after write failure: {error}");
            }
            return Err(SidecarError::Io(format!(
                "failed to write sidecar request frame: {message}"
            )));
        }
        let response_timeout = write_deadline.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(response_timeout) {
            Ok(Ok(response)) => wire::sidecar_response_frame_to_compat(response.response)
                .map_err(wire_protocol_error),
            Ok(Err(error)) => Err(error),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Err(error) = self.cancel_waiter(request.request_id) {
                    eprintln!("failed to cancel timed-out sidecar response waiter: {error}");
                }
                Err(SidecarError::Io(format!(
                    "timed out waiting for sidecar response after {}s",
                    timeout.as_secs()
                )))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(SidecarError::Io(String::from(
                "sidecar response waiter disconnected",
            ))),
        }
    }

    fn send_request_async<'a>(
        &'a self,
        request: crate::protocol::SidecarRequestFrame,
        timeout: Duration,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<crate::protocol::SidecarResponseFrame, SidecarError>,
                > + 'a,
        >,
    > {
        Box::pin(async move {
            let request =
                wire::sidecar_request_frame_from_compat(request).map_err(wire_protocol_error)?;
            let receiver = self.register_async_waiter(request.request_id)?;
            let _waiter_guard = AsyncSidecarWaiterGuard {
                transport: self,
                request_id: request.request_id,
            };
            let deadline = Instant::now() + timeout;
            match tokio::time::timeout(
                timeout,
                self.writer.publish(
                    ProtocolOutputClass::Progress,
                    ProtocolFrame::SidecarRequestFrame(request.clone()),
                ),
            )
            .await
            {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    return Err(SidecarError::Io(format!(
                        "failed to publish sidecar request frame: {error}"
                    )));
                }
                Err(_) => {
                    return Err(SidecarError::Io(format!(
                        "timed out publishing sidecar request frame after {}s",
                        timeout.as_secs()
                    )));
                }
            }
            let response_timeout = deadline.saturating_duration_since(Instant::now());
            match tokio::time::timeout(response_timeout, receiver).await {
                Ok(Ok(Ok(response))) => wire::sidecar_response_frame_to_compat(response.response)
                    .map_err(wire_protocol_error),
                Ok(Ok(Err(error))) => Err(error),
                Ok(Err(_)) => Err(SidecarError::Io(String::from(
                    "sidecar response waiter disconnected",
                ))),
                Err(_) => Err(SidecarError::Io(format!(
                    "timed out waiting for sidecar response after {}s",
                    timeout.as_secs()
                ))),
            }
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocalBridgeError {
    message: String,
}

impl LocalBridgeError {
    fn unsupported(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    fn io(operation: &str, path: &str, error: io::Error) -> Self {
        Self::unsupported(format!("{operation} {path}: {error}"))
    }
}

impl fmt::Display for LocalBridgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for LocalBridgeError {}

impl LocalBridge {
    fn host_path(path: &str) -> PathBuf {
        let candidate = Path::new(path);
        if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(candidate)
        }
    }

    fn file_metadata(metadata: fs::Metadata) -> FileMetadata {
        FileMetadata {
            mode: metadata.permissions().mode(),
            size: metadata.size(),
            kind: Self::file_kind(metadata.file_type()),
        }
    }

    fn file_kind(file_type: fs::FileType) -> agentos_bridge::FileKind {
        if file_type.is_file() {
            agentos_bridge::FileKind::File
        } else if file_type.is_dir() {
            agentos_bridge::FileKind::Directory
        } else if file_type.is_symlink() {
            agentos_bridge::FileKind::SymbolicLink
        } else {
            agentos_bridge::FileKind::Other
        }
    }
}
