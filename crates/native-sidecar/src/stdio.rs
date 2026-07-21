use crate::wire::{
    self, AuthenticatedResponse, ExtEnvelope, OwnershipScope, ProtocolCodecError, ProtocolFrame,
    RequestFrame, RequestId, RequestPayload, ResponseFrame, ResponsePayload, SessionOpenedResponse,
    SidecarResponseFrame, WireDispatchResult, WireFrameCodec,
};
use crate::{
    EventSinkTransport, Extension, ExtensionInterruptRequest, NativeSidecar, NativeSidecarConfig,
    SidecarError, SidecarRequestTransport,
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
use agentos_native_sidecar_core::{
    generated_wire_blocking_extension_interrupt, BlockingExtensionInterrupt,
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
    changed: Arc<Condvar>,
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

impl ProtocolReservation {
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
        self.budget.changed.notify_all();
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
            changed: Arc::new(Condvar::new()),
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

    fn reserve_until(
        &self,
        bytes: usize,
        deadline: Option<Instant>,
    ) -> Result<ProtocolReservation, ProtocolLimitError> {
        if bytes > self.config.max_bytes {
            return self.reserve(bytes);
        }
        loop {
            match self.reserve(bytes) {
                Ok(reservation) => return Ok(reservation),
                Err(error) => {
                    let state = self.state.lock().unwrap_or_else(|poisoned| {
                        eprintln!(
                            "ERR_AGENTOS_PROTOCOL_BUDGET_POISONED: recovering {} budget while waiting",
                            self.config.label
                        );
                        poisoned.into_inner()
                    });
                    if state.frames < self.config.max_frames
                        && state.bytes.saturating_add(bytes) <= self.config.max_bytes
                    {
                        drop(state);
                        continue;
                    }
                    match deadline {
                        Some(deadline) => {
                            let remaining = deadline.saturating_duration_since(Instant::now());
                            if remaining.is_zero() {
                                return Err(error);
                            }
                            let (_state, timeout) = self
                                .changed
                                .wait_timeout(state, remaining)
                                .unwrap_or_else(|poisoned| {
                                    eprintln!(
                                        "ERR_AGENTOS_PROTOCOL_BUDGET_POISONED: recovering {} budget after timed wait",
                                        self.config.label
                                    );
                                    poisoned.into_inner()
                                });
                            if timeout.timed_out() {
                                return Err(error);
                            }
                        }
                        None => {
                            drop(self.changed.wait(state).unwrap_or_else(|poisoned| {
                                eprintln!(
                                    "ERR_AGENTOS_PROTOCOL_BUDGET_POISONED: recovering {} budget after wait",
                                    self.config.label
                                );
                                poisoned.into_inner()
                            }));
                        }
                    }
                }
            }
        }
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
        self.budget.changed.notify_all();
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
    control: VecDeque<EncodedProtocolFrame>,
    open: bool,
}

#[derive(Debug)]
struct ProtocolOutputQueue {
    ordinary_capacity: usize,
    control_capacity: usize,
    state: Mutex<ProtocolOutputQueueState>,
    available: Condvar,
    control_available: Notify,
}

impl ProtocolOutputQueue {
    fn new(ordinary_capacity: usize, control_capacity: usize) -> Self {
        Self {
            ordinary_capacity,
            control_capacity,
            state: Mutex::new(ProtocolOutputQueueState {
                ordinary: VecDeque::new(),
                control: VecDeque::new(),
                open: true,
            }),
            available: Condvar::new(),
            control_available: Notify::new(),
        }
    }

    fn enqueue(
        &self,
        control: bool,
        frame: EncodedProtocolFrame,
    ) -> Result<(), ProtocolTrySendError> {
        let mut state = self.state.lock().unwrap_or_else(|poisoned| {
            eprintln!("ERR_AGENTOS_PROTOCOL_OUTPUT_QUEUE_POISONED: recovering output queue");
            poisoned.into_inner()
        });
        if !state.open {
            return Err(ProtocolTrySendError::Disconnected);
        }
        let (queue, capacity) = if control {
            (&mut state.control, self.control_capacity)
        } else {
            (&mut state.ordinary, self.ordinary_capacity)
        };
        if queue.len() >= capacity {
            return Err(ProtocolTrySendError::Full);
        }
        queue.push_back(frame);
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
            if let Some(frame) = state
                .control
                .pop_front()
                .or_else(|| state.ordinary.pop_front())
            {
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
                if let Some(frame) = state.control.pop_front() {
                    return Some(frame);
                }
                if !state.open {
                    return None;
                }
            }
            notified.await;
        }
    }

    fn close(&self) {
        let mut state = self.state.lock().unwrap_or_else(|poisoned| {
            eprintln!("ERR_AGENTOS_PROTOCOL_OUTPUT_QUEUE_POISONED: recovering output queue");
            poisoned.into_inner()
        });
        state.open = false;
        drop(state);
        self.available.notify_all();
        self.control_available.notify_waiters();
    }
}

#[derive(Clone)]
struct ProtocolFrameWriter {
    output: Arc<ProtocolOutputQueue>,
    codec: WireFrameCodec,
    ordinary_budget: ProtocolBudget,
    control_budget: ProtocolBudget,
}

#[derive(Debug)]
enum ProtocolTrySendError {
    Full,
    Disconnected,
    Rejected(io::Error),
}

impl ProtocolFrameWriter {
    fn encode_with_reservation(
        &self,
        frame: ProtocolFrame,
        mut reservation: ProtocolReservation,
    ) -> Result<EncodedProtocolFrame, io::Error> {
        let bytes = self
            .codec
            .encode(&frame)
            .map_err(wire_protocol_error)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
        reservation.shrink_bytes(bytes.len());
        Ok(EncodedProtocolFrame {
            bytes,
            _reservation: reservation,
        })
    }

    fn prepare(
        &self,
        frame: ProtocolFrame,
        budget: &ProtocolBudget,
    ) -> Result<EncodedProtocolFrame, io::Error> {
        let maximum_encoded_bytes = self.codec.max_frame_bytes().saturating_add(4);
        let reservation = budget
            .reserve(maximum_encoded_bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::WouldBlock, error.to_string()))?;
        self.encode_with_reservation(frame, reservation)
    }

    fn prepare_until(
        &self,
        frame: ProtocolFrame,
        budget: &ProtocolBudget,
        deadline: Option<Instant>,
    ) -> Result<EncodedProtocolFrame, io::Error> {
        let maximum_encoded_bytes = self.codec.max_frame_bytes().saturating_add(4);
        let reservation = budget
            .reserve_until(maximum_encoded_bytes, deadline)
            .map_err(|error| io::Error::new(io::ErrorKind::WouldBlock, error.to_string()))?;
        self.encode_with_reservation(frame, reservation)
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

    fn send(&self, frame: ProtocolFrame) -> Result<(), io::Error> {
        let control = Self::is_control(&frame)?;
        let encoded = if control {
            self.prepare_until(frame, &self.control_budget, None)?
        } else {
            self.prepare_until(frame, &self.ordinary_budget, None)?
        };
        self.output
            .enqueue(control, encoded)
            .map_err(|error| match error {
                ProtocolTrySendError::Full => io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "stdout queue remained full after admission",
                ),
                ProtocolTrySendError::Disconnected => {
                    io::Error::new(io::ErrorKind::BrokenPipe, "stdout writer disconnected")
                }
                ProtocolTrySendError::Rejected(error) => error,
            })
    }

    fn send_until(&self, frame: ProtocolFrame, deadline: Instant) -> Result<(), io::Error> {
        let control = Self::is_control(&frame)?;
        let encoded = if control {
            self.prepare_until(frame, &self.control_budget, Some(deadline))?
        } else {
            self.prepare_until(frame, &self.ordinary_budget, Some(deadline))?
        };
        self.output
            .enqueue(control, encoded)
            .map_err(|error| match error {
                ProtocolTrySendError::Full => io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "stdout queue remained full after admission",
                ),
                ProtocolTrySendError::Disconnected => {
                    io::Error::new(io::ErrorKind::BrokenPipe, "stdout writer disconnected")
                }
                ProtocolTrySendError::Rejected(error) => error,
            })
    }

    fn try_send(&self, frame: ProtocolFrame) -> Result<(), ProtocolTrySendError> {
        let control = Self::is_control(&frame).map_err(ProtocolTrySendError::Rejected)?;
        let encoded = if control {
            self.prepare(frame, &self.control_budget)
                .map_err(ProtocolTrySendError::Rejected)?
        } else {
            self.prepare(frame, &self.ordinary_budget)
                .map_err(ProtocolTrySendError::Rejected)?
        };
        self.output.enqueue(control, encoded)
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
    let config = NativeSidecarConfig {
        compile_cache_root: Some(default_compile_cache_root()),
        ..NativeSidecarConfig::default()
    };
    let runtime = agentos_runtime::SidecarRuntime::process(&config.runtime)?;
    let runtime_context = runtime.context();
    // Initialize the embedded V8 runtime + platform now, on the long-lived main
    // thread, so it is never first-initialized on a transient worker thread (e.g. a
    // VM-create snapshot pre-warm thread that then exits — which corrupts V8's
    // platform and wedges later isolate creation). Best-effort.
    if let Err(error) = agentos_execution::v8_host::ensure_runtime_initialized(&runtime_context) {
        eprintln!("embedded V8 runtime init failed at startup: {error}");
    }
    runtime.block_on(run_async(extensions, config, runtime_context, control_fd))
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
    let mut sidecar = NativeSidecar::with_config_extensions_and_runtime(
        LocalBridge::default(),
        config,
        extensions,
        runtime_context.clone(),
    )?;
    let mut active_sessions = BTreeSet::<SessionScope>::new();
    let mut active_connections = BTreeSet::<String>::new();
    let (stdin_tx, mut stdin_rx) =
        channel::<Result<Option<AccountedProtocolFrame>, String>>(protocol.max_ingress_frames);
    let (stdin_control_tx, mut stdin_control_rx) =
        channel::<AccountedProtocolFrame>(protocol.max_control_frames);
    let (shutdown_tx, mut shutdown_rx) = channel::<wire::ControlFrame>(MAX_SHUTDOWN_QUEUE);
    let stdin_gauge = agentos_bridge::queue_tracker::register_queue(
        TrackedLimit::SidecarStdinFrames,
        protocol.max_ingress_frames,
    );
    let (event_ready_tx, mut event_ready_rx) = channel::<()>(MAX_EVENT_READY_QUEUE);
    let ordinary_egress_budget = ProtocolBudget::new(
        ProtocolBudgetConfig {
            max_frames: protocol.max_egress_frames,
            max_bytes: protocol.max_egress_bytes,
            frame_path: "runtime.protocol.maxEgressFrames",
            byte_path: "runtime.protocol.maxEgressBytes",
            label: "stdio ordinary egress",
            metric: agentos_runtime::metrics::ChannelMetricClass::StdioEgress,
        },
        runtime_context.metrics().clone(),
    );
    let control_egress_budget = ProtocolBudget::new(
        ProtocolBudgetConfig {
            max_frames: protocol.max_control_frames,
            max_bytes: protocol.max_control_bytes,
            frame_path: "runtime.protocol.maxControlFrames",
            byte_path: "runtime.protocol.maxControlBytes",
            label: "stdio response/control egress",
            metric: agentos_runtime::metrics::ChannelMetricClass::StdioEgress,
        },
        runtime_context.metrics().clone(),
    );
    let output_queue = Arc::new(ProtocolOutputQueue::new(
        protocol.max_egress_frames,
        protocol.max_control_frames,
    ));
    let frame_writer = ProtocolFrameWriter {
        output: Arc::clone(&output_queue),
        codec: codec.clone(),
        ordinary_budget: ordinary_egress_budget,
        control_budget: control_egress_budget,
    };
    let (write_error_tx, mut write_error_rx) = channel::<String>(MAX_TRANSPORT_ERROR_QUEUE);

    // Forward limit-registry near-capacity warnings to the host: the global sink
    // fires (edge-triggered, from arbitrary threads) into this channel, and the
    // event loop below drains it and emits a `StructuredEvent` (name
    // "limit_warning"). Keep the host-visible warning path bounded too: a
    // broken consumer must not turn observability into an unbounded heap sink.
    // The callback must never block an arbitrary producer, so it uses bounded
    // nonblocking admission and logs an explicit host-visible drop.
    let (limit_warning_tx, mut limit_warning_rx) =
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
    // Live event sink: lets an extension stream `session/update` (and other)
    // events to stdout mid-dispatch instead of batching them until the request
    // resolves. Shares the same bounded frame writer as the batch path, so
    // ordering and backpressure are identical.
    let event_transport = Arc::new(FrameEventTransport::new(frame_writer.clone()));
    sidecar.set_event_transport(event_transport);
    // Every execution backend and deferred sidecar producer shares this
    // process-level edge. Durable bounded queues retain the data; the notify is
    // only a coalesced prompt to drain them, so no recurring session poll is
    // needed.
    let process_event_notify = Arc::clone(&sidecar.process_event_notify);
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
                output_queue.close();
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
                    control_output_queue.close();
                    break;
                }
            }
        })?;
    }
    let _heartbeat_thread = spawn_heartbeat_thread(frame_writer.clone(), HEARTBEAT_INTERVAL);

    if let Some(mut control_reader) = control_reader.take() {
        // AGENTOS_THREAD_SITE: constant-stdio-reader
        thread::spawn({
            let read_error_tx = write_error_tx.clone();
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
                            &reader_frame_writer,
                            &ingress_budget,
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
        thread::spawn({
            let read_error_tx = write_error_tx.clone();
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
                            &callback_transport,
                            &stdin_control_tx,
                            &shutdown_tx,
                            &reader_frame_writer,
                            &ingress_budget,
                            &control_ingress_budget,
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

    flush_sidecar_requests(&mut sidecar, &frame_writer)?;
    let mut pending_frame: Option<AccountedProtocolFrame> = None;
    let mut limit_warning_closed = false;
    let mut stdin_closed = false;
    'protocol: loop {
        if let Some(frame) = pending_frame.take() {
            handle_protocol_frame(
                frame,
                &mut sidecar,
                &mut stdin_rx,
                &mut pending_frame,
                &frame_writer,
                &mut active_sessions,
                &mut active_connections,
            )
            .await?;
            continue;
        }

        if stdin_closed {
            break;
        }

        tokio::select! {
            biased;
            maybe_shutdown = shutdown_rx.recv() => {
                let Some(control) = maybe_shutdown else {
                    break 'protocol;
                };
                match control.payload {
                    wire::ControlPayload::ShutdownControl(shutdown) => {
                        tracing::debug!(reason = %shutdown.reason, "host requested sidecar shutdown");
                        break 'protocol;
                    }
                }
            }
            maybe_control = stdin_control_rx.recv() => {
                match maybe_control {
                    Some(frame) => {
                        handle_protocol_frame(
                            frame,
                            &mut sidecar,
                            &mut stdin_rx,
                            &mut pending_frame,
                            &frame_writer,
                            &mut active_sessions,
                            &mut active_connections,
                        ).await?;
                    }
                    None => {
                        return Err(io::Error::new(
                            io::ErrorKind::BrokenPipe,
                            "response/control stream closed",
                        ).into());
                    }
                }
            }
            maybe_frame = stdin_rx.recv(), if !stdin_closed => {
                match maybe_frame {
                    Some(frame) => {
                        let Some(frame) = frame.map_err(io::Error::other)? else {
                            stdin_closed = true;
                            continue;
                        };
                        handle_protocol_frame(
                            frame,
                            &mut sidecar,
                            &mut stdin_rx,
                            &mut pending_frame,
                            &frame_writer,
                            &mut active_sessions,
                            &mut active_connections,
                        ).await?;
                    }
                    None => stdin_closed = true,
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
                            let frame = crate::service::structured_event_frame(
                                connection_id,
                                "limit_warning",
                                detail,
                            )?;
                            send_output_frame(&frame_writer, ProtocolFrame::EventFrame(frame))?;
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
            maybe_ready = event_ready_rx.recv() => {
                let Some(()) = maybe_ready else {
                    break;
                };
                loop {
                    let mut emitted_frame = false;
                    for session in active_sessions.iter().cloned().collect::<Vec<_>>() {
                        if let Some(frame) = sidecar
                            .poll_event_wire(&session.ownership_scope(), Duration::ZERO)
                            .await?
                        {
                            send_output_frame(&frame_writer, ProtocolFrame::EventFrame(frame))?;
                            emitted_frame = true;
                        }
                    }

                    if !emitted_frame {
                        break;
                    }
                }
                flush_sidecar_requests(&mut sidecar, &frame_writer)?;
            }
            _ = process_event_notify.notified() => {
                for session in active_sessions.iter().cloned().collect::<Vec<_>>() {
                    if sidecar.pump_process_events(&session.compat_ownership_scope()).await? {
                        match event_ready_tx.try_send(()) {
                            Ok(())
                            | Err(tokio::sync::mpsc::error::TrySendError::Full(())) => {}
                            Err(tokio::sync::mpsc::error::TrySendError::Closed(())) => {
                                return Err(io::Error::new(
                                    io::ErrorKind::BrokenPipe,
                                    "event-ready wake receiver closed",
                                )
                                .into());
                            }
                        }
                    }
                }
                flush_sidecar_requests(&mut sidecar, &frame_writer)?;
            }
            maybe_write_error = write_error_rx.recv() => {
                if let Some(error) = maybe_write_error {
                    return Err(io::Error::new(io::ErrorKind::BrokenPipe, error).into());
                }
            }
        }
    }

    cleanup_connections(&mut sidecar, &active_connections, &mut active_sessions).await;
    Ok(())
}

async fn handle_protocol_frame(
    accounted_frame: AccountedProtocolFrame,
    sidecar: &mut NativeSidecar<LocalBridge>,
    stdin_rx: &mut Receiver<Result<Option<AccountedProtocolFrame>, String>>,
    pending_frame: &mut Option<AccountedProtocolFrame>,
    write_tx: &ProtocolFrameWriter,
    active_sessions: &mut BTreeSet<SessionScope>,
    active_connections: &mut BTreeSet<String>,
) -> Result<(), Box<dyn Error>> {
    let AccountedProtocolFrame {
        frame,
        _reservation,
    } = accounted_frame;
    match frame {
        ProtocolFrame::RequestFrame(request) => {
            let dispatch = dispatch_with_prompt_interrupt(
                sidecar,
                request.clone(),
                stdin_rx,
                pending_frame,
                write_tx,
            )
            .await?;
            track_session_state(
                &dispatch.response.payload,
                active_sessions,
                active_connections,
            );

            send_output_frame(write_tx, ProtocolFrame::ResponseFrame(dispatch.response))?;
            for event in dispatch.events {
                send_output_frame(write_tx, ProtocolFrame::EventFrame(event))?;
            }
            flush_sidecar_requests(sidecar, write_tx)?;
        }
        ProtocolFrame::SidecarResponseFrame(response) => {
            sidecar.accept_wire_sidecar_response(response)?;
            flush_sidecar_requests(sidecar, write_tx)?;
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

async fn dispatch_with_prompt_interrupt(
    sidecar: &mut NativeSidecar<LocalBridge>,
    request: RequestFrame,
    stdin_rx: &mut Receiver<Result<Option<AccountedProtocolFrame>, String>>,
    pending_frame: &mut Option<AccountedProtocolFrame>,
    write_tx: &ProtocolFrameWriter,
) -> Result<WireDispatchResult, Box<dyn Error>> {
    let Some(blocking_request) = blocking_extension_request(sidecar, &request) else {
        return Ok(sidecar.dispatch_wire(request).await?);
    };

    let mut dispatch = Box::pin(sidecar.dispatch_wire(request.clone()));
    loop {
        tokio::select! {
            result = dispatch.as_mut() => return Ok(result?),
            maybe_frame = stdin_rx.recv() => {
                let frame = decode_stdin_frame(maybe_frame)?;
                let Some(frame) = frame else {
                    return Ok(dispatch.await?);
                };
                if let Some(interrupt) = extension_interrupt_response(&blocking_request, &request, &frame.frame) {
                    if let Some(response) = interrupt.interrupting_response {
                        // Permission and cancellation requests are control-lane
                        // interrupts. Their effects are applied synchronously by
                        // `interrupt_blocking_request`, so acknowledge them now
                        // instead of buffering the response until the active prompt
                        // eventually completes.
                        send_output_frame(write_tx, ProtocolFrame::ResponseFrame(response))?;
                    } else {
                        *pending_frame = Some(frame);
                    }
                    if interrupt.interrupt_active {
                        drop(dispatch);
                        return Ok(interrupt.interrupted_dispatch);
                    }
                    continue;
                }
                *pending_frame = Some(frame);
                return Ok(dispatch.await?);
            }
        }
    }
}

fn decode_stdin_frame(
    maybe_frame: Option<Result<Option<AccountedProtocolFrame>, String>>,
) -> Result<Option<AccountedProtocolFrame>, Box<dyn Error>> {
    let Some(frame) = maybe_frame else {
        return Ok(None);
    };
    Ok(frame.map_err(io::Error::other)?)
}

struct BlockingExtensionRequest {
    namespace: String,
    payload: Vec<u8>,
    extension: Arc<dyn Extension>,
}

struct ExtensionInterruptDispatch {
    interrupt_active: bool,
    interrupted_dispatch: WireDispatchResult,
    interrupting_response: Option<ResponseFrame>,
}

fn blocking_extension_request(
    sidecar: &NativeSidecar<LocalBridge>,
    request: &RequestFrame,
) -> Option<BlockingExtensionRequest> {
    let RequestPayload::ExtEnvelope(envelope) = &request.payload else {
        return None;
    };
    let extension = sidecar.extensions.get(&envelope.namespace)?.clone();
    if !extension.is_blocking_request(&envelope.payload) {
        return None;
    }
    Some(BlockingExtensionRequest {
        namespace: envelope.namespace.clone(),
        payload: envelope.payload.clone(),
        extension,
    })
}

fn extension_interrupt_response(
    blocking_request: &BlockingExtensionRequest,
    active_request: &RequestFrame,
    frame: &ProtocolFrame,
) -> Option<ExtensionInterruptDispatch> {
    match frame {
        ProtocolFrame::RequestFrame(request) => {
            let interrupt = generated_wire_blocking_extension_interrupt(
                active_request,
                &blocking_request.namespace,
                request,
            )?;
            let interrupt_ownership =
                crate::wire::ownership_scope_to_compat(request.ownership.clone());
            let interrupt = blocking_request.extension.interrupt_blocking_request(
                &blocking_request.payload,
                match interrupt {
                    BlockingExtensionInterrupt::ExtensionPayload(payload) => {
                        ExtensionInterruptRequest::ExtensionPayload {
                            payload,
                            ownership: &interrupt_ownership,
                        }
                    }
                    BlockingExtensionInterrupt::KillProcess => {
                        ExtensionInterruptRequest::KillProcess
                    }
                },
            )?;
            let interrupted_dispatch = interrupted_extension_dispatch(
                active_request,
                &blocking_request.namespace,
                interrupt.interrupted_response_payload,
            );
            let interrupting_response = interrupt.interrupting_response_payload.map(|payload| {
                response_frame(
                    request.request_id,
                    request.ownership.clone(),
                    ResponsePayload::ExtEnvelope(ExtEnvelope {
                        namespace: blocking_request.namespace.clone(),
                        payload,
                    }),
                )
            });
            Some(ExtensionInterruptDispatch {
                interrupt_active: interrupt.interrupt_active,
                interrupted_dispatch,
                interrupting_response,
            })
        }
        // Response, Event, and SidecarRequest frames are sidecar-to-host only. If one
        // arrives on stdin it is requeued and rejected as a protocol error by
        // handle_protocol_frame, so it must not synthesize a cancelled prompt first.
        // SidecarResponse frames answer sidecar-initiated callbacks and may be the very
        // response the blocked prompt dispatch is waiting on, so they never interrupt.
        ProtocolFrame::ResponseFrame(_)
        | ProtocolFrame::EventFrame(_)
        | ProtocolFrame::SidecarRequestFrame(_)
        | ProtocolFrame::SidecarResponseFrame(_)
        | ProtocolFrame::ControlFrame(_) => None,
    }
}

fn interrupted_extension_dispatch(
    request: &RequestFrame,
    namespace: &str,
    payload: Vec<u8>,
) -> WireDispatchResult {
    if !matches!(request.payload, RequestPayload::ExtEnvelope(_)) {
        unreachable!("interrupted extension dispatch requires an extension request");
    }

    let response = ResponsePayload::ExtEnvelope(ExtEnvelope {
        namespace: namespace.to_string(),
        payload,
    });
    WireDispatchResult {
        response: response_frame(request.request_id, request.ownership.clone(), response),
        events: Vec::new(),
    }
}

async fn cleanup_connections(
    sidecar: &mut NativeSidecar<LocalBridge>,
    active_connections: &BTreeSet<String>,
    active_sessions: &mut BTreeSet<SessionScope>,
) {
    for connection_id in active_connections {
        let _ = sidecar.remove_connection(connection_id).await;
    }
    untrack_disposed_sessions(&sidecar.take_disposed_sessions(), active_sessions);
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
    overload_writer: &ProtocolFrameWriter,
    ingress_budget: &ProtocolBudget,
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
) -> StdinReaderFlow {
    match &decoded.frame {
        ProtocolFrame::RequestFrame(_) => {
            route_decoded_stdin_frame(decoded, ordinary_sender, overload_writer, ingress_budget)
        }
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
    let rejection = ProtocolFrame::ResponseFrame(response_frame(
        request.request_id,
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
    match overload_writer.try_send(rejection) {
        Ok(()) => StdinReaderFlow::Continue,
        Err(ProtocolTrySendError::Full) => {
            eprintln!(
                "{STDIO_INGRESS_LIMIT_ERROR_CODE}: reserved control egress queue is full; dropping rejection"
            );
            StdinReaderFlow::Continue
        }
        Err(ProtocolTrySendError::Disconnected) => StdinReaderFlow::Stop,
        Err(ProtocolTrySendError::Rejected(error)) => {
            eprintln!(
                "{STDIO_INGRESS_LIMIT_ERROR_CODE}: could not encode/admit overload rejection: {error}"
            );
            StdinReaderFlow::Continue
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
) -> Result<(), Box<dyn Error>> {
    while let Some(request) = sidecar.pop_wire_sidecar_request()? {
        send_output_frame(writer, ProtocolFrame::SidecarRequestFrame(request))?;
    }
    Ok(())
}

fn send_output_frame(writer: &ProtocolFrameWriter, frame: ProtocolFrame) -> Result<(), io::Error> {
    // Apply backpressure rather than killing the sidecar when the host reads
    // stdout slowly. A full queue means the dedicated writer thread is blocked on
    // the stdout pipe (the host has not drained it yet) — a transient, recoverable
    // condition. Previously `try_send` turned that backlog into a `BrokenPipe`
    // error that propagated up and exited the whole sidecar process (code 1),
    // taking every session with it. A blocking `send` parks the producer until the
    // writer drains a slot, which transitively backpressures the V8 event bridge
    // and the guest. It never deadlocks: the writer thread runs independently, and
    // if it dies (real broken pipe) the receiver is dropped and `send` returns
    // `Disconnected`, which we still surface as a terminal `BrokenPipe`.
    writer.send(frame)
}

/// Emit a connection-scoped `StructuredEvent { name: "heartbeat" }` frame every
/// `interval` for as long as the stdout writer is alive. This is the host's
/// liveness signal: it resets the host's silence watchdog, so a host that sees
/// no frames at all for several intervals can conclude the sidecar process is
/// dead or wedged rather than merely busy. Runs on a dedicated thread so a
/// long synchronous dispatch cannot starve the heartbeat and trigger a false
/// host-side silence timeout.
fn spawn_heartbeat_thread(
    write_tx: ProtocolFrameWriter,
    interval: Duration,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        loop {
            thread::sleep(interval);
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
            match write_tx.try_send(ProtocolFrame::EventFrame(frame)) {
                Ok(()) => {}
                Err(ProtocolTrySendError::Full) => {
                    // A full outbound lane means the host already has pending
                    // sidecar traffic, which itself satisfies liveness.
                }
                Err(ProtocolTrySendError::Disconnected) => return,
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
    })
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
mod tests {
    use super::*;
    use crate::wire::{AuthenticateRequest, KillProcessRequest};
    use crate::{ExtensionContext, ExtensionFuture, ExtensionInterruptResponse, ExtensionResponse};
    use std::io::Cursor;

    const TEST_EXTENSION_NAMESPACE: &str = "dev.rivet.agentos.test.blocking";

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
        let codec = WireFrameCodec::new(4096);
        let output = Arc::new(ProtocolOutputQueue::new(capacity, capacity));
        let max_bytes = capacity.saturating_mul(codec.max_frame_bytes().saturating_add(4));
        (
            ProtocolFrameWriter {
                output: Arc::clone(&output),
                codec,
                ordinary_budget: test_protocol_budget(capacity, max_bytes, "test ordinary egress"),
                control_budget: test_protocol_budget(capacity, max_bytes, "test control egress"),
            },
            output,
        )
    }

    fn decode_test_output(frame: EncodedProtocolFrame) -> ProtocolFrame {
        WireFrameCodec::new(4096)
            .decode(&frame.bytes)
            .expect("decode test output frame")
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
                &overload_tx,
                &ingress_budget,
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
            .send(ProtocolFrame::EventFrame(event))
            .expect("queue ordinary event");
        writer
            .send(ProtocolFrame::ResponseFrame(response_frame(
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
                &overload_tx,
                &ingress_budget,
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

    // Regression: a full stdout frame queue must apply backpressure (block the
    // producer until the writer drains a slot), NOT tear the sidecar down. The
    // old `try_send` turned a slow host reader into a `BrokenPipe` error that
    // propagated up and exited the whole sidecar process (code 1). Here a slow
    // drainer forces the queue past capacity; with backpressure every send
    // succeeds, and overflow only fails when the writer (receiver) is gone.
    #[test]
    fn stdout_frame_queue_applies_backpressure_instead_of_crashing() {
        let queue_frame = |request_id: RequestId| {
            let mut detail = std::collections::HashMap::new();
            detail.insert(String::from("request_id"), request_id.to_string());
            ProtocolFrame::EventFrame(
                crate::service::structured_event_frame("conn-queue", "queue-test", detail)
                    .expect("queue event"),
            )
        };

        // Small fixed capacity (independent of the production constant) with a
        // drainer slow enough that the queue fills and the producer is forced
        // onto the blocking path. The old try_send path errored on the
        // (capacity + 1)th frame; backpressure accepts all of them.
        let queue_cap = 8usize;
        let total_frames = queue_cap * 3;
        let (stdout_tx, stdout_rx) = test_frame_writer(queue_cap);
        let drainer_rx = Arc::clone(&stdout_rx);
        let drainer = std::thread::spawn(move || {
            let mut drained = 0usize;
            while drainer_rx.recv_ordinary().is_some() {
                drained += 1;
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            drained
        });

        for request_id in 1..=total_frames {
            send_output_frame(&stdout_tx, queue_frame(request_id as RequestId))
                .expect("backpressured stdout queue must accept frames, not crash");
        }
        stdout_rx.close();
        drop(stdout_tx);
        let drained = drainer.join().expect("drainer thread panicked");
        assert_eq!(
            drained, total_frames,
            "every frame must survive the backpressured queue"
        );

        // When the writer (receiver) is gone, overflow is genuinely terminal and
        // still surfaces as a BrokenPipe error rather than blocking forever.
        let (closed_tx, closed_rx) = test_frame_writer(queue_cap);
        closed_rx.close();
        let error = send_output_frame(&closed_tx, queue_frame(1))
            .expect_err("send to a dropped writer must error");
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
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

    #[test]
    fn extension_close_interrupts_matching_blocking_request() {
        let ownership = vm_ownership("conn-1", "session-1", "vm-1");
        let prompt = test_extension_request_frame(10, ownership.clone(), "prompt:ext-session-1");
        let close = ProtocolFrame::RequestFrame(test_extension_request_frame(
            11,
            ownership,
            "close:ext-session-1",
        ));

        let blocking_request = blocking_extension_request(&prompt);
        let interrupt = extension_interrupt_response(&blocking_request, &prompt, &close)
            .expect("close should interrupt prompt");

        assert_eq!(interrupt.interrupted_dispatch.response.request_id, 10);
        let ResponsePayload::ExtEnvelope(envelope) =
            interrupt.interrupted_dispatch.response.payload
        else {
            panic!("expected extension response");
        };
        assert_eq!(envelope.namespace, TEST_EXTENSION_NAMESPACE);
        assert_eq!(envelope.payload, b"prompt-cancelled:ext-session-1");
    }

    #[test]
    fn extension_cancel_interrupt_gets_synthetic_response() {
        let ownership = vm_ownership("conn-1", "session-1", "vm-1");
        let prompt = test_extension_request_frame(10, ownership.clone(), "prompt:ext-session-1");
        let cancel = ProtocolFrame::RequestFrame(test_extension_request_frame(
            11,
            ownership,
            "cancel:ext-session-1",
        ));

        let blocking_request = blocking_extension_request(&prompt);
        let interrupt = extension_interrupt_response(&blocking_request, &prompt, &cancel)
            .expect("cancel should interrupt prompt");
        let response = interrupt
            .interrupting_response
            .expect("cancel should get a response");

        assert_eq!(response.request_id, 11);
        let ResponsePayload::ExtEnvelope(envelope) = response.payload else {
            panic!("expected extension response");
        };
        assert_eq!(envelope.namespace, TEST_EXTENSION_NAMESPACE);
        assert_eq!(envelope.payload, b"cancelled:ext-session-1");
    }

    #[test]
    fn kill_process_interrupts_blocking_extension_request() {
        let ownership = vm_ownership("conn-1", "session-1", "vm-1");
        let prompt = test_extension_request_frame(10, ownership.clone(), "prompt:ext-session-1");
        let kill = ProtocolFrame::RequestFrame(request_frame(
            11,
            ownership,
            RequestPayload::KillProcessRequest(KillProcessRequest {
                process_id: "adapter-process".to_string(),
                signal: "SIGTERM".to_string(),
            }),
        ));

        let blocking_request = blocking_extension_request(&prompt);
        let interrupt = extension_interrupt_response(&blocking_request, &prompt, &kill)
            .expect("kill should interrupt prompt");

        assert_eq!(interrupt.interrupted_dispatch.response.request_id, 10);
        assert!(interrupt.interrupting_response.is_none());
    }

    fn test_extension_request_frame(
        request_id: RequestId,
        ownership: OwnershipScope,
        payload: &str,
    ) -> RequestFrame {
        request_frame(
            request_id,
            ownership,
            RequestPayload::ExtEnvelope(ExtEnvelope {
                namespace: TEST_EXTENSION_NAMESPACE.to_string(),
                payload: payload.as_bytes().to_vec(),
            }),
        )
    }

    fn blocking_extension_request(request: &RequestFrame) -> BlockingExtensionRequest {
        let RequestPayload::ExtEnvelope(envelope) = &request.payload else {
            panic!("expected extension request");
        };
        BlockingExtensionRequest {
            namespace: TEST_EXTENSION_NAMESPACE.to_string(),
            payload: envelope.payload.clone(),
            extension: Arc::new(TestBlockingInterruptExtension),
        }
    }

    struct TestBlockingInterruptExtension;

    impl Extension for TestBlockingInterruptExtension {
        fn namespace(&self) -> &str {
            TEST_EXTENSION_NAMESPACE
        }

        fn handle_request<'a>(
            &'a self,
            _ctx: ExtensionContext<'a>,
            _payload: Vec<u8>,
        ) -> ExtensionFuture<'a, ExtensionResponse> {
            Box::pin(async { Ok(ExtensionResponse::new(Vec::new())) })
        }

        fn is_blocking_request(&self, payload: &[u8]) -> bool {
            parse_test_payload(payload).is_some_and(|(kind, _session_id)| kind == "prompt")
        }

        fn interrupt_blocking_request(
            &self,
            blocking_payload: &[u8],
            interrupt: ExtensionInterruptRequest<'_>,
        ) -> Option<ExtensionInterruptResponse> {
            let (blocking_kind, blocking_session_id) = parse_test_payload(blocking_payload)?;
            if blocking_kind != "prompt" {
                return None;
            }

            let interrupted_response_payload =
                encode_test_response("prompt-cancelled", blocking_session_id);
            match interrupt {
                ExtensionInterruptRequest::KillProcess => Some(ExtensionInterruptResponse {
                    interrupt_active: true,
                    interrupted_response_payload,
                    interrupting_response_payload: None,
                }),
                ExtensionInterruptRequest::ExtensionPayload { payload, .. } => {
                    let (interrupt_kind, interrupt_session_id) = parse_test_payload(payload)?;
                    match interrupt_kind {
                        "close" if interrupt_session_id == blocking_session_id => {
                            Some(ExtensionInterruptResponse {
                                interrupt_active: true,
                                interrupted_response_payload,
                                interrupting_response_payload: None,
                            })
                        }
                        "cancel" if interrupt_session_id == blocking_session_id => {
                            Some(ExtensionInterruptResponse {
                                interrupt_active: true,
                                interrupted_response_payload,
                                interrupting_response_payload: Some(encode_test_response(
                                    "cancelled",
                                    interrupt_session_id,
                                )),
                            })
                        }
                        "prompt" | "close" | "cancel" => None,
                        _ => None,
                    }
                }
            }
        }
    }

    fn parse_test_payload(payload: &[u8]) -> Option<(&str, &str)> {
        let payload = std::str::from_utf8(payload).ok()?;
        payload.split_once(':')
    }

    fn encode_test_response(kind: &str, session_id: &str) -> Vec<u8> {
        format!("{kind}:{session_id}").into_bytes()
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

/// Live event sink backed by the outbound stdout channel. Writes each event as a
/// `ProtocolFrame::EventFrame` immediately, using the same blocking
/// backpressure semantics as the batch event path (`send_output_frame`): a full
/// queue parks the producer until the writer thread drains stdout rather than
/// tearing down the process.
struct FrameEventTransport {
    writer: ProtocolFrameWriter,
}

impl FrameEventTransport {
    fn new(writer: ProtocolFrameWriter) -> Self {
        Self { writer }
    }
}

impl EventSinkTransport for FrameEventTransport {
    fn emit_event(&self, event: crate::wire::EventFrame) -> Result<(), SidecarError> {
        send_output_frame(&self.writer, ProtocolFrame::EventFrame(event))
            .map_err(|error| SidecarError::Bridge(error.to_string()))
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
    sender: mpsc::SyncSender<PendingSidecarResponseResult>,
    count_reservation: PendingResponseReservation,
}

struct FrameSidecarRequestTransport {
    writer: ProtocolFrameWriter,
    pending: Arc<Mutex<BTreeMap<RequestId, PendingSidecarResponseTarget>>>,
    pending_count: Arc<AtomicUsize>,
    pending_response_bytes: Arc<AtomicUsize>,
    limits: FrameSidecarRequestLimits,
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
                sender,
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

        // A registered waiter owns a capacity-one channel and this is its only
        // producer, so try_send cannot block the stdio reader. A timed-out
        // receiver simply drops the response and both reservations here.
        match sender.try_send(delivery) {
            Ok(()) => {}
            Err(mpsc::TrySendError::Full(_)) => eprintln!(
                "sidecar callback response channel unexpectedly full for request_id={request_id}"
            ),
            Err(mpsc::TrySendError::Disconnected(_)) => tracing::debug!(
                target: "agentos_native_sidecar::stdio",
                request_id,
                "sidecar callback response arrived after its waiter disconnected",
            ),
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
        // Bound the request-frame write by the caller's deadline. The protocol
        // budget's condition variable wakes this producer when the writer
        // releases count/byte capacity; no recurring transport poll is needed.
        let write_deadline = Instant::now() + timeout;
        let write_result = self
            .writer
            .send_until(
                ProtocolFrame::SidecarRequestFrame(request.clone()),
                write_deadline,
            )
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
