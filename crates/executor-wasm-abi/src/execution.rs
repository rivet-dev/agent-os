use agentos_executor_contract::backend::{DirectHostReplyHandle, HostServiceError};
use agentos_executor_contract::{
    ExecutionSignalHandlerRegistration, GuestRuntimeConfig, HostRpcRequest,
};
use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

/// Low-cardinality Wasmtime telemetry shared with the composition layer even
/// when the concrete Wasmtime executor is compiled out.
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

/// Sealed standalone-WASM engine choice. JavaScript WebAssembly APIs are not
/// affected by this selector and always remain inside V8.
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

impl PartialEq for WasmExecutionEvent {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Stdout(left), Self::Stdout(right))
            | (Self::Stderr(left), Self::Stderr(right)) => left == right,
            (Self::SyncRpcRequest(left), Self::SyncRpcRequest(right)) => left == right,
            (
                Self::HostCall {
                    request: left_request,
                    reply: left_reply,
                },
                Self::HostCall {
                    request: right_request,
                    reply: right_reply,
                },
            ) => left_request == right_request && left_reply.identity() == right_reply.identity(),
            (
                Self::SignalState {
                    signal: left_signal,
                    registration: left_registration,
                },
                Self::SignalState {
                    signal: right_signal,
                    registration: right_registration,
                },
            ) => left_signal == right_signal && left_registration == right_registration,
            (Self::Exited(left), Self::Exited(right)) => left == right,
            _ => false,
        }
    }
}

impl Eq for WasmExecutionEvent {}

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
    if header.len() >= 4 && &header[..4] == b"\x7fELF" {
        return Some(NativeBinaryFormat::Elf);
    }
    if header.starts_with(b"MZ") {
        return Some(NativeBinaryFormat::PeCoff);
    }
    const MACH_O_MAGICS: [&[u8; 4]; 6] = [
        b"\xfe\xed\xfa\xce",
        b"\xce\xfa\xed\xfe",
        b"\xfe\xed\xfa\xcf",
        b"\xcf\xfa\xed\xfe",
        b"\xca\xfe\xba\xbe",
        b"\xbe\xba\xfe\xca",
    ];
    (header.len() >= 4 && MACH_O_MAGICS.iter().any(|magic| header[..4] == magic[..]))
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
    InvalidLimit(String),
    DeterministicFuelUnsupported {
        fuel: u64,
    },
    InvalidModule(String),
    NativeBinaryNotSupported {
        path: PathBuf,
        header: Vec<u8>,
        format: NativeBinaryFormat,
    },
    NonWasmBinary {
        path: PathBuf,
        header: Vec<u8>,
        shell_shim: bool,
    },
    PrepareWarmPath(std::io::Error),
    WarmupSpawn(std::io::Error),
    WarmupTimeout(Duration),
    WarmupFailed {
        exit_code: i32,
        stderr: String,
    },
    Spawn(std::io::Error),
    Control(std::io::Error),
    RpcResponse(String),
    StdinClosed,
    Stdin(std::io::Error),
    OutputBufferExceeded {
        stream: &'static str,
        limit: usize,
    },
    PendingEventLimit {
        limit_name: &'static str,
        limit: usize,
        observed: usize,
    },
    Host(HostServiceError),
    Internal {
        code: &'static str,
        message: &'static str,
    },
    EventChannelClosed,
}

impl fmt::Display for WasmExecutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingContext(context_id) => {
                write!(f, "unknown guest WebAssembly context: {context_id}")
            }
            Self::VmMismatch { expected, found } => write!(
                f,
                "guest WebAssembly context belongs to vm {expected}, not {found}"
            ),
            Self::MissingModulePath => {
                f.write_str("guest WebAssembly execution requires a module path")
            }
            Self::InvalidLimit(message) => write!(f, "invalid WebAssembly limit: {message}"),
            Self::DeterministicFuelUnsupported { fuel } => write!(
                f,
                "deterministic WebAssembly fuel budget {fuel} is not supported by the V8 compatibility backend"
            ),
            Self::InvalidModule(message) => write!(f, "invalid WebAssembly module: {message}"),
            Self::NativeBinaryNotSupported {
                path,
                header,
                format,
            } => write!(
                f,
                "ERR_NATIVE_BINARY_NOT_SUPPORTED: refused to execute native {} guest binary at {} inside the VM; only WebAssembly binaries are runnable there (header bytes: [{}])",
                format.display_name(),
                path.display(),
                hex_header(header)
            ),
            Self::NonWasmBinary {
                path,
                header,
                shell_shim,
            } if *shell_shim => write!(
                f,
                "refused to compile guest WebAssembly module at {}: file is a shell-shim script (starts with \"#!\", header bytes: [{}]) instead of a \"\\0asm\" WebAssembly binary",
                path.display(),
                hex_header(header)
            ),
            Self::NonWasmBinary { path, header, .. } => write!(
                f,
                "refused to compile guest WebAssembly module at {}: first {} byte(s) [{}] do not match the \"\\0asm\" WebAssembly magic word",
                path.display(),
                header.len(),
                hex_header(header)
            ),
            Self::PrepareWarmPath(error) => {
                write!(f, "failed to prepare shared WebAssembly warm path: {error}")
            }
            Self::WarmupSpawn(error) => {
                write!(f, "failed to start WebAssembly warmup runtime: {error}")
            }
            Self::WarmupTimeout(timeout) => write!(
                f,
                "WebAssembly warmup exceeded the configured timeout after {} ms",
                timeout.as_millis()
            ),
            Self::WarmupFailed { exit_code, stderr } if stderr.trim().is_empty() => {
                write!(f, "WebAssembly warmup exited with status {exit_code}")
            }
            Self::WarmupFailed { exit_code, stderr } => write!(
                f,
                "WebAssembly warmup exited with status {exit_code}: {}",
                stderr.trim()
            ),
            Self::Spawn(error) => write!(f, "failed to start guest WebAssembly runtime: {error}"),
            Self::Control(error) => write!(f, "failed to control guest WebAssembly runtime: {error}"),
            Self::RpcResponse(message) => {
                write!(f, "failed to write guest WebAssembly sync RPC response: {message}")
            }
            Self::StdinClosed => f.write_str("guest WebAssembly stdin is already closed"),
            Self::Stdin(error) => write!(f, "failed to write guest stdin: {error}"),
            Self::OutputBufferExceeded { stream, limit } => write!(
                f,
                "guest WebAssembly {stream} exceeded the captured output limit of {limit} bytes"
            ),
            Self::PendingEventLimit {
                limit_name,
                limit,
                observed,
            } => write!(
                f,
                "ERR_AGENTOS_RESOURCE_LIMIT: {limit_name} limit is {limit}, observed {observed}; raise {limit_name} if needed"
            ),
            Self::Host(error) => write!(f, "{}: {}", error.code, error.message),
            Self::Internal { code, message } => write!(f, "{code}: {message}"),
            Self::EventChannelClosed => {
                f.write_str("guest WebAssembly event channel closed unexpectedly")
            }
        }
    }
}

impl std::error::Error for WasmExecutionError {}

pub fn guest_visible_wasm_env(env: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut guest_env = env
        .iter()
        .filter(|(key, _)| !key.starts_with("AGENTOS_") && !key.starts_with("NODE_SYNC_RPC_"))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<BTreeMap<_, _>>();
    let guest_cwd = env
        .get("PWD")
        .filter(|value| value.starts_with('/'))
        .cloned()
        .or_else(|| {
            env.get("HOME")
                .filter(|value| value.starts_with('/'))
                .cloned()
        })
        .unwrap_or_else(|| String::from("/root"));
    let guest_home = guest_env
        .get("HOME")
        .filter(|value| value.starts_with('/'))
        .cloned()
        .unwrap_or_else(|| guest_cwd.clone());

    for (key, value) in [
        ("HOME", guest_home),
        ("PWD", guest_cwd),
        ("USER", String::from("root")),
        ("LOGNAME", String::from("root")),
        ("SHELL", String::from("/bin/sh")),
        (
            "PATH",
            String::from(
                "/usr/local/sbin:/usr/local/bin:/opt/agentos/bin:/usr/sbin:/usr/bin:/sbin:/bin",
            ),
        ),
        ("TMPDIR", String::from("/tmp")),
    ] {
        guest_env.entry(String::from(key)).or_insert(value);
    }
    guest_env
}

fn hex_header(header: &[u8]) -> String {
    header
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}
