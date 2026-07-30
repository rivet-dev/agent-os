//! Compile-only placeholders for protocol rejection paths in builds without
//! a WebAssembly executor.
//!
//! These types deliberately contain no ABI tables, parser, compiler, or engine
//! implementation. They let the shared runtime return typed "not compiled"
//! errors without pulling `agentos-executor-wasm-abi` into Node/Python-only
//! binaries.

use agentos_executor_contract::backend::{DirectHostReplyHandle, HostServiceError};
use agentos_executor_contract::{
    ExecutionSignalHandlerRegistration, GuestRuntimeConfig, HostRpcRequest,
};
use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WasmtimeMetricsSnapshot {
    pub engine_profiles: usize,
    pub module_entries: usize,
    pub module_cache_hits: u64,
    pub module_cache_misses: u64,
    pub module_cache_evictions: u64,
    pub compiled_source_bytes: u64,
    pub charged_module_bytes: usize,
    pub compile_time: Duration,
    pub process_retained_rss_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WasmPermissionTier {
    Full,
    ReadWrite,
    ReadOnly,
    Isolated,
}

impl WasmPermissionTier {
    pub fn as_env_value(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::ReadWrite => "read-write",
            Self::ReadOnly => "read-only",
            Self::Isolated => "isolated",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum StandaloneWasmBackend {
    #[default]
    V8,
    Wasmtime,
    WasmtimeThreads,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateWasmContextRequest {
    pub vm_id: String,
    pub module_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WasmContext {
    pub context_id: String,
    pub vm_id: String,
    pub module_path: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WasmExecutionLimits {
    pub active_cpu_time_limit_ms: Option<u32>,
    pub wall_clock_limit_ms: Option<u64>,
    pub deterministic_fuel: Option<u64>,
    pub max_memory_bytes: Option<u64>,
    pub max_stack_bytes: Option<u64>,
    pub max_module_file_bytes: Option<u64>,
    pub max_spawn_file_actions: Option<u64>,
    pub max_spawn_file_action_bytes: Option<u64>,
    pub max_open_fds: Option<u64>,
    pub max_sockets: Option<u64>,
    pub max_blocking_read_ms: Option<u64>,
    pub prewarm_timeout_ms: Option<u64>,
    pub runner_heap_limit_mb: Option<u32>,
    pub reactor_work_quantum: Option<usize>,
    pub bridge_call_timeout_ms: Option<u64>,
    pub max_sync_rpc_response_line_bytes: Option<u64>,
    pub pending_event_count: Option<usize>,
    pub pending_event_bytes: Option<usize>,
    pub max_threads: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StartWasmExecutionRequest {
    pub vm_id: String,
    pub context_id: String,
    pub managed_kernel_host: bool,
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: PathBuf,
    pub permission_tier: WasmPermissionTier,
    pub limits: WasmExecutionLimits,
    pub guest_runtime: GuestRuntimeConfig,
}

#[derive(Debug, Clone)]
pub enum WasmExecutionEvent {
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
    SyncRpcRequest(HostRpcRequest),
    HostCall {
        request: HostRpcRequest,
        reply: DirectHostReplyHandle,
    },
    SignalState {
        signal: u32,
        registration: ExecutionSignalHandlerRegistration,
    },
    Exited(i32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WasmExecutionResult {
    pub execution_id: String,
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeBinaryFormat {
    Elf,
    MachO,
    PeCoff,
}

impl NativeBinaryFormat {
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Elf => "ELF",
            Self::MachO => "Mach-O",
            Self::PeCoff => "PE/COFF",
        }
    }
}

pub fn detect_native_binary_format(header: &[u8]) -> Option<NativeBinaryFormat> {
    if header.starts_with(b"\x7fELF") {
        return Some(NativeBinaryFormat::Elf);
    }
    if header.starts_with(b"MZ") {
        return Some(NativeBinaryFormat::PeCoff);
    }
    const MACH_O_MAGICS: [[u8; 4]; 6] = [
        [0xfe, 0xed, 0xfa, 0xce],
        [0xce, 0xfa, 0xed, 0xfe],
        [0xfe, 0xed, 0xfa, 0xcf],
        [0xcf, 0xfa, 0xed, 0xfe],
        [0xca, 0xfe, 0xba, 0xbe],
        [0xbe, 0xba, 0xfe, 0xca],
    ];
    header
        .get(..4)
        .is_some_and(|magic| MACH_O_MAGICS.iter().any(|candidate| magic == candidate))
        .then_some(NativeBinaryFormat::MachO)
}

#[derive(Debug)]
pub enum WasmExecutionError {
    MissingContext(String),
    VmMismatch {
        expected: String,
        found: String,
    },
    MissingModulePath,
    DeterministicFuelUnsupported {
        fuel: u64,
    },
    NativeBinaryNotSupported {
        path: PathBuf,
        header: Vec<u8>,
        format: NativeBinaryFormat,
    },
    Spawn(std::io::Error),
    Host(HostServiceError),
    Internal {
        code: &'static str,
        message: &'static str,
    },
    EventChannelClosed,
}

impl fmt::Display for WasmExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingContext(context) => write!(formatter, "unknown WASM context: {context}"),
            Self::VmMismatch { expected, found } => {
                write!(formatter, "WASM context belongs to {expected}, not {found}")
            }
            Self::MissingModulePath => formatter.write_str("WASM module path is required"),
            Self::DeterministicFuelUnsupported { fuel } => {
                write!(formatter, "WASM deterministic fuel {fuel} is unsupported")
            }
            Self::NativeBinaryNotSupported { path, format, .. } => write!(
                formatter,
                "ERR_NATIVE_BINARY_NOT_SUPPORTED: {} at {} is not WebAssembly",
                format.display_name(),
                path.display()
            ),
            Self::Spawn(error) => write!(formatter, "failed to spawn WASM runtime: {error}"),
            Self::Host(error) => write!(formatter, "{}: {}", error.code, error.message),
            Self::Internal { code, message } => write!(formatter, "{code}: {message}"),
            Self::EventChannelClosed => formatter.write_str("WASM event channel closed"),
        }
    }
}

impl std::error::Error for WasmExecutionError {}
