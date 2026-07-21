#![forbid(unsafe_code)]

//! Process-owned trusted runtime services.
//!
//! Production sidecars construct exactly one [`SidecarRuntime`] at their
//! process entrypoint. Subsystems receive a clone of [`RuntimeContext`]; they
//! never construct Tokio runtimes of their own.

use std::cell::Cell;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::task::{Context, Poll};
use std::thread;
use std::time::{Duration, Instant};

use accounting::{LimitError, Reservation, ResourceClass, ResourceLedger, ResourceLimit};
use fairness::{FairBudget, FairWorkBroker, FairnessConfig};
use metrics::{
    ExecutorMetricClass, RuntimeMetrics, TelemetryFallback, TelemetryFallbackCode,
    TelemetrySeverity, TelemetrySubsystem, WatchdogMetric,
};

pub mod accounting;
pub mod capability;
pub mod executor;
pub mod fairness;
pub mod metrics;
pub mod readiness;
pub mod supervision;

pub use executor::{
    VmExecutorAdmission, VmExecutorAdmissionError, VmExecutorAdmissionSnapshot, VmExecutorPermit,
    VM_EXECUTOR_LIMIT_CONFIG_PATH,
};
pub use supervision::{
    TaskClass, TaskClassSnapshot, TaskOwner, TaskSpawnError, TaskSupervisor, TaskTerminalReason,
    TaskTerminalReport,
};

const DEFAULT_MAX_BLOCKING_JOB_BYTES: usize = 64 * 1024 * 1024;
const DEFAULT_BLOCKING_JOB_TIMEOUT_MS: u64 = 30_000;
const DEFAULT_MAX_BLOCKING_JOBS: usize = 1_028;
const DEFAULT_MAX_QUEUED_BLOCKING_JOBS: usize = 1024;
const DEFAULT_MAX_PROCESS_CAPABILITIES: usize = 16_384;
const DEFAULT_MAX_PROCESS_SOCKETS: usize = 8_192;
const DEFAULT_MAX_PROCESS_CONNECTIONS: usize = 8_192;
const DEFAULT_MAX_PROCESS_SOCKET_BUFFERED_BYTES: usize = 1024 * 1024 * 1024;
const DEFAULT_MAX_PROCESS_DATAGRAMS: usize = 65_536;
const DEFAULT_MAX_PROCESS_TIMERS: usize = 65_536;
const DEFAULT_MAX_PROCESS_TASKS: usize = 65_536;
const DEFAULT_MAX_PROCESS_READY_HANDLES: usize = 16_384;
const DEFAULT_MAX_PROCESS_HANDLE_COMMANDS: usize = 65_536;
const DEFAULT_MAX_PROCESS_HANDLE_COMMAND_BYTES: usize = 256 * 1024 * 1024;
const DEFAULT_MAX_PROCESS_BRIDGE_CALLS: usize = 65_536;
const DEFAULT_MAX_PROCESS_BRIDGE_REQUEST_BYTES: usize = 256 * 1024 * 1024;
const DEFAULT_MAX_PROCESS_BRIDGE_RESPONSE_BYTES: usize = 256 * 1024 * 1024;
const DEFAULT_MAX_PROCESS_ASYNC_COMPLETIONS: usize = 65_536;
const DEFAULT_MAX_PROCESS_ASYNC_COMPLETION_BYTES: usize = 256 * 1024 * 1024;
const DEFAULT_MAX_PROCESS_UDP_DATAGRAMS: usize = 65_536;
const DEFAULT_MAX_PROCESS_UDP_BYTES: usize = 256 * 1024 * 1024;
const DEFAULT_MAX_PROCESS_TLS_BYTES: usize = 256 * 1024 * 1024;
const DEFAULT_MAX_PROCESS_WASM_MEMORY_BYTES: usize = 8 * 1024 * 1024 * 1024;
const DEFAULT_MAX_PROCESS_WASM_THREADS: usize = 256;
const DEFAULT_MAX_PROCESS_HTTP2_CONNECTIONS: usize = 4_096;
const DEFAULT_MAX_PROCESS_HTTP2_STREAMS: usize = 65_536;
const DEFAULT_MAX_PROCESS_HTTP2_BYTES: usize = 512 * 1024 * 1024;
const DEFAULT_MAX_PROCESS_HTTP2_HEADER_BYTES: usize = 128 * 1024 * 1024;
const DEFAULT_MAX_PROCESS_HTTP2_DATA_BYTES: usize = 512 * 1024 * 1024;
const DEFAULT_MAX_PROCESS_HTTP2_COMMANDS: usize = 65_536;
const DEFAULT_MAX_PROCESS_HTTP2_COMMAND_BYTES: usize = 256 * 1024 * 1024;
const DEFAULT_MAX_PROCESS_HTTP2_EVENTS: usize = 65_536;
const DEFAULT_MAX_PROCESS_HTTP2_EVENT_BYTES: usize = 512 * 1024 * 1024;
const DEFAULT_TASK_POLL_WATCHDOG_MS: u64 = 100;
const DEFAULT_MAX_TERMINAL_TASK_REPORTS: usize = 4_096;
const DEFAULT_VM_EXECUTOR_TEARDOWN_TIMEOUT_MS: u64 = 5_000;
pub const DEFAULT_PROTOCOL_MAX_INGRESS_FRAMES: usize = 128;
pub const DEFAULT_PROTOCOL_MAX_INGRESS_BYTES: usize = 64 * 1024 * 1024;
pub const DEFAULT_PROTOCOL_MAX_CONTROL_FRAMES: usize = 1_024;
pub const DEFAULT_PROTOCOL_MAX_CONTROL_BYTES: usize = 64 * 1024 * 1024;
pub const DEFAULT_PROTOCOL_MAX_EGRESS_FRAMES: usize = 4_096;
pub const DEFAULT_PROTOCOL_MAX_EGRESS_BYTES: usize = 256 * 1024 * 1024;
pub const DEFAULT_PROTOCOL_MAX_PENDING_RESPONSES: usize = 10_000;
pub const DEFAULT_PROTOCOL_MAX_PENDING_RESPONSE_BYTES: usize = 256 * 1024 * 1024;
pub const DEFAULT_PROTOCOL_MAX_PROCESS_EVENTS: usize = 10_000;
pub const DEFAULT_PROTOCOL_MAX_OUTBOUND_REQUESTS: usize = 10_000;
pub const DEFAULT_PROTOCOL_MAX_COMPLETED_RESPONSES: usize = 10_000;
const DEFAULT_FAIRNESS_VM_OPERATIONS: usize = 64;
const DEFAULT_FAIRNESS_VM_BYTES: usize = 1024 * 1024;
const DEFAULT_FAIRNESS_CAPABILITY_OPERATIONS: usize = 16;
const DEFAULT_FAIRNESS_CAPABILITY_BYTES: usize = 256 * 1024;
const DEFAULT_FAIRNESS_MAX_VMS: usize = 4_096;
const DEFAULT_FAIRNESS_MAX_CAPABILITIES_PER_VM: usize = 16_384;

thread_local! {
    static IS_AGENTOS_RUNTIME_WORKER: Cell<bool> = const { Cell::new(false) };
}

/// Whether the current OS thread is one of the process runtime's fixed workers.
///
/// Synchronous compatibility adapters use this to reject waits that would
/// consume a trusted runtime worker and could deadlock the work they submitted.
pub fn is_runtime_worker_thread() -> bool {
    IS_AGENTOS_RUNTIME_WORKER.with(Cell::get)
}

/// Process-owned bounds for the multiplexed sidecar transport.
///
/// Ordinary frames and response/control frames have independent admission so
/// an event or request backlog cannot consume the capacity needed to settle an
/// already-registered bridge call. Byte ceilings cover retained decoded or
/// encoded frames; the one active decoder is separately bounded by the wire
/// codec's `max_frame_bytes` setting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeProtocolConfig {
    pub max_ingress_frames: usize,
    pub max_ingress_bytes: usize,
    pub max_control_frames: usize,
    pub max_control_bytes: usize,
    pub max_egress_frames: usize,
    pub max_egress_bytes: usize,
    pub max_pending_responses: usize,
    pub max_pending_response_bytes: usize,
    pub max_process_events: usize,
    pub max_outbound_requests: usize,
    pub max_completed_responses: usize,
}

impl Default for RuntimeProtocolConfig {
    fn default() -> Self {
        Self {
            max_ingress_frames: DEFAULT_PROTOCOL_MAX_INGRESS_FRAMES,
            max_ingress_bytes: DEFAULT_PROTOCOL_MAX_INGRESS_BYTES,
            max_control_frames: DEFAULT_PROTOCOL_MAX_CONTROL_FRAMES,
            max_control_bytes: DEFAULT_PROTOCOL_MAX_CONTROL_BYTES,
            max_egress_frames: DEFAULT_PROTOCOL_MAX_EGRESS_FRAMES,
            max_egress_bytes: DEFAULT_PROTOCOL_MAX_EGRESS_BYTES,
            max_pending_responses: DEFAULT_PROTOCOL_MAX_PENDING_RESPONSES,
            max_pending_response_bytes: DEFAULT_PROTOCOL_MAX_PENDING_RESPONSE_BYTES,
            max_process_events: DEFAULT_PROTOCOL_MAX_PROCESS_EVENTS,
            max_outbound_requests: DEFAULT_PROTOCOL_MAX_OUTBOUND_REQUESTS,
            max_completed_responses: DEFAULT_PROTOCOL_MAX_COMPLETED_RESPONSES,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeFairnessConfig {
    pub vm_quantum_operations: usize,
    pub vm_quantum_bytes: usize,
    pub capability_quantum_operations: usize,
    pub capability_quantum_bytes: usize,
    pub max_vm_deficit_operations: usize,
    pub max_vm_deficit_bytes: usize,
    pub max_capability_deficit_operations: usize,
    pub max_capability_deficit_bytes: usize,
    pub max_vms: usize,
    pub max_capabilities_per_vm: usize,
}

impl Default for RuntimeFairnessConfig {
    fn default() -> Self {
        Self {
            vm_quantum_operations: DEFAULT_FAIRNESS_VM_OPERATIONS,
            vm_quantum_bytes: DEFAULT_FAIRNESS_VM_BYTES,
            capability_quantum_operations: DEFAULT_FAIRNESS_CAPABILITY_OPERATIONS,
            capability_quantum_bytes: DEFAULT_FAIRNESS_CAPABILITY_BYTES,
            max_vm_deficit_operations: DEFAULT_FAIRNESS_VM_OPERATIONS * 4,
            max_vm_deficit_bytes: DEFAULT_FAIRNESS_VM_BYTES * 4,
            max_capability_deficit_operations: DEFAULT_FAIRNESS_CAPABILITY_OPERATIONS * 4,
            max_capability_deficit_bytes: DEFAULT_FAIRNESS_CAPABILITY_BYTES * 4,
            max_vms: DEFAULT_FAIRNESS_MAX_VMS,
            max_capabilities_per_vm: DEFAULT_FAIRNESS_MAX_CAPABILITIES_PER_VM,
        }
    }
}

impl RuntimeFairnessConfig {
    fn scheduler_config(&self) -> FairnessConfig {
        FairnessConfig {
            vm_quantum: FairBudget::new(self.vm_quantum_operations, self.vm_quantum_bytes),
            capability_quantum: FairBudget::new(
                self.capability_quantum_operations,
                self.capability_quantum_bytes,
            ),
            max_vm_deficit: FairBudget::new(
                self.max_vm_deficit_operations,
                self.max_vm_deficit_bytes,
            ),
            max_capability_deficit: FairBudget::new(
                self.max_capability_deficit_operations,
                self.max_capability_deficit_bytes,
            ),
            max_vms: self.max_vms,
            max_capabilities_per_vm: self.max_capabilities_per_vm,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeResourceConfig {
    pub max_capabilities: usize,
    pub max_ready_handles: usize,
    pub max_sockets: usize,
    pub max_connections: usize,
    pub max_socket_buffered_bytes: usize,
    pub max_datagrams: usize,
    pub max_timers: usize,
    pub max_tasks: usize,
    pub max_handle_commands: usize,
    pub max_handle_command_bytes: usize,
    pub max_bridge_calls: usize,
    pub max_bridge_request_bytes: usize,
    pub max_bridge_response_bytes: usize,
    pub max_async_completions: usize,
    pub max_async_completion_bytes: usize,
    pub max_udp_datagrams: usize,
    pub max_udp_bytes: usize,
    pub max_tls_bytes: usize,
    /// Aggregate admitted linear-memory envelopes for active standalone WASM
    /// Stores. This must not share the blocking-work byte ledger.
    pub max_wasm_memory_bytes: usize,
    /// Aggregate admitted native threads across explicitly threaded WASM VMs.
    pub max_wasm_threads: usize,
    pub max_http2_connections: usize,
    pub max_http2_streams: usize,
    pub max_http2_buffered_bytes: usize,
    pub max_http2_header_bytes: usize,
    pub max_http2_data_bytes: usize,
    pub max_http2_commands: usize,
    pub max_http2_command_bytes: usize,
    pub max_http2_events: usize,
    pub max_http2_event_bytes: usize,
}

impl Default for RuntimeResourceConfig {
    fn default() -> Self {
        Self {
            max_capabilities: DEFAULT_MAX_PROCESS_CAPABILITIES,
            max_ready_handles: DEFAULT_MAX_PROCESS_READY_HANDLES,
            max_sockets: DEFAULT_MAX_PROCESS_SOCKETS,
            max_connections: DEFAULT_MAX_PROCESS_CONNECTIONS,
            max_socket_buffered_bytes: DEFAULT_MAX_PROCESS_SOCKET_BUFFERED_BYTES,
            max_datagrams: DEFAULT_MAX_PROCESS_DATAGRAMS,
            max_timers: DEFAULT_MAX_PROCESS_TIMERS,
            max_tasks: DEFAULT_MAX_PROCESS_TASKS,
            max_handle_commands: DEFAULT_MAX_PROCESS_HANDLE_COMMANDS,
            max_handle_command_bytes: DEFAULT_MAX_PROCESS_HANDLE_COMMAND_BYTES,
            max_bridge_calls: DEFAULT_MAX_PROCESS_BRIDGE_CALLS,
            max_bridge_request_bytes: DEFAULT_MAX_PROCESS_BRIDGE_REQUEST_BYTES,
            max_bridge_response_bytes: DEFAULT_MAX_PROCESS_BRIDGE_RESPONSE_BYTES,
            max_async_completions: DEFAULT_MAX_PROCESS_ASYNC_COMPLETIONS,
            max_async_completion_bytes: DEFAULT_MAX_PROCESS_ASYNC_COMPLETION_BYTES,
            max_udp_datagrams: DEFAULT_MAX_PROCESS_UDP_DATAGRAMS,
            max_udp_bytes: DEFAULT_MAX_PROCESS_UDP_BYTES,
            max_tls_bytes: DEFAULT_MAX_PROCESS_TLS_BYTES,
            max_wasm_memory_bytes: DEFAULT_MAX_PROCESS_WASM_MEMORY_BYTES,
            max_wasm_threads: DEFAULT_MAX_PROCESS_WASM_THREADS,
            max_http2_connections: DEFAULT_MAX_PROCESS_HTTP2_CONNECTIONS,
            max_http2_streams: DEFAULT_MAX_PROCESS_HTTP2_STREAMS,
            max_http2_buffered_bytes: DEFAULT_MAX_PROCESS_HTTP2_BYTES,
            max_http2_header_bytes: DEFAULT_MAX_PROCESS_HTTP2_HEADER_BYTES,
            max_http2_data_bytes: DEFAULT_MAX_PROCESS_HTTP2_DATA_BYTES,
            max_http2_commands: DEFAULT_MAX_PROCESS_HTTP2_COMMANDS,
            max_http2_command_bytes: DEFAULT_MAX_PROCESS_HTTP2_COMMAND_BYTES,
            max_http2_events: DEFAULT_MAX_PROCESS_HTTP2_EVENTS,
            max_http2_event_bytes: DEFAULT_MAX_PROCESS_HTTP2_EVENT_BYTES,
        }
    }
}

impl RuntimeResourceConfig {
    fn limits(&self) -> Vec<(ResourceClass, ResourceLimit)> {
        vec![
            (
                ResourceClass::Capabilities,
                ResourceLimit::new(self.max_capabilities, "runtime.resources.maxCapabilities"),
            ),
            (
                ResourceClass::ReadyHandles,
                ResourceLimit::new(self.max_ready_handles, "runtime.resources.maxReadyHandles"),
            ),
            (
                ResourceClass::Sockets,
                ResourceLimit::new(self.max_sockets, "runtime.resources.maxSockets"),
            ),
            (
                ResourceClass::Connections,
                ResourceLimit::new(self.max_connections, "runtime.resources.maxConnections"),
            ),
            (
                ResourceClass::BufferedBytes,
                ResourceLimit::new(
                    self.max_socket_buffered_bytes,
                    "runtime.resources.maxSocketBufferedBytes",
                ),
            ),
            (
                ResourceClass::Datagrams,
                ResourceLimit::new(self.max_datagrams, "runtime.resources.maxDatagrams"),
            ),
            (
                ResourceClass::Timers,
                ResourceLimit::new(self.max_timers, "runtime.resources.maxTimers"),
            ),
            (
                ResourceClass::Tasks,
                ResourceLimit::new(self.max_tasks, "runtime.resources.maxTasks"),
            ),
            (
                ResourceClass::HandleCommands,
                ResourceLimit::new(
                    self.max_handle_commands,
                    "runtime.resources.maxHandleCommands",
                ),
            ),
            (
                ResourceClass::HandleCommandBytes,
                ResourceLimit::new(
                    self.max_handle_command_bytes,
                    "runtime.resources.maxHandleCommandBytes",
                ),
            ),
            (
                ResourceClass::BridgeCalls,
                ResourceLimit::new(self.max_bridge_calls, "runtime.resources.maxBridgeCalls"),
            ),
            (
                ResourceClass::BridgeRequestBytes,
                ResourceLimit::new(
                    self.max_bridge_request_bytes,
                    "runtime.resources.maxBridgeRequestBytes",
                ),
            ),
            (
                ResourceClass::BridgeResponseBytes,
                ResourceLimit::new(
                    self.max_bridge_response_bytes,
                    "runtime.resources.maxBridgeResponseBytes",
                ),
            ),
            (
                ResourceClass::AsyncCompletions,
                ResourceLimit::new(
                    self.max_async_completions,
                    "runtime.resources.maxAsyncCompletions",
                ),
            ),
            (
                ResourceClass::AsyncCompletionBytes,
                ResourceLimit::new(
                    self.max_async_completion_bytes,
                    "runtime.resources.maxAsyncCompletionBytes",
                ),
            ),
            (
                ResourceClass::UdpDatagrams,
                ResourceLimit::new(self.max_udp_datagrams, "runtime.resources.maxUdpDatagrams"),
            ),
            (
                ResourceClass::UdpBytes,
                ResourceLimit::new(self.max_udp_bytes, "runtime.resources.maxUdpBytes"),
            ),
            (
                ResourceClass::TlsBytes,
                ResourceLimit::new(self.max_tls_bytes, "runtime.resources.maxTlsBytes"),
            ),
            (
                ResourceClass::WasmMemoryBytes,
                ResourceLimit::new(
                    self.max_wasm_memory_bytes,
                    "runtime.resources.maxWasmMemoryBytes",
                ),
            ),
            (
                ResourceClass::WasmThreads,
                ResourceLimit::new(self.max_wasm_threads, "runtime.resources.maxWasmThreads"),
            ),
            (
                ResourceClass::Http2Connections,
                ResourceLimit::new(
                    self.max_http2_connections,
                    "runtime.resources.maxHttp2Connections",
                ),
            ),
            (
                ResourceClass::Http2Streams,
                ResourceLimit::new(self.max_http2_streams, "runtime.resources.maxHttp2Streams"),
            ),
            (
                ResourceClass::Http2BufferedBytes,
                ResourceLimit::new(
                    self.max_http2_buffered_bytes,
                    "runtime.resources.maxHttp2BufferedBytes",
                ),
            ),
            (
                ResourceClass::Http2HeaderBytes,
                ResourceLimit::new(
                    self.max_http2_header_bytes,
                    "runtime.resources.maxHttp2HeaderBytes",
                ),
            ),
            (
                ResourceClass::Http2DataBytes,
                ResourceLimit::new(
                    self.max_http2_data_bytes,
                    "runtime.resources.maxHttp2DataBytes",
                ),
            ),
            (
                ResourceClass::Http2Commands,
                ResourceLimit::new(
                    self.max_http2_commands,
                    "runtime.resources.maxHttp2Commands",
                ),
            ),
            (
                ResourceClass::Http2CommandBytes,
                ResourceLimit::new(
                    self.max_http2_command_bytes,
                    "runtime.resources.maxHttp2CommandBytes",
                ),
            ),
            (
                ResourceClass::Http2Events,
                ResourceLimit::new(self.max_http2_events, "runtime.resources.maxHttp2Events"),
            ),
            (
                ResourceClass::Http2EventBytes,
                ResourceLimit::new(
                    self.max_http2_event_bytes,
                    "runtime.resources.maxHttp2EventBytes",
                ),
            ),
        ]
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeConfig {
    pub worker_threads: usize,
    pub max_active_vm_executors: usize,
    pub vm_executor_teardown_timeout_ms: u64,
    pub blocking_worker_threads: usize,
    pub max_blocking_jobs: usize,
    pub max_queued_blocking_jobs: usize,
    pub max_blocking_job_bytes: usize,
    pub blocking_job_timeout_ms: u64,
    pub task_poll_watchdog_ms: u64,
    pub max_terminal_task_reports: usize,
    pub protocol: RuntimeProtocolConfig,
    pub resources: RuntimeResourceConfig,
    pub fairness: RuntimeFairnessConfig,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        let available = thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1);
        Self {
            worker_threads: available.clamp(1, 4),
            max_active_vm_executors: available.max(1),
            vm_executor_teardown_timeout_ms: DEFAULT_VM_EXECUTOR_TEARDOWN_TIMEOUT_MS,
            blocking_worker_threads: available.clamp(1, 4),
            max_blocking_jobs: DEFAULT_MAX_BLOCKING_JOBS,
            max_queued_blocking_jobs: DEFAULT_MAX_QUEUED_BLOCKING_JOBS,
            max_blocking_job_bytes: DEFAULT_MAX_BLOCKING_JOB_BYTES,
            blocking_job_timeout_ms: DEFAULT_BLOCKING_JOB_TIMEOUT_MS,
            task_poll_watchdog_ms: DEFAULT_TASK_POLL_WATCHDOG_MS,
            max_terminal_task_reports: DEFAULT_MAX_TERMINAL_TASK_REPORTS,
            protocol: RuntimeProtocolConfig::default(),
            resources: RuntimeResourceConfig::default(),
            fairness: RuntimeFairnessConfig::default(),
        }
    }
}

impl RuntimeConfig {
    pub fn validate(&self) -> Result<(), RuntimeBuildError> {
        for (field, value) in [
            ("runtime.workerThreads", self.worker_threads),
            (
                "runtime.executor.maxActiveVms",
                self.max_active_vm_executors,
            ),
            (
                "runtime.blocking.workerThreads",
                self.blocking_worker_threads,
            ),
            ("runtime.blocking.maxJobs", self.max_blocking_jobs),
            (
                "runtime.blocking.maxQueuedJobs",
                self.max_queued_blocking_jobs,
            ),
            (
                "runtime.blocking.maxQueuedBytes",
                self.max_blocking_job_bytes,
            ),
            (
                "runtime.tasks.maxTerminalReports",
                self.max_terminal_task_reports,
            ),
            (
                "runtime.protocol.maxIngressFrames",
                self.protocol.max_ingress_frames,
            ),
            (
                "runtime.protocol.maxIngressBytes",
                self.protocol.max_ingress_bytes,
            ),
            (
                "runtime.protocol.maxControlFrames",
                self.protocol.max_control_frames,
            ),
            (
                "runtime.protocol.maxControlBytes",
                self.protocol.max_control_bytes,
            ),
            (
                "runtime.protocol.maxEgressFrames",
                self.protocol.max_egress_frames,
            ),
            (
                "runtime.protocol.maxEgressBytes",
                self.protocol.max_egress_bytes,
            ),
            (
                "runtime.protocol.maxPendingResponses",
                self.protocol.max_pending_responses,
            ),
            (
                "runtime.protocol.maxPendingResponseBytes",
                self.protocol.max_pending_response_bytes,
            ),
            (
                "runtime.protocol.maxProcessEvents",
                self.protocol.max_process_events,
            ),
            (
                "runtime.protocol.maxOutboundRequests",
                self.protocol.max_outbound_requests,
            ),
            (
                "runtime.protocol.maxCompletedResponses",
                self.protocol.max_completed_responses,
            ),
            (
                "runtime.resources.maxCapabilities",
                self.resources.max_capabilities,
            ),
            (
                "runtime.resources.maxReadyHandles",
                self.resources.max_ready_handles,
            ),
            ("runtime.resources.maxSockets", self.resources.max_sockets),
            (
                "runtime.resources.maxConnections",
                self.resources.max_connections,
            ),
            (
                "runtime.resources.maxSocketBufferedBytes",
                self.resources.max_socket_buffered_bytes,
            ),
            (
                "runtime.resources.maxDatagrams",
                self.resources.max_datagrams,
            ),
            ("runtime.resources.maxTimers", self.resources.max_timers),
            ("runtime.resources.maxTasks", self.resources.max_tasks),
            (
                "runtime.resources.maxHandleCommands",
                self.resources.max_handle_commands,
            ),
            (
                "runtime.resources.maxHandleCommandBytes",
                self.resources.max_handle_command_bytes,
            ),
            (
                "runtime.resources.maxBridgeCalls",
                self.resources.max_bridge_calls,
            ),
            (
                "runtime.resources.maxBridgeRequestBytes",
                self.resources.max_bridge_request_bytes,
            ),
            (
                "runtime.resources.maxBridgeResponseBytes",
                self.resources.max_bridge_response_bytes,
            ),
            (
                "runtime.resources.maxAsyncCompletions",
                self.resources.max_async_completions,
            ),
            (
                "runtime.resources.maxAsyncCompletionBytes",
                self.resources.max_async_completion_bytes,
            ),
            (
                "runtime.resources.maxUdpDatagrams",
                self.resources.max_udp_datagrams,
            ),
            (
                "runtime.resources.maxUdpBytes",
                self.resources.max_udp_bytes,
            ),
            (
                "runtime.resources.maxTlsBytes",
                self.resources.max_tls_bytes,
            ),
            (
                "runtime.resources.maxWasmMemoryBytes",
                self.resources.max_wasm_memory_bytes,
            ),
            (
                "runtime.resources.maxHttp2Connections",
                self.resources.max_http2_connections,
            ),
            (
                "runtime.resources.maxHttp2Streams",
                self.resources.max_http2_streams,
            ),
            (
                "runtime.resources.maxHttp2BufferedBytes",
                self.resources.max_http2_buffered_bytes,
            ),
            (
                "runtime.resources.maxHttp2HeaderBytes",
                self.resources.max_http2_header_bytes,
            ),
            (
                "runtime.resources.maxHttp2DataBytes",
                self.resources.max_http2_data_bytes,
            ),
            (
                "runtime.resources.maxHttp2Commands",
                self.resources.max_http2_commands,
            ),
            (
                "runtime.resources.maxHttp2CommandBytes",
                self.resources.max_http2_command_bytes,
            ),
            (
                "runtime.resources.maxHttp2Events",
                self.resources.max_http2_events,
            ),
            (
                "runtime.resources.maxHttp2EventBytes",
                self.resources.max_http2_event_bytes,
            ),
        ] {
            if value == 0 {
                return Err(RuntimeBuildError(format!(
                    "ERR_AGENTOS_RUNTIME_CONFIG: {field} must be greater than zero"
                )));
            }
        }
        if self.task_poll_watchdog_ms == 0 {
            return Err(RuntimeBuildError(String::from(
                "ERR_AGENTOS_RUNTIME_CONFIG: runtime.watchdog.taskPollMs must be greater than zero",
            )));
        }
        if self.vm_executor_teardown_timeout_ms == 0 {
            return Err(RuntimeBuildError(String::from(
                "ERR_AGENTOS_RUNTIME_CONFIG: runtime.executor.teardownTimeoutMs must be greater than zero",
            )));
        }
        if self.blocking_job_timeout_ms == 0 {
            return Err(RuntimeBuildError(String::from(
                "ERR_AGENTOS_RUNTIME_CONFIG: runtime.blocking.jobTimeoutMs must be greater than zero",
            )));
        }
        for (field, value) in [
            (
                "runtime.fairness.vmQuantumOperations",
                self.fairness.vm_quantum_operations,
            ),
            (
                "runtime.fairness.vmQuantumBytes",
                self.fairness.vm_quantum_bytes,
            ),
            (
                "runtime.fairness.capabilityQuantumOperations",
                self.fairness.capability_quantum_operations,
            ),
            (
                "runtime.fairness.capabilityQuantumBytes",
                self.fairness.capability_quantum_bytes,
            ),
            (
                "runtime.fairness.maxVmDeficitOperations",
                self.fairness.max_vm_deficit_operations,
            ),
            (
                "runtime.fairness.maxVmDeficitBytes",
                self.fairness.max_vm_deficit_bytes,
            ),
            (
                "runtime.fairness.maxCapabilityDeficitOperations",
                self.fairness.max_capability_deficit_operations,
            ),
            (
                "runtime.fairness.maxCapabilityDeficitBytes",
                self.fairness.max_capability_deficit_bytes,
            ),
            ("runtime.fairness.maxVms", self.fairness.max_vms),
            (
                "runtime.fairness.maxCapabilitiesPerVm",
                self.fairness.max_capabilities_per_vm,
            ),
        ] {
            if value == 0 {
                return Err(RuntimeBuildError(format!(
                    "ERR_AGENTOS_RUNTIME_CONFIG: {field} must be greater than zero"
                )));
            }
        }
        if self.fairness.max_vm_deficit_operations < self.fairness.vm_quantum_operations
            || self.fairness.max_vm_deficit_bytes < self.fairness.vm_quantum_bytes
            || self.fairness.max_capability_deficit_operations
                < self.fairness.capability_quantum_operations
            || self.fairness.max_capability_deficit_bytes < self.fairness.capability_quantum_bytes
        {
            return Err(RuntimeBuildError(String::from(
                "ERR_AGENTOS_RUNTIME_CONFIG: fairness deficits must be at least their quantum",
            )));
        }
        if self.max_blocking_jobs < self.blocking_worker_threads {
            return Err(RuntimeBuildError(format!(
                "ERR_AGENTOS_RUNTIME_CONFIG: runtime.blocking.maxJobs ({}) must be >= runtime.blocking.workerThreads ({})",
                self.max_blocking_jobs, self.blocking_worker_threads
            )));
        }
        if self.max_queued_blocking_jobs > self.max_blocking_jobs {
            return Err(RuntimeBuildError(format!(
                "ERR_AGENTOS_RUNTIME_CONFIG: runtime.blocking.maxQueuedJobs ({}) must be <= runtime.blocking.maxJobs ({})",
                self.max_queued_blocking_jobs, self.max_blocking_jobs
            )));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeBuildError(String);

impl fmt::Display for RuntimeBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for RuntimeBuildError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockingJobError {
    ResourceLimit(LimitError),
    Capacity { limit: usize },
    ShuttingDown,
    WorkerDropped,
    TimedOut { timeout: Duration },
}

impl fmt::Display for BlockingJobError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ResourceLimit(error) => error.fmt(formatter),
            Self::Capacity { limit } => write!(
                formatter,
                "ERR_AGENTOS_BLOCKING_JOB_LIMIT: blocking executor queue exceeded {limit} jobs; raise runtime.blocking.maxQueuedJobs"
            ),
            Self::ShuttingDown => formatter.write_str(
                "ERR_AGENTOS_BLOCKING_EXECUTOR_SHUTDOWN: blocking executor is shutting down",
            ),
            Self::WorkerDropped => formatter.write_str(
                "ERR_AGENTOS_BLOCKING_WORKER_DROPPED: blocking worker ended without a result",
            ),
            Self::TimedOut { timeout } => write!(
                formatter,
                "ERR_AGENTOS_BLOCKING_JOB_TIMEOUT: blocking job exceeded its {}ms deadline",
                timeout.as_millis()
            ),
        }
    }
}

impl std::error::Error for BlockingJobError {}

type BlockingOperation = Box<dyn FnOnce(Reservation, Reservation) + Send + 'static>;

struct BlockingJob {
    operation: BlockingOperation,
    _slot: Reservation,
    _bytes: Reservation,
}

struct BlockingExecutorState {
    metrics: RuntimeMetrics,
    queued: AtomicUsize,
    active: AtomicUsize,
}

struct BlockingExecutorInner {
    sender: Mutex<Option<mpsc::SyncSender<BlockingJob>>>,
    workers: Mutex<Vec<thread::JoinHandle<()>>>,
    max_queued_jobs: usize,
    max_bytes: usize,
    state: Arc<BlockingExecutorState>,
}

impl Drop for BlockingExecutorInner {
    fn drop(&mut self) {
        self.sender.get_mut().ok().and_then(Option::take);
        let workers = self
            .workers
            .get_mut()
            .map(std::mem::take)
            .unwrap_or_default();
        for worker in workers {
            if worker.join().is_err() {
                eprintln!("ERR_AGENTOS_BLOCKING_WORKER_PANIC: blocking executor worker panicked");
            }
        }
    }
}

#[derive(Clone)]
pub struct BlockingExecutor {
    inner: Arc<BlockingExecutorInner>,
    resources: Arc<ResourceLedger>,
    admission_open: Arc<AtomicBool>,
    admission_gate: Arc<Mutex<()>>,
}

impl fmt::Debug for BlockingExecutor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BlockingExecutor")
            .field("worker_count", &self.worker_count())
            .field("max_queued_jobs", &self.inner.max_queued_jobs)
            .field("max_bytes", &self.inner.max_bytes)
            .field("reserved_bytes", &self.reserved_bytes())
            .finish()
    }
}

impl BlockingExecutor {
    fn new(
        config: &RuntimeConfig,
        resources: Arc<ResourceLedger>,
        metrics: RuntimeMetrics,
        admission_open: Arc<AtomicBool>,
        admission_gate: Arc<Mutex<()>>,
    ) -> Result<Self, RuntimeBuildError> {
        let (sender, receiver) = mpsc::sync_channel::<BlockingJob>(config.max_queued_blocking_jobs);
        let receiver = Arc::new(Mutex::new(receiver));
        let executor_state = Arc::new(BlockingExecutorState {
            metrics,
            queued: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
        });
        let mut workers = Vec::with_capacity(config.blocking_worker_threads);
        for index in 0..config.blocking_worker_threads {
            let receiver = Arc::clone(&receiver);
            let executor_state = Arc::clone(&executor_state);
            // AGENTOS_THREAD_SITE: blocking-executor-worker
            let worker = thread::Builder::new()
                .name(format!("agentos-blocking-{index}"))
                .spawn(move || loop {
                    let job = match receiver.lock() {
                        Ok(receiver) => receiver.recv(),
                        Err(_) => {
                            eprintln!(
                                "ERR_AGENTOS_BLOCKING_QUEUE_POISONED: blocking job receiver lock poisoned"
                            );
                            break;
                        }
                    };
                    match job {
                        Ok(job) => {
                            decrement_saturating(&executor_state.queued);
                            executor_state.active.fetch_add(1, Ordering::Relaxed);
                            executor_state.metrics.observe_executor(
                                ExecutorMetricClass::Blocking,
                                executor_state.active.load(Ordering::Relaxed),
                                executor_state.queued.load(Ordering::Relaxed),
                            );
                            let BlockingJob {
                                operation,
                                _slot,
                                _bytes,
                            } = job;
                            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                operation(_slot, _bytes)
                            }))
                            .is_err()
                            {
                                eprintln!(
                                    "ERR_AGENTOS_BLOCKING_JOB_PANIC: blocking job panicked"
                                );
                            }
                            decrement_saturating(&executor_state.active);
                            executor_state.metrics.observe_executor(
                                ExecutorMetricClass::Blocking,
                                executor_state.active.load(Ordering::Relaxed),
                                executor_state.queued.load(Ordering::Relaxed),
                            );
                        }
                        Err(_) => break,
                    }
                })
                .map_err(|error| {
                    RuntimeBuildError(format!(
                        "ERR_AGENTOS_BLOCKING_WORKER_START: failed to start blocking worker {index}: {error}"
                    ))
                })?;
            workers.push(worker);
        }

        Ok(Self {
            inner: Arc::new(BlockingExecutorInner {
                sender: Mutex::new(Some(sender)),
                workers: Mutex::new(workers),
                max_queued_jobs: config.max_queued_blocking_jobs,
                max_bytes: config.max_blocking_job_bytes,
                state: executor_state,
            }),
            resources,
            admission_open,
            admission_gate,
        })
    }

    pub fn worker_count(&self) -> usize {
        self.inner
            .workers
            .lock()
            .map(|workers| workers.len())
            .unwrap_or(0)
    }

    pub fn reserved_bytes(&self) -> usize {
        self.resources.usage(ResourceClass::ExecutorBytes).used
    }

    /// Share the fixed workers and queue while charging admission to a child
    /// ledger (which atomically charges its process parent too).
    pub fn scoped(
        &self,
        resources: Arc<ResourceLedger>,
        admission_open: Arc<AtomicBool>,
        admission_gate: Arc<Mutex<()>>,
    ) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            resources,
            admission_open,
            admission_gate,
        }
    }

    pub fn submit<F>(&self, reserved_bytes: usize, operation: F) -> Result<(), BlockingJobError>
    where
        F: FnOnce() + Send + 'static,
    {
        let (admission, _slot, _bytes) = self.reserve(reserved_bytes)?;
        let job = BlockingJob {
            operation: Box::new(move |_slot, _bytes| operation()),
            _slot,
            _bytes,
        };
        let result = self.try_enqueue(job);
        drop(admission);
        result
    }

    pub async fn run<T, F>(
        &self,
        reserved_bytes: usize,
        operation: F,
    ) -> Result<T, BlockingJobError>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        let (admission, _slot, _bytes) = self.reserve(reserved_bytes)?;
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        let job = BlockingJob {
            operation: Box::new(move |slot, bytes| {
                let result = operation();
                // Publish completion only after accounting is reconciled. A
                // caller observing the result must never still see the job's
                // reservations charged.
                drop(slot);
                drop(bytes);
                if result_tx.send(result).is_err() {
                    eprintln!(
                        "ERR_AGENTOS_BLOCKING_RESULT_DROPPED: asynchronous caller stopped waiting"
                    );
                }
            }),
            _slot,
            _bytes,
        };

        let enqueue_result = self.try_enqueue(job);
        drop(admission);
        enqueue_result?;

        result_rx.await.map_err(|_| BlockingJobError::WorkerDropped)
    }

    pub fn run_sync<T, F>(
        &self,
        reserved_bytes: usize,
        timeout: Duration,
        operation: F,
    ) -> Result<T, BlockingJobError>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        let (admission, _slot, _bytes) = self.reserve(reserved_bytes)?;
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        let job = BlockingJob {
            operation: Box::new(move |slot, bytes| {
                let result = operation();
                // Synchronous callers can run immediately after recv returns,
                // so release admission before making that result observable.
                drop(slot);
                drop(bytes);
                if result_tx.send(result).is_err() {
                    eprintln!(
                        "ERR_AGENTOS_BLOCKING_RESULT_DROPPED: synchronous caller stopped waiting"
                    );
                }
            }),
            _slot,
            _bytes,
        };

        let enqueue_result = self.try_enqueue(job);
        drop(admission);
        enqueue_result?;

        result_rx
            .recv_timeout(timeout)
            .map_err(|error| match error {
                mpsc::RecvTimeoutError::Timeout => BlockingJobError::TimedOut { timeout },
                mpsc::RecvTimeoutError::Disconnected => BlockingJobError::WorkerDropped,
            })
    }

    fn reserve(
        &self,
        requested: usize,
    ) -> Result<(std::sync::MutexGuard<'_, ()>, Reservation, Reservation), BlockingJobError> {
        // Keep admission open through the queue insertion. Close uses the same
        // gate, so a stale clone cannot enqueue blocking work after close
        // returns and teardown has begun reconciling the VM ledger.
        let admission = self
            .admission_gate
            .lock()
            .map_err(|_| BlockingJobError::ShuttingDown)?;
        if !self.admission_open.load(Ordering::Acquire) {
            return Err(BlockingJobError::ShuttingDown);
        }
        let slot = self
            .resources
            .reserve(ResourceClass::ExecutorSlots, 1)
            .map_err(BlockingJobError::ResourceLimit)?;
        let bytes = self
            .resources
            .reserve(ResourceClass::ExecutorBytes, requested)
            .map_err(BlockingJobError::ResourceLimit)?;
        Ok((admission, slot, bytes))
    }

    fn try_enqueue(&self, job: BlockingJob) -> Result<(), BlockingJobError> {
        let sender = self
            .inner
            .sender
            .lock()
            .map_err(|_| BlockingJobError::ShuttingDown)?;
        let sender = sender.as_ref().ok_or(BlockingJobError::ShuttingDown)?;
        self.record_enqueued();
        if let Err(error) = sender.try_send(job) {
            self.record_enqueue_failed();
            return Err(match error {
                mpsc::TrySendError::Full(_) => BlockingJobError::Capacity {
                    limit: self.inner.max_queued_jobs,
                },
                mpsc::TrySendError::Disconnected(_) => BlockingJobError::ShuttingDown,
            });
        }
        Ok(())
    }

    fn record_enqueued(&self) {
        self.inner.state.queued.fetch_add(1, Ordering::Relaxed);
        self.observe_executor();
    }

    fn record_enqueue_failed(&self) {
        decrement_saturating(&self.inner.state.queued);
        self.observe_executor();
    }

    fn observe_executor(&self) {
        self.inner.state.metrics.observe_executor(
            ExecutorMetricClass::Blocking,
            self.inner.state.active.load(Ordering::Relaxed),
            self.inner.state.queued.load(Ordering::Relaxed),
        );
    }
}

fn decrement_saturating(counter: &AtomicUsize) {
    let mut current = counter.load(Ordering::Relaxed);
    while current != 0 {
        match counter.compare_exchange_weak(
            current,
            current - 1,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return,
            Err(observed) => current = observed,
        }
    }
}

#[derive(Clone, Debug)]
pub struct RuntimeContext {
    handle: tokio::runtime::Handle,
    blocking: BlockingExecutor,
    resources: Arc<ResourceLedger>,
    tasks: TaskSupervisor,
    metrics: RuntimeMetrics,
    fairness: FairWorkBroker,
    terminal_failure: Arc<Mutex<Option<TaskTerminalReport>>>,
    task_poll_watchdog: Duration,
    vm_executors: VmExecutorAdmission,
    vm_executor_teardown_timeout: Duration,
    blocking_job_timeout: Duration,
    admission_open: Arc<AtomicBool>,
    admission_closed: Arc<tokio::sync::Notify>,
    next_vm_generation: Arc<AtomicU64>,
    default_owner: TaskOwner,
}

impl RuntimeContext {
    pub fn handle(&self) -> &tokio::runtime::Handle {
        &self.handle
    }

    pub fn blocking(&self) -> &BlockingExecutor {
        &self.blocking
    }

    /// Aggregate process accountant shared by every VM and trusted subsystem.
    pub fn resources(&self) -> &Arc<ResourceLedger> {
        &self.resources
    }

    pub fn tasks(&self) -> &TaskSupervisor {
        &self.tasks
    }

    pub fn metrics(&self) -> &RuntimeMetrics {
        &self.metrics
    }

    pub fn max_active_vm_executors(&self) -> usize {
        self.vm_executors.maximum()
    }

    pub fn vm_executor_admission(&self) -> &VmExecutorAdmission {
        &self.vm_executors
    }

    pub fn vm_executor_teardown_timeout(&self) -> Duration {
        self.vm_executor_teardown_timeout
    }

    pub fn blocking_job_timeout(&self) -> Duration {
        self.blocking_job_timeout
    }

    pub fn fairness(&self) -> &FairWorkBroker {
        &self.fairness
    }

    /// Allocate an identity in the same process-wide namespace as the shared
    /// fairness broker. Every sidecar facade using this runtime must draw from
    /// this counter; per-facade counters can collide after an earlier VM retires.
    pub fn allocate_vm_generation(&self) -> Result<u64, RuntimeBuildError> {
        self.next_vm_generation
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current.checked_add(1)
            })
            .map(|previous| previous + 1)
            .map_err(|_| {
                RuntimeBuildError(String::from(
                    "ERR_AGENTOS_VM_GENERATION_EXHAUSTED: process VM generation counter overflowed",
                ))
            })
    }

    /// VM generation bound to this admission scope, when this is a VM-scoped
    /// context. Fairness keys use this generation rather than a capability's
    /// per-registry generation, which restarts for every VM.
    pub fn vm_generation(&self) -> Option<u64> {
        match &self.default_owner {
            TaskOwner::Vm { generation } => Some(*generation),
            _ => None,
        }
    }

    /// First failed or panicked task in this process/VM admission scope.
    /// This is a durable lifecycle latch, not a drainable telemetry queue.
    pub fn terminal_failure(&self) -> Option<TaskTerminalReport> {
        self.terminal_failure
            .lock()
            .map(|failure| failure.clone())
            .unwrap_or_else(|_| {
                eprintln!(
                    "ERR_AGENTOS_TASK_FAILURE_LATCH_POISONED: scope={}",
                    self.resources.scope()
                );
                Some(TaskTerminalReport {
                    class: TaskClass::Runtime,
                    owner: self.default_owner.clone(),
                    scope: self.resources.scope().to_owned(),
                    reason: TaskTerminalReason::Panicked,
                })
            })
    }

    /// Permanently close task and blocking-job admission for this context
    /// scope and every clone of it. Existing admitted work retains ownership
    /// until its normal terminal path releases accounting.
    pub fn close_admission(&self) {
        // The supervisor owns the gate shared by task and blocking admission.
        // Closing through it linearizes this transition against both paths.
        self.tasks.close_admission();
        self.admission_closed.notify_waiters();
    }

    pub fn admission_is_open(&self) -> bool {
        self.admission_open.load(Ordering::Acquire)
    }

    /// Resolve when this admission scope begins teardown.
    ///
    /// The notification is armed before checking the atomic flag so a close
    /// between observation and await cannot be lost. Each VM-scoped context
    /// owns a distinct signal; closing one VM never cancels process or sibling
    /// VM work.
    pub async fn admission_closed(&self) {
        loop {
            let notified = self.admission_closed.notified();
            if !self.admission_is_open() {
                return;
            }
            notified.await;
        }
    }

    /// Create a VM-scoped admission view without creating another runtime,
    /// scheduler, queue, or worker.
    pub fn scoped(&self, resources: Arc<ResourceLedger>) -> Self {
        let admission_open = Arc::new(AtomicBool::new(true));
        let admission_gate = Arc::new(Mutex::new(()));
        Self {
            handle: self.handle.clone(),
            blocking: self.blocking.scoped(
                Arc::clone(&resources),
                Arc::clone(&admission_open),
                Arc::clone(&admission_gate),
            ),
            resources: Arc::clone(&resources),
            tasks: self
                .tasks
                .scoped(resources, Arc::clone(&admission_open), admission_gate),
            metrics: self.metrics.clone(),
            fairness: self.fairness.clone(),
            terminal_failure: Arc::new(Mutex::new(None)),
            task_poll_watchdog: self.task_poll_watchdog,
            vm_executors: self.vm_executors.clone(),
            vm_executor_teardown_timeout: self.vm_executor_teardown_timeout,
            blocking_job_timeout: self.blocking_job_timeout,
            admission_open,
            admission_closed: Arc::new(tokio::sync::Notify::new()),
            next_vm_generation: Arc::clone(&self.next_vm_generation),
            default_owner: self.default_owner.clone(),
        }
    }

    pub fn scoped_for_vm(&self, resources: Arc<ResourceLedger>, generation: u64) -> Self {
        let mut scoped = self.scoped(resources);
        scoped.default_owner = TaskOwner::Vm { generation };
        scoped
    }

    pub fn spawn<F>(
        &self,
        class: TaskClass,
        future: F,
    ) -> Result<tokio::task::JoinHandle<F::Output>, TaskSpawnError>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let failure_latch = Arc::clone(&self.terminal_failure);
        let handler = supervision::terminal_handler(move |report| {
            latch_terminal_failure(&failure_latch, report);
        });
        let mut guard = self
            .tasks
            .admit(class, self.default_owner.clone(), Some(handler))?;
        let future =
            WatchdogFuture::new(future, class, self.task_poll_watchdog, self.metrics.clone());
        Ok(self.handle.spawn(async move {
            let output = future.await;
            guard.complete();
            output
        }))
    }

    pub fn spawn_result<F, T, E>(
        &self,
        class: TaskClass,
        future: F,
    ) -> Result<tokio::task::JoinHandle<Result<T, E>>, TaskSpawnError>
    where
        F: Future<Output = Result<T, E>> + Send + 'static,
        T: Send + 'static,
        E: Send + 'static,
    {
        let failure_latch = Arc::clone(&self.terminal_failure);
        let handler = supervision::terminal_handler(move |report| {
            latch_terminal_failure(&failure_latch, report);
        });
        let mut guard = self
            .tasks
            .admit(class, self.default_owner.clone(), Some(handler))?;
        let future =
            WatchdogFuture::new(future, class, self.task_poll_watchdog, self.metrics.clone());
        Ok(self.handle.spawn(async move {
            let output = future.await;
            if output.is_ok() {
                guard.complete();
            } else {
                guard.fail();
            }
            output
        }))
    }

    /// Spawn work with an explicit lifecycle owner. The terminal hook runs for
    /// every exit reason after task accounting is reconciled and outside the
    /// supervisor lock, so it can fail/close the owner without lock inversion.
    pub fn spawn_owned<F, H>(
        &self,
        class: TaskClass,
        owner: TaskOwner,
        on_terminal: H,
        future: F,
    ) -> Result<tokio::task::JoinHandle<F::Output>, TaskSpawnError>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
        H: Fn(&TaskTerminalReport) + Send + Sync + 'static,
    {
        let failure_latch = Arc::clone(&self.terminal_failure);
        let handler = supervision::terminal_handler(move |report| {
            latch_terminal_failure(&failure_latch, report);
            on_terminal(report);
        });
        let mut guard = self.tasks.admit(class, owner, Some(handler))?;
        let future =
            WatchdogFuture::new(future, class, self.task_poll_watchdog, self.metrics.clone());
        Ok(self.handle.spawn(async move {
            let output = future.await;
            guard.complete();
            output
        }))
    }

    pub fn spawn_owned_result<F, T, E, H>(
        &self,
        class: TaskClass,
        owner: TaskOwner,
        on_terminal: H,
        future: F,
    ) -> Result<tokio::task::JoinHandle<Result<T, E>>, TaskSpawnError>
    where
        F: Future<Output = Result<T, E>> + Send + 'static,
        T: Send + 'static,
        E: Send + 'static,
        H: Fn(&TaskTerminalReport) + Send + Sync + 'static,
    {
        let failure_latch = Arc::clone(&self.terminal_failure);
        let handler = supervision::terminal_handler(move |report| {
            latch_terminal_failure(&failure_latch, report);
            on_terminal(report);
        });
        let mut guard = self.tasks.admit(class, owner, Some(handler))?;
        let future =
            WatchdogFuture::new(future, class, self.task_poll_watchdog, self.metrics.clone());
        Ok(self.handle.spawn(async move {
            let output = future.await;
            if output.is_ok() {
                guard.complete();
            } else {
                guard.fail();
            }
            output
        }))
    }
}

fn latch_terminal_failure(latch: &Mutex<Option<TaskTerminalReport>>, report: &TaskTerminalReport) {
    if !matches!(
        report.reason,
        TaskTerminalReason::Failed | TaskTerminalReason::Panicked
    ) {
        return;
    }
    let mut failure = latch.lock().unwrap_or_else(|poisoned| {
        eprintln!("ERR_AGENTOS_TASK_FAILURE_LATCH_POISONED: recovering terminal failure");
        poisoned.into_inner()
    });
    if failure.is_none() {
        *failure = Some(report.clone());
    }
}

struct WatchdogFuture<F> {
    inner: Pin<Box<F>>,
    class: TaskClass,
    threshold: Duration,
    metrics: RuntimeMetrics,
    reported: bool,
}

impl<F> WatchdogFuture<F> {
    fn new(future: F, class: TaskClass, threshold: Duration, metrics: RuntimeMetrics) -> Self {
        Self {
            inner: Box::pin(future),
            class,
            threshold,
            metrics,
            reported: false,
        }
    }
}

impl<F: Future> Future for WatchdogFuture<F> {
    type Output = F::Output;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.as_mut().get_mut();
        let started = Instant::now();
        let result = this.inner.as_mut().poll(context);
        let elapsed = started.elapsed();
        if elapsed >= this.threshold {
            this.metrics
                .record_watchdog(WatchdogMetric::LongTaskPoll, elapsed);
            if !this.reported {
                this.reported = true;
                let message = format!(
                    "task_class={:?} poll_ms={} threshold_ms={}",
                    this.class,
                    elapsed.as_millis(),
                    this.threshold.as_millis()
                );
                this.metrics.emit_stderr_fallback(TelemetryFallback {
                    severity: TelemetrySeverity::Warning,
                    code: TelemetryFallbackCode::RuntimeWorkerStall,
                    subsystem: TelemetrySubsystem::Runtime,
                    message: &message,
                });
            }
        }
        result
    }
}

pub struct SidecarRuntime {
    config: RuntimeConfig,
    runtime: tokio::runtime::Runtime,
    context: RuntimeContext,
}

static PROCESS_RUNTIME: OnceLock<Result<SidecarRuntime, RuntimeBuildError>> = OnceLock::new();

impl SidecarRuntime {
    fn build(config: RuntimeConfig) -> Result<Self, RuntimeBuildError> {
        config.validate()?;
        let mut resource_limits = config.resources.limits();
        resource_limits.push((
            ResourceClass::ExecutorSlots,
            ResourceLimit::new(config.max_blocking_jobs, "runtime.blocking.maxJobs"),
        ));
        resource_limits.push((
            ResourceClass::ExecutorBytes,
            ResourceLimit::new(
                config.max_blocking_job_bytes,
                "runtime.blocking.maxQueuedBytes",
            ),
        ));
        let metrics = RuntimeMetrics::new();
        let vm_executors =
            VmExecutorAdmission::new(config.max_active_vm_executors, metrics.clone());
        let fairness = FairWorkBroker::new(config.fairness.scheduler_config(), metrics.clone())
            .map_err(|error| {
                RuntimeBuildError(format!(
                    "ERR_AGENTOS_RUNTIME_FAIRNESS_START: failed to build process fairness broker: {error}"
                ))
            })?;
        let resources = Arc::new(ResourceLedger::root_with_observer(
            "sidecar-process",
            resource_limits,
            Arc::new(metrics.clone()),
        ));
        let admission_open = Arc::new(AtomicBool::new(true));
        let admission_gate = Arc::new(Mutex::new(()));
        let blocking = BlockingExecutor::new(
            &config,
            Arc::clone(&resources),
            metrics.clone(),
            Arc::clone(&admission_open),
            Arc::clone(&admission_gate),
        )?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(config.worker_threads)
            .thread_name_fn(|| {
                static NEXT_WORKER: AtomicUsize = AtomicUsize::new(0);
                format!(
                    "agentos-runtime-{}",
                    NEXT_WORKER.fetch_add(1, Ordering::Relaxed)
                )
            })
            .on_thread_start(|| IS_AGENTOS_RUNTIME_WORKER.with(|marker| marker.set(true)))
            .on_thread_stop(|| IS_AGENTOS_RUNTIME_WORKER.with(|marker| marker.set(false)))
            .enable_all()
            .build()
            .map_err(|error| {
                RuntimeBuildError(format!(
                    "ERR_AGENTOS_RUNTIME_START: failed to build process runtime: {error}"
                ))
            })?;
        let context = RuntimeContext {
            handle: runtime.handle().clone(),
            blocking,
            resources: Arc::clone(&resources),
            tasks: TaskSupervisor::new(
                resources,
                metrics.clone(),
                Arc::clone(&admission_open),
                admission_gate,
                config.max_terminal_task_reports,
            ),
            metrics,
            fairness,
            terminal_failure: Arc::new(Mutex::new(None)),
            task_poll_watchdog: Duration::from_millis(config.task_poll_watchdog_ms),
            vm_executors,
            vm_executor_teardown_timeout: Duration::from_millis(
                config.vm_executor_teardown_timeout_ms,
            ),
            blocking_job_timeout: Duration::from_millis(config.blocking_job_timeout_ms),
            admission_open,
            admission_closed: Arc::new(tokio::sync::Notify::new()),
            next_vm_generation: Arc::new(AtomicU64::new(0)),
            default_owner: TaskOwner::Process,
        };
        Ok(Self {
            config,
            runtime,
            context,
        })
    }

    /// Return the one Tokio runtime owned by this sidecar process.
    ///
    /// The first caller fixes the process topology. A later caller requesting a
    /// different topology receives a typed configuration error instead of
    /// silently creating a subsystem- or VM-local runtime.
    pub fn process(config: &RuntimeConfig) -> Result<&'static Self, RuntimeBuildError> {
        match PROCESS_RUNTIME.get_or_init(|| Self::build(config.clone())) {
            Ok(runtime) if &runtime.config == config => Ok(runtime),
            Ok(runtime) => Err(RuntimeBuildError(format!(
                "ERR_AGENTOS_RUNTIME_ALREADY_CONFIGURED: process runtime uses {:?}, requested {:?}",
                runtime.config, config
            ))),
            Err(error) => Err(error.clone()),
        }
    }

    /// Obtain the runtime already constructed by the process entrypoint.
    /// Subsystems must never silently select a default topology: doing so can
    /// race trusted configuration and make the first incidental caller own the
    /// process scheduler.
    pub fn process_context() -> Result<RuntimeContext, RuntimeBuildError> {
        match PROCESS_RUNTIME.get() {
            Some(Ok(runtime)) => Ok(runtime.context()),
            Some(Err(error)) => Err(error.clone()),
            #[cfg(test)]
            None => Self::process(&RuntimeConfig::default()).map(Self::context),
            #[cfg(not(test))]
            None => Err(RuntimeBuildError(String::from(
                "ERR_AGENTOS_RUNTIME_NOT_INITIALIZED: the process entrypoint must construct SidecarRuntime before starting subsystems",
            ))),
        }
    }

    pub fn context(&self) -> RuntimeContext {
        self.context.clone()
    }

    pub fn block_on<F: Future>(&self, future: F) -> F::Output {
        self.runtime.block_on(future)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_runtime_bounds_every_resource_class_by_default() {
        let runtime = SidecarRuntime::build(RuntimeConfig::default()).expect("build runtime");
        for resource in ResourceClass::ALL {
            let usage = runtime.context().resources().usage(resource);
            assert_eq!(usage.used, 0, "{} starts charged", resource.name());
            assert!(
                usage.limit.is_some_and(|limit| limit > 0),
                "{} has no positive process limit",
                resource.name()
            );
        }
    }

    #[test]
    fn vm_generation_allocator_is_shared_by_scoped_contexts() {
        let runtime = SidecarRuntime::build(RuntimeConfig::default()).expect("build runtime");
        let process = runtime.context();
        let resources = Arc::new(ResourceLedger::root("vm-generation-test", []));
        let scoped = process.scoped(Arc::clone(&resources));

        let first = process
            .allocate_vm_generation()
            .expect("allocate process generation");
        let second = scoped
            .allocate_vm_generation()
            .expect("allocate scoped generation");

        assert_eq!(second, first + 1);
    }

    #[test]
    fn validates_nonzero_runtime_limits() {
        let error = RuntimeConfig {
            worker_threads: 0,
            ..RuntimeConfig::default()
        }
        .validate()
        .expect_err("zero worker count must be rejected");
        assert!(error.to_string().contains("runtime.workerThreads"));

        let error = RuntimeConfig {
            max_terminal_task_reports: 0,
            ..RuntimeConfig::default()
        }
        .validate()
        .expect_err("zero terminal-report capacity must be rejected");
        assert!(error
            .to_string()
            .contains("runtime.tasks.maxTerminalReports"));

        let error = RuntimeConfig {
            max_active_vm_executors: 0,
            ..RuntimeConfig::default()
        }
        .validate()
        .expect_err("zero VM executor capacity must be rejected");
        assert!(error.to_string().contains("runtime.executor.maxActiveVms"));

        let error = RuntimeConfig {
            vm_executor_teardown_timeout_ms: 0,
            ..RuntimeConfig::default()
        }
        .validate()
        .expect_err("zero VM executor teardown timeout must be rejected");
        assert!(error
            .to_string()
            .contains("runtime.executor.teardownTimeoutMs"));

        let error = RuntimeConfig {
            blocking_job_timeout_ms: 0,
            ..RuntimeConfig::default()
        }
        .validate()
        .expect_err("zero blocking-job timeout must be rejected");
        assert!(error.to_string().contains("runtime.blocking.jobTimeoutMs"));

        let error = RuntimeConfig {
            protocol: RuntimeProtocolConfig {
                max_ingress_bytes: 0,
                ..RuntimeProtocolConfig::default()
            },
            ..RuntimeConfig::default()
        }
        .validate()
        .expect_err("zero protocol byte capacity must be rejected");
        assert!(error
            .to_string()
            .contains("runtime.protocol.maxIngressBytes"));
    }

    #[test]
    fn blocking_executor_enforces_byte_reservations() {
        let runtime = SidecarRuntime::build(RuntimeConfig {
            worker_threads: 1,
            blocking_worker_threads: 1,
            max_queued_blocking_jobs: 1,
            max_blocking_job_bytes: 8,
            ..RuntimeConfig::default()
        })
        .expect("build runtime");
        let blocking = runtime.context().blocking().clone();
        let error = runtime
            .block_on(blocking.run(9, || 1usize))
            .expect_err("oversize blocking job must be rejected");
        assert!(matches!(error, BlockingJobError::ResourceLimit(_)));
        assert_eq!(blocking.reserved_bytes(), 0);
    }

    #[test]
    fn blocking_executor_runs_on_fixed_workers_and_releases_bytes() {
        let runtime = SidecarRuntime::build(RuntimeConfig {
            worker_threads: 1,
            blocking_worker_threads: 2,
            max_queued_blocking_jobs: 2,
            max_blocking_job_bytes: 32,
            ..RuntimeConfig::default()
        })
        .expect("build runtime");
        let blocking = runtime.context().blocking().clone();
        let worker_name = runtime
            .block_on(blocking.run(4, || {
                thread::current().name().unwrap_or_default().to_owned()
            }))
            .expect("blocking job result");
        assert!(worker_name.starts_with("agentos-blocking-"));
        assert_eq!(blocking.worker_count(), 2);
        assert_eq!(blocking.reserved_bytes(), 0);
        let metrics = runtime.context().metrics().snapshot();
        assert_eq!(
            metrics.buffers[metrics::BufferMetricClass::Executor.index()].current,
            0
        );
        assert_eq!(
            metrics.buffers[metrics::BufferMetricClass::Executor.index()].high_water,
            4
        );
        assert!(
            metrics.executors[ExecutorMetricClass::Blocking.index()]
                .active
                .high_water
                >= 1
        );
    }

    #[test]
    fn blocking_executor_supports_bounded_synchronous_callers() {
        let runtime = SidecarRuntime::build(RuntimeConfig {
            worker_threads: 1,
            blocking_worker_threads: 1,
            max_queued_blocking_jobs: 1,
            max_blocking_job_bytes: 32,
            ..RuntimeConfig::default()
        })
        .expect("build runtime");
        let blocking = runtime.context().blocking().clone();
        let value = blocking
            .run_sync(4, Duration::from_secs(1), || 42usize)
            .expect("synchronous blocking job result");
        assert_eq!(value, 42);
        assert_eq!(blocking.reserved_bytes(), 0);
    }

    #[test]
    fn task_supervisor_reports_every_terminal_reason_and_reconciles() {
        let runtime = SidecarRuntime::build(RuntimeConfig {
            worker_threads: 1,
            blocking_worker_threads: 1,
            max_queued_blocking_jobs: 1,
            ..RuntimeConfig::default()
        })
        .expect("build runtime");
        let context = runtime.context();

        runtime.block_on(async {
            context
                .spawn(TaskClass::Runtime, async {})
                .expect("completed task admission")
                .await
                .expect("completed task join");

            let failed = context
                .spawn_result(TaskClass::Runtime, async { Err::<(), _>("failed") })
                .expect("failed task admission")
                .await
                .expect("failed task join");
            assert_eq!(failed, Err("failed"));

            let panicked = context
                .spawn(TaskClass::Runtime, async { panic!("task panic fixture") })
                .expect("panicked task admission")
                .await;
            assert!(panicked.expect_err("task must panic").is_panic());

            let cancelled = context
                .spawn(TaskClass::Runtime, std::future::pending::<()>())
                .expect("cancelled task admission");
            cancelled.abort();
            assert!(cancelled
                .await
                .expect_err("task must cancel")
                .is_cancelled());
        });

        let snapshot = context.tasks().snapshot(TaskClass::Runtime);
        assert_eq!(snapshot.active, 0);
        assert_eq!(snapshot.completed, 1);
        assert_eq!(snapshot.failed, 1);
        assert_eq!(snapshot.panicked, 1);
        assert_eq!(snapshot.cancelled, 1);
        assert_eq!(context.resources().usage(ResourceClass::Tasks).used, 0);
        let terminal_failure = context
            .terminal_failure()
            .expect("failed task must latch owner failure");
        assert_eq!(terminal_failure.reason, TaskTerminalReason::Failed);
        assert_eq!(terminal_failure.owner, TaskOwner::Process);
        let metric = context.metrics().snapshot().task(TaskClass::Runtime);
        assert_eq!(metric.active, 0);
        assert_eq!(metric.terminal_count(TaskTerminalReason::Completed), 1);
        assert_eq!(metric.terminal_count(TaskTerminalReason::Failed), 1);
        assert_eq!(metric.terminal_count(TaskTerminalReason::Panicked), 1);
        assert_eq!(metric.terminal_count(TaskTerminalReason::Cancelled), 1);
    }

    #[test]
    fn long_task_poll_records_watchdog_once_without_dynamic_labels() {
        let runtime = SidecarRuntime::build(RuntimeConfig {
            worker_threads: 1,
            blocking_worker_threads: 1,
            max_queued_blocking_jobs: 1,
            task_poll_watchdog_ms: 1,
            ..RuntimeConfig::default()
        })
        .expect("build runtime");
        let context = runtime.context();
        runtime.block_on(async {
            context
                .spawn(TaskClass::Runtime, async {
                    std::thread::sleep(Duration::from_millis(5));
                })
                .expect("task admission")
                .await
                .expect("task join");
        });
        let snapshot = context.metrics().snapshot();
        let watchdog = snapshot.watchdogs[WatchdogMetric::LongTaskPoll.index()];
        assert_eq!(watchdog.events, 1);
        assert!(watchdog.max_stall_micros >= 1_000);
        assert_eq!(
            snapshot.stderr_fallbacks[TelemetrySeverity::Warning.index()],
            1
        );
    }

    #[test]
    fn vm_scoped_context_shares_workers_and_charges_both_ledgers() {
        let runtime = SidecarRuntime::build(RuntimeConfig {
            worker_threads: 1,
            blocking_worker_threads: 1,
            max_queued_blocking_jobs: 1,
            max_blocking_job_bytes: 32,
            ..RuntimeConfig::default()
        })
        .expect("build runtime");
        let process = Arc::clone(runtime.context().resources());
        let vm_ledger = Arc::new(ResourceLedger::child(
            "vm=1 generation=1",
            [
                (
                    ResourceClass::Tasks,
                    ResourceLimit::new(1, "limits.reactor.maxTasks"),
                ),
                (
                    ResourceClass::ExecutorSlots,
                    ResourceLimit::new(1, "limits.blocking.maxJobs"),
                ),
                (
                    ResourceClass::ExecutorBytes,
                    ResourceLimit::new(8, "limits.blocking.maxQueuedBytes"),
                ),
            ],
            Arc::clone(&process),
        ));
        let scoped = runtime.context().scoped(Arc::clone(&vm_ledger));
        assert_eq!(scoped.blocking().worker_count(), 1);

        let value = runtime
            .block_on(scoped.blocking().run(8, || 42usize))
            .expect("VM-scoped blocking job");
        assert_eq!(value, 42);
        assert!(vm_ledger.is_zero());
        assert_eq!(
            process.usage(ResourceClass::ExecutorBytes).used,
            0,
            "child release must reconcile the process parent"
        );

        runtime.block_on(async {
            scoped
                .spawn(TaskClass::Vm, async {})
                .expect("VM task admission")
                .await
                .expect("VM task join");
        });
        assert!(vm_ledger.is_zero());
    }

    fn assert_vm_generation_churn_reconciles(generation_count: u64, logical_vm_count: u64) {
        use crate::capability::{CapabilityBackend, CapabilityKind, CapabilityRegistry};

        assert!(generation_count > 0);
        assert!(logical_vm_count > 0);

        let runtime = SidecarRuntime::build(RuntimeConfig {
            worker_threads: 2,
            blocking_worker_threads: 2,
            max_queued_blocking_jobs: 4,
            max_blocking_jobs: 8,
            max_blocking_job_bytes: 1024,
            ..RuntimeConfig::default()
        })
        .expect("build runtime");
        let process = Arc::clone(runtime.context().resources());

        runtime.block_on(async {
            for generation in 1..=generation_count {
                let vm_index = (generation - 1) % logical_vm_count;
                let resources = Arc::new(ResourceLedger::child(
                    format!("vm=churn-{vm_index} generation={generation}"),
                    [
                        (
                            ResourceClass::Capabilities,
                            ResourceLimit::new(2, "limits.reactor.maxCapabilities"),
                        ),
                        (
                            ResourceClass::ReadyHandles,
                            ResourceLimit::new(2, "limits.reactor.maxReadyHandles"),
                        ),
                        (
                            ResourceClass::Sockets,
                            ResourceLimit::new(2, "limits.resources.maxSockets"),
                        ),
                        (
                            ResourceClass::Connections,
                            ResourceLimit::new(2, "limits.resources.maxConnections"),
                        ),
                        (
                            ResourceClass::Tasks,
                            ResourceLimit::new(2, "limits.reactor.maxTasks"),
                        ),
                        (
                            ResourceClass::ExecutorSlots,
                            ResourceLimit::new(4, "limits.reactor.maxBlockingJobs"),
                        ),
                        (
                            ResourceClass::ExecutorBytes,
                            ResourceLimit::new(64, "limits.reactor.maxBlockingBytes"),
                        ),
                    ],
                    Arc::clone(&process),
                ));
                let context = runtime
                    .context()
                    .scoped_for_vm(Arc::clone(&resources), generation);
                let capabilities = CapabilityRegistry::new(generation, Arc::clone(&resources));
                let lease = capabilities
                    .reserve(CapabilityKind::TcpSocket)
                    .expect("reserve churn socket before allocation")
                    .commit(CapabilityBackend::Kernel {
                        socket_id: generation,
                    })
                    .expect("commit churn socket");

                let task = context
                    .spawn(TaskClass::Socket, async { tokio::task::yield_now().await })
                    .expect("admit churn task");
                let blocking = context
                    .blocking()
                    .run(16, move || generation)
                    .await
                    .expect("fixed blocking worker result");
                assert_eq!(blocking, generation);
                task.await.expect("churn task join");

                let capability_id = lease.id();
                let turn = context
                    .fairness()
                    .acquire(generation, capability_id, FairBudget::new(1, 64))
                    .await
                    .expect("churn fairness turn");
                turn.complete(FairBudget::new(1, 16), false)
                    .expect("complete churn fairness turn");
                drop(lease);
                capabilities
                    .close_admission()
                    .expect("close churn capability admission");
                context.close_admission();
                context.tasks().wait_empty().await;
                capabilities.wait_empty().await;
                context
                    .fairness()
                    .retire_capability(generation, capability_id)
                    .expect("retire churn capability fairness state");
                context
                    .fairness()
                    .retire_vm(generation)
                    .expect("retire churn VM fairness state");
                assert!(
                    resources.is_zero(),
                    "generation {generation} leaked accounting"
                );
                assert!(resources.integrity_ok());
            }
        });

        assert!(process.is_zero(), "VM churn drifted process accounting");
        assert!(process.integrity_ok());
        assert_eq!(runtime.context().tasks().active_total(), 0);
    }

    #[test]
    fn vm_generation_churn_reconciles_tasks_capabilities_fairness_and_bytes() {
        // Fast default-CI safeguard: enough generations to cross logical VM
        // identities repeatedly while keeping this test deterministic.
        assert_vm_generation_churn_reconciles(256, 8);
    }

    #[test]
    #[ignore = "expensive: multi-VM runtime accounting soak; run explicitly with --ignored"]
    fn multi_vm_generation_soak_has_no_accounting_or_scheduler_drift() {
        // The same invariant as the default regression, at a scale intended to
        // expose cumulative reservation, tombstone, and task-census drift.
        assert_vm_generation_churn_reconciles(50_000, 64);
    }

    #[test]
    fn closing_vm_context_rejects_stale_task_and_blocking_clones() {
        let runtime = SidecarRuntime::build(RuntimeConfig {
            worker_threads: 1,
            blocking_worker_threads: 1,
            max_queued_blocking_jobs: 1,
            ..RuntimeConfig::default()
        })
        .expect("build runtime");
        let vm_ledger = Arc::new(ResourceLedger::child(
            "vm-generation-77",
            [
                (
                    ResourceClass::Tasks,
                    ResourceLimit::new(4, "limits.reactor.maxTasks"),
                ),
                (
                    ResourceClass::ExecutorSlots,
                    ResourceLimit::new(4, "limits.reactor.maxBlockingJobs"),
                ),
                (
                    ResourceClass::ExecutorBytes,
                    ResourceLimit::new(64, "limits.reactor.maxBlockingBytes"),
                ),
            ],
            Arc::clone(runtime.context().resources()),
        ));
        let scoped = runtime.context().scoped_for_vm(Arc::clone(&vm_ledger), 77);
        let stale = scoped.clone();
        scoped.close_admission();

        let task_error = stale
            .spawn(TaskClass::Vm, async {})
            .expect_err("closed VM must reject stale task clone");
        assert!(matches!(task_error, TaskSpawnError::AdmissionClosed { .. }));
        let blocking_error = stale
            .blocking()
            .submit(1, || {})
            .expect_err("closed VM must reject stale blocking clone");
        assert_eq!(blocking_error, BlockingJobError::ShuttingDown);
        assert!(vm_ledger.is_zero());
    }

    #[test]
    fn close_admission_linearizes_task_and_blocking_rejection() {
        let runtime = SidecarRuntime::build(RuntimeConfig {
            worker_threads: 1,
            blocking_worker_threads: 1,
            max_queued_blocking_jobs: 1,
            ..RuntimeConfig::default()
        })
        .expect("build runtime");
        let vm_ledger = Arc::new(ResourceLedger::child(
            "vm-generation-88",
            [
                (
                    ResourceClass::Tasks,
                    ResourceLimit::new(2, "limits.reactor.maxTasks"),
                ),
                (
                    ResourceClass::ExecutorSlots,
                    ResourceLimit::new(2, "limits.reactor.maxBlockingJobs"),
                ),
                (
                    ResourceClass::ExecutorBytes,
                    ResourceLimit::new(8, "limits.reactor.maxBlockingBytes"),
                ),
            ],
            Arc::clone(runtime.context().resources()),
        ));
        let scoped = runtime.context().scoped_for_vm(Arc::clone(&vm_ledger), 88);

        // Blocking admission and task admission share this exact gate. Holding
        // it models an admission operation at its linearization point.
        let gate = Arc::clone(&scoped.blocking.admission_gate);
        let held = gate.lock().expect("admission gate");
        let closing = scoped.clone();
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (closed_tx, closed_rx) = mpsc::sync_channel(1);
        let close_thread = thread::spawn(move || {
            started_tx.send(()).expect("publish close start");
            closing.close_admission();
            closed_tx.send(()).expect("publish close completion");
        });
        started_rx.recv().expect("close started");
        assert_eq!(
            closed_rx.recv_timeout(Duration::from_millis(20)),
            Err(mpsc::RecvTimeoutError::Timeout),
            "close must wait for the shared admission linearization gate"
        );
        drop(held);
        closed_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("close completed after admission released");
        close_thread.join().expect("close thread");

        let task_error = scoped
            .spawn(TaskClass::Vm, async {})
            .expect_err("task admission after linearized close must fail");
        assert!(matches!(task_error, TaskSpawnError::AdmissionClosed { .. }));
        let blocking_error = scoped
            .blocking()
            .submit(1, || {})
            .expect_err("blocking admission after linearized close must fail");
        assert_eq!(blocking_error, BlockingJobError::ShuttingDown);
        assert!(vm_ledger.is_zero());
    }

    #[test]
    fn closing_vm_context_wakes_admitted_readiness_waiters() {
        let runtime = SidecarRuntime::build(RuntimeConfig {
            worker_threads: 1,
            ..RuntimeConfig::default()
        })
        .expect("build runtime");
        let resources = Arc::new(ResourceLedger::child(
            "vm-generation-88",
            [(
                ResourceClass::Tasks,
                ResourceLimit::new(2, "limits.reactor.maxTasks"),
            )],
            Arc::clone(runtime.context().resources()),
        ));
        let scoped = runtime.context().scoped_for_vm(resources, 88);
        let waiter_context = scoped.clone();
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
        let waiter = scoped
            .spawn(TaskClass::Socket, async move {
                started_tx.send(()).expect("signal waiter admission");
                waiter_context.admission_closed().await;
            })
            .expect("spawn readiness waiter");

        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("waiter started");
        scoped.close_admission();
        runtime.context().handle().block_on(async {
            tokio::time::timeout(Duration::from_secs(1), waiter)
                .await
                .expect("close wakes waiter before teardown deadline")
                .expect("waiter joins")
        });
        assert_eq!(scoped.tasks().active_scoped(), 0);
    }
}
