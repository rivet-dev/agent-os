//! Typed, operator-tunable VM-scoped runtime limits.
//!
//! `VmLimits` is the single home for runtime bounds that operators may tune through the typed
//! create-VM JSON config. Every field is a concrete value (not `Option`): the `Default` impls own
//! the numbers and they are byte-identical to the historical hardcoded constants, so behavior is
//! unchanged unless an operator overrides a config field.

use agentos_kernel::resource_accounting::ResourceLimits;
#[cfg(test)]
use agentos_vm_config::ExecutionLimitsConfig;
use agentos_vm_config::{
    Http2LimitsConfig, ReactorLimitsConfig, ResourceLimitsConfig, TlsLimitsConfig, UdpLimitsConfig,
    VmLimitsConfig,
};

use crate::SidecarCoreError;

/// Default cap on `vm.fetch()` buffered response bodies. Historically aliased to the wire frame
/// cap; decoupled here but still validated to stay within the negotiated frame budget.
pub const DEFAULT_MAX_FETCH_RESPONSE_BYTES: usize = 1024 * 1024;

pub const DEFAULT_BINDING_TIMEOUT_MS: u64 = 30_000;
pub const MAX_BINDING_TIMEOUT_MS: u64 = 300_000;
pub const MAX_REGISTERED_BINDING_COLLECTIONS: usize = 64;
pub const MAX_REGISTERED_BINDINGS_PER_VM: usize = 256;
pub const MAX_BINDINGS_PER_COLLECTION: usize = 64;
pub const MAX_BINDING_SCHEMA_BYTES: usize = 16 * 1024;
pub const MAX_EXAMPLES_PER_BINDING: usize = 16;
pub const MAX_BINDING_EXAMPLE_INPUT_BYTES: usize = 4 * 1024;

pub const MAX_PERSISTED_MANIFEST_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_PERSISTED_MANIFEST_FILE_BYTES: u64 = 1024 * 1024 * 1024;

pub const DEFAULT_ACP_MAX_READ_LINE_BYTES: usize = 16 * 1024 * 1024;
pub const DEFAULT_ACP_STDOUT_BUFFER_BYTE_LIMIT: usize = 1024 * 1024;
pub const DEFAULT_ACP_MAX_COMPLETED_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
pub const DEFAULT_ACP_MAX_TURN_OUTPUT_BYTES: usize = 256 * 1024 * 1024;
pub const DEFAULT_ACP_MAX_PROMPT_BYTES: usize = 64 * 1024 * 1024;
pub const DEFAULT_ACP_MAX_PROMPT_BLOCKS: usize = 16_384;
pub const DEFAULT_ACP_MAX_FALLBACK_CONTINUATION_BYTES: usize = 4 * 1024 * 1024;
pub const DEFAULT_ACP_MAX_SESSION_HISTORY_BYTES: usize = 1024 * 1024 * 1024;
pub const DEFAULT_ACP_MAX_SESSION_HISTORY_EVENTS: usize = 1_000_000;
pub const DEFAULT_ACP_MAX_HISTORY_PAGE_ENTRIES: usize = 10_000;
pub const DEFAULT_ACP_MAX_SESSION_LIST_ENTRIES: usize = 10_000;
pub const DEFAULT_ACP_MAX_SESSIONS_PER_VM: usize = 10_000;
pub const DEFAULT_ACP_MAX_PROMPTS_PER_SESSION: usize = 100_000;
pub const DEFAULT_ACP_MAX_PROMPTS_PER_VM: usize = 1_000_000;
pub const DEFAULT_ACP_MAX_PENDING_PERMISSIONS_PER_SESSION: usize = 1_000;
pub const DEFAULT_ACP_MAX_PENDING_PERMISSIONS_PER_VM: usize = 10_000;
pub const DEFAULT_ACP_MAX_PERMISSION_OUTCOMES_PER_SESSION: usize = 10_000;
pub const DEFAULT_ACP_MAX_PERMISSION_OUTCOMES_PER_VM: usize = 100_000;
pub const DEFAULT_SQLITE_MAX_RESULT_BYTES: usize = 128 * 1024 * 1024;

pub const DEFAULT_JS_CAPTURED_OUTPUT_LIMIT_BYTES: usize = 16 * 1024 * 1024;
pub const DEFAULT_JS_STDIN_BUFFER_LIMIT_BYTES: usize = 16 * 1024 * 1024;
pub const DEFAULT_JS_EVENT_PAYLOAD_LIMIT_BYTES: usize = 1024 * 1024;
pub const DEFAULT_JS_MAX_TIMERS: usize = 4_096;
pub const DEFAULT_V8_IPC_MAX_FRAME_BYTES: u32 = 64 * 1024 * 1024;
pub const DEFAULT_V8_HEAP_LIMIT_MB: u32 = 128;
pub const DEFAULT_V8_CPU_TIME_LIMIT_MS: u32 = 30_000;
pub const DEFAULT_V8_WALL_CLOCK_LIMIT_MS: u32 = 0;
pub const DEFAULT_NODE_IMPORT_CACHE_MATERIALIZE_TIMEOUT_MS: u64 = 120_000;

pub const DEFAULT_PYTHON_OUTPUT_BUFFER_MAX_BYTES: usize = 1024 * 1024;
pub const DEFAULT_PYTHON_EXECUTION_TIMEOUT_MS: u64 = 5 * 60 * 1000;
/// `0` keeps the Pyodide runner's V8 old-space at the engine default.
pub const DEFAULT_PYTHON_MAX_OLD_SPACE_MB: usize = 0;
pub const DEFAULT_PYTHON_VFS_RPC_TIMEOUT_MS: u64 = 30 * 1000;

pub const DEFAULT_WASM_MAX_MODULE_FILE_BYTES: u64 = 256 * 1024 * 1024;
pub const DEFAULT_WASM_CAPTURED_OUTPUT_LIMIT_BYTES: usize = 16 * 1024 * 1024;
pub const DEFAULT_WASM_SYNC_READ_LIMIT_BYTES: usize = 16 * 1024 * 1024;
pub const DEFAULT_WASM_PREWARM_TIMEOUT_MS: u64 = 30_000;
pub const DEFAULT_WASM_RUNNER_HEAP_LIMIT_MB: u32 = 2048;
pub const DEFAULT_WASM_ACTIVE_CPU_TIME_LIMIT_MS: u32 = 30_000;
pub const DEFAULT_WASM_MAX_THREADS: usize = 16;
pub const DEFAULT_WASM_MAX_CONCURRENT_THREADS: usize = 64;
pub const DEFAULT_PROCESS_PENDING_STDIN_BYTES: usize = 64 * 1024 * 1024;
pub const DEFAULT_PROCESS_MAX_SPAWN_FILE_ACTIONS: usize = 4096;
pub const DEFAULT_PROCESS_MAX_SPAWN_FILE_ACTION_BYTES: usize = 1024 * 1024;
pub const DEFAULT_PROCESS_PENDING_EVENT_COUNT: usize = 10_000;
pub const DEFAULT_PROCESS_PENDING_EVENT_BYTES: usize = 64 * 1024 * 1024;
pub const DEFAULT_EXECUTION_COMPLETED_TTL_MS: u64 = 5 * 60 * 1000;
pub const DEFAULT_EXECUTION_MAX_COMPLETED_EXECUTIONS: usize = 1_024;
pub const DEFAULT_EXECUTION_LIVE_WARNING_THRESHOLD: usize = 64;
pub const DEFAULT_PROCESS_MAX_PENDING_CHILD_SYNC_COUNT: usize = 64;
pub const DEFAULT_PROCESS_MAX_PENDING_CHILD_SYNC_BYTES: usize = 64 * 1024 * 1024;

pub const DEFAULT_REACTOR_MAX_CAPABILITIES: usize = 4096;
pub const DEFAULT_REACTOR_MAX_READY_HANDLES: usize = 4096;
pub const DEFAULT_REACTOR_MAX_TASKS: usize = 8192;
pub const DEFAULT_REACTOR_WORK_QUANTUM: usize = 64;
pub const DEFAULT_REACTOR_BYTE_QUANTUM: usize = 256 * 1024;
pub const DEFAULT_REACTOR_MAX_HANDLE_COMMANDS: usize = 256;
pub const DEFAULT_REACTOR_MAX_HANDLE_COMMAND_BYTES: usize = 1024 * 1024;
pub const DEFAULT_REACTOR_MAX_BRIDGE_CALLS: usize = 16_384;
pub const DEFAULT_REACTOR_MAX_BRIDGE_REQUEST_BYTES: usize = 16 * 1024 * 1024;
pub const DEFAULT_REACTOR_MAX_BRIDGE_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
pub const DEFAULT_REACTOR_MAX_ASYNC_COMPLETIONS: usize = 1024;
pub const DEFAULT_REACTOR_MAX_ASYNC_COMPLETION_BYTES: usize = 16 * 1024 * 1024;
pub const DEFAULT_REACTOR_MAX_BLOCKING_JOBS: usize = 256;
pub const DEFAULT_REACTOR_MAX_BLOCKING_BYTES: usize = 64 * 1024 * 1024;
pub const DEFAULT_REACTOR_PER_HANDLE_OPERATION_QUANTUM: usize = 16;
pub const DEFAULT_REACTOR_ACCEPT_QUANTUM: usize = 256;
pub const DEFAULT_REACTOR_DATAGRAM_QUANTUM: usize = 64;
pub const DEFAULT_REACTOR_COMPLETION_QUANTUM: usize = 64;
pub const DEFAULT_REACTOR_SIGNAL_QUANTUM: usize = 64;
pub const DEFAULT_REACTOR_SHUTDOWN_DEADLINE_MS: u64 = 5_000;
pub const DEFAULT_REACTOR_OPERATION_DEADLINE_MS: u64 = 30_000;

pub const DEFAULT_UDP_MAX_BUFFERED_DATAGRAMS: usize = 1024;
pub const DEFAULT_UDP_MAX_BUFFERED_BYTES: usize = 4 * 1024 * 1024;
pub const DEFAULT_TLS_MAX_BUFFERED_BYTES: usize = 1024 * 1024;
pub const DEFAULT_HTTP2_MAX_CONNECTIONS: usize = 256;
pub const DEFAULT_HTTP2_MAX_STREAMS: usize = 4096;
pub const DEFAULT_HTTP2_MAX_STREAMS_PER_CONNECTION: usize = 256;
pub const DEFAULT_HTTP2_MAX_BUFFERED_BYTES: usize = 4 * 1024 * 1024;
pub const DEFAULT_HTTP2_MAX_HEADER_BYTES: usize = 64 * 1024;
pub const DEFAULT_HTTP2_MAX_DATA_BYTES: usize = 4 * 1024 * 1024;
pub const DEFAULT_HTTP2_MAX_PENDING_COMMANDS: usize = 256;
pub const DEFAULT_HTTP2_MAX_PENDING_COMMAND_BYTES: usize = 1024 * 1024;
pub const DEFAULT_HTTP2_MAX_PENDING_EVENTS: usize = 256;
pub const DEFAULT_HTTP2_MAX_PENDING_EVENT_BYTES: usize = 4 * 1024 * 1024;

/// All operator-tunable VM-scoped limits. Fields are concrete values; the `Default` impls own the
/// numbers and equal today's hardcoded constants, so unset operator config leaves behavior
/// unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VmLimits {
    pub reactor: ReactorLimits,
    /// Kernel resource limits (existing type, existing `resource.*` keys).
    pub resources: ResourceLimits,
    pub http: HttpLimits,
    pub udp: UdpLimits,
    pub tls: TlsLimits,
    pub http2: Http2Limits,
    pub bindings: BindingLimits,
    pub plugins: PluginLimits,
    pub acp: AcpLimits,
    pub sqlite: SqliteLimits,
    pub js_runtime: JsRuntimeLimits,
    pub python: PythonLimits,
    pub wasm: WasmLimits,
    pub execution: ExecutionLimits,
    pub process: ProcessLimits,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionLimits {
    pub completed_ttl_ms: u64,
    pub max_completed_executions: usize,
    pub live_execution_warning_threshold: usize,
}

impl Default for ExecutionLimits {
    fn default() -> Self {
        Self {
            completed_ttl_ms: DEFAULT_EXECUTION_COMPLETED_TTL_MS,
            max_completed_executions: DEFAULT_EXECUTION_MAX_COMPLETED_EXECUTIONS,
            live_execution_warning_threshold: DEFAULT_EXECUTION_LIVE_WARNING_THRESHOLD,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReactorLimits {
    pub max_capabilities: usize,
    pub max_ready_handles: usize,
    pub max_tasks: usize,
    pub work_quantum: usize,
    pub byte_quantum: usize,
    pub max_handle_commands: usize,
    pub max_handle_command_bytes: usize,
    pub max_bridge_calls: usize,
    pub max_bridge_request_bytes: usize,
    pub max_bridge_response_bytes: usize,
    pub max_async_completions: usize,
    pub max_async_completion_bytes: usize,
    pub max_blocking_jobs: usize,
    pub max_blocking_bytes: usize,
    pub per_handle_operation_quantum: usize,
    pub accept_quantum: usize,
    pub datagram_quantum: usize,
    pub completion_quantum: usize,
    pub signal_quantum: usize,
    pub shutdown_deadline_ms: u64,
    pub operation_deadline_ms: u64,
}

impl Default for ReactorLimits {
    fn default() -> Self {
        Self {
            max_capabilities: DEFAULT_REACTOR_MAX_CAPABILITIES,
            max_ready_handles: DEFAULT_REACTOR_MAX_READY_HANDLES,
            max_tasks: DEFAULT_REACTOR_MAX_TASKS,
            work_quantum: DEFAULT_REACTOR_WORK_QUANTUM,
            byte_quantum: DEFAULT_REACTOR_BYTE_QUANTUM,
            max_handle_commands: DEFAULT_REACTOR_MAX_HANDLE_COMMANDS,
            max_handle_command_bytes: DEFAULT_REACTOR_MAX_HANDLE_COMMAND_BYTES,
            max_bridge_calls: DEFAULT_REACTOR_MAX_BRIDGE_CALLS,
            max_bridge_request_bytes: DEFAULT_REACTOR_MAX_BRIDGE_REQUEST_BYTES,
            max_bridge_response_bytes: DEFAULT_REACTOR_MAX_BRIDGE_RESPONSE_BYTES,
            max_async_completions: DEFAULT_REACTOR_MAX_ASYNC_COMPLETIONS,
            max_async_completion_bytes: DEFAULT_REACTOR_MAX_ASYNC_COMPLETION_BYTES,
            max_blocking_jobs: DEFAULT_REACTOR_MAX_BLOCKING_JOBS,
            max_blocking_bytes: DEFAULT_REACTOR_MAX_BLOCKING_BYTES,
            per_handle_operation_quantum: DEFAULT_REACTOR_PER_HANDLE_OPERATION_QUANTUM,
            accept_quantum: DEFAULT_REACTOR_ACCEPT_QUANTUM,
            datagram_quantum: DEFAULT_REACTOR_DATAGRAM_QUANTUM,
            completion_quantum: DEFAULT_REACTOR_COMPLETION_QUANTUM,
            signal_quantum: DEFAULT_REACTOR_SIGNAL_QUANTUM,
            shutdown_deadline_ms: DEFAULT_REACTOR_SHUTDOWN_DEADLINE_MS,
            operation_deadline_ms: DEFAULT_REACTOR_OPERATION_DEADLINE_MS,
        }
    }
}

pub fn virtual_os_cpu_count(resource_limits: &ResourceLimits) -> usize {
    resource_limits.virtual_cpu_count.unwrap_or(1).max(1)
}

pub fn virtual_os_totalmem_bytes(resource_limits: &ResourceLimits) -> u64 {
    resource_limits
        .max_wasm_memory_bytes
        .unwrap_or(1024 * 1024 * 1024)
}

pub fn virtual_os_freemem_bytes(resource_limits: &ResourceLimits) -> u64 {
    resource_limits
        .max_wasm_memory_bytes
        .unwrap_or(512 * 1024 * 1024)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpLimits {
    /// Cap on `vm.fetch()` buffered response bodies. Must be `<=` the sidecar wire frame cap.
    pub max_fetch_response_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UdpLimits {
    pub max_buffered_datagrams: usize,
    pub max_buffered_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TlsLimits {
    pub max_buffered_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Http2Limits {
    pub max_connections: usize,
    pub max_streams: usize,
    pub max_streams_per_connection: usize,
    pub max_buffered_bytes: usize,
    pub max_header_bytes: usize,
    pub max_data_bytes: usize,
    pub max_pending_commands: usize,
    pub max_pending_command_bytes: usize,
    pub max_pending_events: usize,
    pub max_pending_event_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingLimits {
    pub default_binding_timeout_ms: u64,
    pub max_binding_timeout_ms: u64,
    pub max_registered_collections: usize,
    pub max_registered_bindings_per_vm: usize,
    pub max_bindings_per_collection: usize,
    pub max_binding_schema_bytes: usize,
    pub max_examples_per_binding: usize,
    pub max_binding_example_input_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginLimits {
    pub max_persisted_manifest_bytes: usize,
    pub max_persisted_manifest_file_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpLimits {
    /// Maximum length of a single ACP adapter stdout line. Threaded into `AcpClientOptions`.
    pub max_read_line_bytes: usize,
    /// Pre-session ACP adapter stdout buffer cap.
    pub stdout_buffer_byte_limit: usize,
    /// Maximum serialized bytes retained while completing one message.
    pub max_completed_message_bytes: usize,
    /// Maximum serialized ACP update bytes accepted during one turn.
    pub max_turn_output_bytes: usize,
    /// Maximum serialized bytes accepted in one ACP prompt content array.
    pub max_prompt_bytes: usize,
    /// Maximum content blocks accepted in one ACP prompt.
    pub max_prompt_blocks: usize,
    /// Maximum recent durable history bytes included in a fallback continuation preamble.
    pub max_fallback_continuation_bytes: usize,
    /// Per-session durable history retention budget. Oldest completed events are pruned.
    pub max_session_history_bytes: usize,
    /// Per-session durable history event retention budget.
    pub max_session_history_events: usize,
    /// Maximum entries in one durable history response.
    pub max_history_page_entries: usize,
    /// Maximum entries in one session-list response.
    pub max_session_list_entries: usize,
    /// Maximum durable sessions stored in one VM database.
    pub max_sessions_per_vm: usize,
    /// Maximum retained prompt/idempotency records for one session.
    pub max_prompts_per_session: usize,
    /// Maximum retained prompt/idempotency records across one VM.
    pub max_prompts_per_vm: usize,
    /// Maximum actionable permission requests for one session.
    pub max_pending_permissions_per_session: usize,
    /// Maximum actionable permission requests across one VM.
    pub max_pending_permissions_per_vm: usize,
    /// Maximum retained terminal permission outcomes for one session.
    pub max_permission_outcomes_per_session: usize,
    /// Maximum retained terminal permission outcomes across one VM.
    pub max_permission_outcomes_per_vm: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqliteLimits {
    /// Maximum materialized bytes returned by one SQLite statement.
    pub max_result_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsRuntimeLimits {
    /// `None` keeps the V8 engine default heap. Carried as the typed
    /// `JavascriptExecutionLimits.v8_heap_limit_mb` on the execution request
    /// (no longer the `AGENTOS_V8_HEAP_LIMIT_MB` env knob).
    pub v8_heap_limit_mb: Option<u32>,
    /// Sync-RPC blocking-wait ceiling in ms. `None` keeps the engine default.
    pub sync_rpc_wait_timeout_ms: Option<u64>,
    /// Active JavaScript CPU-time budget in ms. `0` disables the CPU watchdog.
    pub cpu_time_limit_ms: u32,
    /// JavaScript wall-clock backstop in ms. `0` disables the wall-clock watchdog.
    pub wall_clock_limit_ms: u32,
    /// Timeout for materializing the per-VM Node import cache.
    pub import_cache_materialize_timeout_ms: u64,
    pub captured_output_limit_bytes: usize,
    pub stdin_buffer_limit_bytes: usize,
    pub event_payload_limit_bytes: usize,
    /// Maximum live timers owned by one VM execution. Each timer is also
    /// charged to the process-wide `runtime.resources.maxTimers` ledger.
    pub max_timers: usize,
    /// V8 IPC codec frame cap. Must feed both codec sides (`crates/execution/src/v8_ipc.rs` and
    /// `crates/v8-runtime/src/ipc_binary.rs`).
    pub v8_ipc_max_frame_bytes: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PythonLimits {
    pub output_buffer_max_bytes: usize,
    pub execution_timeout_ms: u64,
    /// Pyodide V8 old-space cap in MB (`0` keeps the V8 default).
    pub max_old_space_mb: usize,
    pub vfs_rpc_timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WasmLimits {
    pub max_module_file_bytes: u64,
    pub captured_output_limit_bytes: usize,
    /// WASM sync read cap. Also templated into the JS runner shim, so it must flow from one field.
    pub sync_read_limit_bytes: usize,
    /// Best-effort warmup/compile-cache timeout.
    pub prewarm_timeout_ms: u64,
    /// V8 heap cap for the trusted JS runner isolate that hosts WASI/WASM.
    pub runner_heap_limit_mb: u32,
    /// Active CPU-time cap for standalone WASM execution. The V8 compatibility
    /// backend applies this to its runner isolate's CPU watchdog.
    pub active_cpu_time_limit_ms: u32,
    /// Optional elapsed wall-clock backstop for standalone WASM execution.
    pub wall_clock_limit_ms: Option<u64>,
    /// Optional deterministic instruction budget. The V8 compatibility backend
    /// rejects explicit values because V8 cannot meter deterministic fuel.
    pub deterministic_fuel: Option<u64>,
    /// Maximum threads in one explicitly threaded standalone-WASM group,
    /// including its initial thread.
    pub max_threads: usize,
    /// Maximum threads transactionally reserved by all concurrently running
    /// threaded standalone-WASM groups in one VM.
    pub max_concurrent_threads: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessLimits {
    /// Maximum file actions decoded for one posix_spawn request.
    pub max_spawn_file_actions: usize,
    /// Maximum serialized file-action bytes accepted for one spawn request.
    pub max_spawn_file_action_bytes: usize,
    /// Host-side bytes accepted for pipe-backed stdin but not yet written.
    pub pending_stdin_bytes: usize,
    /// Maximum queued process events at each sidecar queue stage.
    pub pending_event_count: usize,
    /// Maximum aggregate payload bytes retained at each process-event stage.
    pub pending_event_bytes: usize,
    /// Maximum concurrent synchronous child-process calls retained by one VM.
    pub max_pending_child_sync_count: usize,
    /// Maximum aggregate input and output capacity retained for synchronous
    /// child-process calls by one VM.
    pub max_pending_child_sync_bytes: usize,
}

impl Default for HttpLimits {
    fn default() -> Self {
        Self {
            max_fetch_response_bytes: DEFAULT_MAX_FETCH_RESPONSE_BYTES,
        }
    }
}

impl Default for UdpLimits {
    fn default() -> Self {
        Self {
            max_buffered_datagrams: DEFAULT_UDP_MAX_BUFFERED_DATAGRAMS,
            max_buffered_bytes: DEFAULT_UDP_MAX_BUFFERED_BYTES,
        }
    }
}

impl Default for TlsLimits {
    fn default() -> Self {
        Self {
            max_buffered_bytes: DEFAULT_TLS_MAX_BUFFERED_BYTES,
        }
    }
}

impl Default for Http2Limits {
    fn default() -> Self {
        Self {
            max_connections: DEFAULT_HTTP2_MAX_CONNECTIONS,
            max_streams: DEFAULT_HTTP2_MAX_STREAMS,
            max_streams_per_connection: DEFAULT_HTTP2_MAX_STREAMS_PER_CONNECTION,
            max_buffered_bytes: DEFAULT_HTTP2_MAX_BUFFERED_BYTES,
            max_header_bytes: DEFAULT_HTTP2_MAX_HEADER_BYTES,
            max_data_bytes: DEFAULT_HTTP2_MAX_DATA_BYTES,
            max_pending_commands: DEFAULT_HTTP2_MAX_PENDING_COMMANDS,
            max_pending_command_bytes: DEFAULT_HTTP2_MAX_PENDING_COMMAND_BYTES,
            max_pending_events: DEFAULT_HTTP2_MAX_PENDING_EVENTS,
            max_pending_event_bytes: DEFAULT_HTTP2_MAX_PENDING_EVENT_BYTES,
        }
    }
}

impl Default for BindingLimits {
    fn default() -> Self {
        Self {
            default_binding_timeout_ms: DEFAULT_BINDING_TIMEOUT_MS,
            max_binding_timeout_ms: MAX_BINDING_TIMEOUT_MS,
            max_registered_collections: MAX_REGISTERED_BINDING_COLLECTIONS,
            max_registered_bindings_per_vm: MAX_REGISTERED_BINDINGS_PER_VM,
            max_bindings_per_collection: MAX_BINDINGS_PER_COLLECTION,
            max_binding_schema_bytes: MAX_BINDING_SCHEMA_BYTES,
            max_examples_per_binding: MAX_EXAMPLES_PER_BINDING,
            max_binding_example_input_bytes: MAX_BINDING_EXAMPLE_INPUT_BYTES,
        }
    }
}

impl Default for PluginLimits {
    fn default() -> Self {
        Self {
            max_persisted_manifest_bytes: MAX_PERSISTED_MANIFEST_BYTES,
            max_persisted_manifest_file_bytes: MAX_PERSISTED_MANIFEST_FILE_BYTES,
        }
    }
}

impl Default for AcpLimits {
    fn default() -> Self {
        Self {
            max_read_line_bytes: DEFAULT_ACP_MAX_READ_LINE_BYTES,
            stdout_buffer_byte_limit: DEFAULT_ACP_STDOUT_BUFFER_BYTE_LIMIT,
            max_completed_message_bytes: DEFAULT_ACP_MAX_COMPLETED_MESSAGE_BYTES,
            max_turn_output_bytes: DEFAULT_ACP_MAX_TURN_OUTPUT_BYTES,
            max_prompt_bytes: DEFAULT_ACP_MAX_PROMPT_BYTES,
            max_prompt_blocks: DEFAULT_ACP_MAX_PROMPT_BLOCKS,
            max_fallback_continuation_bytes: DEFAULT_ACP_MAX_FALLBACK_CONTINUATION_BYTES,
            max_session_history_bytes: DEFAULT_ACP_MAX_SESSION_HISTORY_BYTES,
            max_session_history_events: DEFAULT_ACP_MAX_SESSION_HISTORY_EVENTS,
            max_history_page_entries: DEFAULT_ACP_MAX_HISTORY_PAGE_ENTRIES,
            max_session_list_entries: DEFAULT_ACP_MAX_SESSION_LIST_ENTRIES,
            max_sessions_per_vm: DEFAULT_ACP_MAX_SESSIONS_PER_VM,
            max_prompts_per_session: DEFAULT_ACP_MAX_PROMPTS_PER_SESSION,
            max_prompts_per_vm: DEFAULT_ACP_MAX_PROMPTS_PER_VM,
            max_pending_permissions_per_session: DEFAULT_ACP_MAX_PENDING_PERMISSIONS_PER_SESSION,
            max_pending_permissions_per_vm: DEFAULT_ACP_MAX_PENDING_PERMISSIONS_PER_VM,
            max_permission_outcomes_per_session: DEFAULT_ACP_MAX_PERMISSION_OUTCOMES_PER_SESSION,
            max_permission_outcomes_per_vm: DEFAULT_ACP_MAX_PERMISSION_OUTCOMES_PER_VM,
        }
    }
}

impl Default for SqliteLimits {
    fn default() -> Self {
        Self {
            max_result_bytes: DEFAULT_SQLITE_MAX_RESULT_BYTES,
        }
    }
}

impl Default for JsRuntimeLimits {
    fn default() -> Self {
        Self {
            // Workers-style 128 MiB heap cap by default. Operators can raise or
            // clear this through trusted VM config when a VM needs more room.
            v8_heap_limit_mb: Some(DEFAULT_V8_HEAP_LIMIT_MB),
            sync_rpc_wait_timeout_ms: None,
            cpu_time_limit_ms: DEFAULT_V8_CPU_TIME_LIMIT_MS,
            wall_clock_limit_ms: DEFAULT_V8_WALL_CLOCK_LIMIT_MS,
            import_cache_materialize_timeout_ms: DEFAULT_NODE_IMPORT_CACHE_MATERIALIZE_TIMEOUT_MS,
            captured_output_limit_bytes: DEFAULT_JS_CAPTURED_OUTPUT_LIMIT_BYTES,
            stdin_buffer_limit_bytes: DEFAULT_JS_STDIN_BUFFER_LIMIT_BYTES,
            event_payload_limit_bytes: DEFAULT_JS_EVENT_PAYLOAD_LIMIT_BYTES,
            max_timers: DEFAULT_JS_MAX_TIMERS,
            v8_ipc_max_frame_bytes: DEFAULT_V8_IPC_MAX_FRAME_BYTES,
        }
    }
}

impl Default for PythonLimits {
    fn default() -> Self {
        Self {
            output_buffer_max_bytes: DEFAULT_PYTHON_OUTPUT_BUFFER_MAX_BYTES,
            execution_timeout_ms: DEFAULT_PYTHON_EXECUTION_TIMEOUT_MS,
            max_old_space_mb: DEFAULT_PYTHON_MAX_OLD_SPACE_MB,
            vfs_rpc_timeout_ms: DEFAULT_PYTHON_VFS_RPC_TIMEOUT_MS,
        }
    }
}

impl Default for WasmLimits {
    fn default() -> Self {
        Self {
            max_module_file_bytes: DEFAULT_WASM_MAX_MODULE_FILE_BYTES,
            captured_output_limit_bytes: DEFAULT_WASM_CAPTURED_OUTPUT_LIMIT_BYTES,
            sync_read_limit_bytes: DEFAULT_WASM_SYNC_READ_LIMIT_BYTES,
            prewarm_timeout_ms: DEFAULT_WASM_PREWARM_TIMEOUT_MS,
            runner_heap_limit_mb: DEFAULT_WASM_RUNNER_HEAP_LIMIT_MB,
            active_cpu_time_limit_ms: DEFAULT_WASM_ACTIVE_CPU_TIME_LIMIT_MS,
            wall_clock_limit_ms: None,
            deterministic_fuel: None,
            max_threads: DEFAULT_WASM_MAX_THREADS,
            max_concurrent_threads: DEFAULT_WASM_MAX_CONCURRENT_THREADS,
        }
    }
}

impl Default for ProcessLimits {
    fn default() -> Self {
        Self {
            max_spawn_file_actions: DEFAULT_PROCESS_MAX_SPAWN_FILE_ACTIONS,
            max_spawn_file_action_bytes: DEFAULT_PROCESS_MAX_SPAWN_FILE_ACTION_BYTES,
            pending_stdin_bytes: DEFAULT_PROCESS_PENDING_STDIN_BYTES,
            pending_event_count: DEFAULT_PROCESS_PENDING_EVENT_COUNT,
            pending_event_bytes: DEFAULT_PROCESS_PENDING_EVENT_BYTES,
            max_pending_child_sync_count: DEFAULT_PROCESS_MAX_PENDING_CHILD_SYNC_COUNT,
            max_pending_child_sync_bytes: DEFAULT_PROCESS_MAX_PENDING_CHILD_SYNC_BYTES,
        }
    }
}

pub fn vm_limits_from_config(
    config: Option<&VmLimitsConfig>,
    sidecar_max_frame_bytes: usize,
) -> Result<VmLimits, SidecarCoreError> {
    let mut limits = VmLimits::default();
    let Some(config) = config else {
        validate_vm_limits(&limits, sidecar_max_frame_bytes)?;
        return Ok(limits);
    };

    if let Some(reactor) = config.reactor.as_ref() {
        apply_reactor_limits_config(&mut limits.reactor, reactor)?;
    }

    if let Some(resources) = config.resources.as_ref() {
        apply_resource_limits_config(&mut limits.resources, resources)?;
    }
    if let Some(http) = config.http.as_ref() {
        set_usize(
            &mut limits.http.max_fetch_response_bytes,
            http.max_fetch_response_bytes,
            "limits.http.maxFetchResponseBytes",
        )?;
    }
    if let Some(udp) = config.udp.as_ref() {
        apply_udp_limits_config(&mut limits.udp, udp)?;
    }
    if let Some(tls) = config.tls.as_ref() {
        apply_tls_limits_config(&mut limits.tls, tls)?;
    }
    if let Some(http2) = config.http2.as_ref() {
        apply_http2_limits_config(&mut limits.http2, http2)?;
    }
    if let Some(bindings) = config.bindings.as_ref() {
        set_u64(
            &mut limits.bindings.default_binding_timeout_ms,
            bindings.default_binding_timeout_ms,
            "limits.bindings.defaultBindingTimeoutMs",
        )?;
        set_u64(
            &mut limits.bindings.max_binding_timeout_ms,
            bindings.max_binding_timeout_ms,
            "limits.bindings.maxBindingTimeoutMs",
        )?;
        set_usize(
            &mut limits.bindings.max_registered_collections,
            bindings.max_registered_collections,
            "limits.bindings.maxRegisteredCollections",
        )?;
        set_usize(
            &mut limits.bindings.max_registered_bindings_per_vm,
            bindings.max_registered_bindings_per_vm,
            "limits.bindings.maxRegisteredBindingsPerVm",
        )?;
        set_usize(
            &mut limits.bindings.max_bindings_per_collection,
            bindings.max_bindings_per_collection,
            "limits.bindings.maxBindingsPerCollection",
        )?;
        set_usize(
            &mut limits.bindings.max_binding_schema_bytes,
            bindings.max_binding_schema_bytes,
            "limits.bindings.maxBindingSchemaBytes",
        )?;
        set_usize(
            &mut limits.bindings.max_examples_per_binding,
            bindings.max_examples_per_binding,
            "limits.bindings.maxExamplesPerBinding",
        )?;
        set_usize(
            &mut limits.bindings.max_binding_example_input_bytes,
            bindings.max_binding_example_input_bytes,
            "limits.bindings.maxBindingExampleInputBytes",
        )?;
    }
    if let Some(plugins) = config.plugins.as_ref() {
        set_usize(
            &mut limits.plugins.max_persisted_manifest_bytes,
            plugins.max_persisted_manifest_bytes,
            "limits.plugins.maxPersistedManifestBytes",
        )?;
        set_u64(
            &mut limits.plugins.max_persisted_manifest_file_bytes,
            plugins.max_persisted_manifest_file_bytes,
            "limits.plugins.maxPersistedManifestFileBytes",
        )?;
    }
    if let Some(acp) = config.acp.as_ref() {
        set_usize(
            &mut limits.acp.max_read_line_bytes,
            acp.max_read_line_bytes,
            "limits.acp.maxReadLineBytes",
        )?;
        set_usize(
            &mut limits.acp.stdout_buffer_byte_limit,
            acp.stdout_buffer_byte_limit,
            "limits.acp.stdoutBufferByteLimit",
        )?;
        set_usize(
            &mut limits.acp.max_completed_message_bytes,
            acp.max_completed_message_bytes,
            "limits.acp.maxCompletedMessageBytes",
        )?;
        set_usize(
            &mut limits.acp.max_turn_output_bytes,
            acp.max_turn_output_bytes,
            "limits.acp.maxTurnOutputBytes",
        )?;
        set_usize(
            &mut limits.acp.max_prompt_bytes,
            acp.max_prompt_bytes,
            "limits.acp.maxPromptBytes",
        )?;
        set_usize(
            &mut limits.acp.max_prompt_blocks,
            acp.max_prompt_blocks,
            "limits.acp.maxPromptBlocks",
        )?;
        set_usize(
            &mut limits.acp.max_fallback_continuation_bytes,
            acp.max_fallback_continuation_bytes,
            "limits.acp.maxFallbackContinuationBytes",
        )?;
        set_usize(
            &mut limits.acp.max_session_history_bytes,
            acp.max_session_history_bytes,
            "limits.acp.maxSessionHistoryBytes",
        )?;
        set_usize(
            &mut limits.acp.max_session_history_events,
            acp.max_session_history_events,
            "limits.acp.maxSessionHistoryEvents",
        )?;
        set_usize(
            &mut limits.acp.max_history_page_entries,
            acp.max_history_page_entries,
            "limits.acp.maxHistoryPageEntries",
        )?;
        set_usize(
            &mut limits.acp.max_session_list_entries,
            acp.max_session_list_entries,
            "limits.acp.maxSessionListEntries",
        )?;
        set_usize(
            &mut limits.acp.max_sessions_per_vm,
            acp.max_sessions_per_vm,
            "limits.acp.maxSessionsPerVm",
        )?;
        set_usize(
            &mut limits.acp.max_prompts_per_session,
            acp.max_prompts_per_session,
            "limits.acp.maxPromptsPerSession",
        )?;
        set_usize(
            &mut limits.acp.max_prompts_per_vm,
            acp.max_prompts_per_vm,
            "limits.acp.maxPromptsPerVm",
        )?;
        set_usize(
            &mut limits.acp.max_pending_permissions_per_session,
            acp.max_pending_permissions_per_session,
            "limits.acp.maxPendingPermissionsPerSession",
        )?;
        set_usize(
            &mut limits.acp.max_pending_permissions_per_vm,
            acp.max_pending_permissions_per_vm,
            "limits.acp.maxPendingPermissionsPerVm",
        )?;
        set_usize(
            &mut limits.acp.max_permission_outcomes_per_session,
            acp.max_permission_outcomes_per_session,
            "limits.acp.maxPermissionOutcomesPerSession",
        )?;
        set_usize(
            &mut limits.acp.max_permission_outcomes_per_vm,
            acp.max_permission_outcomes_per_vm,
            "limits.acp.maxPermissionOutcomesPerVm",
        )?;
    }
    if let Some(sqlite) = config.sqlite.as_ref() {
        set_usize(
            &mut limits.sqlite.max_result_bytes,
            sqlite.max_result_bytes,
            "limits.sqlite.maxResultBytes",
        )?;
    }
    if let Some(js_runtime) = config.js_runtime.as_ref() {
        if let Some(value) = js_runtime.v8_heap_limit_mb {
            limits.js_runtime.v8_heap_limit_mb = Some(
                u32::try_from(value)
                    .map_err(|_| integer_too_large("limits.jsRuntime.v8HeapLimitMb", value))?,
            );
        }
        if let Some(value) = js_runtime.cpu_time_limit_ms {
            limits.js_runtime.cpu_time_limit_ms = u32::try_from(value)
                .map_err(|_| integer_too_large("limits.jsRuntime.cpuTimeLimitMs", value))?;
        }
        if let Some(value) = js_runtime.wall_clock_limit_ms {
            limits.js_runtime.wall_clock_limit_ms = u32::try_from(value)
                .map_err(|_| integer_too_large("limits.jsRuntime.wallClockLimitMs", value))?;
        }
        set_u64(
            &mut limits.js_runtime.import_cache_materialize_timeout_ms,
            js_runtime.import_cache_materialize_timeout_ms,
            "limits.jsRuntime.importCacheMaterializeTimeoutMs",
        )?;
        set_usize(
            &mut limits.js_runtime.captured_output_limit_bytes,
            js_runtime.captured_output_limit_bytes,
            "limits.jsRuntime.capturedOutputLimitBytes",
        )?;
        set_usize(
            &mut limits.js_runtime.stdin_buffer_limit_bytes,
            js_runtime.stdin_buffer_limit_bytes,
            "limits.jsRuntime.stdinBufferLimitBytes",
        )?;
        set_usize(
            &mut limits.js_runtime.event_payload_limit_bytes,
            js_runtime.event_payload_limit_bytes,
            "limits.jsRuntime.eventPayloadLimitBytes",
        )?;
        set_usize(
            &mut limits.js_runtime.max_timers,
            js_runtime.max_timers,
            "limits.jsRuntime.maxTimers",
        )?;
        if let Some(value) = js_runtime.v8_ipc_max_frame_bytes {
            limits.js_runtime.v8_ipc_max_frame_bytes = u32::try_from(value)
                .map_err(|_| integer_too_large("limits.jsRuntime.v8IpcMaxFrameBytes", value))?;
        }
        if let Some(value) = js_runtime.sync_rpc_wait_timeout_ms {
            limits.js_runtime.sync_rpc_wait_timeout_ms = Some(value);
        }
    }
    if let Some(python) = config.python.as_ref() {
        set_usize(
            &mut limits.python.output_buffer_max_bytes,
            python.output_buffer_max_bytes,
            "limits.python.outputBufferMaxBytes",
        )?;
        set_u64(
            &mut limits.python.execution_timeout_ms,
            python.execution_timeout_ms,
            "limits.python.executionTimeoutMs",
        )?;
        set_usize(
            &mut limits.python.max_old_space_mb,
            python.max_old_space_mb,
            "limits.python.maxOldSpaceMb",
        )?;
        set_u64(
            &mut limits.python.vfs_rpc_timeout_ms,
            python.vfs_rpc_timeout_ms,
            "limits.python.vfsRpcTimeoutMs",
        )?;
    }
    if let Some(wasm) = config.wasm.as_ref() {
        set_u64(
            &mut limits.wasm.max_module_file_bytes,
            wasm.max_module_file_bytes,
            "limits.wasm.maxModuleFileBytes",
        )?;
        set_usize(
            &mut limits.wasm.captured_output_limit_bytes,
            wasm.captured_output_limit_bytes,
            "limits.wasm.capturedOutputLimitBytes",
        )?;
        set_usize(
            &mut limits.wasm.sync_read_limit_bytes,
            wasm.sync_read_limit_bytes,
            "limits.wasm.syncReadLimitBytes",
        )?;
        set_u64(
            &mut limits.wasm.prewarm_timeout_ms,
            wasm.prewarm_timeout_ms,
            "limits.wasm.prewarmTimeoutMs",
        )?;
        if let Some(value) = wasm.runner_heap_limit_mb {
            limits.wasm.runner_heap_limit_mb = u32::try_from(value)
                .map_err(|_| integer_too_large("limits.wasm.runnerHeapLimitMb", value))?;
        }
        if let Some(value) = wasm.active_cpu_time_limit_ms {
            limits.wasm.active_cpu_time_limit_ms = u32::try_from(value)
                .map_err(|_| integer_too_large("limits.wasm.activeCpuTimeLimitMs", value))?;
        }
        limits.wasm.wall_clock_limit_ms = wasm.wall_clock_limit_ms;
        limits.wasm.deterministic_fuel = wasm.deterministic_fuel;
        set_usize(
            &mut limits.wasm.max_threads,
            wasm.max_threads,
            "limits.wasm.maxThreads",
        )?;
        set_usize(
            &mut limits.wasm.max_concurrent_threads,
            wasm.max_concurrent_threads,
            "limits.wasm.maxConcurrentThreads",
        )?;
    }
    if let Some(execution) = config.execution.as_ref() {
        set_u64(
            &mut limits.execution.completed_ttl_ms,
            execution.completed_ttl_ms,
            "limits.execution.completedTtlMs",
        )?;
        set_usize(
            &mut limits.execution.max_completed_executions,
            execution.max_completed_executions,
            "limits.execution.maxCompletedExecutions",
        )?;
        set_usize(
            &mut limits.execution.live_execution_warning_threshold,
            execution.live_execution_warning_threshold,
            "limits.execution.liveExecutionWarningThreshold",
        )?;
    }
    if let Some(process) = config.process.as_ref() {
        set_usize(
            &mut limits.process.max_spawn_file_actions,
            process.max_spawn_file_actions,
            "limits.process.maxSpawnFileActions",
        )?;
        set_usize(
            &mut limits.process.max_spawn_file_action_bytes,
            process.max_spawn_file_action_bytes,
            "limits.process.maxSpawnFileActionBytes",
        )?;
        set_usize(
            &mut limits.process.pending_stdin_bytes,
            process.pending_stdin_bytes,
            "limits.process.pendingStdinBytes",
        )?;
        set_usize(
            &mut limits.process.pending_event_count,
            process.pending_event_count,
            "limits.process.pendingEventCount",
        )?;
        set_usize(
            &mut limits.process.pending_event_bytes,
            process.pending_event_bytes,
            "limits.process.pendingEventBytes",
        )?;
        set_usize(
            &mut limits.process.max_pending_child_sync_count,
            process.max_pending_child_sync_count,
            "limits.process.maxPendingChildSyncCount",
        )?;
        set_usize(
            &mut limits.process.max_pending_child_sync_bytes,
            process.max_pending_child_sync_bytes,
            "limits.process.maxPendingChildSyncBytes",
        )?;
    }

    validate_vm_limits(&limits, sidecar_max_frame_bytes)?;
    Ok(limits)
}

fn apply_reactor_limits_config(
    limits: &mut ReactorLimits,
    config: &ReactorLimitsConfig,
) -> Result<(), SidecarCoreError> {
    set_usize(
        &mut limits.max_capabilities,
        config.max_capabilities,
        "limits.reactor.maxCapabilities",
    )?;
    set_usize(
        &mut limits.max_ready_handles,
        config.max_ready_handles,
        "limits.reactor.maxReadyHandles",
    )?;
    set_usize(
        &mut limits.max_tasks,
        config.max_tasks,
        "limits.reactor.maxTasks",
    )?;
    set_usize(
        &mut limits.work_quantum,
        config.work_quantum,
        "limits.reactor.workQuantum",
    )?;
    set_usize(
        &mut limits.byte_quantum,
        config.byte_quantum,
        "limits.reactor.byteQuantum",
    )?;
    set_usize(
        &mut limits.max_handle_commands,
        config.max_handle_commands,
        "limits.reactor.maxHandleCommands",
    )?;
    set_usize(
        &mut limits.max_handle_command_bytes,
        config.max_handle_command_bytes,
        "limits.reactor.maxHandleCommandBytes",
    )?;
    set_usize(
        &mut limits.max_bridge_calls,
        config.max_bridge_calls,
        "limits.reactor.maxBridgeCalls",
    )?;
    set_usize(
        &mut limits.max_bridge_request_bytes,
        config.max_bridge_request_bytes,
        "limits.reactor.maxBridgeRequestBytes",
    )?;
    set_usize(
        &mut limits.max_bridge_response_bytes,
        config.max_bridge_response_bytes,
        "limits.reactor.maxBridgeResponseBytes",
    )?;
    set_usize(
        &mut limits.max_async_completions,
        config.max_async_completions,
        "limits.reactor.maxAsyncCompletions",
    )?;
    set_usize(
        &mut limits.max_async_completion_bytes,
        config.max_async_completion_bytes,
        "limits.reactor.maxAsyncCompletionBytes",
    )?;
    set_usize(
        &mut limits.max_blocking_jobs,
        config.max_blocking_jobs,
        "limits.reactor.maxBlockingJobs",
    )?;
    set_usize(
        &mut limits.max_blocking_bytes,
        config.max_blocking_bytes,
        "limits.reactor.maxBlockingBytes",
    )?;
    set_usize(
        &mut limits.per_handle_operation_quantum,
        config.per_handle_operation_quantum,
        "limits.reactor.perHandleOperationQuantum",
    )?;
    set_usize(
        &mut limits.accept_quantum,
        config.accept_quantum,
        "limits.reactor.acceptQuantum",
    )?;
    set_usize(
        &mut limits.datagram_quantum,
        config.datagram_quantum,
        "limits.reactor.datagramQuantum",
    )?;
    set_usize(
        &mut limits.completion_quantum,
        config.completion_quantum,
        "limits.reactor.completionQuantum",
    )?;
    set_usize(
        &mut limits.signal_quantum,
        config.signal_quantum,
        "limits.reactor.signalQuantum",
    )?;
    set_u64(
        &mut limits.shutdown_deadline_ms,
        config.shutdown_deadline_ms,
        "limits.reactor.shutdownDeadlineMs",
    )?;
    set_u64(
        &mut limits.operation_deadline_ms,
        config.operation_deadline_ms,
        "limits.reactor.operationDeadlineMs",
    )?;
    Ok(())
}

fn apply_udp_limits_config(
    limits: &mut UdpLimits,
    config: &UdpLimitsConfig,
) -> Result<(), SidecarCoreError> {
    set_usize(
        &mut limits.max_buffered_datagrams,
        config.max_buffered_datagrams,
        "limits.udp.maxBufferedDatagrams",
    )?;
    set_usize(
        &mut limits.max_buffered_bytes,
        config.max_buffered_bytes,
        "limits.udp.maxBufferedBytes",
    )?;
    Ok(())
}

fn apply_tls_limits_config(
    limits: &mut TlsLimits,
    config: &TlsLimitsConfig,
) -> Result<(), SidecarCoreError> {
    set_usize(
        &mut limits.max_buffered_bytes,
        config.max_buffered_bytes,
        "limits.tls.maxBufferedBytes",
    )
}

fn apply_http2_limits_config(
    limits: &mut Http2Limits,
    config: &Http2LimitsConfig,
) -> Result<(), SidecarCoreError> {
    set_usize(
        &mut limits.max_connections,
        config.max_connections,
        "limits.http2.maxConnections",
    )?;
    set_usize(
        &mut limits.max_streams,
        config.max_streams,
        "limits.http2.maxStreams",
    )?;
    set_usize(
        &mut limits.max_streams_per_connection,
        config.max_streams_per_connection,
        "limits.http2.maxStreamsPerConnection",
    )?;
    set_usize(
        &mut limits.max_buffered_bytes,
        config.max_buffered_bytes,
        "limits.http2.maxBufferedBytes",
    )?;
    set_usize(
        &mut limits.max_header_bytes,
        config.max_header_bytes,
        "limits.http2.maxHeaderBytes",
    )?;
    set_usize(
        &mut limits.max_data_bytes,
        config.max_data_bytes,
        "limits.http2.maxDataBytes",
    )?;
    set_usize(
        &mut limits.max_pending_commands,
        config.max_pending_commands,
        "limits.http2.maxPendingCommands",
    )?;
    set_usize(
        &mut limits.max_pending_command_bytes,
        config.max_pending_command_bytes,
        "limits.http2.maxPendingCommandBytes",
    )?;
    set_usize(
        &mut limits.max_pending_events,
        config.max_pending_events,
        "limits.http2.maxPendingEvents",
    )?;
    set_usize(
        &mut limits.max_pending_event_bytes,
        config.max_pending_event_bytes,
        "limits.http2.maxPendingEventBytes",
    )?;
    Ok(())
}

fn apply_resource_limits_config(
    limits: &mut ResourceLimits,
    config: &ResourceLimitsConfig,
) -> Result<(), SidecarCoreError> {
    set_optional_usize(
        &mut limits.virtual_cpu_count,
        config.cpu_count,
        "limits.resources.cpuCount",
    )?;
    set_optional_usize(
        &mut limits.max_processes,
        config.max_processes,
        "limits.resources.maxProcesses",
    )?;
    set_optional_usize(
        &mut limits.max_open_fds,
        config.max_open_fds,
        "limits.resources.maxOpenFds",
    )?;
    set_optional_usize(
        &mut limits.max_pipes,
        config.max_pipes,
        "limits.resources.maxPipes",
    )?;
    set_optional_usize(
        &mut limits.max_ptys,
        config.max_ptys,
        "limits.resources.maxPtys",
    )?;
    set_optional_usize(
        &mut limits.max_sockets,
        config.max_sockets,
        "limits.resources.maxSockets",
    )?;
    set_optional_usize(
        &mut limits.max_connections,
        config.max_connections,
        "limits.resources.maxConnections",
    )?;
    set_optional_usize(
        &mut limits.max_socket_buffered_bytes,
        config.max_socket_buffered_bytes,
        "limits.resources.maxSocketBufferedBytes",
    )?;
    set_optional_usize(
        &mut limits.max_socket_datagram_queue_len,
        config.max_socket_datagram_queue_len,
        "limits.resources.maxSocketDatagramQueueLen",
    )?;
    set_optional_u64(
        &mut limits.max_filesystem_bytes,
        config.max_filesystem_bytes,
    );
    set_optional_usize(
        &mut limits.max_inode_count,
        config.max_inode_count,
        "limits.resources.maxInodeCount",
    )?;
    set_optional_u64(
        &mut limits.max_blocking_read_ms,
        config.max_blocking_read_ms,
    );
    set_optional_usize(
        &mut limits.max_pread_bytes,
        config.max_pread_bytes,
        "limits.resources.maxPreadBytes",
    )?;
    set_optional_usize(
        &mut limits.max_fd_write_bytes,
        config.max_fd_write_bytes,
        "limits.resources.maxFdWriteBytes",
    )?;
    set_optional_usize(
        &mut limits.max_process_argv_bytes,
        config.max_process_argv_bytes,
        "limits.resources.maxProcessArgvBytes",
    )?;
    set_optional_usize(
        &mut limits.max_process_env_bytes,
        config.max_process_env_bytes,
        "limits.resources.maxProcessEnvBytes",
    )?;
    set_optional_usize(
        &mut limits.max_readdir_entries,
        config.max_readdir_entries,
        "limits.resources.maxReaddirEntries",
    )?;
    set_optional_usize(
        &mut limits.max_recursive_fs_depth,
        config.max_recursive_fs_depth,
        "limits.resources.maxRecursiveFsDepth",
    )?;
    set_optional_usize(
        &mut limits.max_recursive_fs_entries,
        config.max_recursive_fs_entries,
        "limits.resources.maxRecursiveFsEntries",
    )?;
    set_optional_u64(
        &mut limits.max_wasm_memory_bytes,
        config.max_wasm_memory_bytes,
    );
    set_optional_usize(
        &mut limits.max_wasm_stack_bytes,
        config.max_wasm_stack_bytes,
        "limits.resources.maxWasmStackBytes",
    )?;
    Ok(())
}

fn set_usize(target: &mut usize, value: Option<u64>, key: &str) -> Result<(), SidecarCoreError> {
    if let Some(value) = value {
        *target = usize::try_from(value).map_err(|_| integer_too_large(key, value))?;
    }
    Ok(())
}

fn set_u64(target: &mut u64, value: Option<u64>, _key: &str) -> Result<(), SidecarCoreError> {
    if let Some(value) = value {
        *target = value;
    }
    Ok(())
}

fn set_optional_usize(
    target: &mut Option<usize>,
    value: Option<u64>,
    key: &str,
) -> Result<(), SidecarCoreError> {
    if let Some(value) = value {
        *target = Some(usize::try_from(value).map_err(|_| integer_too_large(key, value))?);
    }
    Ok(())
}

fn set_optional_u64(target: &mut Option<u64>, value: Option<u64>) {
    if let Some(value) = value {
        *target = Some(value);
    }
}

fn integer_too_large(key: &str, value: u64) -> SidecarCoreError {
    SidecarCoreError::new(format!("{key} value {value} does not fit this platform"))
}

fn required_resource_parent(value: Option<usize>, path: &str) -> Result<usize, SidecarCoreError> {
    match value {
        Some(0) | None => Err(SidecarCoreError::new(format!(
            "{path} must be configured and greater than zero"
        ))),
        Some(value) => Ok(value),
    }
}

fn validate_parent_limit(
    child_path: &str,
    child: usize,
    parent_path: &str,
    parent: usize,
) -> Result<(), SidecarCoreError> {
    if child > parent {
        return Err(SidecarCoreError::new(format!(
            "{child_path} ({child}) must be <= {parent_path} ({parent})"
        )));
    }
    Ok(())
}

/// Cross-field validation. Fail-by-default: reject any configuration that would deadlock or
/// violate the wire frame budget with an explicit, actionable message.
pub fn validate_vm_limits(
    limits: &VmLimits,
    sidecar_max_frame_bytes: usize,
) -> Result<(), SidecarCoreError> {
    for (path, value) in [
        (
            "limits.execution.completedTtlMs",
            limits.execution.completed_ttl_ms,
        ),
        (
            "limits.execution.maxCompletedExecutions",
            limits.execution.max_completed_executions as u64,
        ),
        (
            "limits.execution.liveExecutionWarningThreshold",
            limits.execution.live_execution_warning_threshold as u64,
        ),
    ] {
        if value == 0 {
            return Err(SidecarCoreError::new(format!(
                "{path} must be greater than zero"
            )));
        }
    }
    for (path, value) in [
        (
            "limits.reactor.maxCapabilities",
            limits.reactor.max_capabilities,
        ),
        (
            "limits.reactor.maxReadyHandles",
            limits.reactor.max_ready_handles,
        ),
        ("limits.reactor.maxTasks", limits.reactor.max_tasks),
        ("limits.reactor.workQuantum", limits.reactor.work_quantum),
        ("limits.reactor.byteQuantum", limits.reactor.byte_quantum),
        (
            "limits.reactor.maxHandleCommands",
            limits.reactor.max_handle_commands,
        ),
        (
            "limits.reactor.maxHandleCommandBytes",
            limits.reactor.max_handle_command_bytes,
        ),
        (
            "limits.reactor.maxBridgeCalls",
            limits.reactor.max_bridge_calls,
        ),
        (
            "limits.reactor.maxBridgeRequestBytes",
            limits.reactor.max_bridge_request_bytes,
        ),
        (
            "limits.reactor.maxBridgeResponseBytes",
            limits.reactor.max_bridge_response_bytes,
        ),
        (
            "limits.reactor.maxAsyncCompletions",
            limits.reactor.max_async_completions,
        ),
        (
            "limits.reactor.maxAsyncCompletionBytes",
            limits.reactor.max_async_completion_bytes,
        ),
        (
            "limits.reactor.maxBlockingJobs",
            limits.reactor.max_blocking_jobs,
        ),
        (
            "limits.reactor.maxBlockingBytes",
            limits.reactor.max_blocking_bytes,
        ),
        (
            "limits.reactor.perHandleOperationQuantum",
            limits.reactor.per_handle_operation_quantum,
        ),
        (
            "limits.reactor.acceptQuantum",
            limits.reactor.accept_quantum,
        ),
        (
            "limits.reactor.datagramQuantum",
            limits.reactor.datagram_quantum,
        ),
        (
            "limits.reactor.completionQuantum",
            limits.reactor.completion_quantum,
        ),
        (
            "limits.reactor.signalQuantum",
            limits.reactor.signal_quantum,
        ),
    ] {
        if value == 0 {
            return Err(SidecarCoreError::new(format!(
                "{path} must be greater than zero"
            )));
        }
    }
    for (path, value) in [
        (
            "limits.reactor.shutdownDeadlineMs",
            limits.reactor.shutdown_deadline_ms,
        ),
        (
            "limits.reactor.operationDeadlineMs",
            limits.reactor.operation_deadline_ms,
        ),
    ] {
        if value == 0 {
            return Err(SidecarCoreError::new(format!(
                "{path} must be greater than zero"
            )));
        }
    }
    if limits.reactor.max_capabilities > limits.reactor.max_ready_handles {
        return Err(SidecarCoreError::new(format!(
            "limits.reactor.maxCapabilities ({}) must be <= limits.reactor.maxReadyHandles ({})",
            limits.reactor.max_capabilities, limits.reactor.max_ready_handles
        )));
    }
    validate_parent_limit(
        "limits.reactor.perHandleOperationQuantum",
        limits.reactor.per_handle_operation_quantum,
        "limits.reactor.maxHandleCommands",
        limits.reactor.max_handle_commands,
    )?;
    validate_parent_limit(
        "limits.reactor.acceptQuantum",
        limits.reactor.accept_quantum,
        "limits.reactor.maxCapabilities",
        limits.reactor.max_capabilities,
    )?;
    validate_parent_limit(
        "limits.reactor.datagramQuantum",
        limits.reactor.datagram_quantum,
        "limits.udp.maxBufferedDatagrams",
        limits.udp.max_buffered_datagrams,
    )?;
    validate_parent_limit(
        "limits.reactor.completionQuantum",
        limits.reactor.completion_quantum,
        "limits.reactor.maxAsyncCompletions",
        limits.reactor.max_async_completions,
    )?;

    let aggregate_socket_bytes = required_resource_parent(
        limits.resources.max_socket_buffered_bytes,
        "limits.resources.maxSocketBufferedBytes",
    )?;
    let aggregate_datagrams = required_resource_parent(
        limits.resources.max_socket_datagram_queue_len,
        "limits.resources.maxSocketDatagramQueueLen",
    )?;
    let aggregate_connections = required_resource_parent(
        limits.resources.max_connections,
        "limits.resources.maxConnections",
    )?;

    if limits.http.max_fetch_response_bytes == 0 {
        return Err(SidecarCoreError::new(
            "limits.http.maxFetchResponseBytes must be greater than zero".to_string(),
        ));
    }
    if limits.http.max_fetch_response_bytes > sidecar_max_frame_bytes {
        return Err(SidecarCoreError::new(format!(
            "limits.http.maxFetchResponseBytes ({}) must be <= the sidecar wire frame cap ({})",
            limits.http.max_fetch_response_bytes, sidecar_max_frame_bytes
        )));
    }
    validate_parent_limit(
        "limits.http.maxFetchResponseBytes",
        limits.http.max_fetch_response_bytes,
        "limits.resources.maxSocketBufferedBytes",
        aggregate_socket_bytes,
    )?;

    for (path, value) in [
        (
            "limits.udp.maxBufferedDatagrams",
            limits.udp.max_buffered_datagrams,
        ),
        ("limits.udp.maxBufferedBytes", limits.udp.max_buffered_bytes),
        ("limits.tls.maxBufferedBytes", limits.tls.max_buffered_bytes),
        ("limits.http2.maxConnections", limits.http2.max_connections),
        ("limits.http2.maxStreams", limits.http2.max_streams),
        (
            "limits.http2.maxStreamsPerConnection",
            limits.http2.max_streams_per_connection,
        ),
        (
            "limits.http2.maxBufferedBytes",
            limits.http2.max_buffered_bytes,
        ),
        ("limits.http2.maxHeaderBytes", limits.http2.max_header_bytes),
        ("limits.http2.maxDataBytes", limits.http2.max_data_bytes),
        (
            "limits.http2.maxPendingCommands",
            limits.http2.max_pending_commands,
        ),
        (
            "limits.http2.maxPendingCommandBytes",
            limits.http2.max_pending_command_bytes,
        ),
        (
            "limits.http2.maxPendingEvents",
            limits.http2.max_pending_events,
        ),
        (
            "limits.http2.maxPendingEventBytes",
            limits.http2.max_pending_event_bytes,
        ),
    ] {
        if value == 0 {
            return Err(SidecarCoreError::new(format!(
                "{path} must be greater than zero"
            )));
        }
    }

    validate_parent_limit(
        "limits.reactor.maxHandleCommandBytes",
        limits.reactor.max_handle_command_bytes,
        "limits.resources.maxSocketBufferedBytes",
        aggregate_socket_bytes,
    )?;
    validate_parent_limit(
        "limits.udp.maxBufferedDatagrams",
        limits.udp.max_buffered_datagrams,
        "limits.resources.maxSocketDatagramQueueLen",
        aggregate_datagrams,
    )?;
    for (path, value) in [
        ("limits.udp.maxBufferedBytes", limits.udp.max_buffered_bytes),
        ("limits.tls.maxBufferedBytes", limits.tls.max_buffered_bytes),
        (
            "limits.http2.maxBufferedBytes",
            limits.http2.max_buffered_bytes,
        ),
    ] {
        validate_parent_limit(
            path,
            value,
            "limits.resources.maxSocketBufferedBytes",
            aggregate_socket_bytes,
        )?;
    }
    validate_parent_limit(
        "limits.http2.maxConnections",
        limits.http2.max_connections,
        "limits.resources.maxConnections",
        aggregate_connections,
    )?;
    validate_parent_limit(
        "limits.http2.maxStreamsPerConnection",
        limits.http2.max_streams_per_connection,
        "limits.http2.maxStreams",
        limits.http2.max_streams,
    )?;
    for (path, value) in [
        ("limits.http2.maxHeaderBytes", limits.http2.max_header_bytes),
        ("limits.http2.maxDataBytes", limits.http2.max_data_bytes),
        (
            "limits.http2.maxPendingCommandBytes",
            limits.http2.max_pending_command_bytes,
        ),
        (
            "limits.http2.maxPendingEventBytes",
            limits.http2.max_pending_event_bytes,
        ),
    ] {
        validate_parent_limit(
            path,
            value,
            "limits.http2.maxBufferedBytes",
            limits.http2.max_buffered_bytes,
        )?;
    }
    validate_parent_limit(
        "limits.reactor.maxBridgeRequestBytes",
        limits.reactor.max_bridge_request_bytes,
        "sidecar maxFrameBytes",
        sidecar_max_frame_bytes,
    )?;
    validate_parent_limit(
        "limits.reactor.maxBridgeRequestBytes",
        limits.reactor.max_bridge_request_bytes,
        "limits.jsRuntime.v8IpcMaxFrameBytes",
        limits.js_runtime.v8_ipc_max_frame_bytes as usize,
    )?;
    validate_parent_limit(
        "limits.reactor.maxBridgeResponseBytes",
        limits.reactor.max_bridge_response_bytes,
        "sidecar maxFrameBytes",
        sidecar_max_frame_bytes,
    )?;
    validate_parent_limit(
        "limits.reactor.maxBridgeResponseBytes",
        limits.reactor.max_bridge_response_bytes,
        "limits.jsRuntime.v8IpcMaxFrameBytes",
        limits.js_runtime.v8_ipc_max_frame_bytes as usize,
    )?;
    validate_parent_limit(
        "limits.reactor.maxAsyncCompletionBytes",
        limits.reactor.max_async_completion_bytes,
        "limits.jsRuntime.v8IpcMaxFrameBytes",
        limits.js_runtime.v8_ipc_max_frame_bytes as usize,
    )?;
    validate_parent_limit(
        "limits.acp.maxCompletedMessageBytes",
        limits.acp.max_completed_message_bytes,
        "limits.acp.maxTurnOutputBytes",
        limits.acp.max_turn_output_bytes,
    )?;
    validate_parent_limit(
        "limits.acp.maxCompletedMessageBytes",
        limits.acp.max_completed_message_bytes,
        "limits.acp.maxSessionHistoryBytes",
        limits.acp.max_session_history_bytes,
    )?;
    validate_parent_limit(
        "limits.acp.maxHistoryPageEntries",
        limits.acp.max_history_page_entries,
        "limits.acp.maxSessionHistoryEvents",
        limits.acp.max_session_history_events,
    )?;
    validate_parent_limit(
        "limits.acp.maxFallbackContinuationBytes",
        limits.acp.max_fallback_continuation_bytes,
        "limits.acp.maxSessionHistoryBytes",
        limits.acp.max_session_history_bytes,
    )?;
    validate_parent_limit(
        "limits.acp.maxPromptsPerSession",
        limits.acp.max_prompts_per_session,
        "limits.acp.maxPromptsPerVm",
        limits.acp.max_prompts_per_vm,
    )?;
    validate_parent_limit(
        "limits.acp.maxPendingPermissionsPerSession",
        limits.acp.max_pending_permissions_per_session,
        "limits.acp.maxPendingPermissionsPerVm",
        limits.acp.max_pending_permissions_per_vm,
    )?;
    validate_parent_limit(
        "limits.acp.maxPermissionOutcomesPerSession",
        limits.acp.max_permission_outcomes_per_session,
        "limits.acp.maxPermissionOutcomesPerVm",
        limits.acp.max_permission_outcomes_per_vm,
    )?;

    if limits.bindings.default_binding_timeout_ms > limits.bindings.max_binding_timeout_ms {
        return Err(SidecarCoreError::new(format!(
            "limits.bindings.default_binding_timeout_ms ({}) must be <= limits.bindings.max_binding_timeout_ms ({})",
            limits.bindings.default_binding_timeout_ms, limits.bindings.max_binding_timeout_ms
        )));
    }

    let nonzero_usize: [(&str, usize); 38] = [
        (
            "limits.bindings.max_registered_collections",
            limits.bindings.max_registered_collections,
        ),
        (
            "limits.bindings.max_registered_bindings_per_vm",
            limits.bindings.max_registered_bindings_per_vm,
        ),
        (
            "limits.bindings.max_bindings_per_collection",
            limits.bindings.max_bindings_per_collection,
        ),
        (
            "limits.bindings.max_binding_schema_bytes",
            limits.bindings.max_binding_schema_bytes,
        ),
        (
            "limits.bindings.max_binding_example_input_bytes",
            limits.bindings.max_binding_example_input_bytes,
        ),
        (
            "limits.plugins.max_persisted_manifest_bytes",
            limits.plugins.max_persisted_manifest_bytes,
        ),
        (
            "limits.acp.max_read_line_bytes",
            limits.acp.max_read_line_bytes,
        ),
        (
            "limits.acp.stdout_buffer_byte_limit",
            limits.acp.stdout_buffer_byte_limit,
        ),
        (
            "limits.acp.max_completed_message_bytes",
            limits.acp.max_completed_message_bytes,
        ),
        (
            "limits.acp.max_turn_output_bytes",
            limits.acp.max_turn_output_bytes,
        ),
        ("limits.acp.max_prompt_bytes", limits.acp.max_prompt_bytes),
        ("limits.acp.max_prompt_blocks", limits.acp.max_prompt_blocks),
        (
            "limits.acp.max_fallback_continuation_bytes",
            limits.acp.max_fallback_continuation_bytes,
        ),
        (
            "limits.acp.max_session_history_bytes",
            limits.acp.max_session_history_bytes,
        ),
        (
            "limits.acp.max_session_history_events",
            limits.acp.max_session_history_events,
        ),
        (
            "limits.acp.max_history_page_entries",
            limits.acp.max_history_page_entries,
        ),
        (
            "limits.acp.max_session_list_entries",
            limits.acp.max_session_list_entries,
        ),
        (
            "limits.acp.max_sessions_per_vm",
            limits.acp.max_sessions_per_vm,
        ),
        (
            "limits.acp.max_prompts_per_session",
            limits.acp.max_prompts_per_session,
        ),
        (
            "limits.acp.max_prompts_per_vm",
            limits.acp.max_prompts_per_vm,
        ),
        (
            "limits.acp.max_pending_permissions_per_session",
            limits.acp.max_pending_permissions_per_session,
        ),
        (
            "limits.acp.max_pending_permissions_per_vm",
            limits.acp.max_pending_permissions_per_vm,
        ),
        (
            "limits.acp.max_permission_outcomes_per_session",
            limits.acp.max_permission_outcomes_per_session,
        ),
        (
            "limits.acp.max_permission_outcomes_per_vm",
            limits.acp.max_permission_outcomes_per_vm,
        ),
        (
            "limits.sqlite.max_result_bytes",
            limits.sqlite.max_result_bytes,
        ),
        (
            "limits.js_runtime.captured_output_limit_bytes",
            limits.js_runtime.captured_output_limit_bytes,
        ),
        (
            "limits.js_runtime.stdin_buffer_limit_bytes",
            limits.js_runtime.stdin_buffer_limit_bytes,
        ),
        (
            "limits.js_runtime.event_payload_limit_bytes",
            limits.js_runtime.event_payload_limit_bytes,
        ),
        ("limits.js_runtime.max_timers", limits.js_runtime.max_timers),
        (
            "limits.python.output_buffer_max_bytes",
            limits.python.output_buffer_max_bytes,
        ),
        (
            "limits.wasm.captured_output_limit_bytes",
            limits.wasm.captured_output_limit_bytes,
        ),
        (
            "limits.process.max_spawn_file_actions",
            limits.process.max_spawn_file_actions,
        ),
        (
            "limits.process.max_spawn_file_action_bytes",
            limits.process.max_spawn_file_action_bytes,
        ),
        (
            "limits.process.pending_stdin_bytes",
            limits.process.pending_stdin_bytes,
        ),
        (
            "limits.process.pending_event_count",
            limits.process.pending_event_count,
        ),
        (
            "limits.process.pending_event_bytes",
            limits.process.pending_event_bytes,
        ),
        (
            "limits.process.max_pending_child_sync_count",
            limits.process.max_pending_child_sync_count,
        ),
        (
            "limits.process.max_pending_child_sync_bytes",
            limits.process.max_pending_child_sync_bytes,
        ),
    ];
    for (key, value) in nonzero_usize {
        if value == 0 {
            return Err(SidecarCoreError::new(format!(
                "{key} must be greater than zero"
            )));
        }
    }

    if limits.wasm.sync_read_limit_bytes == 0 {
        return Err(SidecarCoreError::new(
            "limits.wasm.sync_read_limit_bytes must be greater than zero".to_string(),
        ));
    }
    if limits.wasm.prewarm_timeout_ms == 0 {
        return Err(SidecarCoreError::new(
            "limits.wasm.prewarm_timeout_ms must be greater than zero".to_string(),
        ));
    }
    if limits.wasm.runner_heap_limit_mb == 0 {
        return Err(SidecarCoreError::new(
            "limits.wasm.runner_heap_limit_mb must be greater than zero".to_string(),
        ));
    }
    if limits.wasm.max_module_file_bytes == 0 {
        return Err(SidecarCoreError::new(
            "limits.wasm.max_module_file_bytes must be greater than zero".to_string(),
        ));
    }
    if limits.wasm.max_threads == 0 {
        return Err(SidecarCoreError::new(
            "limits.wasm.max_threads must be greater than zero",
        ));
    }
    if limits.wasm.max_concurrent_threads == 0 {
        return Err(SidecarCoreError::new(
            "limits.wasm.max_concurrent_threads must be greater than zero",
        ));
    }
    if limits.wasm.max_threads > limits.wasm.max_concurrent_threads {
        return Err(SidecarCoreError::new(
            "limits.wasm.max_threads must not exceed limits.wasm.max_concurrent_threads",
        ));
    }
    if limits.js_runtime.v8_ipc_max_frame_bytes == 0 {
        return Err(SidecarCoreError::new(
            "limits.js_runtime.v8_ipc_max_frame_bytes must be greater than zero".to_string(),
        ));
    }
    if limits.python.execution_timeout_ms == 0 {
        return Err(SidecarCoreError::new(
            "limits.python.execution_timeout_ms must be greater than zero".to_string(),
        ));
    }
    if limits.python.vfs_rpc_timeout_ms == 0 {
        return Err(SidecarCoreError::new(
            "limits.python.vfs_rpc_timeout_ms must be greater than zero".to_string(),
        ));
    }
    if let Some(0) = limits.js_runtime.v8_heap_limit_mb {
        return Err(SidecarCoreError::new(
            "limits.js_runtime.v8_heap_limit_mb must be greater than zero".to_string(),
        ));
    }
    if limits.js_runtime.import_cache_materialize_timeout_ms == 0 {
        return Err(SidecarCoreError::new(
            "limits.js_runtime.import_cache_materialize_timeout_ms must be greater than zero"
                .to_string(),
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_vm_config::{
        AcpLimitsConfig, Http2LimitsConfig, ProcessLimitsConfig, ReactorLimitsConfig,
        TlsLimitsConfig, UdpLimitsConfig, WasmLimitsConfig,
    };

    const FRAME_CAP: usize = 16 * 1024 * 1024;

    #[test]
    fn canonical_defaults_are_concrete_and_valid() {
        let limits = VmLimits::default();
        validate_vm_limits(&limits, FRAME_CAP).expect("canonical defaults must validate");

        assert_eq!(
            limits.reactor.max_handle_commands,
            DEFAULT_REACTOR_MAX_HANDLE_COMMANDS
        );
        assert_eq!(
            limits.reactor.max_bridge_request_bytes,
            DEFAULT_REACTOR_MAX_BRIDGE_REQUEST_BYTES
        );
        assert_eq!(
            limits.reactor.max_bridge_response_bytes,
            DEFAULT_REACTOR_MAX_BRIDGE_RESPONSE_BYTES
        );
        assert_eq!(
            limits.reactor.max_blocking_jobs,
            DEFAULT_REACTOR_MAX_BLOCKING_JOBS
        );
        assert_eq!(
            limits.udp.max_buffered_datagrams,
            DEFAULT_UDP_MAX_BUFFERED_DATAGRAMS
        );
        assert_eq!(
            limits.tls.max_buffered_bytes,
            DEFAULT_TLS_MAX_BUFFERED_BYTES
        );
        assert_eq!(
            limits.http2.max_pending_commands,
            DEFAULT_HTTP2_MAX_PENDING_COMMANDS
        );
        assert_eq!(
            limits.http2.max_pending_events,
            DEFAULT_HTTP2_MAX_PENDING_EVENTS
        );
        assert_eq!(
            limits.wasm.active_cpu_time_limit_ms,
            DEFAULT_WASM_ACTIVE_CPU_TIME_LIMIT_MS
        );
        assert_eq!(limits.wasm.wall_clock_limit_ms, None);
        assert_eq!(limits.wasm.deterministic_fuel, None);
    }

    #[test]
    fn process_spawn_action_limits_have_bounded_defaults_and_accept_overrides() {
        let defaults = vm_limits_from_config(None, 64 * 1024 * 1024).expect("default limits");
        assert_eq!(
            defaults.process.max_spawn_file_actions,
            DEFAULT_PROCESS_MAX_SPAWN_FILE_ACTIONS
        );
        assert_eq!(
            defaults.process.max_spawn_file_action_bytes,
            DEFAULT_PROCESS_MAX_SPAWN_FILE_ACTION_BYTES
        );
        assert_eq!(
            defaults.process.max_pending_child_sync_count,
            DEFAULT_PROCESS_MAX_PENDING_CHILD_SYNC_COUNT
        );
        assert_eq!(
            defaults.process.max_pending_child_sync_bytes,
            DEFAULT_PROCESS_MAX_PENDING_CHILD_SYNC_BYTES
        );

        let config = VmLimitsConfig {
            process: Some(ProcessLimitsConfig {
                max_spawn_file_actions: Some(7),
                max_spawn_file_action_bytes: Some(321),
                max_pending_child_sync_count: Some(3),
                max_pending_child_sync_bytes: Some(12_345),
                ..ProcessLimitsConfig::default()
            }),
            ..VmLimitsConfig::default()
        };
        let overridden = vm_limits_from_config(Some(&config), 64 * 1024 * 1024).expect("overrides");
        assert_eq!(overridden.process.max_spawn_file_actions, 7);
        assert_eq!(overridden.process.max_spawn_file_action_bytes, 321);
        assert_eq!(overridden.process.max_pending_child_sync_count, 3);
        assert_eq!(overridden.process.max_pending_child_sync_bytes, 12_345);

        for (max_actions, max_bytes, field) in [
            (Some(0), Some(321), "limits.process.max_spawn_file_actions"),
            (
                Some(7),
                Some(0),
                "limits.process.max_spawn_file_action_bytes",
            ),
        ] {
            let config = VmLimitsConfig {
                process: Some(ProcessLimitsConfig {
                    max_spawn_file_actions: max_actions,
                    max_spawn_file_action_bytes: max_bytes,
                    ..ProcessLimitsConfig::default()
                }),
                ..VmLimitsConfig::default()
            };
            let error = vm_limits_from_config(Some(&config), 64 * 1024 * 1024)
                .expect_err("zero process spawn limit must be rejected");
            assert!(error.to_string().contains(field), "{error}");
        }

        for (count, bytes, field) in [
            (
                Some(0),
                Some(1024),
                "limits.process.max_pending_child_sync_count",
            ),
            (
                Some(1),
                Some(0),
                "limits.process.max_pending_child_sync_bytes",
            ),
        ] {
            let config = VmLimitsConfig {
                process: Some(ProcessLimitsConfig {
                    max_pending_child_sync_count: count,
                    max_pending_child_sync_bytes: bytes,
                    ..ProcessLimitsConfig::default()
                }),
                ..VmLimitsConfig::default()
            };
            let error = vm_limits_from_config(Some(&config), 64 * 1024 * 1024)
                .expect_err("zero pending child-sync limit must be rejected");
            assert!(error.to_string().contains(field), "{error}");
        }
    }

    #[test]
    fn process_queue_limits_reject_zero_before_runtime_construction() {
        type ProcessLimitMutation = fn(&mut ProcessLimitsConfig);
        let cases: [(&str, ProcessLimitMutation); 3] = [
            ("limits.process.pending_stdin_bytes", |process| {
                process.pending_stdin_bytes = Some(0)
            }),
            ("limits.process.pending_event_count", |process| {
                process.pending_event_count = Some(0)
            }),
            ("limits.process.pending_event_bytes", |process| {
                process.pending_event_bytes = Some(0)
            }),
        ];

        for (field, set_zero) in cases {
            let mut process = ProcessLimitsConfig::default();
            set_zero(&mut process);
            let config = VmLimitsConfig {
                process: Some(process),
                ..VmLimitsConfig::default()
            };
            let error = vm_limits_from_config(Some(&config), FRAME_CAP)
                .expect_err("zero process queue limit must be rejected during VM admission");
            assert!(error.to_string().contains(field), "{error}");
        }
    }

    #[test]
    fn execution_retention_limits_have_bounded_defaults_and_accept_overrides() {
        let defaults = vm_limits_from_config(None, 64 * 1024 * 1024).expect("default limits");
        assert_eq!(
            defaults.execution.completed_ttl_ms,
            DEFAULT_EXECUTION_COMPLETED_TTL_MS
        );
        assert_eq!(
            defaults.execution.max_completed_executions,
            DEFAULT_EXECUTION_MAX_COMPLETED_EXECUTIONS
        );
        assert_eq!(
            defaults.execution.live_execution_warning_threshold,
            DEFAULT_EXECUTION_LIVE_WARNING_THRESHOLD
        );

        let config = VmLimitsConfig {
            execution: Some(ExecutionLimitsConfig {
                completed_ttl_ms: Some(25),
                max_completed_executions: Some(3),
                live_execution_warning_threshold: Some(2),
            }),
            ..VmLimitsConfig::default()
        };
        let limits = vm_limits_from_config(Some(&config), 64 * 1024 * 1024).expect("overrides");
        assert_eq!(limits.execution.completed_ttl_ms, 25);
        assert_eq!(limits.execution.max_completed_executions, 3);
        assert_eq!(limits.execution.live_execution_warning_threshold, 2);

        for (field, config) in [
            (
                "limits.execution.completedTtlMs",
                ExecutionLimitsConfig {
                    completed_ttl_ms: Some(0),
                    ..ExecutionLimitsConfig::default()
                },
            ),
            (
                "limits.execution.maxCompletedExecutions",
                ExecutionLimitsConfig {
                    max_completed_executions: Some(0),
                    ..ExecutionLimitsConfig::default()
                },
            ),
            (
                "limits.execution.liveExecutionWarningThreshold",
                ExecutionLimitsConfig {
                    live_execution_warning_threshold: Some(0),
                    ..ExecutionLimitsConfig::default()
                },
            ),
        ] {
            let config = VmLimitsConfig {
                execution: Some(config),
                ..VmLimitsConfig::default()
            };
            let error = vm_limits_from_config(Some(&config), 64 * 1024 * 1024)
                .expect_err("zero execution limit must be rejected");
            assert!(error.to_string().contains(field), "{error}");
        }
    }

    #[test]
    fn canonical_overrides_convert_into_concrete_limits() {
        let config = VmLimitsConfig {
            reactor: Some(ReactorLimitsConfig {
                max_handle_commands: Some(64),
                max_handle_command_bytes: Some(512 * 1024),
                max_bridge_calls: Some(128),
                max_bridge_request_bytes: Some(8 * 1024 * 1024),
                max_bridge_response_bytes: Some(8 * 1024 * 1024),
                max_async_completions: Some(128),
                max_async_completion_bytes: Some(4 * 1024 * 1024),
                max_blocking_jobs: Some(8),
                max_blocking_bytes: Some(2 * 1024 * 1024),
                per_handle_operation_quantum: Some(8),
                completion_quantum: Some(32),
                shutdown_deadline_ms: Some(2500),
                operation_deadline_ms: Some(15_000),
                ..ReactorLimitsConfig::default()
            }),
            udp: Some(UdpLimitsConfig {
                max_buffered_datagrams: Some(128),
                max_buffered_bytes: Some(1024 * 1024),
            }),
            tls: Some(TlsLimitsConfig {
                max_buffered_bytes: Some(512 * 1024),
            }),
            http2: Some(Http2LimitsConfig {
                max_connections: Some(64),
                max_streams: Some(512),
                max_streams_per_connection: Some(64),
                max_buffered_bytes: Some(2 * 1024 * 1024),
                max_header_bytes: Some(32 * 1024),
                max_data_bytes: Some(2 * 1024 * 1024),
                max_pending_commands: Some(64),
                max_pending_command_bytes: Some(512 * 1024),
                max_pending_events: Some(64),
                max_pending_event_bytes: Some(2 * 1024 * 1024),
            }),
            acp: Some(AcpLimitsConfig {
                max_prompt_bytes: Some(32 * 1024 * 1024),
                max_prompt_blocks: Some(8_192),
                max_fallback_continuation_bytes: Some(2 * 1024 * 1024),
                max_sessions_per_vm: Some(123),
                max_prompts_per_session: Some(234),
                max_prompts_per_vm: Some(345),
                max_pending_permissions_per_session: Some(12),
                max_pending_permissions_per_vm: Some(23),
                max_permission_outcomes_per_session: Some(34),
                max_permission_outcomes_per_vm: Some(45),
                ..AcpLimitsConfig::default()
            }),
            wasm: Some(WasmLimitsConfig {
                active_cpu_time_limit_ms: Some(45_000),
                wall_clock_limit_ms: Some(90_000),
                deterministic_fuel: Some(1_000_000),
                ..WasmLimitsConfig::default()
            }),
            ..VmLimitsConfig::default()
        };

        let limits = vm_limits_from_config(Some(&config), FRAME_CAP).expect("valid overrides");
        assert_eq!(limits.reactor.max_handle_commands, 64);
        assert_eq!(limits.reactor.max_blocking_jobs, 8);
        assert_eq!(limits.reactor.max_blocking_bytes, 2 * 1024 * 1024);
        assert_eq!(limits.udp.max_buffered_datagrams, 128);
        assert_eq!(limits.tls.max_buffered_bytes, 512 * 1024);
        assert_eq!(limits.http2.max_streams_per_connection, 64);
        assert_eq!(limits.http2.max_pending_event_bytes, 2 * 1024 * 1024);
        assert_eq!(limits.acp.max_prompt_bytes, 32 * 1024 * 1024);
        assert_eq!(limits.acp.max_prompt_blocks, 8_192);
        assert_eq!(limits.acp.max_fallback_continuation_bytes, 2 * 1024 * 1024);
        assert_eq!(limits.acp.max_sessions_per_vm, 123);
        assert_eq!(limits.acp.max_prompts_per_session, 234);
        assert_eq!(limits.acp.max_prompts_per_vm, 345);
        assert_eq!(limits.acp.max_pending_permissions_per_session, 12);
        assert_eq!(limits.acp.max_pending_permissions_per_vm, 23);
        assert_eq!(limits.acp.max_permission_outcomes_per_session, 34);
        assert_eq!(limits.acp.max_permission_outcomes_per_vm, 45);
        assert_eq!(limits.wasm.active_cpu_time_limit_ms, 45_000);
        assert_eq!(limits.wasm.wall_clock_limit_ms, Some(90_000));
        assert_eq!(limits.wasm.deterministic_fuel, Some(1_000_000));
    }

    #[test]
    fn canonical_relationship_errors_name_both_limit_paths() {
        let mut limits = VmLimits::default();
        limits.reactor.max_ready_handles = limits.reactor.max_capabilities - 1;
        let error = validate_vm_limits(&limits, FRAME_CAP).expect_err("ready parent too small");
        assert!(error.to_string().contains("limits.reactor.maxCapabilities"));
        assert!(error.to_string().contains("limits.reactor.maxReadyHandles"));

        let mut limits = VmLimits::default();
        limits.http2.max_header_bytes = limits.http2.max_buffered_bytes + 1;
        let error = validate_vm_limits(&limits, FRAME_CAP).expect_err("H2 child too large");
        assert!(error.to_string().contains("limits.http2.maxHeaderBytes"));
        assert!(error.to_string().contains("limits.http2.maxBufferedBytes"));

        let mut limits = VmLimits::default();
        limits.udp.max_buffered_bytes = limits
            .resources
            .max_socket_buffered_bytes
            .expect("default aggregate")
            + 1;
        let error = validate_vm_limits(&limits, FRAME_CAP).expect_err("UDP child too large");
        assert!(error.to_string().contains("limits.udp.maxBufferedBytes"));
        assert!(error
            .to_string()
            .contains("limits.resources.maxSocketBufferedBytes"));

        type AcpRelationshipCase = (&'static str, &'static str, fn(&mut VmLimits));
        let acp_relationship_cases: [AcpRelationshipCase; 3] = [
            (
                "limits.acp.maxPromptsPerSession",
                "limits.acp.maxPromptsPerVm",
                |limits: &mut VmLimits| {
                    limits.acp.max_prompts_per_session = limits.acp.max_prompts_per_vm + 1
                },
            ),
            (
                "limits.acp.maxPendingPermissionsPerSession",
                "limits.acp.maxPendingPermissionsPerVm",
                |limits: &mut VmLimits| {
                    limits.acp.max_pending_permissions_per_session =
                        limits.acp.max_pending_permissions_per_vm + 1
                },
            ),
            (
                "limits.acp.maxPermissionOutcomesPerSession",
                "limits.acp.maxPermissionOutcomesPerVm",
                |limits: &mut VmLimits| {
                    limits.acp.max_permission_outcomes_per_session =
                        limits.acp.max_permission_outcomes_per_vm + 1
                },
            ),
        ];
        for (child_path, parent_path, set_invalid) in acp_relationship_cases {
            let mut limits = VmLimits::default();
            set_invalid(&mut limits);
            let error = validate_vm_limits(&limits, FRAME_CAP)
                .expect_err("ACP per-session collection limit exceeds per-VM limit");
            assert!(error.to_string().contains(child_path), "{error}");
            assert!(error.to_string().contains(parent_path), "{error}");
        }

        let mut limits = VmLimits::default();
        limits.reactor.operation_deadline_ms = 0;
        let error = validate_vm_limits(&limits, FRAME_CAP).expect_err("zero deadline");
        assert!(error
            .to_string()
            .contains("limits.reactor.operationDeadlineMs"));

        let mut limits = VmLimits::default();
        limits.reactor.max_async_completions = 0;
        let error = validate_vm_limits(&limits, FRAME_CAP).expect_err("zero completion capacity");
        assert!(error
            .to_string()
            .contains("limits.reactor.maxAsyncCompletions"));
    }
}
