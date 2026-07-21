use crate::backend::{
    DescendantOutputOwnership, DescendantWaitOwnership, DirectHostReplyHandle, ExecutionBackend,
    ExecutionBackendKind, ExecutionExit, ExecutionWakeHandle, ExecutionWakeIdentity,
    HostServiceError, PublishedSignalCheckpoint, ShutdownOutcome, ShutdownReason,
    SignalCheckpointOutcome, SynchronousFdWritePolicy,
};
use crate::common::{
    encode_json_string, encode_json_string_array, encode_json_string_map, frozen_time_ms,
};
use crate::host::ProcessHostCapabilitySet;
use crate::javascript::{
    CreateJavascriptContextRequest, GuestRuntimeConfig, HostRpcRequest, JavascriptExecution,
    JavascriptExecutionEngine, JavascriptExecutionError, JavascriptExecutionEvent,
    JavascriptExecutionLimits, JavascriptSyncRpcResponder, StartJavascriptExecutionRequest,
};
use crate::node_import_cache::NodeImportCache;
use crate::runtime_support::{env_flag_enabled, file_fingerprint, warmup_marker_path};
use crate::signal::{ExecutionSignalDispositionAction, ExecutionSignalHandlerRegistration};
use crate::v8_host::{V8RuntimeHost, V8SessionHandle};
use crate::v8_runtime;
use agentos_bridge::queue_tracker::{
    register_limit, warn_limit_exhausted, QueueGauge, TrackedLimit,
};
use agentos_runtime::RuntimeContext;
use base64::Engine as _;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fmt;
use std::fs;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::os::unix::fs::{FileExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::Notify;

#[path = "wasm/profile.rs"]
mod profile;
#[path = "wasm/wasmtime/mod.rs"]
pub mod wasmtime;

const WASM_MODULE_PATH_ENV: &str = "AGENTOS_WASM_MODULE_PATH";
const WASM_GUEST_ARGV_ENV: &str = "AGENTOS_GUEST_ARGV";
const WASM_GUEST_ENV_ENV: &str = "AGENTOS_GUEST_ENV";
const WASM_PERMISSION_TIER_ENV: &str = "AGENTOS_WASM_PERMISSION_TIER";
const WASM_PREWARM_ONLY_ENV: &str = "AGENTOS_WASM_PREWARM_ONLY";
const WASM_HOST_CWD_ENV: &str = "AGENTOS_WASM_HOST_CWD";
const WASM_SANDBOX_ROOT_ENV: &str = "AGENTOS_SANDBOX_ROOT";
const WASM_WARMUP_DEBUG_ENV: &str = "AGENTOS_WASM_WARMUP_DEBUG";
pub const WASM_MAX_MEMORY_BYTES_ENV: &str = "AGENTOS_WASM_MAX_MEMORY_BYTES";
pub const WASM_MAX_STACK_BYTES_ENV: &str = "AGENTOS_WASM_MAX_STACK_BYTES";
pub const WASM_MAX_MODULE_FILE_BYTES_ENV: &str = "AGENTOS_WASM_MAX_MODULE_FILE_BYTES";
const WASM_MAX_OPEN_FDS_ENV: &str = "AGENTOS_WASM_MAX_OPEN_FDS";
const WASM_MAX_SPAWN_FILE_ACTIONS_ENV: &str = "AGENTOS_WASM_MAX_SPAWN_FILE_ACTIONS";
const WASM_MAX_SPAWN_FILE_ACTION_BYTES_ENV: &str = "AGENTOS_WASM_MAX_SPAWN_FILE_ACTION_BYTES";
const WASM_MAX_SOCKETS_ENV: &str = "AGENTOS_WASM_MAX_SOCKETS";
const WASM_MAX_BLOCKING_READ_MS_ENV: &str = "AGENTOS_WASM_MAX_BLOCKING_READ_MS";
const WASM_INTERNAL_MAX_STACK_BYTES_ENV: &str = "AGENTOS_INTERNAL_WASM_MAX_STACK_BYTES";
const WASM_INTERNAL_SYNC_RPC_RESPONSE_LINE_BYTES_ENV: &str =
    "AGENTOS_INTERNAL_WASM_SYNC_RPC_RESPONSE_LINE_BYTES";
const WASM_WARMUP_METRICS_PREFIX: &str = "__AGENTOS_WASM_WARMUP_METRICS__:";
const WASM_SIGNAL_STATE_PREFIX: &str = "__AGENTOS_WASM_SIGNAL_STATE__:";
const WASM_WARMUP_MARKER_VERSION: &str = "1";
const WASM_PAGE_BYTES: u64 = 65_536;
const WASM_TIMEOUT_EXIT_CODE: i32 = 124;
const MAX_WASM_MODULE_FILE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_WASM_IMPORT_SECTION_ENTRIES: usize = 16_384;
const MAX_WASM_MEMORY_SECTION_ENTRIES: usize = 1_024;
const MAX_WASM_VARUINT_BYTES: usize = 10;
const DEFAULT_WASM_GUEST_HOME: &str = "/root";
const DEFAULT_WASM_GUEST_USER: &str = "root";
const DEFAULT_WASM_GUEST_SHELL: &str = "/bin/sh";
const DEFAULT_WASM_GUEST_PATH: &str =
    "/usr/local/sbin:/usr/local/bin:/opt/agentos/bin:/usr/sbin:/usr/bin:/sbin:/bin";
// Warmup is a best-effort compile-cache optimization; fall back to a cold start
// instead of burning minutes on a stalled prewarm session.
const DEFAULT_WASM_PREWARM_TIMEOUT_MS: u64 = 30_000;
/// Default V8 heap cap (MB) for the wasm *runner* isolate.
///
/// The runner is trusted sidecar infrastructure: it compiles the WASI runtime +
/// the guest's wasm module (e.g. `bash.wasm`) into its own isolate before the
/// guest runs. That compilation routinely needs far more than the 128 MiB
/// per-*guest*-isolate budget (`isolate::DEFAULT_HEAP_LIMIT_MB`); leaving the
/// runner on that default makes warmup OOM mid-compile, terminating the isolate
/// with an uncatchable (message-less) exception that surfaces as the opaque
/// `WebAssembly warmup exited with status 1 (Error: null)`. Raising the runner
/// heap does NOT weaken guest isolation — the guest module's memory/fuel/stack are
/// bounded separately, Rust-side, from `request.limits`. The value is a ceiling
/// (`heap_limits(0, cap)`), committed only as used, and operators may tune it via
/// typed `limits.wasm.runnerHeapLimitMb`.
///
/// Note the ceiling is reachable by guest-driven work: the runner compiles the
/// guest's wasm module, so a large/hostile module can push the runner heap toward
/// this cap. That is contained per-isolate (the near-heap-limit guard terminates
/// the offending isolate, never the shared process), but operators running many
/// concurrent wasm commands on memory-constrained hosts may want to lower it.
const DEFAULT_WASM_RUNNER_HEAP_LIMIT_MB: u32 = 2048;
// The whole point of the runner heap default is to exceed the 128 MiB per-guest
// isolate budget that OOMs warmup; enforce that invariant at compile time.
const _: () = assert!(DEFAULT_WASM_RUNNER_HEAP_LIMIT_MB > 128);
const MAX_SYNC_WASM_PREWARM_MODULE_BYTES: u64 = 16 * 1024 * 1024;
const WASM_CAPTURED_OUTPUT_LIMIT_BYTES: usize = 16 * 1024 * 1024;
const DEFAULT_WASM_PENDING_EVENT_COUNT: usize = 512;
const DEFAULT_WASM_PENDING_EVENT_BYTES: usize = 16 * 1024 * 1024;
const WASM_SYNC_READ_LIMIT_BYTES: usize = 16 * 1024 * 1024;
const DEFAULT_WASM_SYNC_RPC_RESPONSE_LINE_BYTES: u64 = 16 * 1024 * 1024;
// `_processWasmSyncRpc` returns file-read bytes as one CBOR byte string. The
// bridge contract bounds the encoded response payload, not the unencoded file
// bytes, so the runner must leave room for CBOR's byte-string header.
const WASM_PROCESS_SYNC_RPC_RESPONSE_BYTES: usize = 256 * 1024;
const WASM_INLINE_RUNNER_ENTRYPOINT: &str = "./__agentos_wasm_runner__.mjs";
const WASM_SNAPSHOT_RUNNER_ENV: &str = "AGENTOS_WASM_SNAPSHOT_RUNNER";
const WASM_RUNNER_NO_CACHE_ENV: &str = "AGENTOS_WASM_RUNNER_NO_CACHE";
const WASM_MODULE_BYTES_CACHE_CAPACITY: usize = 64;
const NODE_WASI_MODULE_SOURCE: &str = include_str!("../assets/runners/wasi-module.js");
const WASM_SIDECAR_ROUTED_FS_SYNC_METHODS: &[&str] = &[
    "fs.accessSync",
    "fs.blockingIoTimeoutMsSync",
    "fs.chmodForProcessSync",
    "fs.chmodSync",
    "fs.chownSync",
    "fs.closeSync",
    "fs.collapseRangeSync",
    "fs.existsSync",
    "fs.fallocateSync",
    "fs.fdatasyncSync",
    "fs.fiemapSync",
    "fs.fiemapAtSync",
    "fs.fstatSync",
    "fs.fsyncSync",
    "fs.ftruncateSync",
    "fs.getxattrSync",
    "fs.insertRangeSync",
    "fs.lchownSync",
    "fs.linkFdSync",
    "fs.linkSync",
    "fs.listxattrSync",
    "fs.lstatSync",
    "fs.mkdirSync",
    "fs.mknodSync",
    "fs.namedFifoPeerReadySync",
    "fs.openSync",
    "fs.openTmpfileSync",
    "fs.punchHoleSync",
    "fs.readFileSync",
    "fs.readSync",
    "fs.readdirSync",
    "fs.readlinkSync",
    "fs.remountSync",
    "fs.removexattrSync",
    "fs.renameAt2Sync",
    "fs.renameSync",
    "fs.rmdirSync",
    "fs.setxattrSync",
    "fs.statfsSync",
    "fs.statSync",
    "fs.symlinkSync",
    "fs.truncateForProcessSync",
    "fs.unlinkSync",
    "fs.zeroRangeSync",
    "fs.writeFileSync",
    "fs.writeSync",
];
const WASM_SIDECAR_ROUTED_KERNEL_SYNC_METHODS: &[&str] = &[
    "__kernel_isatty",
    "__kernel_poll",
    "__kernel_stdin_read",
    "__kernel_stdio_write",
    "__kernel_tty_size",
    "__pty_set_raw_mode",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WasmPermissionTier {
    Full,
    ReadWrite,
    ReadOnly,
    Isolated,
}

/// Sealed standalone-WASM engine choice. JavaScript WebAssembly APIs are not
/// affected by this selector and always remain inside V8.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum StandaloneWasmBackend {
    #[default]
    V8,
    Wasmtime,
    /// Explicit Wasmtime pthread/shared-memory profile. Ordinary Wasmtime
    /// executions retain the single-thread feature surface.
    WasmtimeThreads,
}

impl WasmPermissionTier {
    fn as_env_value(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::ReadWrite => "read-write",
            Self::ReadOnly => "read-only",
            Self::Isolated => "isolated",
        }
    }
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

/// Per-execution WebAssembly runtime limits, carried as typed fields rather
/// than `AGENTOS_WASM_*` env vars. Populated by the sidecar from the per-VM
/// kernel `ResourceLimits` (originating from `CreateVmConfig` on the BARE wire);
/// `None` selects "unlimited / engine default". See the env-vs-wire rule in
/// `crates/sidecar/CLAUDE.md`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WasmExecutionLimits {
    /// Active CPU-time budget in milliseconds. The V8 compatibility backend
    /// maps this to the trusted runner isolate's CPU watchdog.
    pub active_cpu_time_limit_ms: Option<u32>,
    /// Optional elapsed wall-clock backstop in milliseconds.
    pub wall_clock_limit_ms: Option<u64>,
    /// Optional deterministic instruction budget. V8 cannot meter this and
    /// rejects an explicit value before starting guest execution.
    pub deterministic_fuel: Option<u64>,
    /// Linear-memory cap in bytes, validated against the module's declared
    /// initial/maximum memory before execution.
    pub max_memory_bytes: Option<u64>,
    /// Stack cap in bytes. Until the V8 runner exposes an enforceable per-module
    /// stack lever, any configured value fails closed rather than silently using
    /// V8's unrelated default stack bound.
    pub max_stack_bytes: Option<u64>,
    /// Maximum executable image bytes accepted for initial and replacement
    /// modules. The trusted runner needs the typed value for fexecve preads.
    pub max_module_file_bytes: Option<u64>,
    /// Maximum number of file actions decoded for one posix_spawn call.
    pub max_spawn_file_actions: Option<u64>,
    /// Maximum serialized file-action bytes accepted for one posix_spawn call.
    pub max_spawn_file_action_bytes: Option<u64>,
    /// Maximum guest-visible open descriptors, including runner-owned sockets.
    pub max_open_fds: Option<u64>,
    /// Maximum runner-owned guest sockets.
    pub max_sockets: Option<u64>,
    /// Maximum time a blocking runner syscall may cooperatively wait.
    pub max_blocking_read_ms: Option<u64>,
    /// Best-effort warmup/compile-cache timeout in ms.
    pub prewarm_timeout_ms: Option<u64>,
    /// V8 heap cap for the trusted JS runner isolate that hosts WASI/WASM.
    pub runner_heap_limit_mb: Option<u32>,
    /// VM readiness work bound forwarded unchanged to the WASI V8 runner.
    pub reactor_work_quantum: Option<usize>,
    /// Per-call host bridge deadline forwarded unchanged to the WASI V8 runner.
    pub bridge_call_timeout_ms: Option<u64>,
    /// Maximum encoded line retained by the fallback synchronous RPC response
    /// pipe. Sidecar VMs supply `limits.reactor.maxBridgeResponseBytes` here.
    pub max_sync_rpc_response_line_bytes: Option<u64>,
    /// Maximum compatibility-adapter events retained before the sidecar
    /// consumes them. Sidecar VMs supply limits.process.pendingEventCount.
    pub pending_event_count: Option<usize>,
    /// Maximum aggregate bytes retained by compatibility-adapter event queues.
    /// Sidecar VMs supply limits.process.pendingEventBytes.
    pub pending_event_bytes: Option<usize>,
    /// Maximum native guest threads in an explicitly threaded Wasmtime group,
    /// including the initial thread.
    pub max_threads: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StartWasmExecutionRequest {
    pub vm_id: String,
    pub context_id: String,
    /// Whether this execution is attached to the sidecar's authoritative
    /// kernel host. This is trusted adapter configuration, not guest env.
    pub managed_kernel_host: bool,
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: PathBuf,
    pub permission_tier: WasmPermissionTier,
    /// Per-execution runtime limits (see [`WasmExecutionLimits`]).
    pub limits: WasmExecutionLimits,
    /// Per-execution guest-runtime config, forwarded to the WASI runner's JS
    /// execution (see [`JavascriptExecutionLimits`]'s sibling
    /// [`crate::javascript::GuestRuntimeConfig`]).
    pub guest_runtime: GuestRuntimeConfig,
}

#[derive(Debug, Clone)]
pub enum WasmExecutionEvent {
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
    SyncRpcRequest(HostRpcRequest),
    /// Native executor request carrying its own generation-bound direct waiter.
    /// This is decoded into the same runtime-neutral HostOperation path as the
    /// compatibility runner, but has no V8 responder or line protocol.
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedWasmModule {
    specifier: String,
    resolved_path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeBinaryFormat {
    Elf,
    MachO,
    PeCoff,
}

impl NativeBinaryFormat {
    fn display_name(self) -> &'static str {
        match self {
            Self::Elf => "ELF",
            Self::MachO => "Mach-O",
            Self::PeCoff => "PE/COFF",
        }
    }
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
            Self::VmMismatch { expected, found } => {
                write!(
                    f,
                    "guest WebAssembly context belongs to vm {expected}, not {found}"
                )
            }
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
            } => {
                let header_hex = header
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                write!(
                    f,
                    "ERR_NATIVE_BINARY_NOT_SUPPORTED: refused to execute native {} guest binary at {} inside the VM; only WebAssembly binaries are runnable there (header bytes: [{header_hex}])",
                    format.display_name(),
                    path.display()
                )
            }
            Self::NonWasmBinary {
                path,
                header,
                shell_shim,
            } => {
                let header_hex = header
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                if *shell_shim {
                    write!(
                        f,
                        "refused to compile guest WebAssembly module at {}: file is a shell-shim script (starts with \"#!\", header bytes: [{header_hex}]) instead of a \"\\0asm\" WebAssembly binary",
                        path.display()
                    )
                } else {
                    write!(
                        f,
                        "refused to compile guest WebAssembly module at {}: first {} byte(s) [{header_hex}] do not match the \"\\0asm\" WebAssembly magic word",
                        path.display(),
                        header.len()
                    )
                }
            }
            Self::PrepareWarmPath(err) => {
                write!(f, "failed to prepare shared WebAssembly warm path: {err}")
            }
            Self::WarmupSpawn(err) => {
                write!(f, "failed to start WebAssembly warmup runtime: {err}")
            }
            Self::WarmupTimeout(timeout) => {
                write!(
                    f,
                    "WebAssembly warmup exceeded the configured timeout after {} ms",
                    timeout.as_millis()
                )
            }
            Self::WarmupFailed { exit_code, stderr } => {
                if stderr.trim().is_empty() {
                    write!(f, "WebAssembly warmup exited with status {exit_code}")
                } else {
                    write!(
                        f,
                        "WebAssembly warmup exited with status {exit_code}: {}",
                        stderr.trim()
                    )
                }
            }
            Self::Spawn(err) => write!(f, "failed to start guest WebAssembly runtime: {err}"),
            Self::Control(err) => write!(f, "failed to control guest WebAssembly runtime: {err}"),
            Self::RpcResponse(message) => {
                write!(
                    f,
                    "failed to write guest WebAssembly sync RPC response: {message}"
                )
            }
            Self::StdinClosed => f.write_str("guest WebAssembly stdin is already closed"),
            Self::Stdin(err) => write!(f, "failed to write guest stdin: {err}"),
            Self::OutputBufferExceeded { stream, limit } => {
                write!(
                    f,
                    "guest WebAssembly {stream} exceeded the captured output limit of {limit} bytes"
                )
            }
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

#[derive(Debug)]
pub struct WasmExecution {
    backend: WasmExecutionBackend,
}

#[derive(Debug)]
enum WasmExecutionBackend {
    V8(Box<V8WasmExecution>),
    Wasmtime(wasmtime::WasmtimeExecution),
}

#[derive(Debug)]
struct V8WasmExecution {
    execution_id: String,
    child_pid: u32,
    inner: JavascriptExecution,
    execution_timeout: Option<Duration>,
    execution_started_at: Instant,
    timeout_reported: bool,
    wall_clock_gauge: Option<Arc<QueueGauge>>,
    internal_sync_rpc: WasmInternalSyncRpc,
    pending_events: WasmEventQueue,
    signal_checkpoints: WasmSignalCheckpointInbox,
    stdout_stream_buffer: Vec<u8>,
    stderr_stream_buffer: Vec<u8>,
    max_stack_bytes: Option<u64>,
    pending_v8_stack_overflow: Option<Vec<u8>>,
}

#[derive(Debug)]
struct WasmInternalSyncRpc {
    module_guest_paths: Vec<String>,
    module_host_path: PathBuf,
    guest_cwd: String,
    host_cwd: PathBuf,
    sandbox_root: Option<PathBuf>,
    guest_path_mappings: Vec<WasmGuestPathMapping>,
    route_fs_through_sidecar: bool,
    next_fd: u32,
    open_files: BTreeMap<u32, fs::File>,
    pending_events: WasmEventQueue,
}

#[derive(Debug)]
struct QueuedSignalCheckpoint {
    identity: ExecutionWakeIdentity,
    delivery: PublishedSignalCheckpoint,
    retained_bytes: usize,
    budget: Arc<WasmPendingEventBudget>,
}

impl Drop for QueuedSignalCheckpoint {
    fn drop(&mut self) {
        self.budget.release(self.retained_bytes);
    }
}

#[derive(Debug)]
struct WasmSignalCheckpointInbox {
    checkpoints: Mutex<VecDeque<QueuedSignalCheckpoint>>,
    budget: Arc<WasmPendingEventBudget>,
}

impl WasmSignalCheckpointInbox {
    fn new(budget: Arc<WasmPendingEventBudget>) -> Self {
        Self {
            checkpoints: Mutex::new(VecDeque::new()),
            budget,
        }
    }

    fn publish(
        &self,
        identity: ExecutionWakeIdentity,
        delivery: PublishedSignalCheckpoint,
    ) -> Result<(), HostServiceError> {
        let retained_bytes = std::mem::size_of::<QueuedSignalCheckpoint>();
        self.budget.reserve(retained_bytes).map_err(|error| {
            HostServiceError::new("ERR_AGENTOS_RESOURCE_LIMIT", error.to_string())
        })?;
        let mut checkpoints = match self.checkpoints.lock() {
            Ok(checkpoints) => checkpoints,
            Err(_) => {
                self.budget.release(retained_bytes);
                return Err(HostServiceError::new(
                    "EIO",
                    "ERR_AGENTOS_WASM_SIGNAL_INBOX_POISONED: signal checkpoint state was poisoned by a prior panic",
                ));
            }
        };
        checkpoints.push_back(QueuedSignalCheckpoint {
            identity,
            delivery,
            retained_bytes,
            budget: Arc::clone(&self.budget),
        });
        Ok(())
    }

    fn take(
        &self,
        identity: ExecutionWakeIdentity,
    ) -> Result<Option<PublishedSignalCheckpoint>, HostServiceError> {
        let mut checkpoints = self
            .checkpoints
            .lock()
            .map_err(|_| {
                HostServiceError::new(
                    "EIO",
                    "ERR_AGENTOS_WASM_SIGNAL_INBOX_POISONED: signal checkpoint state was poisoned by a prior panic",
                )
            })?;
        let Some(pending) = checkpoints.front() else {
            return Ok(None);
        };
        if pending.identity != identity {
            return Err(HostServiceError::new(
                "ESTALE",
                "published signal delivery identity does not match the active execution",
            ));
        }
        Ok(checkpoints.pop_front().map(|pending| pending.delivery))
    }

    fn discard(
        &self,
        identity: ExecutionWakeIdentity,
        delivery_token: u64,
    ) -> Result<(), HostServiceError> {
        let mut checkpoints = self.checkpoints.lock().map_err(|_| {
            HostServiceError::new(
                "EIO",
                "ERR_AGENTOS_WASM_SIGNAL_INBOX_POISONED: signal checkpoint state was poisoned by a prior panic",
            )
        })?;
        let index = checkpoints
            .iter()
            .rposition(|pending| {
                pending.identity == identity && pending.delivery.delivery_token == delivery_token
            })
            .ok_or_else(|| {
                HostServiceError::new(
                    "ESTALE",
                    "failed compatibility-WASM signal publication was no longer queued",
                )
            })?;
        checkpoints.remove(index);
        Ok(())
    }

    fn discard_identity(&self, identity: ExecutionWakeIdentity) -> Result<(), HostServiceError> {
        let mut checkpoints = self.checkpoints.lock().map_err(|_| {
            HostServiceError::new(
                "EIO",
                "ERR_AGENTOS_WASM_SIGNAL_INBOX_POISONED: signal checkpoint state was poisoned by a prior panic",
            )
        })?;
        checkpoints.retain(|pending| pending.identity != identity);
        Ok(())
    }
}

#[derive(Debug)]
struct WasmPendingEventBudget {
    state: Mutex<(usize, usize)>,
    count_limit: usize,
    byte_limit: usize,
    count_gauge: Arc<QueueGauge>,
    byte_gauge: Arc<QueueGauge>,
}

impl WasmPendingEventBudget {
    fn new(count_limit: usize, byte_limit: usize) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new((0, 0)),
            count_limit,
            byte_limit,
            count_gauge: agentos_bridge::queue_tracker::register_queue(
                TrackedLimit::PendingExecutionEvents,
                count_limit,
            ),
            byte_gauge: agentos_bridge::queue_tracker::register_queue(
                TrackedLimit::PendingExecutionEventBytes,
                byte_limit,
            ),
        })
    }

    fn reserve(&self, bytes: usize) -> Result<(), WasmExecutionError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| WasmExecutionError::Internal {
                code: "ERR_AGENTOS_WASM_EVENT_ACCOUNTING_POISONED",
                message: "pending-event accounting was poisoned by a prior panic",
            })?;
        let observed_count = state.0.saturating_add(1);
        if observed_count > self.count_limit {
            return Err(WasmExecutionError::PendingEventLimit {
                limit_name: "limits.process.pendingEventCount",
                limit: self.count_limit,
                observed: observed_count,
            });
        }
        let observed_bytes = state.1.saturating_add(bytes);
        if observed_bytes > self.byte_limit {
            return Err(WasmExecutionError::PendingEventLimit {
                limit_name: "limits.process.pendingEventBytes",
                limit: self.byte_limit,
                observed: observed_bytes,
            });
        }
        *state = (observed_count, observed_bytes);
        self.count_gauge.observe_depth(observed_count);
        self.byte_gauge.observe_depth(observed_bytes);
        Ok(())
    }

    fn release(&self, bytes: usize) {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => {
                eprintln!(
                    "ERR_AGENTOS_WASM_EVENT_ACCOUNTING_POISONED: recovering pending-event accounting while releasing a reservation"
                );
                poisoned.into_inner()
            }
        };
        state.0 = state.0.saturating_sub(1);
        state.1 = state.1.saturating_sub(bytes);
        self.count_gauge.observe_depth(state.0);
        self.byte_gauge.observe_depth(state.1);
    }

    #[cfg(test)]
    fn usage(&self) -> (usize, usize) {
        *self.state.lock().expect("pending-event accounting")
    }
}

#[derive(Debug)]
struct QueuedWasmEvent {
    event: Option<WasmExecutionEvent>,
    retained_bytes: usize,
    budget: Arc<WasmPendingEventBudget>,
}

impl Drop for QueuedWasmEvent {
    fn drop(&mut self) {
        self.budget.release(self.retained_bytes);
    }
}

#[derive(Debug)]
struct WasmEventQueue {
    events: VecDeque<QueuedWasmEvent>,
    budget: Arc<WasmPendingEventBudget>,
}

impl WasmEventQueue {
    fn new(budget: Arc<WasmPendingEventBudget>) -> Self {
        Self {
            events: VecDeque::new(),
            budget,
        }
    }

    fn push_back(&mut self, event: WasmExecutionEvent) -> Result<(), WasmExecutionError> {
        let retained_bytes = wasm_event_retained_bytes(&event);
        self.budget.reserve(retained_bytes)?;
        self.events.push_back(QueuedWasmEvent {
            event: Some(event),
            retained_bytes,
            budget: Arc::clone(&self.budget),
        });
        Ok(())
    }

    fn pop_front(&mut self) -> Option<WasmExecutionEvent> {
        self.events
            .pop_front()
            .and_then(|mut queued| queued.event.take())
    }
}

impl Default for WasmEventQueue {
    fn default() -> Self {
        Self::new(WasmPendingEventBudget::new(
            DEFAULT_WASM_PENDING_EVENT_COUNT,
            DEFAULT_WASM_PENDING_EVENT_BYTES,
        ))
    }
}

fn wasm_event_retained_bytes(event: &WasmExecutionEvent) -> usize {
    let envelope = std::mem::size_of::<WasmExecutionEvent>();
    match event {
        WasmExecutionEvent::Stdout(bytes) | WasmExecutionEvent::Stderr(bytes) => {
            envelope.saturating_add(bytes.len())
        }
        WasmExecutionEvent::SyncRpcRequest(request)
        | WasmExecutionEvent::HostCall { request, .. } => envelope
            .saturating_add(request.method.len())
            .saturating_add(request.raw_bytes_args.values().map(Vec::len).sum::<usize>())
            // JSON args arrive through an independently frame-bounded bridge;
            // retain a conservative envelope without reserializing attacker
            // input merely to account it.
            .saturating_add(4 * 1024),
        WasmExecutionEvent::SignalState { registration, .. } => envelope.saturating_add(
            registration
                .mask
                .len()
                .saturating_mul(std::mem::size_of::<u32>()),
        ),
        WasmExecutionEvent::Exited(_) => envelope,
    }
}

#[derive(Debug, Clone)]
struct WasmGuestPathMapping {
    guest_path: String,
    host_path: PathBuf,
    read_only: bool,
}

impl V8WasmExecution {
    pub fn sync_rpc_responder(&self) -> JavascriptSyncRpcResponder {
        self.inner.sync_rpc_responder()
    }

    pub fn execution_id(&self) -> &str {
        &self.execution_id
    }

    pub fn native_process_id(&self) -> Option<u32> {
        (self.child_pid != 0).then_some(self.child_pid)
    }

    pub fn v8_session_handle(&self) -> V8SessionHandle {
        self.inner.v8_session_handle()
    }

    pub fn start_prepared(&mut self) -> Result<(), WasmExecutionError> {
        self.inner.start_prepared().map_err(map_javascript_error)?;
        self.execution_started_at = Instant::now();
        Ok(())
    }

    #[doc(hidden)]
    pub fn is_prepared_for_start(&self) -> bool {
        self.inner.is_prepared_for_start()
    }

    pub fn write_stdin(&mut self, chunk: &[u8]) -> Result<(), WasmExecutionError> {
        self.inner.write_stdin(chunk).map_err(map_javascript_error)
    }

    /// Feed stdin WITHOUT emitting a `stdin` stream event to the V8 session.
    /// Sidecar-managed wasm always reads stdin through the kernel
    /// (`__kernel_stdin_read`); the stream event is never consumed there, and
    /// while the guest thread is blocked in a sync bridge call every
    /// unconsumed event lands in the session's bounded deferred-message queue
    /// — one dead event per keystroke until the queue limit kills the session.
    pub fn write_stdin_kernel_only(&mut self, chunk: &[u8]) -> Result<(), WasmExecutionError> {
        self.inner
            .write_kernel_stdin_only(chunk)
            .map_err(map_javascript_error)
    }

    pub fn close_stdin(&mut self) -> Result<(), WasmExecutionError> {
        self.inner.close_stdin().map_err(map_javascript_error)
    }

    pub fn send_stream_event(
        &self,
        event_type: &str,
        payload: Value,
    ) -> Result<(), WasmExecutionError> {
        self.inner
            .send_stream_event(event_type, payload)
            .map_err(map_javascript_error)
    }

    pub fn terminate(&self) -> Result<(), WasmExecutionError> {
        self.inner.terminate().map_err(map_javascript_error)
    }

    pub fn pause(&self) -> Result<(), WasmExecutionError> {
        self.inner.pause().map_err(map_javascript_error)
    }

    pub fn resume(&self) -> Result<(), WasmExecutionError> {
        self.inner.resume().map_err(map_javascript_error)
    }

    pub fn respond_sync_rpc_success(
        &mut self,
        id: u64,
        result: Value,
    ) -> Result<(), WasmExecutionError> {
        self.inner
            .respond_sync_rpc_success(id, result)
            .map_err(map_javascript_error)
    }

    pub fn claim_sync_rpc_response(&mut self, id: u64) -> Result<bool, WasmExecutionError> {
        self.inner
            .claim_sync_rpc_response(id)
            .map_err(map_javascript_error)
    }

    pub fn respond_claimed_sync_rpc_success(
        &mut self,
        id: u64,
        result: Value,
    ) -> Result<(), WasmExecutionError> {
        self.inner
            .respond_claimed_sync_rpc_success(id, result)
            .map_err(map_javascript_error)
    }

    pub fn respond_sync_rpc_raw_success(
        &mut self,
        id: u64,
        payload: Vec<u8>,
    ) -> Result<(), WasmExecutionError> {
        self.inner
            .respond_sync_rpc_raw_success(id, payload)
            .map_err(map_javascript_error)
    }

    pub fn respond_sync_rpc_error(
        &mut self,
        id: u64,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<(), WasmExecutionError> {
        self.inner
            .respond_sync_rpc_error(id, code, message)
            .map_err(map_javascript_error)
    }

    pub fn respond_claimed_sync_rpc_error(
        &mut self,
        id: u64,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<(), WasmExecutionError> {
        self.inner
            .respond_claimed_sync_rpc_error(id, code, message)
            .map_err(map_javascript_error)
    }

    pub async fn poll_event(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<WasmExecutionEvent>, WasmExecutionError> {
        loop {
            if let Some(event) = self.pending_events.pop_front() {
                return Ok(Some(event));
            }
            if let Some(event) = self.internal_sync_rpc.pending_events.pop_front() {
                self.enqueue_wasm_event(event)?;
                continue;
            }
            if let Some(event) = self.timeout_event_if_expired()? {
                return Ok(Some(event));
            }
            let poll_timeout = self.deadline_capped_timeout(timeout);
            match self
                .inner
                .poll_event(poll_timeout)
                .await
                .map_err(map_javascript_error)?
            {
                Some(event) => {
                    if let JavascriptExecutionEvent::SyncRpcRequest(request) = &event {
                        if self.handle_internal_sync_rpc(request)? {
                            continue;
                        }
                    }
                    self.enqueue_javascript_event(event)?;
                }
                None if poll_timeout < timeout => continue,
                None => return Ok(None),
            }
        }
    }

    pub fn try_poll_event(&mut self) -> Result<Option<WasmExecutionEvent>, WasmExecutionError> {
        loop {
            if let Some(event) = self.pending_events.pop_front() {
                return Ok(Some(event));
            }
            if let Some(event) = self.internal_sync_rpc.pending_events.pop_front() {
                self.enqueue_wasm_event(event)?;
                continue;
            }
            if let Some(event) = self.timeout_event_if_expired()? {
                return Ok(Some(event));
            }
            let Some(event) = self.inner.try_poll_event().map_err(map_javascript_error)? else {
                return Ok(None);
            };
            if let JavascriptExecutionEvent::SyncRpcRequest(request) = &event {
                if self.handle_internal_sync_rpc(request)? {
                    continue;
                }
            }
            self.enqueue_javascript_event(event)?;
        }
    }

    pub fn poll_event_blocking(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<WasmExecutionEvent>, WasmExecutionError> {
        loop {
            if let Some(event) = self.pending_events.pop_front() {
                return Ok(Some(event));
            }
            if let Some(event) = self.internal_sync_rpc.pending_events.pop_front() {
                self.enqueue_wasm_event(event)?;
                continue;
            }
            if let Some(event) = self.timeout_event_if_expired()? {
                return Ok(Some(event));
            }
            let poll_timeout = self.deadline_capped_timeout(timeout);
            match self
                .inner
                .poll_event_blocking(poll_timeout)
                .map_err(map_javascript_error)?
            {
                Some(event) => {
                    if let JavascriptExecutionEvent::SyncRpcRequest(request) = &event {
                        if self.handle_internal_sync_rpc(request)? {
                            continue;
                        }
                    }
                    self.enqueue_javascript_event(event)?;
                }
                None if poll_timeout < timeout => continue,
                None => return Ok(None),
            }
        }
    }

    pub fn wait(mut self) -> Result<WasmExecutionResult, WasmExecutionError> {
        self.close_stdin()?;
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        loop {
            match self.wait_event_blocking()? {
                WasmExecutionEvent::Stdout(chunk) => {
                    append_wasm_captured_output(&mut stdout, &chunk, "stdout")?;
                }
                WasmExecutionEvent::Stderr(chunk) => {
                    append_wasm_captured_output(&mut stderr, &chunk, "stderr")?;
                }
                WasmExecutionEvent::SyncRpcRequest(request) => {
                    if self.handle_wait_sync_rpc_request(&request, &mut stdout, &mut stderr)? {
                        continue;
                    }
                    return Err(WasmExecutionError::RpcResponse(format!(
                        "unexpected guest WebAssembly sync RPC request {} while waiting",
                        request.method
                    )));
                }
                WasmExecutionEvent::HostCall { .. } => return Err(no_native_host_consumer()),
                WasmExecutionEvent::SignalState { .. } => {}
                WasmExecutionEvent::Exited(exit_code) => {
                    return Ok(WasmExecutionResult {
                        execution_id: self.execution_id,
                        exit_code,
                        stdout,
                        stderr,
                    });
                }
            }
        }
    }

    /// Wait for one meaningful WASM event without a recurring adapter poll.
    /// A configured execution deadline becomes one deadline-capped wait; an
    /// execution without a deadline blocks directly on the event receiver.
    fn wait_event_blocking(&mut self) -> Result<WasmExecutionEvent, WasmExecutionError> {
        loop {
            if let Some(event) = self.pending_events.pop_front() {
                return Ok(event);
            }
            if let Some(event) = self.internal_sync_rpc.pending_events.pop_front() {
                self.enqueue_wasm_event(event)?;
                continue;
            }
            if let Some(event) = self.timeout_event_if_expired()? {
                return Ok(event);
            }

            let event = if let Some(limit) = self.execution_timeout {
                let remaining = limit.saturating_sub(self.execution_started_at.elapsed());
                if remaining.is_zero() {
                    continue;
                }
                let Some(event) = self
                    .inner
                    .poll_event_blocking(remaining)
                    .map_err(map_javascript_error)?
                else {
                    // The single deadline-aware wait expired. The next turn
                    // materializes the typed timeout events exactly once.
                    continue;
                };
                event
            } else {
                self.inner
                    .next_event_blocking()
                    .map_err(map_javascript_error)?
            };

            if let JavascriptExecutionEvent::SyncRpcRequest(request) = &event {
                if self.handle_internal_sync_rpc(request)? {
                    continue;
                }
            }
            self.enqueue_javascript_event(event)?;
        }
    }

    fn deadline_capped_timeout(&self, timeout: Duration) -> Duration {
        self.execution_timeout
            .map(|limit| {
                let elapsed = self.execution_started_at.elapsed();
                if elapsed >= limit {
                    Duration::ZERO
                } else {
                    timeout.min(limit.saturating_sub(elapsed))
                }
            })
            .unwrap_or(timeout)
    }

    fn timeout_event_if_expired(
        &mut self,
    ) -> Result<Option<WasmExecutionEvent>, WasmExecutionError> {
        if self.timeout_reported {
            return Ok(None);
        }
        let Some(limit) = self.execution_timeout else {
            return Ok(None);
        };
        let elapsed = self.execution_started_at.elapsed();
        // Observe elapsed usage on real event boundaries. The terminal path
        // below records the exact configured capacity when the one-shot
        // deadline wait expires.
        if let Some(gauge) = &self.wall_clock_gauge {
            gauge.observe_depth(duration_millis_saturating_usize(elapsed));
        }
        if elapsed < limit {
            return Ok(None);
        }

        self.inner.terminate().map_err(map_javascript_error)?;
        self.timeout_reported = true;
        let capacity = duration_millis_saturating_usize(limit);
        warn_limit_exhausted(TrackedLimit::WasmWallClockMs, capacity, capacity);
        self.enqueue_wasm_event(WasmExecutionEvent::Stderr(
            b"WebAssembly wall-clock limit exceeded\n".to_vec(),
        ))?;
        self.enqueue_wasm_event(WasmExecutionEvent::Exited(WASM_TIMEOUT_EXIT_CODE))?;
        Ok(self.pending_events.pop_front())
    }

    fn handle_internal_sync_rpc(
        &mut self,
        request: &HostRpcRequest,
    ) -> Result<bool, WasmExecutionError> {
        handle_internal_wasm_sync_rpc_request(&mut self.inner, &mut self.internal_sync_rpc, request)
    }

    fn enqueue_javascript_event(
        &mut self,
        event: JavascriptExecutionEvent,
    ) -> Result<(), WasmExecutionError> {
        match event {
            JavascriptExecutionEvent::Stdout(chunk) => {
                self.enqueue_stream_chunk(StreamChannel::Stdout, chunk)?
            }
            JavascriptExecutionEvent::Stderr(chunk) => {
                if self.max_stack_bytes.is_some() && is_v8_stack_overflow_stderr(&chunk) {
                    let pending = self.pending_v8_stack_overflow.get_or_insert_with(Vec::new);
                    ensure_wasm_output_capacity(
                        pending.len(),
                        chunk.len(),
                        "pending stack-overflow stderr",
                    )?;
                    pending.extend_from_slice(&chunk);
                } else {
                    self.enqueue_stream_chunk(StreamChannel::Stderr, chunk)?
                }
            }
            JavascriptExecutionEvent::SyncRpcRequest(request) => {
                self.pending_events
                    .push_back(WasmExecutionEvent::SyncRpcRequest(request))?;
            }
            JavascriptExecutionEvent::SignalState {
                signal,
                registration,
            } => {
                self.pending_events
                    .push_back(WasmExecutionEvent::SignalState {
                        signal,
                        registration,
                    })?;
            }
            JavascriptExecutionEvent::Exited(code) => {
                if let Some(original) = self.pending_v8_stack_overflow.take() {
                    let chunk = if code != 0 {
                        configured_wasm_stack_limit_error(
                            self.max_stack_bytes
                                .expect("stack-overflow buffering requires a configured limit"),
                        )
                        .into_bytes()
                    } else {
                        original
                    };
                    self.enqueue_stream_chunk(StreamChannel::Stderr, chunk)?;
                }
                self.flush_stream_buffers()?;
                self.pending_events
                    .push_back(WasmExecutionEvent::Exited(code))?;
            }
        }
        Ok(())
    }

    fn enqueue_wasm_event(&mut self, event: WasmExecutionEvent) -> Result<(), WasmExecutionError> {
        match event {
            WasmExecutionEvent::Stdout(chunk) => {
                self.enqueue_stream_chunk(StreamChannel::Stdout, chunk)?
            }
            WasmExecutionEvent::Stderr(chunk) => {
                self.enqueue_stream_chunk(StreamChannel::Stderr, chunk)?
            }
            WasmExecutionEvent::Exited(code) => {
                self.flush_stream_buffers()?;
                self.pending_events
                    .push_back(WasmExecutionEvent::Exited(code))?;
            }
            other => self.pending_events.push_back(other)?,
        }
        Ok(())
    }

    fn enqueue_stream_chunk(
        &mut self,
        channel: StreamChannel,
        chunk: Vec<u8>,
    ) -> Result<(), WasmExecutionError> {
        let buffer = match channel {
            StreamChannel::Stdout => &mut self.stdout_stream_buffer,
            StreamChannel::Stderr => &mut self.stderr_stream_buffer,
        };
        let stream = match channel {
            StreamChannel::Stdout => "stdout",
            StreamChannel::Stderr => "stderr",
        };
        ensure_wasm_output_capacity(buffer.len(), chunk.len(), stream)?;
        buffer.extend_from_slice(&chunk);

        let mut pending_stream_chunk = Vec::new();
        while let Some(newline_index) = buffer.iter().position(|byte| *byte == b'\n') {
            let line = buffer.drain(..=newline_index).collect::<Vec<_>>();
            if let Some(signal_state) = parse_wasm_signal_state_line(&line)? {
                if !pending_stream_chunk.is_empty() {
                    self.pending_events.push_back(match channel {
                        StreamChannel::Stdout => {
                            WasmExecutionEvent::Stdout(std::mem::take(&mut pending_stream_chunk))
                        }
                        StreamChannel::Stderr => {
                            WasmExecutionEvent::Stderr(std::mem::take(&mut pending_stream_chunk))
                        }
                    })?;
                }
                self.pending_events.push_back(signal_state)?;
                continue;
            }
            pending_stream_chunk.extend_from_slice(&line);
        }
        if !pending_stream_chunk.is_empty() {
            self.pending_events.push_back(match channel {
                StreamChannel::Stdout => WasmExecutionEvent::Stdout(pending_stream_chunk),
                StreamChannel::Stderr => WasmExecutionEvent::Stderr(pending_stream_chunk),
            })?;
        }

        Ok(())
    }

    fn flush_stream_buffers(&mut self) -> Result<(), WasmExecutionError> {
        if !self.stdout_stream_buffer.is_empty() {
            self.pending_events
                .push_back(WasmExecutionEvent::Stdout(std::mem::take(
                    &mut self.stdout_stream_buffer,
                )))?;
        }
        if !self.stderr_stream_buffer.is_empty() {
            self.pending_events
                .push_back(WasmExecutionEvent::Stderr(std::mem::take(
                    &mut self.stderr_stream_buffer,
                )))?;
        }
        Ok(())
    }

    fn handle_wait_sync_rpc_request(
        &mut self,
        request: &HostRpcRequest,
        stdout: &mut Vec<u8>,
        stderr: &mut Vec<u8>,
    ) -> Result<bool, WasmExecutionError> {
        if self
            .inner
            .handle_kernel_stdin_sync_rpc(request)
            .map_err(map_javascript_error)?
        {
            return Ok(true);
        }

        // `wait()` is the standalone compatibility helper and has no kernel
        // process table to own signal dispositions. Keep its historical
        // best-effort behavior for direct engine consumers, but never take
        // this path while the sidecar is driving events: pollers receive the
        // request and reply only after committing it to the kernel.
        if request.method == "process.signal_state" {
            self.respond_sync_rpc_success(request.id, Value::Null)?;
            return Ok(true);
        }
        if request.method == "process.signal_mask" {
            self.respond_sync_rpc_success(request.id, json!({ "signals": [] }))?;
            return Ok(true);
        }
        if request.method == "process.signal_mask_scope_begin" {
            self.respond_sync_rpc_success(request.id, json!(1))?;
            return Ok(true);
        }
        if matches!(
            request.method.as_str(),
            "process.signal_mask_scope_end" | "process.signal_end"
        ) {
            self.respond_sync_rpc_success(request.id, Value::Null)?;
            return Ok(true);
        }
        if matches!(
            request.method.as_str(),
            "process.take_signal" | "process.signal_begin"
        ) {
            self.respond_sync_rpc_success(request.id, Value::Null)?;
            return Ok(true);
        }
        if request.method == "process.wasm_sync_rpc" {
            match request.args.first().and_then(Value::as_str) {
                Some("process.signal_mask") => {
                    self.respond_sync_rpc_success(request.id, json!({ "signals": [] }))?;
                    return Ok(true);
                }
                Some("process.signal_mask_scope_begin") => {
                    self.respond_sync_rpc_success(request.id, json!(1))?;
                    return Ok(true);
                }
                Some("process.signal_mask_scope_end" | "process.signal_end") => {
                    self.respond_sync_rpc_success(request.id, Value::Null)?;
                    return Ok(true);
                }
                Some("process.take_signal" | "process.signal_begin") => {
                    self.respond_sync_rpc_success(request.id, Value::Null)?;
                    return Ok(true);
                }
                _ => {}
            }
        }

        if request.method != "__kernel_stdio_write" {
            return Ok(false);
        }

        let Some(descriptor) = request.args.first().and_then(Value::as_u64) else {
            return Err(WasmExecutionError::RpcResponse(String::from(
                "missing __kernel_stdio_write descriptor",
            )));
        };
        let bytes = decode_wasm_bytes_arg(
            request.args.get(1),
            "__kernel_stdio_write payload bytes",
            WASM_CAPTURED_OUTPUT_LIMIT_BYTES,
        )?;

        match descriptor {
            1 => append_wasm_captured_output(stdout, &bytes, "stdout")?,
            2 => append_wasm_captured_output(stderr, &bytes, "stderr")?,
            other => {
                return Err(WasmExecutionError::RpcResponse(format!(
                    "unsupported __kernel_stdio_write descriptor {other}",
                )));
            }
        }

        self.respond_sync_rpc_success(request.id, json!(bytes.len()))?;
        Ok(true)
    }
}

impl ExecutionBackend for V8WasmExecution {
    fn kind(&self) -> ExecutionBackendKind {
        ExecutionBackendKind::WebAssembly
    }

    fn synchronous_fd_write_policy(&self) -> SynchronousFdWritePolicy {
        SynchronousFdWritePolicy::NonblockingRetry
    }

    fn descendant_wait_ownership(&self) -> DescendantWaitOwnership {
        DescendantWaitOwnership::Guest
    }

    fn descendant_output_ownership(&self) -> DescendantOutputOwnership {
        DescendantOutputOwnership::GuestDescriptors
    }

    fn native_process_id(&self) -> Option<u32> {
        V8WasmExecution::native_process_id(self)
    }

    fn wake_handle(&self, identity: ExecutionWakeIdentity) -> Option<ExecutionWakeHandle> {
        self.inner.wake_handle(identity)
    }

    fn is_prepared_for_start(&self) -> bool {
        V8WasmExecution::is_prepared_for_start(self)
    }

    fn start_prepared(&mut self) -> Result<(), HostServiceError> {
        V8WasmExecution::start_prepared(self).map_err(|error| {
            HostServiceError::new("ERR_AGENTOS_EXECUTION_START", error.to_string())
        })
    }

    fn begin_shutdown(
        &mut self,
        reason: ShutdownReason,
    ) -> Result<ShutdownOutcome, HostServiceError> {
        if let ShutdownReason::Signal(signal) = reason {
            if let Some(process_id) = self.native_process_id() {
                return Ok(ShutdownOutcome::ForwardSignal { process_id, signal });
            }
            self.terminate().map_err(|error| {
                HostServiceError::new("ERR_AGENTOS_EXECUTION_SHUTDOWN", error.to_string())
            })?;
            return Ok(ShutdownOutcome::Exited(ExecutionExit::Signaled {
                signal,
                core_dumped: false,
            }));
        }
        self.terminate().map_err(|error| {
            HostServiceError::new("ERR_AGENTOS_EXECUTION_SHUTDOWN", error.to_string())
        })?;
        Ok(if reason == ShutdownReason::RuntimeFault {
            ShutdownOutcome::Exited(ExecutionExit::Exited(1))
        } else {
            ShutdownOutcome::AwaitExit
        })
    }

    fn set_paused(&self, paused: bool) -> Result<(), HostServiceError> {
        let result = if paused {
            V8WasmExecution::pause(self)
        } else {
            V8WasmExecution::resume(self)
        };
        result.map_err(|error| {
            HostServiceError::new("ERR_AGENTOS_EXECUTION_CONTROL", error.to_string())
        })
    }

    fn write_stdin(&mut self, _bytes: &[u8]) -> Result<(), HostServiceError> {
        // Sidecar-managed compatibility WASM reads fd 0 from the kernel pipe.
        Ok(())
    }

    fn close_stdin(&mut self) -> Result<(), HostServiceError> {
        V8WasmExecution::close_stdin(self).map_err(|error| {
            HostServiceError::new("ERR_AGENTOS_EXECUTION_STDIN", error.to_string())
        })
    }

    fn deliver_signal_checkpoint(
        &self,
        identity: ExecutionWakeIdentity,
        signal: i32,
        delivery_token: u64,
        flags: u32,
        thread_id: u32,
    ) -> Result<SignalCheckpointOutcome, HostServiceError> {
        let Some(wake) = self.wake_handle(identity) else {
            return Ok(if let Some(process_id) = self.native_process_id() {
                SignalCheckpointOutcome::ForwardToProcess { process_id }
            } else {
                SignalCheckpointOutcome::Unsupported
            });
        };
        self.signal_checkpoints.publish(
            identity,
            PublishedSignalCheckpoint {
                signal,
                delivery_token,
                flags,
                thread_id,
            },
        )?;
        if let Err(error) = wake.publish_signal(signal, delivery_token) {
            self.signal_checkpoints.discard(identity, delivery_token)?;
            return Err(HostServiceError::new(error.code(), error.to_string()));
        }
        Ok(SignalCheckpointOutcome::Published)
    }

    fn take_signal_checkpoint(
        &self,
        identity: ExecutionWakeIdentity,
    ) -> Result<Option<PublishedSignalCheckpoint>, HostServiceError> {
        self.signal_checkpoints.take(identity)
    }

    fn discard_signal_checkpoints(
        &self,
        identity: ExecutionWakeIdentity,
    ) -> Result<(), HostServiceError> {
        self.signal_checkpoints.discard_identity(identity)
    }
}

impl WasmExecution {
    pub fn standalone_backend(&self) -> StandaloneWasmBackend {
        match &self.backend {
            WasmExecutionBackend::V8(_) => StandaloneWasmBackend::V8,
            WasmExecutionBackend::Wasmtime(execution) if execution.is_threaded() => {
                StandaloneWasmBackend::WasmtimeThreads
            }
            WasmExecutionBackend::Wasmtime(_) => StandaloneWasmBackend::Wasmtime,
        }
    }

    pub fn sync_rpc_responder(&self) -> Option<JavascriptSyncRpcResponder> {
        match &self.backend {
            WasmExecutionBackend::V8(execution) => Some(execution.sync_rpc_responder()),
            WasmExecutionBackend::Wasmtime(_) => None,
        }
    }

    pub fn execution_id(&self) -> &str {
        match &self.backend {
            WasmExecutionBackend::V8(execution) => execution.execution_id(),
            WasmExecutionBackend::Wasmtime(execution) => execution.execution_id(),
        }
    }

    pub fn native_process_id(&self) -> Option<u32> {
        match &self.backend {
            WasmExecutionBackend::V8(execution) => execution.native_process_id(),
            WasmExecutionBackend::Wasmtime(_) => None,
        }
    }

    pub fn v8_session_handle(&self) -> Option<V8SessionHandle> {
        match &self.backend {
            WasmExecutionBackend::V8(execution) => Some(execution.v8_session_handle()),
            WasmExecutionBackend::Wasmtime(_) => None,
        }
    }

    pub fn start_prepared(&mut self) -> Result<(), WasmExecutionError> {
        match &mut self.backend {
            WasmExecutionBackend::V8(execution) => execution.start_prepared(),
            WasmExecutionBackend::Wasmtime(execution) => execution.start_prepared(),
        }
    }

    #[doc(hidden)]
    pub fn is_prepared_for_start(&self) -> bool {
        match &self.backend {
            WasmExecutionBackend::V8(execution) => execution.is_prepared_for_start(),
            WasmExecutionBackend::Wasmtime(execution) => execution.is_prepared_for_start(),
        }
    }

    pub fn write_stdin(&mut self, chunk: &[u8]) -> Result<(), WasmExecutionError> {
        match &mut self.backend {
            WasmExecutionBackend::V8(execution) => execution.write_stdin(chunk),
            WasmExecutionBackend::Wasmtime(_) => Ok(()),
        }
    }

    pub fn write_stdin_kernel_only(&mut self, chunk: &[u8]) -> Result<(), WasmExecutionError> {
        match &mut self.backend {
            WasmExecutionBackend::V8(execution) => execution.write_stdin_kernel_only(chunk),
            WasmExecutionBackend::Wasmtime(_) => Ok(()),
        }
    }

    pub fn close_stdin(&mut self) -> Result<(), WasmExecutionError> {
        match &mut self.backend {
            WasmExecutionBackend::V8(execution) => execution.close_stdin(),
            WasmExecutionBackend::Wasmtime(_) => Ok(()),
        }
    }

    pub fn send_stream_event(
        &self,
        event_type: &str,
        payload: Value,
    ) -> Result<(), WasmExecutionError> {
        match &self.backend {
            WasmExecutionBackend::V8(execution) => execution.send_stream_event(event_type, payload),
            WasmExecutionBackend::Wasmtime(_) => Err(wasmtime_adapter_only_error(
                "ENOTSUP",
                "native Wasmtime executions do not accept V8 stream events",
            )),
        }
    }

    pub fn terminate(&self) -> Result<(), WasmExecutionError> {
        match &self.backend {
            WasmExecutionBackend::V8(execution) => execution.terminate(),
            WasmExecutionBackend::Wasmtime(execution) => {
                execution.terminate();
                Ok(())
            }
        }
    }

    pub fn pause(&self) -> Result<(), WasmExecutionError> {
        match &self.backend {
            WasmExecutionBackend::V8(execution) => execution.pause(),
            WasmExecutionBackend::Wasmtime(execution) => {
                execution.set_paused(true);
                Ok(())
            }
        }
    }

    pub fn resume(&self) -> Result<(), WasmExecutionError> {
        match &self.backend {
            WasmExecutionBackend::V8(execution) => execution.resume(),
            WasmExecutionBackend::Wasmtime(execution) => {
                execution.set_paused(false);
                Ok(())
            }
        }
    }

    pub fn respond_sync_rpc_success(
        &mut self,
        id: u64,
        result: Value,
    ) -> Result<(), WasmExecutionError> {
        match &mut self.backend {
            WasmExecutionBackend::V8(execution) => execution.respond_sync_rpc_success(id, result),
            WasmExecutionBackend::Wasmtime(_) => Err(no_native_sync_rpc()),
        }
    }

    pub fn claim_sync_rpc_response(&mut self, id: u64) -> Result<bool, WasmExecutionError> {
        match &mut self.backend {
            WasmExecutionBackend::V8(execution) => execution.claim_sync_rpc_response(id),
            WasmExecutionBackend::Wasmtime(_) => Err(no_native_sync_rpc()),
        }
    }

    pub fn respond_claimed_sync_rpc_success(
        &mut self,
        id: u64,
        result: Value,
    ) -> Result<(), WasmExecutionError> {
        match &mut self.backend {
            WasmExecutionBackend::V8(execution) => {
                execution.respond_claimed_sync_rpc_success(id, result)
            }
            WasmExecutionBackend::Wasmtime(_) => Err(no_native_sync_rpc()),
        }
    }

    pub fn respond_sync_rpc_raw_success(
        &mut self,
        id: u64,
        payload: Vec<u8>,
    ) -> Result<(), WasmExecutionError> {
        match &mut self.backend {
            WasmExecutionBackend::V8(execution) => {
                execution.respond_sync_rpc_raw_success(id, payload)
            }
            WasmExecutionBackend::Wasmtime(_) => Err(no_native_sync_rpc()),
        }
    }

    pub fn respond_sync_rpc_error(
        &mut self,
        id: u64,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<(), WasmExecutionError> {
        match &mut self.backend {
            WasmExecutionBackend::V8(execution) => {
                execution.respond_sync_rpc_error(id, code, message)
            }
            WasmExecutionBackend::Wasmtime(_) => Err(no_native_sync_rpc()),
        }
    }

    pub fn respond_claimed_sync_rpc_error(
        &mut self,
        id: u64,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<(), WasmExecutionError> {
        match &mut self.backend {
            WasmExecutionBackend::V8(execution) => {
                execution.respond_claimed_sync_rpc_error(id, code, message)
            }
            WasmExecutionBackend::Wasmtime(_) => Err(no_native_sync_rpc()),
        }
    }

    pub async fn poll_event(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<WasmExecutionEvent>, WasmExecutionError> {
        match &mut self.backend {
            WasmExecutionBackend::V8(execution) => execution.poll_event(timeout).await,
            WasmExecutionBackend::Wasmtime(execution) => execution.poll_event(timeout).await,
        }
    }

    pub fn try_poll_event(&mut self) -> Result<Option<WasmExecutionEvent>, WasmExecutionError> {
        match &mut self.backend {
            WasmExecutionBackend::V8(execution) => execution.try_poll_event(),
            WasmExecutionBackend::Wasmtime(execution) => execution.try_poll_event(),
        }
    }

    pub fn poll_event_blocking(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<WasmExecutionEvent>, WasmExecutionError> {
        match &mut self.backend {
            WasmExecutionBackend::V8(execution) => execution.poll_event_blocking(timeout),
            WasmExecutionBackend::Wasmtime(execution) => execution.poll_event_blocking(timeout),
        }
    }

    pub fn wait(self) -> Result<WasmExecutionResult, WasmExecutionError> {
        match self.backend {
            WasmExecutionBackend::V8(execution) => execution.wait(),
            WasmExecutionBackend::Wasmtime(execution) => {
                let mut stdout = Vec::new();
                let mut stderr = Vec::new();
                loop {
                    match execution.next_event_blocking()? {
                        WasmExecutionEvent::Stdout(chunk) => stdout.extend_from_slice(&chunk),
                        WasmExecutionEvent::Stderr(chunk) => stderr.extend_from_slice(&chunk),
                        WasmExecutionEvent::Exited(exit_code) => {
                            return Ok(WasmExecutionResult {
                                execution_id: execution.execution_id().to_owned(),
                                exit_code,
                                stdout,
                                stderr,
                            });
                        }
                        WasmExecutionEvent::SyncRpcRequest(_) => {
                            return Err(no_native_sync_rpc());
                        }
                        WasmExecutionEvent::HostCall { .. } => {
                            return Err(no_native_host_consumer());
                        }
                        WasmExecutionEvent::SignalState { .. } => {}
                    }
                }
            }
        }
    }
}

fn no_native_sync_rpc() -> WasmExecutionError {
    wasmtime_adapter_only_error(
        "ENOTSUP",
        "native Wasmtime imports use direct typed host waiters, not V8 sync RPC",
    )
}

fn no_native_host_consumer() -> WasmExecutionError {
    wasmtime_adapter_only_error(
        "ENOTCONN",
        "native Wasmtime host calls require the sidecar host-event consumer",
    )
}

fn wasmtime_adapter_only_error(code: &'static str, message: &'static str) -> WasmExecutionError {
    WasmExecutionError::Host(HostServiceError::new(code, message))
}

impl ExecutionBackend for WasmExecution {
    fn kind(&self) -> ExecutionBackendKind {
        ExecutionBackendKind::WebAssembly
    }

    fn synchronous_fd_write_policy(&self) -> SynchronousFdWritePolicy {
        match &self.backend {
            WasmExecutionBackend::V8(execution) => execution.synchronous_fd_write_policy(),
            WasmExecutionBackend::Wasmtime(_) => SynchronousFdWritePolicy::Blocking,
        }
    }

    fn descendant_wait_ownership(&self) -> DescendantWaitOwnership {
        DescendantWaitOwnership::Guest
    }

    fn descendant_output_ownership(&self) -> DescendantOutputOwnership {
        DescendantOutputOwnership::GuestDescriptors
    }

    fn native_process_id(&self) -> Option<u32> {
        WasmExecution::native_process_id(self)
    }

    fn wake_handle(&self, identity: ExecutionWakeIdentity) -> Option<ExecutionWakeHandle> {
        match &self.backend {
            WasmExecutionBackend::V8(execution) => execution.wake_handle(identity),
            WasmExecutionBackend::Wasmtime(_) => None,
        }
    }

    fn configure_host_services(&mut self, host: ProcessHostCapabilitySet) {
        if let WasmExecutionBackend::Wasmtime(execution) = &self.backend {
            execution.configure_host_services(host);
        }
    }

    fn is_prepared_for_start(&self) -> bool {
        WasmExecution::is_prepared_for_start(self)
    }

    fn start_prepared(&mut self) -> Result<(), HostServiceError> {
        WasmExecution::start_prepared(self).map_err(wasm_execution_host_error)
    }

    fn begin_shutdown(
        &mut self,
        reason: ShutdownReason,
    ) -> Result<ShutdownOutcome, HostServiceError> {
        match &mut self.backend {
            WasmExecutionBackend::V8(execution) => execution.begin_shutdown(reason),
            WasmExecutionBackend::Wasmtime(execution) => {
                execution.terminate();
                Ok(match reason {
                    ShutdownReason::Signal(signal) => {
                        ShutdownOutcome::Exited(ExecutionExit::Signaled {
                            signal,
                            core_dumped: false,
                        })
                    }
                    ShutdownReason::RuntimeFault => {
                        ShutdownOutcome::Exited(ExecutionExit::Exited(1))
                    }
                    _ => ShutdownOutcome::AwaitExit,
                })
            }
        }
    }

    fn set_paused(&self, paused: bool) -> Result<(), HostServiceError> {
        match &self.backend {
            WasmExecutionBackend::V8(execution) => execution.set_paused(paused),
            WasmExecutionBackend::Wasmtime(execution) => {
                execution.set_paused(paused);
                Ok(())
            }
        }
    }

    fn write_stdin(&mut self, _bytes: &[u8]) -> Result<(), HostServiceError> {
        Ok(())
    }

    fn close_stdin(&mut self) -> Result<(), HostServiceError> {
        WasmExecution::close_stdin(self).map_err(wasm_execution_host_error)
    }

    fn deliver_signal_checkpoint(
        &self,
        identity: ExecutionWakeIdentity,
        signal: i32,
        delivery_token: u64,
        flags: u32,
        thread_id: u32,
    ) -> Result<SignalCheckpointOutcome, HostServiceError> {
        match &self.backend {
            WasmExecutionBackend::V8(execution) => execution.deliver_signal_checkpoint(
                identity,
                signal,
                delivery_token,
                flags,
                thread_id,
            ),
            WasmExecutionBackend::Wasmtime(execution) => {
                execution.deliver_signal_checkpoint(
                    identity,
                    signal,
                    delivery_token,
                    flags,
                    thread_id,
                )?;
                Ok(SignalCheckpointOutcome::Published)
            }
        }
    }

    fn take_signal_checkpoint(
        &self,
        identity: ExecutionWakeIdentity,
    ) -> Result<Option<PublishedSignalCheckpoint>, HostServiceError> {
        match &self.backend {
            WasmExecutionBackend::V8(execution) => execution.take_signal_checkpoint(identity),
            WasmExecutionBackend::Wasmtime(execution) => execution.take_signal_checkpoint(identity),
        }
    }

    fn take_signal_checkpoint_for_thread(
        &self,
        identity: ExecutionWakeIdentity,
        thread_id: u32,
    ) -> Result<Option<PublishedSignalCheckpoint>, HostServiceError> {
        match &self.backend {
            WasmExecutionBackend::V8(execution) if thread_id == 0 => {
                execution.take_signal_checkpoint(identity)
            }
            WasmExecutionBackend::V8(_) => Ok(None),
            WasmExecutionBackend::Wasmtime(execution) => {
                execution.take_signal_checkpoint_for_thread(identity, thread_id)
            }
        }
    }

    fn discard_signal_checkpoints(
        &self,
        identity: ExecutionWakeIdentity,
    ) -> Result<(), HostServiceError> {
        match &self.backend {
            WasmExecutionBackend::V8(execution) => execution.discard_signal_checkpoints(identity),
            WasmExecutionBackend::Wasmtime(execution) => {
                execution.discard_signal_checkpoints(identity)
            }
        }
    }
}

fn wasm_execution_host_error(error: WasmExecutionError) -> HostServiceError {
    match error {
        WasmExecutionError::Host(error) => error,
        error => HostServiceError::new("ERR_AGENTOS_WASM_EXECUTION", error.to_string()),
    }
}

#[derive(Clone, Copy)]
enum StreamChannel {
    Stdout,
    Stderr,
}

#[derive(Debug)]
pub struct WasmExecutionEngine {
    runtime: Option<RuntimeContext>,
    next_context_id: usize,
    next_execution_id: usize,
    contexts: BTreeMap<String, WasmContext>,
    import_caches: BTreeMap<String, NodeImportCache>,
    javascript_context_ids: BTreeMap<String, String>,
    javascript_engine: JavascriptExecutionEngine,
    event_notify: Option<Arc<Notify>>,
}

impl Default for WasmExecutionEngine {
    fn default() -> Self {
        let runtime = default_wasm_test_runtime_context();
        let javascript_engine = runtime
            .as_ref()
            .map_or_else(JavascriptExecutionEngine::default, |runtime| {
                JavascriptExecutionEngine::new(runtime.clone())
            });
        Self {
            runtime,
            next_context_id: 0,
            next_execution_id: 0,
            contexts: BTreeMap::new(),
            import_caches: BTreeMap::new(),
            javascript_context_ids: BTreeMap::new(),
            javascript_engine,
            event_notify: None,
        }
    }
}

#[cfg(test)]
fn default_wasm_test_runtime_context() -> Option<RuntimeContext> {
    agentos_runtime::SidecarRuntime::process(&agentos_runtime::RuntimeConfig::default())
        .ok()
        .map(agentos_runtime::SidecarRuntime::context)
}

#[cfg(not(test))]
fn default_wasm_test_runtime_context() -> Option<RuntimeContext> {
    None
}

impl WasmExecutionEngine {
    /// Process-wide Wasmtime cache/profile/RSS metrics for operator telemetry.
    pub fn wasmtime_metrics(&self) -> Result<wasmtime::WasmtimeMetricsSnapshot, HostServiceError> {
        wasmtime::WasmtimeExecutionEngine::metrics()
    }

    pub fn new(runtime: RuntimeContext) -> Self {
        Self {
            runtime: Some(runtime.clone()),
            next_context_id: 0,
            next_execution_id: 0,
            contexts: BTreeMap::new(),
            import_caches: BTreeMap::new(),
            javascript_context_ids: BTreeMap::new(),
            javascript_engine: JavascriptExecutionEngine::new(runtime),
            event_notify: None,
        }
    }

    pub fn set_runtime_context(&mut self, runtime: RuntimeContext) {
        self.javascript_engine.set_runtime_context(runtime.clone());
        self.runtime = Some(runtime);
    }

    fn runtime_context(&self) -> Result<&RuntimeContext, WasmExecutionError> {
        self.runtime.as_ref().ok_or_else(|| {
            WasmExecutionError::Spawn(std::io::Error::other(
                "ERR_AGENTOS_RUNTIME_NOT_INJECTED: WasmExecutionEngine requires a process RuntimeContext; construct it with WasmExecutionEngine::new(runtime)",
            ))
        })
    }

    pub fn set_event_notify(&mut self, notify: Option<Arc<Notify>>) {
        self.javascript_engine.set_event_notify(notify.clone());
        self.event_notify = notify;
    }

    pub fn create_context(&mut self, request: CreateWasmContextRequest) -> WasmContext {
        self.next_context_id += 1;
        self.import_caches.entry(request.vm_id.clone()).or_default();
        let javascript_context =
            self.javascript_engine
                .create_context(CreateJavascriptContextRequest {
                    vm_id: request.vm_id.clone(),
                    bootstrap_module: None,
                    compile_cache_root: None,
                });

        let context = WasmContext {
            context_id: format!("wasm-ctx-{}", self.next_context_id),
            vm_id: request.vm_id,
            module_path: request.module_path,
        };
        self.javascript_context_ids
            .insert(context.context_id.clone(), javascript_context.context_id);
        self.contexts
            .insert(context.context_id.clone(), context.clone());
        context
    }

    /// Dispose the WASM context and the private JavaScript bridge context that
    /// belongs to it. A started execution has already cloned all required
    /// runtime state and remains valid after this returns.
    pub fn dispose_context(&mut self, context_id: &str) -> bool {
        let removed = self.contexts.remove(context_id).is_some();
        if let Some(javascript_context_id) = self.javascript_context_ids.remove(context_id) {
            self.javascript_engine
                .dispose_context(&javascript_context_id);
        }
        removed
    }

    #[doc(hidden)]
    pub fn context_count_for_test(&self) -> usize {
        self.contexts.len()
    }

    #[doc(hidden)]
    pub fn javascript_context_count_for_test(&self) -> usize {
        self.javascript_engine.context_count_for_test()
    }

    pub fn start_execution(
        &mut self,
        request: StartWasmExecutionRequest,
    ) -> Result<WasmExecution, WasmExecutionError> {
        let runtime = self.runtime_context()?.clone();
        self.create_execution_with_runtime(request, runtime, false)
    }

    pub fn start_execution_for_backend(
        &mut self,
        request: StartWasmExecutionRequest,
        backend: StandaloneWasmBackend,
    ) -> Result<WasmExecution, WasmExecutionError> {
        let runtime = self.runtime_context()?.clone();
        self.create_execution_for_backend(request, runtime, false, backend)
    }

    pub fn prepare_execution(
        &mut self,
        request: StartWasmExecutionRequest,
    ) -> Result<WasmExecution, WasmExecutionError> {
        let runtime = self.runtime_context()?.clone();
        self.create_execution_with_runtime(request, runtime, true)
    }

    pub fn prepare_execution_for_backend(
        &mut self,
        request: StartWasmExecutionRequest,
        backend: StandaloneWasmBackend,
    ) -> Result<WasmExecution, WasmExecutionError> {
        let runtime = self.runtime_context()?.clone();
        self.create_execution_for_backend(request, runtime, true, backend)
    }

    pub fn prepare_execution_with_runtime_for_backend(
        &mut self,
        request: StartWasmExecutionRequest,
        runtime: RuntimeContext,
        backend: StandaloneWasmBackend,
    ) -> Result<WasmExecution, WasmExecutionError> {
        self.create_execution_for_backend(request, runtime, true, backend)
    }

    pub fn start_execution_with_runtime(
        &mut self,
        request: StartWasmExecutionRequest,
        runtime: RuntimeContext,
    ) -> Result<WasmExecution, WasmExecutionError> {
        self.create_execution_with_runtime(request, runtime, false)
    }

    pub fn start_execution_with_runtime_for_backend(
        &mut self,
        request: StartWasmExecutionRequest,
        runtime: RuntimeContext,
        backend: StandaloneWasmBackend,
    ) -> Result<WasmExecution, WasmExecutionError> {
        self.create_execution_for_backend(request, runtime, false, backend)
    }

    fn create_execution_for_backend(
        &mut self,
        request: StartWasmExecutionRequest,
        runtime: RuntimeContext,
        defer_execute: bool,
        backend: StandaloneWasmBackend,
    ) -> Result<WasmExecution, WasmExecutionError> {
        match backend {
            StandaloneWasmBackend::V8 => {
                self.create_execution_with_runtime(request, runtime, defer_execute)
            }
            StandaloneWasmBackend::Wasmtime => {
                self.create_wasmtime_execution(request, runtime, defer_execute, false)
            }
            StandaloneWasmBackend::WasmtimeThreads => {
                self.create_wasmtime_execution(request, runtime, defer_execute, true)
            }
        }
    }

    fn create_wasmtime_execution(
        &mut self,
        request: StartWasmExecutionRequest,
        runtime: RuntimeContext,
        defer_execute: bool,
        threaded: bool,
    ) -> Result<WasmExecution, WasmExecutionError> {
        let context = self
            .contexts
            .get(&request.context_id)
            .cloned()
            .ok_or_else(|| WasmExecutionError::MissingContext(request.context_id.clone()))?;
        if context.vm_id != request.vm_id {
            return Err(WasmExecutionError::VmMismatch {
                expected: context.vm_id,
                found: request.vm_id,
            });
        }
        let module_path = context
            .module_path
            .ok_or(WasmExecutionError::MissingModulePath)?;
        self.next_execution_id += 1;
        let execution_id = format!("exec-{}", self.next_execution_id);
        let execution = wasmtime::WasmtimeExecution::spawn(
            execution_id,
            module_path,
            request,
            runtime,
            self.event_notify.clone(),
            defer_execute,
            threaded,
        )?;
        Ok(WasmExecution {
            backend: WasmExecutionBackend::Wasmtime(execution),
        })
    }

    fn create_execution_with_runtime(
        &mut self,
        request: StartWasmExecutionRequest,
        runtime: RuntimeContext,
        defer_execute: bool,
    ) -> Result<WasmExecution, WasmExecutionError> {
        let context = self
            .contexts
            .get(&request.context_id)
            .cloned()
            .ok_or_else(|| WasmExecutionError::MissingContext(request.context_id.clone()))?;

        if context.vm_id != request.vm_id {
            return Err(WasmExecutionError::VmMismatch {
                expected: context.vm_id,
                found: request.vm_id,
            });
        }

        reject_v8_deterministic_fuel(&request)?;

        let resolved_module = resolve_wasm_module(&context, &request)?;
        verify_wasm_module_header(&resolved_module)?;
        // Enforce bounded structural parsing before the complete feature
        // validator. Oversized section counts and pathological varuints must
        // fail at AgentOS's explicit parser limits rather than being obscured
        // by a later engine/validator EOF diagnostic.
        validate_module_limits(&resolved_module, &request)?;
        validate_module_profile(&resolved_module)?;
        let prewarm_timeout = resolve_wasm_prewarm_timeout(&request)?;
        let javascript_context_id = self
            .javascript_context_ids
            .get(&context.context_id)
            .cloned()
            .ok_or_else(|| WasmExecutionError::MissingContext(context.context_id.clone()))?;
        {
            let import_cache = self.import_caches.entry(context.vm_id.clone()).or_default();
            import_cache
                .ensure_materialized_with_timeout_and_runtime(&runtime, prewarm_timeout)
                .map_err(WasmExecutionError::PrepareWarmPath)?;
        }
        let frozen_time_ms = frozen_time_ms();
        // Fail closed when a stack byte budget is configured. The V8 runner does
        // not yet expose a per-module stack lever, so accepting the value would
        // claim to enforce a policy that the runtime actually ignores.
        wasm_stack_limit_bytes(&request)?;
        let execution_timeout = resolve_wasm_wall_clock_limit(&request)?;
        let import_cache = self
            .import_caches
            .get(&context.vm_id)
            .expect("vm import cache should exist after materialization");
        let warmup_metrics = match prewarm_wasm_path(
            import_cache,
            &mut self.javascript_engine,
            &javascript_context_id,
            &resolved_module,
            &request,
            WasmPrewarmOptions {
                frozen_time_ms,
                timeout: prewarm_timeout,
                runtime: &runtime,
            },
        ) {
            Ok(metrics) => metrics,
            Err(WasmExecutionError::WarmupTimeout(_)) => None,
            Err(error) => return Err(error),
        };

        self.finish_start_execution(
            request,
            runtime,
            &context.vm_id,
            javascript_context_id,
            resolved_module,
            frozen_time_ms,
            execution_timeout,
            warmup_metrics,
            defer_execute,
        )
    }

    /// Start a WASM execution from an async sidecar dispatch path. Import-cache
    /// materialization and the optional V8 prewarm await their existing bounded
    /// workers instead of blocking a Tokio runtime worker.
    pub async fn start_execution_with_runtime_async(
        &mut self,
        request: StartWasmExecutionRequest,
        runtime: RuntimeContext,
    ) -> Result<WasmExecution, WasmExecutionError> {
        let context = self
            .contexts
            .get(&request.context_id)
            .cloned()
            .ok_or_else(|| WasmExecutionError::MissingContext(request.context_id.clone()))?;

        if context.vm_id != request.vm_id {
            return Err(WasmExecutionError::VmMismatch {
                expected: context.vm_id,
                found: request.vm_id,
            });
        }

        reject_v8_deterministic_fuel(&request)?;

        let resolved_module = resolve_wasm_module(&context, &request)?;
        verify_wasm_module_header(&resolved_module)?;
        validate_module_limits(&resolved_module, &request)?;
        validate_module_profile(&resolved_module)?;
        let prewarm_timeout = resolve_wasm_prewarm_timeout(&request)?;
        let javascript_context_id = self
            .javascript_context_ids
            .get(&context.context_id)
            .cloned()
            .ok_or_else(|| WasmExecutionError::MissingContext(context.context_id.clone()))?;
        {
            let import_cache = self.import_caches.entry(context.vm_id.clone()).or_default();
            import_cache
                .ensure_materialized_with_timeout_and_runtime_async(&runtime, prewarm_timeout)
                .await
                .map_err(WasmExecutionError::PrepareWarmPath)?;
        }
        let frozen_time_ms = frozen_time_ms();
        wasm_stack_limit_bytes(&request)?;
        let execution_timeout = resolve_wasm_wall_clock_limit(&request)?;
        let import_cache = self
            .import_caches
            .get(&context.vm_id)
            .expect("vm import cache should exist after materialization");
        let warmup_metrics = match prewarm_wasm_path_async(
            import_cache,
            &mut self.javascript_engine,
            &javascript_context_id,
            &resolved_module,
            &request,
            WasmPrewarmOptions {
                frozen_time_ms,
                timeout: prewarm_timeout,
                runtime: &runtime,
            },
        )
        .await
        {
            Ok(metrics) => metrics,
            Err(WasmExecutionError::WarmupTimeout(_)) => None,
            Err(error) => return Err(error),
        };

        self.finish_start_execution(
            request,
            runtime,
            &context.vm_id,
            javascript_context_id,
            resolved_module,
            frozen_time_ms,
            execution_timeout,
            warmup_metrics,
            false,
        )
    }

    pub async fn start_execution_with_runtime_async_for_backend(
        &mut self,
        request: StartWasmExecutionRequest,
        runtime: RuntimeContext,
        backend: StandaloneWasmBackend,
    ) -> Result<WasmExecution, WasmExecutionError> {
        match backend {
            StandaloneWasmBackend::V8 => {
                self.start_execution_with_runtime_async(request, runtime)
                    .await
            }
            StandaloneWasmBackend::Wasmtime => {
                self.create_wasmtime_execution(request, runtime, false, false)
            }
            StandaloneWasmBackend::WasmtimeThreads => {
                self.create_wasmtime_execution(request, runtime, false, true)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_start_execution(
        &mut self,
        request: StartWasmExecutionRequest,
        runtime: RuntimeContext,
        vm_id: &str,
        javascript_context_id: String,
        resolved_module: ResolvedWasmModule,
        frozen_time_ms: u128,
        execution_timeout: Option<Duration>,
        warmup_metrics: Option<Vec<u8>>,
        defer_execute: bool,
    ) -> Result<WasmExecution, WasmExecutionError> {
        let import_cache = self
            .import_caches
            .get(vm_id)
            .expect("vm import cache should exist after materialization");
        self.next_execution_id += 1;
        let execution_id = format!("exec-{}", self.next_execution_id);
        let javascript_execution = start_wasm_javascript_execution(
            &mut self.javascript_engine,
            &runtime,
            import_cache,
            &javascript_context_id,
            &resolved_module,
            &request,
            WasmJavascriptExecutionOptions {
                frozen_time_ms,
                prewarm_only: false,
                warmup_metrics: warmup_metrics.as_deref(),
                defer_execute,
            },
        )?;
        let child_pid = javascript_execution.native_process_id().unwrap_or_default();
        let sandbox_root = wasm_sandbox_root(&request.env);
        let guest_path_mappings = wasm_guest_path_mappings(&request);
        let pending_event_budget = wasm_pending_event_budget(&request.limits)?;

        Ok(WasmExecution {
            backend: WasmExecutionBackend::V8(Box::new(V8WasmExecution {
                execution_id,
                child_pid,
                inner: javascript_execution,
                execution_timeout,
                execution_started_at: Instant::now(),
                timeout_reported: false,
                // Approach-warn (~80%) before the optional WASM elapsed deadline;
                // only registered when a wall-clock limit is configured.
                wall_clock_gauge: execution_timeout.map(|limit| {
                    register_limit(
                        TrackedLimit::WasmWallClockMs,
                        duration_millis_saturating_usize(limit),
                    )
                }),
                pending_events: WasmEventQueue::new(Arc::clone(&pending_event_budget)),
                signal_checkpoints: WasmSignalCheckpointInbox::new(Arc::clone(
                    &pending_event_budget,
                )),
                stdout_stream_buffer: Vec::new(),
                stderr_stream_buffer: Vec::new(),
                max_stack_bytes: request.limits.max_stack_bytes,
                pending_v8_stack_overflow: None,
                internal_sync_rpc: WasmInternalSyncRpc {
                    module_guest_paths: wasm_guest_module_paths(
                        &resolved_module.specifier,
                        &request.env,
                    ),
                    module_host_path: resolved_module.resolved_path.clone(),
                    guest_cwd: wasm_guest_cwd(&request.env),
                    host_cwd: request.cwd.clone(),
                    sandbox_root: sandbox_root.clone(),
                    guest_path_mappings,
                    route_fs_through_sidecar: sandbox_root.is_some(),
                    next_fd: 64,
                    open_files: BTreeMap::new(),
                    pending_events: WasmEventQueue::new(pending_event_budget),
                },
            })),
        })
    }

    pub fn dispose_vm(&mut self, vm_id: &str) {
        self.contexts.retain(|_, context| context.vm_id != vm_id);
        self.javascript_context_ids
            .retain(|wasm_context_id, _| self.contexts.contains_key(wasm_context_id));
        self.import_caches.remove(vm_id);
        self.javascript_engine.dispose_vm(vm_id);
    }
}

fn wasm_pending_event_budget(
    limits: &WasmExecutionLimits,
) -> Result<Arc<WasmPendingEventBudget>, WasmExecutionError> {
    let count_limit = limits
        .pending_event_count
        .unwrap_or(DEFAULT_WASM_PENDING_EVENT_COUNT);
    let byte_limit = limits
        .pending_event_bytes
        .unwrap_or(DEFAULT_WASM_PENDING_EVENT_BYTES);
    if count_limit == 0 {
        return Err(WasmExecutionError::InvalidLimit(String::from(
            "limits.process.pendingEventCount must be greater than zero",
        )));
    }
    if byte_limit == 0 {
        return Err(WasmExecutionError::InvalidLimit(String::from(
            "limits.process.pendingEventBytes must be greater than zero",
        )));
    }
    Ok(WasmPendingEventBudget::new(count_limit, byte_limit))
}

fn map_javascript_error(error: JavascriptExecutionError) -> WasmExecutionError {
    match error {
        JavascriptExecutionError::EmptyArgv => WasmExecutionError::Spawn(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "guest WebAssembly bootstrap requires a JavaScript entrypoint",
        )),
        JavascriptExecutionError::InvalidLimit(message) => {
            WasmExecutionError::InvalidLimit(message)
        }
        JavascriptExecutionError::MissingContext(context_id) => {
            WasmExecutionError::MissingContext(context_id)
        }
        JavascriptExecutionError::VmMismatch { expected, found } => {
            WasmExecutionError::VmMismatch { expected, found }
        }
        JavascriptExecutionError::PrepareImportCache(error) => {
            WasmExecutionError::PrepareWarmPath(error)
        }
        JavascriptExecutionError::Spawn(error) => WasmExecutionError::Spawn(error),
        JavascriptExecutionError::PendingSyncRpcRequest(id) => WasmExecutionError::RpcResponse(
            format!("guest WebAssembly sync RPC request {id} is still pending"),
        ),
        JavascriptExecutionError::PendingSyncRpcLimit { limit, observed } => {
            WasmExecutionError::PendingEventLimit {
                limit_name: "limits.reactor.maxBridgeCalls",
                limit,
                observed,
            }
        }
        JavascriptExecutionError::ExpiredSyncRpcRequest(id) => WasmExecutionError::RpcResponse(
            format!("guest WebAssembly sync RPC request {id} is no longer pending"),
        ),
        JavascriptExecutionError::RpcResponse(message) => WasmExecutionError::RpcResponse(message),
        JavascriptExecutionError::BridgeSettlement(error) => {
            WasmExecutionError::RpcResponse(error.to_string())
        }
        JavascriptExecutionError::Terminate(error) => WasmExecutionError::Spawn(error),
        JavascriptExecutionError::Control(error) => WasmExecutionError::Control(error),
        JavascriptExecutionError::StdinClosed => WasmExecutionError::StdinClosed,
        JavascriptExecutionError::Stdin(error) => WasmExecutionError::Stdin(error),
        JavascriptExecutionError::OutputBufferExceeded { stream, limit } => {
            WasmExecutionError::OutputBufferExceeded { stream, limit }
        }
        JavascriptExecutionError::EventChannelClosed => WasmExecutionError::EventChannelClosed,
    }
}

fn handle_internal_wasm_sync_rpc_request(
    execution: &mut JavascriptExecution,
    internal_sync_rpc: &mut WasmInternalSyncRpc,
    request: &HostRpcRequest,
) -> Result<bool, WasmExecutionError> {
    // Module-resolution sync RPCs (the wasm runner imports node builtins +
    // its own ESM) are serviced host-directly via the execution's own
    // translator; the prewarm has no kernel/service loop.
    if execution
        .try_service_standalone_module_sync_rpc(request)
        .map_err(map_javascript_error)?
    {
        return Ok(true);
    }

    if matches!(
        request.method.as_str(),
        "fs.promises.readFile" | "fs.readFileSync"
    ) && request
        .args
        .first()
        .and_then(Value::as_str)
        .is_some_and(|path| {
            internal_sync_rpc
                .module_guest_paths
                .iter()
                .any(|candidate| candidate == path)
        })
    {
        let module_bytes =
            fs::read(&internal_sync_rpc.module_host_path).map_err(WasmExecutionError::Spawn)?;
        execution
            .respond_sync_rpc_success(
                request.id,
                Value::String(v8_runtime::base64_encode_pub(&module_bytes)),
            )
            .map_err(map_javascript_error)?;
        return Ok(true);
    }

    if wasm_sync_rpc_method_routes_through_sidecar_kernel(request, internal_sync_rpc) {
        return Ok(false);
    }

    if request.method == "__kernel_isatty" {
        execution
            .respond_sync_rpc_success(request.id, Value::Bool(false))
            .map_err(map_javascript_error)?;
        return Ok(true);
    }

    if request.method == "__kernel_tty_size" {
        execution
            .respond_sync_rpc_success(request.id, json!([80, 24]))
            .map_err(map_javascript_error)?;
        return Ok(true);
    }

    if request.method == "fs.openSync" {
        let Some(path) = request.args.first().and_then(Value::as_str) else {
            return Err(WasmExecutionError::RpcResponse(String::from(
                "missing fs.openSync path",
            )));
        };
        let Some(host_path) = translate_wasm_guest_path(path, internal_sync_rpc) else {
            return Ok(false);
        };
        let flags = request.args.get(1).unwrap_or(&Value::Null);
        if wasm_open_flags_require_write(flags)
            && wasm_host_path_is_read_only(&host_path, internal_sync_rpc)
        {
            return respond_wasm_sync_rpc_value(
                execution,
                request,
                path,
                Err(wasm_read_only_filesystem_error(path)),
            )
            .map(|()| true);
        }
        let file = match open_wasm_guest_file(&host_path, flags) {
            Ok(file) => file,
            Err(error) => {
                return respond_wasm_sync_rpc_value(execution, request, path, Err(error))
                    .map(|()| true);
            }
        };
        let fd = internal_sync_rpc.next_fd;
        internal_sync_rpc.next_fd += 1;
        internal_sync_rpc.open_files.insert(fd, file);
        execution
            .respond_sync_rpc_success(request.id, json!(fd))
            .map_err(map_javascript_error)?;
        return Ok(true);
    }

    if matches!(request.method.as_str(), "fs.statSync" | "fs.lstatSync") {
        let Some(path) = request.args.first().and_then(Value::as_str) else {
            return Err(WasmExecutionError::RpcResponse(format!(
                "missing {} path",
                request.method
            )));
        };
        let Some(host_path) = translate_wasm_guest_path(path, internal_sync_rpc) else {
            return Ok(false);
        };
        let metadata = if request.method == "fs.lstatSync" {
            fs::symlink_metadata(&host_path)
        } else {
            fs::metadata(&host_path)
        };
        return respond_wasm_sync_rpc_metadata(execution, request, path, metadata).map(|()| true);
    }

    if request.method == "fs.fstatSync" {
        let Some(fd) = request.args.first().and_then(Value::as_u64) else {
            return Err(WasmExecutionError::RpcResponse(String::from(
                "missing fs.fstatSync fd",
            )));
        };
        let Some(file) = internal_sync_rpc.open_files.get(&(fd as u32)) else {
            return Ok(false);
        };
        return respond_wasm_sync_rpc_metadata(
            execution,
            request,
            &fd.to_string(),
            file.metadata(),
        )
        .map(|()| true);
    }

    if request.method == "fs.ftruncateSync" {
        let Some(fd) = request.args.first().and_then(Value::as_u64) else {
            return Err(WasmExecutionError::RpcResponse(String::from(
                "missing fs.ftruncateSync fd",
            )));
        };
        let length = request.args.get(1).and_then(Value::as_u64).unwrap_or(0);
        let Some(file) = internal_sync_rpc.open_files.get_mut(&(fd as u32)) else {
            return Ok(false);
        };
        let result = file.set_len(length);
        return respond_wasm_sync_rpc_unit(execution, request, &fd.to_string(), result)
            .map(|()| true);
    }

    if request.method == "fs.closeSync" {
        let Some(fd) = request.args.first().and_then(Value::as_u64) else {
            return Err(WasmExecutionError::RpcResponse(String::from(
                "missing fs.closeSync fd",
            )));
        };
        if internal_sync_rpc.open_files.remove(&(fd as u32)).is_none() {
            return Ok(false);
        }
        execution
            .respond_sync_rpc_success(request.id, Value::Null)
            .map_err(map_javascript_error)?;
        return Ok(true);
    }

    if request.method == "fs.chmodSync" {
        let Some(path) = request.args.first().and_then(Value::as_str) else {
            return Err(WasmExecutionError::RpcResponse(String::from(
                "missing fs.chmodSync path",
            )));
        };
        let Some(host_path) = translate_wasm_guest_path(path, internal_sync_rpc) else {
            return Ok(false);
        };
        if wasm_host_path_is_read_only(&host_path, internal_sync_rpc) {
            return respond_wasm_sync_rpc_unit(
                execution,
                request,
                path,
                Err(wasm_read_only_filesystem_error(path)),
            )
            .map(|()| true);
        }
        let mode = request.args.get(1).and_then(Value::as_u64).unwrap_or(0) as u32;
        let result = (|| -> Result<(), std::io::Error> {
            let mut permissions = fs::metadata(&host_path)?.permissions();
            permissions.set_mode(mode);
            fs::set_permissions(&host_path, permissions)
        })();
        return respond_wasm_sync_rpc_unit(execution, request, path, result).map(|()| true);
    }

    if request.method == "fs.mkdirSync" {
        let Some(path) = request.args.first().and_then(Value::as_str) else {
            return Err(WasmExecutionError::RpcResponse(String::from(
                "missing fs.mkdirSync path",
            )));
        };
        let Some(host_path) = translate_wasm_guest_path(path, internal_sync_rpc) else {
            return Ok(false);
        };
        if wasm_host_path_is_read_only(&host_path, internal_sync_rpc) {
            return respond_wasm_sync_rpc_unit(
                execution,
                request,
                path,
                Err(wasm_read_only_filesystem_error(path)),
            )
            .map(|()| true);
        }
        let recursive = request
            .args
            .get(1)
            .map(|value| match value {
                Value::Bool(flag) => *flag,
                Value::Object(options) => options
                    .get("recursive")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                _ => false,
            })
            .unwrap_or(false);
        let result = if recursive {
            fs::create_dir_all(&host_path)
        } else {
            fs::create_dir(&host_path)
        };
        return respond_wasm_sync_rpc_unit(execution, request, path, result).map(|()| true);
    }

    if request.method == "fs.rmdirSync" {
        let Some(path) = request.args.first().and_then(Value::as_str) else {
            return Err(WasmExecutionError::RpcResponse(String::from(
                "missing fs.rmdirSync path",
            )));
        };
        let Some(host_path) = translate_wasm_guest_path(path, internal_sync_rpc) else {
            return Ok(false);
        };
        if wasm_host_path_is_read_only(&host_path, internal_sync_rpc) {
            return respond_wasm_sync_rpc_unit(
                execution,
                request,
                path,
                Err(wasm_read_only_filesystem_error(path)),
            )
            .map(|()| true);
        }
        return respond_wasm_sync_rpc_unit(execution, request, path, fs::remove_dir(&host_path))
            .map(|()| true);
    }

    if request.method == "fs.unlinkSync" {
        let Some(path) = request.args.first().and_then(Value::as_str) else {
            return Err(WasmExecutionError::RpcResponse(String::from(
                "missing fs.unlinkSync path",
            )));
        };
        let Some(host_path) = translate_wasm_guest_path(path, internal_sync_rpc) else {
            return Ok(false);
        };
        if wasm_host_path_is_read_only(&host_path, internal_sync_rpc) {
            return respond_wasm_sync_rpc_unit(
                execution,
                request,
                path,
                Err(wasm_read_only_filesystem_error(path)),
            )
            .map(|()| true);
        }
        return respond_wasm_sync_rpc_unit(execution, request, path, fs::remove_file(&host_path))
            .map(|()| true);
    }

    if request.method == "fs.renameSync" {
        let Some(source) = request.args.first().and_then(Value::as_str) else {
            return Err(WasmExecutionError::RpcResponse(String::from(
                "missing fs.renameSync source",
            )));
        };
        let Some(destination) = request.args.get(1).and_then(Value::as_str) else {
            return Err(WasmExecutionError::RpcResponse(String::from(
                "missing fs.renameSync destination",
            )));
        };
        let Some(host_source) = translate_wasm_guest_path(source, internal_sync_rpc) else {
            return Ok(false);
        };
        let Some(host_destination) = translate_wasm_guest_path(destination, internal_sync_rpc)
        else {
            return Ok(false);
        };
        if wasm_mutation_touches_read_only_mapping(
            &host_source,
            &host_destination,
            internal_sync_rpc,
        ) {
            return respond_wasm_sync_rpc_unit(
                execution,
                request,
                source,
                Err(wasm_read_only_filesystem_error(source)),
            )
            .map(|()| true);
        }
        return respond_wasm_sync_rpc_unit(
            execution,
            request,
            source,
            fs::rename(&host_source, &host_destination),
        )
        .map(|()| true);
    }

    if request.method == "fs.linkSync" {
        let Some(source) = request.args.first().and_then(Value::as_str) else {
            return Err(WasmExecutionError::RpcResponse(String::from(
                "missing fs.linkSync source",
            )));
        };
        let Some(destination) = request.args.get(1).and_then(Value::as_str) else {
            return Err(WasmExecutionError::RpcResponse(String::from(
                "missing fs.linkSync destination",
            )));
        };
        let Some(host_source) = translate_wasm_guest_path(source, internal_sync_rpc) else {
            return Ok(false);
        };
        let Some(host_destination) = translate_wasm_guest_path(destination, internal_sync_rpc)
        else {
            return Ok(false);
        };
        if wasm_host_path_is_read_only(&host_source, internal_sync_rpc)
            || wasm_host_path_is_read_only(&host_destination, internal_sync_rpc)
        {
            return respond_wasm_sync_rpc_unit(
                execution,
                request,
                source,
                Err(wasm_read_only_filesystem_error(source)),
            )
            .map(|()| true);
        }
        return respond_wasm_sync_rpc_unit(
            execution,
            request,
            source,
            fs::hard_link(&host_source, &host_destination),
        )
        .map(|()| true);
    }

    if request.method == "fs.symlinkSync" {
        let Some(target) = request.args.first().and_then(Value::as_str) else {
            return Err(WasmExecutionError::RpcResponse(String::from(
                "missing fs.symlinkSync target",
            )));
        };
        let Some(link_path) = request.args.get(1).and_then(Value::as_str) else {
            return Err(WasmExecutionError::RpcResponse(String::from(
                "missing fs.symlinkSync path",
            )));
        };
        let target_path = if target.starts_with('/') {
            let Some(path) = translate_wasm_guest_path(target, internal_sync_rpc) else {
                return Ok(false);
            };
            path
        } else {
            PathBuf::from(target)
        };
        let Some(host_link_path) = translate_wasm_guest_path(link_path, internal_sync_rpc) else {
            return Ok(false);
        };
        if wasm_host_path_is_read_only(&host_link_path, internal_sync_rpc) {
            return respond_wasm_sync_rpc_unit(
                execution,
                request,
                link_path,
                Err(wasm_read_only_filesystem_error(link_path)),
            )
            .map(|()| true);
        }
        return respond_wasm_sync_rpc_unit(
            execution,
            request,
            link_path,
            std::os::unix::fs::symlink(&target_path, &host_link_path),
        )
        .map(|()| true);
    }

    if request.method == "fs.readdirSync" {
        let Some(path) = request.args.first().and_then(Value::as_str) else {
            return Err(WasmExecutionError::RpcResponse(String::from(
                "missing fs.readdirSync path",
            )));
        };
        let Some(host_path) = translate_wasm_guest_path(path, internal_sync_rpc) else {
            return Ok(false);
        };
        let entries = fs::read_dir(&host_path)
            .and_then(|entries| {
                entries
                    .map(|entry| {
                        entry.map(|value| value.file_name().to_string_lossy().into_owned())
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .map(|entries| json!(entries));
        return respond_wasm_sync_rpc_value(execution, request, path, entries).map(|()| true);
    }

    if request.method == "fs.readlinkSync" {
        let Some(path) = request.args.first().and_then(Value::as_str) else {
            return Err(WasmExecutionError::RpcResponse(String::from(
                "missing fs.readlinkSync path",
            )));
        };
        let Some(host_path) = translate_wasm_guest_path(path, internal_sync_rpc) else {
            return Ok(false);
        };
        let target = fs::read_link(&host_path).map(|target| {
            Value::String(
                translate_wasm_host_symlink_target(&target, internal_sync_rpc)
                    .unwrap_or_else(|| target.to_string_lossy().into_owned()),
            )
        });
        return respond_wasm_sync_rpc_value(execution, request, path, target).map(|()| true);
    }

    if request.method == "fs.writeSync" {
        let Some(fd) = request.args.first().and_then(Value::as_u64) else {
            return Err(WasmExecutionError::RpcResponse(String::from(
                "missing fs.writeSync fd",
            )));
        };
        let bytes = decode_wasm_bytes_arg(
            request.args.get(1),
            "fs.writeSync bytes",
            WASM_CAPTURED_OUTPUT_LIMIT_BYTES,
        )?;
        if fd == 1 || fd == 2 {
            let bytes_len = bytes.len();
            internal_sync_rpc.pending_events.push_back(if fd == 1 {
                WasmExecutionEvent::Stdout(bytes)
            } else {
                WasmExecutionEvent::Stderr(bytes)
            })?;
            execution
                .respond_sync_rpc_success(request.id, json!(bytes_len))
                .map_err(map_javascript_error)?;
            return Ok(true);
        }
        let position = request.args.get(2).and_then(Value::as_u64);
        let Some(file) = internal_sync_rpc.open_files.get_mut(&(fd as u32)) else {
            return Ok(false);
        };
        let written = if let Some(position) = position {
            file.write_at(&bytes, position)
                .map_err(WasmExecutionError::Spawn)?
        } else {
            file.write(&bytes).map_err(WasmExecutionError::Spawn)?
        };
        execution
            .respond_sync_rpc_success(request.id, json!(written))
            .map_err(map_javascript_error)?;
        return Ok(true);
    }

    if request.method == "fs.readSync" {
        let Some(fd) = request.args.first().and_then(Value::as_u64) else {
            return Err(WasmExecutionError::RpcResponse(String::from(
                "missing fs.readSync fd",
            )));
        };
        let length = wasm_sync_read_length(request.args.get(1).and_then(Value::as_u64))?;
        let position = request.args.get(2).and_then(Value::as_u64);
        let Some(file) = internal_sync_rpc.open_files.get_mut(&(fd as u32)) else {
            return Ok(false);
        };
        let mut buffer = vec![0u8; length];
        let bytes_read = if let Some(position) = position {
            file.read_at(&mut buffer, position)
                .map_err(WasmExecutionError::Spawn)?
        } else {
            file.read(&mut buffer).map_err(WasmExecutionError::Spawn)?
        };
        buffer.truncate(bytes_read);
        execution
            .respond_sync_rpc_success(
                request.id,
                json!({
                    "__agentOSType": "bytes",
                    "base64": v8_runtime::base64_encode_pub(&buffer),
                }),
            )
            .map_err(map_javascript_error)?;
        return Ok(true);
    }

    Ok(false)
}

fn wasm_sync_rpc_method_routes_through_sidecar_kernel(
    request: &HostRpcRequest,
    internal_sync_rpc: &WasmInternalSyncRpc,
) -> bool {
    internal_sync_rpc.route_fs_through_sidecar
        && (WASM_SIDECAR_ROUTED_FS_SYNC_METHODS.contains(&request.method.as_str())
            || WASM_SIDECAR_ROUTED_KERNEL_SYNC_METHODS.contains(&request.method.as_str()))
}

fn translate_wasm_guest_path(
    path: &str,
    internal_sync_rpc: &WasmInternalSyncRpc,
) -> Option<PathBuf> {
    if let Some(host_path) = translate_wasm_host_runtime_path(path, internal_sync_rpc) {
        return confine_wasm_host_path(host_path, internal_sync_rpc);
    }

    let normalized_path = if path.starts_with('/') {
        normalize_guest_path(path)
    } else {
        join_guest_path(&internal_sync_rpc.guest_cwd, path)
    };

    if normalized_path == internal_sync_rpc.module_host_path.to_string_lossy() {
        return Some(internal_sync_rpc.module_host_path.clone());
    }
    if internal_sync_rpc
        .module_guest_paths
        .iter()
        .any(|candidate| candidate == &normalized_path)
    {
        return Some(internal_sync_rpc.module_host_path.clone());
    }
    for mapping in &internal_sync_rpc.guest_path_mappings {
        if let Some(suffix) = strip_guest_prefix(&normalized_path, &mapping.guest_path) {
            return confine_wasm_host_path(
                join_host_path(&mapping.host_path, &suffix),
                internal_sync_rpc,
            );
        }
    }
    if let Some(suffix) = strip_guest_prefix(&normalized_path, &internal_sync_rpc.guest_cwd) {
        return confine_wasm_host_path(
            join_host_path(&internal_sync_rpc.host_cwd, &suffix),
            internal_sync_rpc,
        );
    }
    if normalized_path.starts_with('/') {
        let root_candidate = internal_sync_rpc
            .sandbox_root
            .as_ref()
            .map(|root| join_host_path(root, normalized_path.trim_start_matches('/')));
        if let Some(candidate) = root_candidate.as_ref() {
            if candidate.exists() {
                return confine_wasm_host_path(candidate.clone(), internal_sync_rpc);
            }
        }

        // Some shipped WASI command binaries still collapse guest-cwd-relative paths like
        // `note.txt` into `/note.txt` before they reach the host bridge. Prefer the true root
        // path when it exists, but fall back to the current guest cwd when only that target exists.
        if internal_sync_rpc.guest_cwd != "/" {
            let cwd_relative_guest_path = join_guest_path(
                &internal_sync_rpc.guest_cwd,
                normalized_path.trim_start_matches('/'),
            );
            for mapping in &internal_sync_rpc.guest_path_mappings {
                if let Some(suffix) =
                    strip_guest_prefix(&cwd_relative_guest_path, &mapping.guest_path)
                {
                    let candidate = join_host_path(&mapping.host_path, &suffix);
                    if candidate.exists() {
                        return confine_wasm_host_path(candidate, internal_sync_rpc);
                    }
                }
            }
            if let Some(suffix) =
                strip_guest_prefix(&cwd_relative_guest_path, &internal_sync_rpc.guest_cwd)
            {
                let candidate = join_host_path(&internal_sync_rpc.host_cwd, &suffix);
                if candidate.exists() {
                    return confine_wasm_host_path(candidate, internal_sync_rpc);
                }
            }
        }

        return root_candidate.and_then(|path| confine_wasm_host_path(path, internal_sync_rpc));
    }
    None
}

fn confine_wasm_host_path(
    host_path: PathBuf,
    internal_sync_rpc: &WasmInternalSyncRpc,
) -> Option<PathBuf> {
    if host_path == internal_sync_rpc.module_host_path {
        return Some(host_path);
    }

    let allowed_roots = wasm_allowed_host_roots(internal_sync_rpc);
    if allowed_roots.is_empty() {
        return None;
    }

    if let Ok(canonical_path) = fs::canonicalize(&host_path) {
        return wasm_canonical_path_is_allowed(&canonical_path, &allowed_roots)
            .then_some(host_path);
    }

    let existing_ancestor = nearest_existing_wasm_host_ancestor(&host_path)?;
    let canonical_ancestor = fs::canonicalize(existing_ancestor).ok()?;
    wasm_canonical_path_is_allowed(&canonical_ancestor, &allowed_roots).then_some(host_path)
}

fn wasm_allowed_host_roots(internal_sync_rpc: &WasmInternalSyncRpc) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for root in internal_sync_rpc
        .guest_path_mappings
        .iter()
        .map(|mapping| mapping.host_path.as_path())
        .chain(std::iter::once(internal_sync_rpc.host_cwd.as_path()))
        .chain(internal_sync_rpc.sandbox_root.as_deref())
    {
        if let Ok(canonical_root) = fs::canonicalize(root) {
            if !roots.iter().any(|existing| existing == &canonical_root) {
                roots.push(canonical_root);
            }
        }
    }
    roots
}

fn wasm_canonical_path_is_allowed(path: &Path, allowed_roots: &[PathBuf]) -> bool {
    allowed_roots
        .iter()
        .any(|root| path == root || path.starts_with(root))
}

fn nearest_existing_wasm_host_ancestor(path: &Path) -> Option<&Path> {
    let mut candidate = Some(path);
    while let Some(current) = candidate {
        if fs::symlink_metadata(current).is_ok() {
            return Some(current);
        }
        candidate = current.parent();
    }
    None
}

fn translate_wasm_host_runtime_path(
    path: &str,
    internal_sync_rpc: &WasmInternalSyncRpc,
) -> Option<PathBuf> {
    let candidate = Path::new(path);
    if !candidate.is_absolute() {
        return None;
    }

    if candidate == internal_sync_rpc.module_host_path {
        return Some(candidate.to_path_buf());
    }

    let mapped_host_root = internal_sync_rpc
        .guest_path_mappings
        .iter()
        .map(|mapping| mapping.host_path.as_path())
        .find(|root| candidate == *root || candidate.starts_with(root));
    if let Some(root) = mapped_host_root {
        let _ = root;
        return Some(candidate.to_path_buf());
    }

    if candidate == internal_sync_rpc.host_cwd || candidate.starts_with(&internal_sync_rpc.host_cwd)
    {
        return Some(candidate.to_path_buf());
    }

    if let Some(sandbox_root) = internal_sync_rpc.sandbox_root.as_ref() {
        if candidate == sandbox_root || candidate.starts_with(sandbox_root) {
            return Some(candidate.to_path_buf());
        }
    }

    None
}

fn translate_wasm_host_symlink_target(
    target: &Path,
    internal_sync_rpc: &WasmInternalSyncRpc,
) -> Option<String> {
    if !target.is_absolute() {
        return None;
    }

    for mapping in &internal_sync_rpc.guest_path_mappings {
        if let Ok(suffix) = target.strip_prefix(&mapping.host_path) {
            return Some(join_guest_path(
                &mapping.guest_path,
                &suffix.to_string_lossy().replace('\\', "/"),
            ));
        }
    }

    if let Some(suffix) = target
        .strip_prefix(&internal_sync_rpc.host_cwd)
        .ok()
        .filter(|_| internal_sync_rpc.guest_cwd.starts_with('/'))
    {
        return Some(join_guest_path(
            &internal_sync_rpc.guest_cwd,
            &suffix.to_string_lossy().replace('\\', "/"),
        ));
    }

    if let Some(sandbox_root) = internal_sync_rpc.sandbox_root.as_ref() {
        if let Ok(suffix) = target.strip_prefix(sandbox_root) {
            return Some(join_guest_path(
                "/",
                &suffix.to_string_lossy().replace('\\', "/"),
            ));
        }
    }

    None
}

fn wasm_host_path_is_read_only(host_path: &Path, internal_sync_rpc: &WasmInternalSyncRpc) -> bool {
    let canonical_path = fs::canonicalize(host_path)
        .ok()
        .or_else(|| {
            nearest_existing_wasm_host_ancestor(host_path)
                .and_then(|ancestor| fs::canonicalize(ancestor).ok())
        })
        .unwrap_or_else(|| host_path.to_path_buf());

    internal_sync_rpc
        .guest_path_mappings
        .iter()
        .filter_map(|mapping| {
            let root = fs::canonicalize(&mapping.host_path).ok()?;
            (canonical_path == root || canonical_path.starts_with(&root))
                .then_some((root.components().count(), mapping.read_only))
        })
        .max_by_key(|(depth, _)| *depth)
        .is_some_and(|(_, read_only)| read_only)
}

fn wasm_mutation_touches_read_only_mapping(
    source: &Path,
    destination: &Path,
    internal_sync_rpc: &WasmInternalSyncRpc,
) -> bool {
    wasm_host_path_is_read_only(source, internal_sync_rpc)
        || wasm_host_path_is_read_only(destination, internal_sync_rpc)
}

fn wasm_open_flags_require_write(flags: &Value) -> bool {
    match flags.as_str() {
        Some(value) => value.contains('w') || value.contains('a') || value.contains('+'),
        None if flags.as_u64().unwrap_or(0) == 0 => false,
        _ => {
            let numeric = flags.as_u64().unwrap_or(0);
            (numeric & 0o1) != 0
                || (numeric & 0o2) != 0
                || (numeric & 0o100) != 0
                || (numeric & 0o1000) != 0
                || (numeric & 0o2000) != 0
        }
    }
}

fn wasm_read_only_filesystem_error(path: &str) -> std::io::Error {
    let _ = path;
    std::io::Error::from_raw_os_error(30)
}

fn respond_wasm_sync_rpc_metadata(
    execution: &mut JavascriptExecution,
    request: &HostRpcRequest,
    label: &str,
    metadata: Result<fs::Metadata, std::io::Error>,
) -> Result<(), WasmExecutionError> {
    respond_wasm_sync_rpc_value(
        execution,
        request,
        label,
        metadata.map(|value| wasm_host_stat_value(&value)),
    )
}

fn respond_wasm_sync_rpc_unit(
    execution: &mut JavascriptExecution,
    request: &HostRpcRequest,
    label: &str,
    result: Result<(), std::io::Error>,
) -> Result<(), WasmExecutionError> {
    respond_wasm_sync_rpc_value(execution, request, label, result.map(|()| Value::Null))
}

fn respond_wasm_sync_rpc_value(
    execution: &mut JavascriptExecution,
    request: &HostRpcRequest,
    label: &str,
    result: Result<Value, std::io::Error>,
) -> Result<(), WasmExecutionError> {
    match result {
        Ok(value) => execution
            .respond_sync_rpc_success(request.id, value)
            .map_err(map_javascript_error),
        Err(error) => execution
            .respond_sync_rpc_error(
                request.id,
                wasm_sync_rpc_error_code(&error),
                format!("{} {} failed: {error}", request.method, label),
            )
            .map_err(map_javascript_error),
    }
}

fn wasm_sync_rpc_error_code(error: &std::io::Error) -> &'static str {
    use std::io::ErrorKind;

    if error.raw_os_error() == Some(30) {
        return "EROFS";
    }

    match error.kind() {
        ErrorKind::NotFound => "ENOENT",
        ErrorKind::PermissionDenied => "EACCES",
        ErrorKind::AlreadyExists => "EEXIST",
        ErrorKind::InvalidInput => "EINVAL",
        ErrorKind::IsADirectory => "EISDIR",
        ErrorKind::NotADirectory => "ENOTDIR",
        _ => "EIO",
    }
}

fn wasm_host_stat_value(metadata: &fs::Metadata) -> Value {
    json!({
        "mode": metadata.mode(),
        "size": metadata.size(),
        "blocks": metadata.blocks(),
        "dev": metadata.dev(),
        "rdev": metadata.rdev(),
        "isDirectory": metadata.is_dir(),
        "isSymbolicLink": metadata.file_type().is_symlink(),
        "atimeMs": metadata.atime() * 1000 + (metadata.atime_nsec() / 1_000_000),
        "mtimeMs": metadata.mtime() * 1000 + (metadata.mtime_nsec() / 1_000_000),
        "ctimeMs": metadata.ctime() * 1000 + (metadata.ctime_nsec() / 1_000_000),
        "birthtimeMs": metadata.ctime() * 1000 + (metadata.ctime_nsec() / 1_000_000),
        "ino": metadata.ino(),
        "nlink": metadata.nlink(),
        "uid": metadata.uid(),
        "gid": metadata.gid(),
    })
}

fn strip_guest_prefix(path: &str, prefix: &str) -> Option<String> {
    let normalized_path = normalize_guest_path(path);
    let normalized_prefix = normalize_guest_path(prefix);
    if normalized_path == normalized_prefix {
        return Some(String::new());
    }
    normalized_path
        .strip_prefix(&(normalized_prefix + "/"))
        .map(str::to_owned)
}

fn join_host_path(base: &Path, suffix: &str) -> PathBuf {
    if suffix.is_empty() {
        return base.to_path_buf();
    }
    suffix
        .split('/')
        .filter(|segment| !segment.is_empty())
        .fold(base.to_path_buf(), |path, segment| path.join(segment))
}

fn decode_wasm_bytes_arg(
    value: Option<&Value>,
    label: &'static str,
    limit: usize,
) -> Result<Vec<u8>, WasmExecutionError> {
    let base64 = value
        .and_then(Value::as_object)
        .and_then(|value| value.get("base64"))
        .and_then(Value::as_str)
        .ok_or_else(|| WasmExecutionError::RpcResponse(format!("missing {label}")))?;
    let decoded_len = base64_decoded_len(base64)
        .ok_or_else(|| WasmExecutionError::RpcResponse(format!("invalid {label} base64")))?;
    if decoded_len > limit {
        return Err(WasmExecutionError::OutputBufferExceeded {
            stream: label,
            limit,
        });
    }
    base64::engine::general_purpose::STANDARD
        .decode(base64)
        .map_err(|_| WasmExecutionError::RpcResponse(format!("invalid {label} base64")))
}

fn base64_decoded_len(base64: &str) -> Option<usize> {
    let len = base64.len();
    let padding = base64
        .as_bytes()
        .iter()
        .rev()
        .take_while(|byte| **byte == b'=')
        .take(2)
        .count();
    let full_quads = len / 4;
    let remainder = len % 4;
    let base_len = full_quads.checked_mul(3)?.checked_sub(padding)?;
    match remainder {
        0 => Some(base_len),
        1 => None,
        2 => base_len.checked_add(1),
        3 => base_len.checked_add(2),
        _ => None,
    }
}

fn is_v8_stack_overflow_stderr(chunk: &[u8]) -> bool {
    std::str::from_utf8(chunk).is_ok_and(|message| {
        message.starts_with("RangeError: Maximum call stack size exceeded")
            && message.contains("wasm-function")
    })
}

fn configured_wasm_stack_limit_error(limit: u64) -> String {
    format!(
        "WebAssembly guest exhausted its configured stack budget ({limit} bytes); \
raise limits.resources.maxWasmStackBytes to allow deeper guest call stacks.\n"
    )
}

fn append_wasm_captured_output(
    buffer: &mut Vec<u8>,
    chunk: &[u8],
    stream: &'static str,
) -> Result<(), WasmExecutionError> {
    ensure_wasm_output_capacity(buffer.len(), chunk.len(), stream)?;
    buffer.extend_from_slice(chunk);
    Ok(())
}

fn ensure_wasm_output_capacity(
    current_len: usize,
    chunk_len: usize,
    stream: &'static str,
) -> Result<(), WasmExecutionError> {
    let Some(next_len) = current_len.checked_add(chunk_len) else {
        return Err(WasmExecutionError::OutputBufferExceeded {
            stream,
            limit: WASM_CAPTURED_OUTPUT_LIMIT_BYTES,
        });
    };
    if next_len > WASM_CAPTURED_OUTPUT_LIMIT_BYTES {
        return Err(WasmExecutionError::OutputBufferExceeded {
            stream,
            limit: WASM_CAPTURED_OUTPUT_LIMIT_BYTES,
        });
    }
    Ok(())
}

fn wasm_sync_read_length(length: Option<u64>) -> Result<usize, WasmExecutionError> {
    let length = length.unwrap_or(0);
    let length = usize::try_from(length).map_err(|_| {
        WasmExecutionError::InvalidLimit(format!("fs.readSync length {length} exceeds host usize"))
    })?;
    if length > WASM_SYNC_READ_LIMIT_BYTES {
        return Err(WasmExecutionError::InvalidLimit(format!(
            "fs.readSync length {length} exceeds maximum {WASM_SYNC_READ_LIMIT_BYTES}"
        )));
    }
    Ok(length)
}

fn open_wasm_guest_file(path: &Path, flags: &Value) -> std::io::Result<fs::File> {
    let mut options = OpenOptions::new();
    let flags_label = flags.to_string();

    match flags.as_str() {
        Some("r") | None if flags.as_u64().unwrap_or(0) == 0 => {
            options.read(true);
        }
        Some("r+") => {
            options.read(true).write(true);
        }
        Some("w") => {
            options.write(true).create(true).truncate(true);
        }
        Some("w+") => {
            options.read(true).write(true).create(true).truncate(true);
        }
        Some("a") => {
            options.append(true).create(true);
        }
        Some("a+") => {
            options.read(true).append(true).create(true);
        }
        _ => {
            let numeric = flags.as_u64().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("unsupported fs.openSync flags: {flags_label}"),
                )
            })?;
            let write_only = (numeric & 0o1) != 0;
            let read_write = (numeric & 0o2) != 0;
            let create = (numeric & 0o100) != 0;
            let truncate = (numeric & 0o1000) != 0;
            let append = (numeric & 0o2000) != 0;

            if read_write {
                options.read(true).write(true);
            } else if write_only {
                options.write(true);
            } else {
                options.read(true);
            }
            if create {
                options.create(true);
            }
            if truncate {
                options.truncate(true);
            }
            if append {
                options.append(true);
            }
        }
    }

    options.open(path).map_err(|error| {
        std::io::Error::new(
            error.kind(),
            format!(
                "failed to open guest file {} with flags {}: {error}",
                path.display(),
                flags_label
            ),
        )
    })
}

fn parse_wasm_signal_state_line(
    line: &[u8],
) -> Result<Option<WasmExecutionEvent>, WasmExecutionError> {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    let payload = match line.strip_prefix(WASM_SIGNAL_STATE_PREFIX.as_bytes()) {
        Some(payload) => payload,
        None => return Ok(None),
    };
    let payload = std::str::from_utf8(payload)
        .map_err(|error| WasmExecutionError::RpcResponse(error.to_string()))?;
    let message: Value = serde_json::from_str(payload)
        .map_err(|error| WasmExecutionError::RpcResponse(error.to_string()))?;
    let signal = message
        .get("signal")
        .and_then(Value::as_u64)
        .ok_or_else(|| WasmExecutionError::RpcResponse(String::from("missing signal number")))?;
    let registration = message
        .get("registration")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            WasmExecutionError::RpcResponse(String::from("missing signal registration"))
        })?;
    let action = match registration
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("default")
    {
        "ignore" => ExecutionSignalDispositionAction::Ignore,
        "user" => ExecutionSignalDispositionAction::User,
        _ => ExecutionSignalDispositionAction::Default,
    };
    let mask = registration
        .get("mask")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(Value::as_u64)
                .map(|value| value as u32)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let flags = registration
        .get("flags")
        .and_then(Value::as_u64)
        .unwrap_or_default() as u32;

    Ok(Some(WasmExecutionEvent::SignalState {
        signal: signal as u32,
        registration: ExecutionSignalHandlerRegistration {
            action,
            mask,
            flags,
        },
    }))
}

struct WasmJavascriptExecutionOptions<'a> {
    frozen_time_ms: u128,
    prewarm_only: bool,
    warmup_metrics: Option<&'a [u8]>,
    defer_execute: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WasmSnapshotRunnerMode {
    Auto,
    Block,
    Off,
}

fn wasm_snapshot_runner_mode() -> WasmSnapshotRunnerMode {
    match std::env::var(WASM_SNAPSHOT_RUNNER_ENV) {
        Ok(value) if value.eq_ignore_ascii_case("block") => WasmSnapshotRunnerMode::Block,
        Ok(value) if value.eq_ignore_ascii_case("off") => WasmSnapshotRunnerMode::Off,
        Ok(value) if value.eq_ignore_ascii_case("auto") => WasmSnapshotRunnerMode::Auto,
        Ok(value) => {
            tracing::warn!(
                value,
                "{WASM_SNAPSHOT_RUNNER_ENV} must be auto, block, or off; using auto"
            );
            WasmSnapshotRunnerMode::Auto
        }
        Err(_) => WasmSnapshotRunnerMode::Auto,
    }
}

fn start_wasm_javascript_execution(
    javascript_engine: &mut JavascriptExecutionEngine,
    runtime: &RuntimeContext,
    import_cache: &NodeImportCache,
    javascript_context_id: &str,
    resolved_module: &ResolvedWasmModule,
    request: &StartWasmExecutionRequest,
    options: WasmJavascriptExecutionOptions<'_>,
) -> Result<JavascriptExecution, WasmExecutionError> {
    let wasm_module_bytes = cached_wasm_module_bytes(&resolved_module.resolved_path)?;
    let internal_env = build_wasm_internal_env(
        resolved_module,
        request,
        options.frozen_time_ms,
        options.prewarm_only,
    )?;
    let snapshot_mode = wasm_snapshot_runner_mode();
    let mut env = wasm_runner_base_env(request);
    let mut guest_runtime = request.guest_runtime.clone();

    let inline_code = match snapshot_mode {
        WasmSnapshotRunnerMode::Off => {
            env.extend(
                internal_env
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone())),
            );
            build_wasm_runner_module_source(
                import_cache,
                &internal_env,
                options.warmup_metrics,
                request.managed_kernel_host,
            )?
        }
        WasmSnapshotRunnerMode::Auto | WasmSnapshotRunnerMode::Block => {
            let userland_bundle = build_wasm_runner_userland_bundle(import_cache)?;
            let runner_heap_limit_mb = wasm_runner_heap_limit_mb(request);
            let runtime = javascript_engine
                .runtime_context()
                .map_err(map_javascript_error)?;
            V8RuntimeHost::warm_snapshot_async(runtime, userland_bundle.clone());
            let use_snapshot = match snapshot_mode {
                WasmSnapshotRunnerMode::Block => {
                    if !javascript_engine
                        .snapshot_userland_ready(&userland_bundle)
                        .map_err(map_javascript_error)?
                    {
                        javascript_engine
                            .pre_warm_snapshot(&userland_bundle)
                            .map_err(map_javascript_error)?;
                    }
                    javascript_engine
                        .pre_warm_workers(
                            &userland_bundle,
                            runner_heap_limit_mb,
                            v8_warm_worker_count(),
                        )
                        .map_err(map_javascript_error)?;
                    javascript_engine
                        .pre_warm_workers("", 0, v8_warm_worker_count())
                        .map_err(map_javascript_error)?;
                    true
                }
                WasmSnapshotRunnerMode::Auto => {
                    let snapshot_ready = javascript_engine
                        .snapshot_userland_ready(&userland_bundle)
                        .unwrap_or(false);
                    if snapshot_ready {
                        javascript_engine
                            .pre_warm_workers(
                                &userland_bundle,
                                runner_heap_limit_mb,
                                v8_warm_worker_count(),
                            )
                            .map_err(map_javascript_error)?;
                    }
                    snapshot_ready
                }
                WasmSnapshotRunnerMode::Off => false,
            };

            if use_snapshot {
                env = wasm_snapshot_runner_base_env(request);
                env.extend(
                    internal_env
                        .iter()
                        .map(|(key, value)| (key.clone(), value.clone())),
                );
                guest_runtime.snapshot_userland_code = Some(userland_bundle);
                build_wasm_snapshot_runner_inline_code(
                    options.warmup_metrics,
                    request.managed_kernel_host,
                )
            } else {
                env.extend(
                    internal_env
                        .iter()
                        .map(|(key, value)| (key.clone(), value.clone())),
                );
                build_wasm_runner_module_source(
                    import_cache,
                    &internal_env,
                    options.warmup_metrics,
                    request.managed_kernel_host,
                )?
            }
        }
    };

    let javascript_request = StartJavascriptExecutionRequest {
        vm_id: request.vm_id.clone(),
        context_id: javascript_context_id.to_owned(),
        argv: vec![String::from(WASM_INLINE_RUNNER_ENTRYPOINT)],
        argv0: None,
        env,
        cwd: request.cwd.clone(),
        // Guest WASM fuel/memory caps are enforced from `request.limits`,
        // and stack caps are validated there until runtime stack enforcement
        // lands. These are separate from the runner's V8 heap: the trusted
        // runner still has to compile the WASI runtime + guest module into
        // its own isolate, which can overflow the 128 MiB per-guest default,
        // so size the runner heap explicitly (operator-tunable).
        limits: wasm_runner_javascript_limits(&request.limits, wasm_runner_heap_limit_mb(request)),
        // Forward the guest-runtime identity so the runner's shim sets
        // process.* from typed config rather than env.
        guest_runtime,
        inline_code: Some(inline_code),
        wasm_module_bytes: Some(wasm_module_bytes),
    };
    if options.defer_execute {
        javascript_engine.prepare_execution_with_runtime(javascript_request, runtime.clone())
    } else {
        javascript_engine.start_execution_with_runtime(javascript_request, runtime.clone())
    }
    .map_err(map_javascript_error)
}

fn wasm_runner_javascript_limits(
    limits: &WasmExecutionLimits,
    runner_heap_limit_mb: u32,
) -> JavascriptExecutionLimits {
    JavascriptExecutionLimits {
        v8_heap_limit_mb: Some(runner_heap_limit_mb),
        cpu_time_limit_ms: limits.active_cpu_time_limit_ms,
        reactor_work_quantum: limits.reactor_work_quantum,
        bridge_call_timeout_ms: limits.bridge_call_timeout_ms,
        ..JavascriptExecutionLimits::default()
    }
}

struct WasmModuleBytesCache {
    entries: HashMap<PathBuf, (String, Arc<Vec<u8>>)>,
}

fn wasm_module_bytes_cache() -> &'static Mutex<WasmModuleBytesCache> {
    static CACHE: OnceLock<Mutex<WasmModuleBytesCache>> = OnceLock::new();
    CACHE.get_or_init(|| {
        Mutex::new(WasmModuleBytesCache {
            entries: HashMap::new(),
        })
    })
}

fn cached_wasm_module_bytes(path: &Path) -> Result<Arc<Vec<u8>>, WasmExecutionError> {
    let current_fingerprint = file_fingerprint(path);
    {
        let cache = wasm_module_bytes_cache()
            .lock()
            .expect("wasm module bytes cache lock poisoned");
        if let Some((fingerprint, bytes)) = cache.entries.get(path) {
            if fingerprint == &current_fingerprint {
                return Ok(Arc::clone(bytes));
            }
        }
    }

    let module_bytes = Arc::new(fs::read(path).map_err(WasmExecutionError::PrepareWarmPath)?);
    let fingerprint = file_fingerprint(path);
    let mut cache = wasm_module_bytes_cache()
        .lock()
        .expect("wasm module bytes cache lock poisoned");
    if !cache.entries.contains_key(path) && cache.entries.len() >= WASM_MODULE_BYTES_CACHE_CAPACITY
    {
        if let Some(evicted_path) = cache.entries.keys().next().cloned() {
            cache.entries.remove(&evicted_path);
            tracing::warn!(
                path = %evicted_path.display(),
                "evicting cached wasm module bytes entry"
            );
        }
    }
    cache
        .entries
        .insert(path.to_path_buf(), (fingerprint, Arc::clone(&module_bytes)));
    let cumulative_bytes: usize = cache.entries.values().map(|(_, bytes)| bytes.len()).sum();
    tracing::debug!(
        path = %path.display(),
        raw_bytes = module_bytes.len(),
        cumulative_bytes,
        "cached wasm module bytes entry"
    );
    Ok(module_bytes)
}

fn build_wasm_internal_env(
    resolved_module: &ResolvedWasmModule,
    request: &StartWasmExecutionRequest,
    frozen_time_ms: u128,
    prewarm_only: bool,
) -> Result<BTreeMap<String, String>, WasmExecutionError> {
    let guest_path_mappings = wasm_guest_path_mappings(request);
    let mut internal_env = request
        .env
        .iter()
        .filter(|(key, _)| key.starts_with("AGENTOS_"))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<BTreeMap<_, _>>();
    if let Some(value) = request.env.get("AGENTOS_KEEP_STDIN_OPEN") {
        internal_env.insert(String::from("AGENTOS_KEEP_STDIN_OPEN"), value.clone());
    }
    scrub_migrated_wasm_limit_env(&mut internal_env);
    insert_optional_u64_env(
        &mut internal_env,
        WASM_MAX_MEMORY_BYTES_ENV,
        request.limits.max_memory_bytes,
    );
    insert_optional_u64_env(
        &mut internal_env,
        WASM_MAX_MODULE_FILE_BYTES_ENV,
        request.limits.max_module_file_bytes,
    );
    insert_optional_u64_env(
        &mut internal_env,
        WASM_MAX_OPEN_FDS_ENV,
        request.limits.max_open_fds,
    );
    insert_optional_u64_env(
        &mut internal_env,
        WASM_MAX_SPAWN_FILE_ACTIONS_ENV,
        request.limits.max_spawn_file_actions,
    );
    insert_optional_u64_env(
        &mut internal_env,
        WASM_MAX_SPAWN_FILE_ACTION_BYTES_ENV,
        request.limits.max_spawn_file_action_bytes,
    );
    insert_optional_u64_env(
        &mut internal_env,
        WASM_MAX_SOCKETS_ENV,
        request.limits.max_sockets,
    );
    insert_optional_u64_env(
        &mut internal_env,
        WASM_MAX_BLOCKING_READ_MS_ENV,
        request.limits.max_blocking_read_ms,
    );
    insert_optional_u64_env(
        &mut internal_env,
        WASM_INTERNAL_MAX_STACK_BYTES_ENV,
        request.limits.max_stack_bytes,
    );
    internal_env.insert(
        WASM_INTERNAL_SYNC_RPC_RESPONSE_LINE_BYTES_ENV.to_string(),
        request
            .limits
            .max_sync_rpc_response_line_bytes
            .unwrap_or(DEFAULT_WASM_SYNC_RPC_RESPONSE_LINE_BYTES)
            .to_string(),
    );
    internal_env.insert(
        WASM_MODULE_PATH_ENV.to_string(),
        resolved_module.specifier.clone(),
    );
    internal_env.insert(
        String::from("AGENTOS_FORWARD_KERNEL_STDIN_RPC"),
        String::from("1"),
    );
    internal_env.insert(
        WASM_GUEST_ARGV_ENV.to_string(),
        encode_json_string_array(&warmup_guest_argv(resolved_module, request)),
    );
    internal_env.insert(
        WASM_GUEST_ENV_ENV.to_string(),
        encode_json_string_map(&guest_visible_wasm_env(&request.env)),
    );
    insert_wasm_runner_identity_env(&mut internal_env, &request.guest_runtime);
    internal_env.insert(
        WASM_HOST_CWD_ENV.to_string(),
        request.cwd.to_string_lossy().into_owned(),
    );
    internal_env.insert(
        String::from("AGENTOS_GUEST_PATH_MAPPINGS"),
        encode_wasm_guest_path_mappings(&guest_path_mappings),
    );
    internal_env.insert(
        WASM_PERMISSION_TIER_ENV.to_string(),
        request.permission_tier.as_env_value().to_string(),
    );
    internal_env.insert(
        String::from("AGENTOS_FROZEN_TIME_MS"),
        frozen_time_ms.to_string(),
    );

    if prewarm_only {
        internal_env.insert(WASM_PREWARM_ONLY_ENV.to_string(), String::from("1"));
    } else {
        internal_env.remove(WASM_PREWARM_ONLY_ENV);
    }
    Ok(internal_env)
}

fn wasm_runner_base_env(request: &StartWasmExecutionRequest) -> BTreeMap<String, String> {
    let mut env = request.env.clone();
    scrub_migrated_wasm_limit_env(&mut env);
    env
}

fn wasm_snapshot_runner_base_env(request: &StartWasmExecutionRequest) -> BTreeMap<String, String> {
    let mut env = request
        .env
        .iter()
        .filter(|(key, _)| !is_internal_wasm_guest_env_key(key))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<BTreeMap<_, _>>();
    scrub_migrated_wasm_limit_env(&mut env);
    env
}

fn scrub_migrated_wasm_limit_env(env: &mut BTreeMap<String, String>) {
    for key in [
        WASM_MAX_MEMORY_BYTES_ENV,
        WASM_MAX_STACK_BYTES_ENV,
        WASM_MAX_MODULE_FILE_BYTES_ENV,
        WASM_MAX_OPEN_FDS_ENV,
        WASM_MAX_SPAWN_FILE_ACTIONS_ENV,
        WASM_MAX_SPAWN_FILE_ACTION_BYTES_ENV,
        WASM_MAX_SOCKETS_ENV,
        WASM_MAX_BLOCKING_READ_MS_ENV,
        WASM_INTERNAL_SYNC_RPC_RESPONSE_LINE_BYTES_ENV,
        "AGENTOS_WASM_PREWARM_TIMEOUT_MS",
        "AGENTOS_WASM_RUNNER_HEAP_LIMIT_MB",
    ] {
        env.remove(key);
    }
}

fn insert_optional_u64_env(env: &mut BTreeMap<String, String>, key: &str, value: Option<u64>) {
    if let Some(value) = value {
        env.insert(key.to_string(), value.to_string());
    } else {
        env.remove(key);
    }
}

fn insert_wasm_runner_identity_env(
    env: &mut BTreeMap<String, String>,
    guest_runtime: &GuestRuntimeConfig,
) {
    insert_optional_u64_env(
        env,
        "AGENTOS_VIRTUAL_PROCESS_UID",
        guest_runtime.virtual_uid,
    );
    insert_optional_u64_env(
        env,
        "AGENTOS_VIRTUAL_PROCESS_GID",
        guest_runtime.virtual_gid,
    );
    insert_optional_u64_env(
        env,
        "AGENTOS_VIRTUAL_PROCESS_PID",
        guest_runtime.virtual_pid,
    );
    insert_optional_u64_env(
        env,
        "AGENTOS_VIRTUAL_PROCESS_PPID",
        guest_runtime.virtual_ppid,
    );
}

fn build_wasm_runner_module_source(
    import_cache: &NodeImportCache,
    internal_env: &BTreeMap<String, String>,
    warmup_metrics: Option<&[u8]>,
    managed_kernel_host: bool,
) -> Result<String, WasmExecutionError> {
    let runner_source = transformed_wasm_runner_source(import_cache)?;
    let bootstrap = build_wasm_runner_bootstrap(internal_env, warmup_metrics);
    let bootstrap = format!(
        "{}\n{bootstrap}",
        managed_kernel_host_bootstrap(managed_kernel_host)
    );
    Ok(insert_wasm_runner_bootstrap(&runner_source, &bootstrap))
}

fn transformed_wasm_runner_source(
    import_cache: &NodeImportCache,
) -> Result<String, WasmExecutionError> {
    if std::env::var(WASM_RUNNER_NO_CACHE_ENV).as_deref() == Ok("1") {
        return read_transformed_wasm_runner_source(import_cache);
    }

    static RUNNER_SOURCE: OnceLock<Result<Arc<str>, Arc<str>>> = OnceLock::new();
    RUNNER_SOURCE
        .get_or_init(|| {
            read_transformed_wasm_runner_source(import_cache)
                .map(Arc::<str>::from)
                .map_err(|error| Arc::<str>::from(error.to_string()))
        })
        .as_ref()
        .map(|source| source.to_string())
        .map_err(|message| {
            WasmExecutionError::PrepareWarmPath(std::io::Error::other(message.to_string()))
        })
}

fn read_transformed_wasm_runner_source(
    import_cache: &NodeImportCache,
) -> Result<String, WasmExecutionError> {
    let runner_source = fs::read_to_string(import_cache.wasm_runner_path())
        .map_err(WasmExecutionError::PrepareWarmPath)?;
    Ok(runner_source.replace(
        "import { WASI } from 'node:wasi';\n",
        "const { WASI } = globalThis.__agentOSWasiModule;\n",
    ))
}

fn build_wasm_runner_userland_bundle(
    import_cache: &NodeImportCache,
) -> Result<String, WasmExecutionError> {
    if std::env::var(WASM_RUNNER_NO_CACHE_ENV).as_deref() == Ok("1") {
        return build_wasm_runner_userland_bundle_uncached(import_cache);
    }

    static USERLAND_BUNDLE: OnceLock<Result<Arc<str>, Arc<str>>> = OnceLock::new();
    USERLAND_BUNDLE
        .get_or_init(|| {
            build_wasm_runner_userland_bundle_uncached(import_cache)
                .map(Arc::<str>::from)
                .map_err(|error| Arc::<str>::from(error.to_string()))
        })
        .as_ref()
        .map(|bundle| bundle.to_string())
        .map_err(|message| {
            WasmExecutionError::PrepareWarmPath(std::io::Error::other(message.to_string()))
        })
}

fn build_wasm_runner_userland_bundle_uncached(
    import_cache: &NodeImportCache,
) -> Result<String, WasmExecutionError> {
    let runner_source = transformed_wasm_runner_source(import_cache)?;
    if runner_source
        .lines()
        .any(|line| line.trim_start().starts_with("import "))
    {
        return Err(WasmExecutionError::PrepareWarmPath(std::io::Error::other(
            "transformed wasm runner still contains an ESM import statement",
        )));
    }

    let mut bundle = build_wasm_runner_snapshot_prelude();
    bundle.push_str("\nglobalThis.__agentOSWasmRunnerRun = async function () {\n");
    bundle.push_str(&runner_source);
    bundle.push_str("\n};\n");
    Ok(bundle)
}

fn build_wasm_runner_snapshot_prelude() -> String {
    let bootstrap = build_wasm_runner_bootstrap(&BTreeMap::new(), None);
    let bootstrap = bootstrap
        .strip_prefix("const __agentOSWasmInternalEnv = {};\n")
        .unwrap_or(&bootstrap);
    bootstrap.replace(wasm_internal_env_merge_source(), "")
}

fn managed_kernel_host_bootstrap(managed_kernel_host: bool) -> String {
    format!(
        r#"Object.defineProperty(globalThis, "__agentOSManagedKernelHost", {{
  configurable: false,
  enumerable: false,
  value: {managed_kernel_host},
  writable: false,
}});"#
    )
}

fn build_wasm_snapshot_runner_inline_code(
    warmup_metrics: Option<&[u8]>,
    managed_kernel_host: bool,
) -> String {
    let warmup_emit = wasm_warmup_metrics_emit_source(warmup_metrics);
    let managed_kernel_host = managed_kernel_host_bootstrap(managed_kernel_host);
    format!(
        r#"{managed_kernel_host}
{warmup_emit}if (typeof process !== "undefined" && typeof globalThis.__agentOSProcessConfigEnv === "object") {{
  process.env = {{ ...(process.env || {{}}), ...globalThis.__agentOSProcessConfigEnv }};
}}
await globalThis.__agentOSWasmRunnerRun();"#
    )
}

fn build_wasm_runner_bootstrap(
    internal_env: &BTreeMap<String, String>,
    warmup_metrics: Option<&[u8]>,
) -> String {
    let internal_env_json =
        serde_json::to_string(internal_env).unwrap_or_else(|_| String::from("{}"));
    let warmup_emit = wasm_warmup_metrics_emit_source(warmup_metrics);
    let wasi_module_source = render_native_wasi_module_source();
    let env_merge_source = wasm_internal_env_merge_source();
    let wasm_sync_rpc_read_payload_bytes =
        max_cbor_byte_string_payload_bytes(WASM_PROCESS_SYNC_RPC_RESPONSE_BYTES);

    format!(
        r#"const __agentOSWasmInternalEnv = {internal_env_json};
	const __agentOSWasmSyncRpcReadPayloadBytes = {wasm_sync_rpc_read_payload_bytes};
	const __agentOSWasmEntropyLimitBytes = {WASM_SYNC_READ_LIMIT_BYTES};
const __agentOSRequireBuiltin = (specifier) => {{
  if (typeof globalThis.require === "function") {{
    return globalThis.require(specifier);
  }}
  if (typeof process?.getBuiltinModule === "function") {{
    return process.getBuiltinModule(specifier);
  }}
  throw new Error(`agentos WASM bootstrap cannot load ${{specifier}}`);
}};
{wasi_module_source}
{env_merge_source}
if (typeof globalThis !== "undefined") {{
  const __agentOSNormalizeBytes = (value) => {{
    if (value == null) {{
      return value;
    }}
    if (typeof Buffer !== "undefined" && Buffer.isBuffer(value)) {{
      return value;
    }}
    if (value instanceof Uint8Array) {{
      return Buffer.from(value);
    }}
    if (ArrayBuffer.isView(value)) {{
      return Buffer.from(value.buffer, value.byteOffset, value.byteLength);
    }}
    if (value instanceof ArrayBuffer) {{
      return Buffer.from(value);
    }}
    if (
      value &&
      typeof value === "object" &&
      value.__agentOSType === "bytes" &&
      typeof value.base64 === "string"
    ) {{
      return Buffer.from(value.base64, "base64");
    }}
    return value;
  }};
  const __agentOSWasmSyncRpc = {{
    callSync(method, args = []) {{
      switch (method) {{
        case "fs.fstatSync":
          return __agentOSRequireBuiltin("node:fs").fstatSync(...args);
        case "fs.lstatSync":
          return __agentOSRequireBuiltin("node:fs").lstatSync(...args);
        case "fs.statSync":
          return __agentOSRequireBuiltin("node:fs").statSync(...args);
        case "fs.chmodSync":
          return __agentOSRequireBuiltin("node:fs").chmodSync(...args);
        case "__kernel_stdio_write":
          if (typeof _kernelStdioWriteRaw === "undefined") {{
            throw new Error("agentos WASM kernel stdio bridge is unavailable");
          }}
          return _kernelStdioWriteRaw.applySync(void 0, args);
        case "__kernel_stdin_read":
          if (typeof _kernelStdinReadRaw === "undefined") {{
            throw new Error("agentos WASM kernel stdin bridge is unavailable");
          }}
          return _kernelStdinReadRaw.applySync(void 0, args);
        case "__kernel_poll":
          if (typeof _kernelPollRaw === "undefined") {{
            throw new Error("agentos WASM kernel poll bridge is unavailable");
          }}
          return _kernelPollRaw.applySync(void 0, args);
        case "__kernel_isatty":
          if (typeof _kernelIsattyRaw === "undefined") {{
            throw new Error("agentos WASM kernel isatty bridge is unavailable");
          }}
          return _kernelIsattyRaw.applySync(void 0, args);
        case "__kernel_flock_path":
          if (typeof _kernelFlockRaw === "undefined") {{
            throw new Error("agentos WASM kernel file-lock bridge is unavailable");
          }}
          return _kernelFlockRaw.applySync(void 0, args);
        case "__kernel_tty_size":
          if (typeof _kernelTtySizeRaw === "undefined") {{
            throw new Error("agentos WASM kernel tty size bridge is unavailable");
          }}
          return _kernelTtySizeRaw.applySync(void 0, args);
        case "__pty_set_raw_mode":
          if (typeof _ptySetRawMode === "undefined") {{
            throw new Error("agentos WASM PTY raw-mode bridge is unavailable");
          }}
          return _ptySetRawMode.applySync(void 0, args);
        case "child_process.spawn": {{
          if (typeof _childProcessSpawnStart === "undefined") {{
            throw new Error("agentos WASM child_process bridge is unavailable");
          }}
          const [request] = args;
          return _childProcessSpawnStart.applySync(void 0, [
            request?.command ?? "",
            JSON.stringify(request?.args ?? []),
            JSON.stringify(request?.options ?? {{}}),
          ]);
        }}
        case "child_process.poll":
          if (typeof _childProcessPoll === "undefined") {{
            throw new Error("agentos WASM child_process poll bridge is unavailable");
          }}
          return _childProcessPoll.applySync(void 0, args);
        case "child_process.kill":
          if (typeof _childProcessKill === "undefined") {{
            throw new Error("agentos WASM child_process kill bridge is unavailable");
          }}
          return _childProcessKill.applySync(void 0, args);
        case "process.kill":
          if (typeof _processKill === "undefined") {{
            throw new Error("agentos WASM process kill bridge is unavailable");
          }}
          return _processKill.applySync(void 0, args);
        case "process.exec":
          if (typeof _processExec === "undefined") {{
            throw new Error("agentos WASM process exec bridge is unavailable");
          }}
          return _processExec.applySync(void 0, args);
        case "process.exec_fd_image_commit":
          if (typeof _processExecFdImageCommit === "undefined") {{
            throw new Error("agentos WASM process fd image commit bridge is unavailable");
          }}
          return _processExecFdImageCommit.applySync(void 0, args);
        case "child_process.write_stdin": {{
          if (typeof _childProcessStdinWrite === "undefined") {{
            throw new Error("agentos WASM child_process stdin bridge is unavailable");
          }}
          const [childId, chunk] = args;
          return _childProcessStdinWrite.applySync(void 0, [
            childId,
            __agentOSNormalizeBytes(chunk),
          ]);
        }}
        case "child_process.close_stdin":
          if (typeof _childProcessStdinClose === "undefined") {{
            throw new Error("agentos WASM child_process stdin-close bridge is unavailable");
          }}
          return _childProcessStdinClose.applySync(void 0, args);
        case "net.connect":
          if (typeof _netSocketConnectRaw === "undefined") {{
            throw new Error("agentos WASM net.connect bridge is unavailable");
          }}
          return _netSocketConnectRaw.applySync(void 0, args);
        case "net.bind_unix":
          if (typeof _netBindUnixRaw === "undefined") {{
            throw new Error("agentos WASM net.bind_unix bridge is unavailable");
          }}
          return _netBindUnixRaw.applySync(void 0, args);
        case "net.bind_connected_unix":
          if (typeof _netBindConnectedUnixRaw === "undefined") {{
            throw new Error("agentos WASM net.bind_connected_unix bridge is unavailable");
          }}
          return _netBindConnectedUnixRaw.applySync(void 0, args);
        case "net.reserve_tcp_port":
          if (typeof _netReserveTcpPortRaw === "undefined") {{
            throw new Error("agentos WASM net.reserve_tcp_port bridge is unavailable");
          }}
          return _netReserveTcpPortRaw.applySync(void 0, args);
        case "net.release_tcp_port":
          if (typeof _netReleaseTcpPortRaw === "undefined") {{
            throw new Error("agentos WASM net.release_tcp_port bridge is unavailable");
          }}
          return _netReleaseTcpPortRaw.applySync(void 0, args);
        case "net.listen":
          if (typeof _netServerListenRaw === "undefined") {{
            throw new Error("agentos WASM net.listen bridge is unavailable");
          }}
          return _netServerListenRaw.applySync(void 0, args);
        case "net.server_accept":
          if (typeof _netServerAcceptRaw === "undefined") {{
            throw new Error("agentos WASM net.server_accept bridge is unavailable");
          }}
          return _netServerAcceptRaw.applySync(void 0, args);
        case "net.server_close":
          if (typeof _netServerCloseSyncRaw === "undefined") {{
            throw new Error("agentos WASM net.server_close bridge is unavailable");
          }}
          return _netServerCloseSyncRaw.applySync(void 0, args);
        case "net.poll":
          if (typeof _netSocketPollRaw === "undefined") {{
            throw new Error("agentos WASM net.poll bridge is unavailable");
          }}
          return _netSocketPollRaw.applySync(void 0, args);
        case "net.socket_read":
          if (typeof _netSocketReadRaw === "undefined") {{
            throw new Error("agentos WASM net.socket_read bridge is unavailable");
          }}
          return _netSocketReadRaw.applySync(void 0, args);
        case "net.socket_wait_connect":
          if (typeof _netSocketWaitConnectSyncRaw === "undefined") {{
            throw new Error("agentos WASM net.socket_wait_connect bridge is unavailable");
          }}
          return _netSocketWaitConnectSyncRaw.applySync(void 0, args);
        case "net.write":
          if (typeof _netSocketWriteSyncRaw === "undefined") {{
            throw new Error("agentos WASM net.write bridge is unavailable");
          }}
          return _netSocketWriteSyncRaw.applySync(void 0, [
            args[0],
            __agentOSNormalizeBytes(args[1]),
            args[2],
          ]);
        case "net.destroy":
          if (typeof _netSocketDestroyRaw === "undefined") {{
            throw new Error("agentos WASM net.destroy bridge is unavailable");
          }}
          return _netSocketDestroyRaw.applySync(void 0, args);
        case "net.socket_upgrade_tls":
          if (typeof _netSocketUpgradeTlsRaw === "undefined") {{
            throw new Error("agentos WASM TLS-upgrade bridge is unavailable");
          }}
          return _netSocketUpgradeTlsRaw.applySync(void 0, args);
        case "dgram.createSocket":
          if (typeof _dgramSocketCreateRaw === "undefined") {{
            throw new Error("agentos WASM dgram.createSocket bridge is unavailable");
          }}
          return _dgramSocketCreateRaw.applySync(void 0, args);
        case "dgram.bind":
          if (typeof _dgramSocketBindRaw === "undefined") {{
            throw new Error("agentos WASM dgram.bind bridge is unavailable");
          }}
          return _dgramSocketBindRaw.applySync(void 0, args);
        case "dgram.send": {{
          if (typeof _dgramSocketSendRaw === "undefined") {{
            throw new Error("agentos WASM dgram.send bridge is unavailable");
          }}
          const [socketId, chunk, options = {{}}] = args;
          return _dgramSocketSendRaw.applySync(void 0, [
            socketId,
            __agentOSNormalizeBytes(chunk),
            options,
          ]);
        }}
        case "dgram.poll":
          if (typeof _dgramSocketRecvRaw === "undefined") {{
            throw new Error("agentos WASM dgram.poll bridge is unavailable");
          }}
          const event = _dgramSocketRecvRaw.applySync(void 0, args);
          if (event && event.type === "message") {{
            const data = __agentOSNormalizeBytes(event.data);
            if (typeof Buffer !== "undefined" && Buffer.isBuffer(data)) {{
              return {{
                ...event,
                data: {{ base64: data.toString("base64") }},
              }};
            }}
          }}
          if (
            event &&
            event.type === "message" &&
            event.data &&
            typeof event.data === "object" &&
            typeof event.data.base64 === "string"
          ) {{
            return {{
              ...event,
              data: {{ base64: event.data.base64 }},
            }};
          }}
          return event;
        case "dgram.close":
          if (typeof _dgramSocketCloseRaw === "undefined") {{
            throw new Error("agentos WASM dgram.close bridge is unavailable");
          }}
          return _dgramSocketCloseRaw.applySync(void 0, args);
        case "dgram.address":
          if (typeof _dgramSocketAddressRaw === "undefined") {{
            throw new Error("agentos WASM dgram.address bridge is unavailable");
          }}
          return _dgramSocketAddressRaw.applySync(void 0, args);
        case "dgram.setBufferSize":
          if (typeof _dgramSocketSetBufferSizeRaw === "undefined") {{
            throw new Error("agentos WASM dgram.setBufferSize bridge is unavailable");
          }}
          return _dgramSocketSetBufferSizeRaw.applySync(void 0, args);
        case "dgram.getBufferSize":
          if (typeof _dgramSocketGetBufferSizeRaw === "undefined") {{
            throw new Error("agentos WASM dgram.getBufferSize bridge is unavailable");
          }}
          return _dgramSocketGetBufferSizeRaw.applySync(void 0, args);
        case "dns.lookup":
          if (typeof _networkDnsLookupSyncRaw === "undefined") {{
            throw new Error("agentos WASM dns.lookup bridge is unavailable");
          }}
          return _networkDnsLookupSyncRaw.applySync(void 0, args);
        case "process.signal_state": {{
          if (typeof _processSignalState === "undefined") {{
            throw new Error("agentos WASM signal-state bridge is unavailable");
          }}
          const [signal, action = "default", maskJson = "[]", flags = 0] = args;
          return _processSignalState.applySyncPromise(void 0, [
            signal,
            action,
            maskJson,
            flags,
          ]);
        }}
        case "process.take_signal":
          if (typeof _processTakeSignal === "undefined") {{
            throw new Error("agentos WASM signal-drain bridge is unavailable");
          }}
          return _processTakeSignal.applySync(void 0, args);
        case "process.exec_image_open":
        case "process.exec_image_open_fd":
        case "process.exec_image_read":
        case "process.exec_image_close":
        case "process.image":
        case "process.getpgid":
        case "process.getrlimit":
        case "process.setrlimit":
        case "process.getuid":
        case "process.getgid":
        case "process.geteuid":
        case "process.getegid":
        case "process.getresuid":
        case "process.getresgid":
        case "process.getgroups":
        case "process.getpwuid":
        case "process.getpwnam":
        case "process.getpwent":
        case "process.getgrgid":
        case "process.getgrnam":
        case "process.getgrent":
        case "process.setuid":
        case "process.seteuid":
        case "process.setreuid":
        case "process.setresuid":
        case "process.setgid":
        case "process.setegid":
        case "process.setregid":
        case "process.setresgid":
        case "process.setgroups":
        case "process.umask":
        case "process.clock_time":
        case "process.clock_resolution":
        case "process.sleep":
        case "process.system_identity":
        case "process.signal_begin":
        case "process.signal_end":
        case "process.signal_mask":
        case "process.signal_mask_scope_begin":
        case "process.signal_mask_scope_end":
        case "__kernel_tcgetattr":
        case "__kernel_tcsetattr":
        case "__kernel_tcgetpgrp":
        case "__kernel_tcsetpgrp":
        case "__kernel_tcgetsid":
        case "__kernel_tty_set_size":
        case "fs.accessSync":
        case "fs.blockingIoTimeoutMsSync":
        case "fs.chmodForProcessSync":
        case "fs.chownSync":
        case "fs.collapseRangeSync":
        case "fs.fallocateSync":
        case "fs.fiemapSync":
        case "fs.fiemapAtSync":
        case "fs.fgetxattrSync":
        case "fs.flistxattrSync":
        case "fs.fsetxattrSync":
        case "fs.fremovexattrSync":
        case "fs.getxattrSync":
        case "fs.insertRangeSync":
        case "fs.lchownSync":
        case "fs.linkFdSync":
        case "fs.listxattrSync":
        case "fs.mknodSync":
        case "fs.namedFifoPeerReadySync":
        case "fs.openTmpfileSync":
        case "fs.punchHoleSync":
        case "fs.remountSync":
        case "fs.removexattrSync":
        case "fs.renameAt2Sync":
        case "fs.setxattrSync":
        case "fs.statfsSync":
        case "fs.truncateForProcessSync":
        case "fs.zeroRangeSync":
        case "process.setpgid":
        case "process.waitpid_transition":
        case "process.waitpid":
        case "process.itimer_real":
        case "process.fd_pipe":
        case "process.fd_open":
        case "process.path_open_at":
        case "process.path_mkdir_at":
        case "process.path_stat_at":
        case "process.path_chmod_at":
        case "process.path_utimes_at":
        case "process.path_chown_at":
        case "process.path_link_at":
        case "process.path_readlink_at":
        case "process.path_remove_dir_at":
        case "process.path_rename_at":
        case "process.path_symlink_at":
        case "process.path_unlink_at":
        case "process.random_get":
        case "process.fd_snapshot":
        case "process.hostnet_fd_open":
        case "process.hostnet_bind":
        case "process.hostnet_connect":
        case "process.hostnet_listen":
        case "process.hostnet_accept":
        case "process.hostnet_validate":
        case "process.hostnet_recv":
        case "process.hostnet_send":
        case "process.hostnet_local_address":
        case "process.hostnet_peer_address":
        case "process.hostnet_get_option":
        case "process.hostnet_set_option":
        case "process.hostnet_poll":
        case "process.hostnet_tls_connect":
        case "process.posix_poll":
        case "process.fd_description_identity":
        case "process.fd_description_alias_count":
        case "process.fd_preopens":
        case "process.fd_preopen":
        case "process.fd_read":
        case "process.fd_pread":
        case "process.fd_write":
        case "process.fd_pwrite":
        case "process.fd_sync":
        case "process.fd_datasync":
        case "process.fd_readdir":
        case "process.fd_close":
        case "process.fd_closefrom":
        case "process.fd_stat":
        case "process.fd_filestat":
        case "process.fd_chmod":
        case "process.fd_chown":
        case "process.fd_truncate":
        case "process.fd_set_flags":
        case "process.fd_getfd":
        case "process.fd_setfd":
        case "process.fd_flock":
        case "process.fd_record_lock":
        case "process.fd_record_lock_cancel":
        case "process.fd_dup":
        case "process.fd_move":
        case "process.fd_dup2":
        case "process.fd_dup_min":
        case "process.fd_seek":
        case "process.fd_path":
        case "process.fd_chdir_path":
        case "process.fd_socketpair":
        case "process.pty_open":
        case "process.fd_sendmsg_rights":
        case "process.fd_recvmsg_rights":
        case "process.fd_socket_shutdown":
        case "dns.resolveRawRr":
          if (typeof _processWasmSyncRpc === "undefined") {{
            throw new Error("agentos WASM process-syscall bridge is unavailable");
          }}
          return _processWasmSyncRpc.applySync(void 0, [method, ...args]);
        default:
          throw new Error(`agentos WASM sync RPC method not implemented in V8 runtime: ${{method}}`);
      }}
    }},
    async call(method, args = []) {{
      return this.callSync(method, args);
    }},
  }};
  Object.defineProperty(globalThis, "__agentOSSyncRpc", {{
    configurable: true,
    enumerable: false,
    value: __agentOSWasmSyncRpc,
    writable: true,
  }});
}}
{warmup_emit}"#
    )
}

fn max_cbor_byte_string_payload_bytes(encoded_limit: usize) -> usize {
    // CBOR byte-string lengths use 1 byte inline through 23, then 2/3/5/9-byte
    // headers for u8/u16/u32/u64 lengths. Select the largest payload whose
    // encoded representation remains within the bridge response payload cap.
    for payload_bytes in (encoded_limit.saturating_sub(9)..=encoded_limit).rev() {
        let header_bytes = if payload_bytes <= 23 {
            1
        } else if u8::try_from(payload_bytes).is_ok() {
            2
        } else if u16::try_from(payload_bytes).is_ok() {
            3
        } else if u32::try_from(payload_bytes).is_ok() {
            5
        } else {
            9
        };
        if payload_bytes
            .checked_add(header_bytes)
            .is_some_and(|encoded_bytes| encoded_bytes <= encoded_limit)
        {
            return payload_bytes;
        }
    }
    0
}

fn wasm_warmup_metrics_emit_source(warmup_metrics: Option<&[u8]>) -> String {
    let warmup_metrics_json = warmup_metrics.map(|bytes| {
        serde_json::to_string(&String::from_utf8_lossy(bytes).to_string())
            .unwrap_or_else(|_| String::from("\"\""))
    });
    warmup_metrics_json
        .map(|metrics| {
            format!(
                "if (typeof process?.stderr?.write === \"function\") {{\n  process.stderr.write({metrics});\n}}\n"
            )
        })
        .unwrap_or_default()
}

fn wasm_internal_env_merge_source() -> &'static str {
    r#"if (typeof process !== "undefined") {
  process.env = { ...(process.env || {}), ...__agentOSWasmInternalEnv };
}
"#
}

fn render_native_wasi_module_source() -> &'static str {
    static SOURCE: OnceLock<String> = OnceLock::new();
    SOURCE.get_or_init(|| {
        NODE_WASI_MODULE_SOURCE.replace(
            "__AGENTOS_WASM_SYNC_READ_LIMIT_BYTES__",
            &WASM_SYNC_READ_LIMIT_BYTES.to_string(),
        )
    })
}

fn insert_wasm_runner_bootstrap(source: &str, bootstrap: &str) -> String {
    let mut insert_at = 0usize;
    let mut saw_import = false;
    for line in source.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with("import ") || (saw_import && trimmed.is_empty()) {
            insert_at += line.len();
            saw_import = saw_import || trimmed.starts_with("import ");
            continue;
        }
        break;
    }

    format!(
        "{}{}{}",
        &source[..insert_at],
        bootstrap,
        &source[insert_at..]
    )
}

struct WasmPrewarmOptions<'a> {
    frozen_time_ms: u128,
    timeout: Duration,
    runtime: &'a RuntimeContext,
}

fn prewarm_wasm_path(
    import_cache: &NodeImportCache,
    javascript_engine: &mut JavascriptExecutionEngine,
    javascript_context_id: &str,
    resolved_module: &ResolvedWasmModule,
    request: &StartWasmExecutionRequest,
    options: WasmPrewarmOptions<'_>,
) -> Result<Option<Vec<u8>>, WasmExecutionError> {
    let debug_enabled = env_flag_enabled(&request.env, WASM_WARMUP_DEBUG_ENV);
    let marker_contents = warmup_marker_contents(resolved_module);
    let marker_path = warmup_marker_path(
        import_cache.prewarm_marker_dir(),
        "wasm-runner-prewarm",
        WASM_WARMUP_MARKER_VERSION,
        &marker_contents,
    );

    if let Ok(metadata) = fs::metadata(&resolved_module.resolved_path) {
        if metadata.len() > MAX_SYNC_WASM_PREWARM_MODULE_BYTES {
            return Ok(warmup_metrics_line(
                debug_enabled,
                false,
                "skipped-large-module",
                import_cache,
                &resolved_module.specifier,
            ));
        }
    }

    if marker_path.exists() {
        return Ok(warmup_metrics_line(
            debug_enabled,
            false,
            "cached",
            import_cache,
            &resolved_module.specifier,
        ));
    }

    let mut prewarm_execution = start_wasm_javascript_execution(
        javascript_engine,
        options.runtime,
        import_cache,
        javascript_context_id,
        resolved_module,
        request,
        WasmJavascriptExecutionOptions {
            frozen_time_ms: options.frozen_time_ms,
            prewarm_only: true,
            warmup_metrics: None,
            defer_execute: false,
        },
    )
    .map_err(|error| match error {
        WasmExecutionError::Spawn(err) => WasmExecutionError::WarmupSpawn(err),
        other => other,
    })?;
    let mut internal_sync_rpc = WasmInternalSyncRpc {
        module_guest_paths: wasm_guest_module_paths(&resolved_module.specifier, &request.env),
        module_host_path: resolved_module.resolved_path.clone(),
        guest_cwd: wasm_guest_cwd(&request.env),
        host_cwd: request.cwd.clone(),
        sandbox_root: wasm_sandbox_root(&request.env),
        guest_path_mappings: wasm_guest_path_mappings(request),
        route_fs_through_sidecar: false,
        next_fd: 64,
        open_files: BTreeMap::new(),
        pending_events: WasmEventQueue::default(),
    };
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let started = Instant::now();

    loop {
        let poll_timeout = options.timeout.saturating_sub(started.elapsed());
        if poll_timeout.is_zero() {
            if let Err(error) = prewarm_execution.terminate() {
                eprintln!(
                    "ERR_AGENTOS_WASM_PREWARM_TERMINATE: timed-out prewarm did not terminate cleanly: {error}"
                );
            }
            return Err(WasmExecutionError::WarmupTimeout(options.timeout));
        }

        match prewarm_execution
            .poll_event_blocking(poll_timeout)
            .map_err(map_javascript_error)?
        {
            Some(JavascriptExecutionEvent::Stdout(chunk)) => {
                append_wasm_captured_output(&mut stdout, &chunk, "stdout")?;
            }
            Some(JavascriptExecutionEvent::Stderr(chunk)) => {
                append_wasm_captured_output(&mut stderr, &chunk, "stderr")?;
            }
            Some(JavascriptExecutionEvent::Exited(exit_code)) => {
                if exit_code != 0 {
                    return Err(WasmExecutionError::WarmupFailed {
                        exit_code,
                        stderr: String::from_utf8_lossy(&stderr).into_owned(),
                    });
                }
                break;
            }
            Some(JavascriptExecutionEvent::SyncRpcRequest(sync_request)) => {
                let handled = handle_internal_wasm_sync_rpc_request(
                    &mut prewarm_execution,
                    &mut internal_sync_rpc,
                    &sync_request,
                )?;
                if !handled {
                    return Err(WasmExecutionError::WarmupFailed {
                        exit_code: 1,
                        stderr: format!(
                            "unexpected WebAssembly prewarm sync RPC request {} {} {:?}",
                            sync_request.id, sync_request.method, sync_request.args
                        ),
                    });
                }
            }
            Some(JavascriptExecutionEvent::SignalState { .. }) => {}
            None => {
                if let Err(error) = prewarm_execution.terminate() {
                    eprintln!(
                        "ERR_AGENTOS_WASM_PREWARM_TERMINATE: timed-out prewarm did not terminate cleanly: {error}"
                    );
                }
                return Err(WasmExecutionError::WarmupTimeout(options.timeout));
            }
        }
    }

    let _ = stdout;
    fs::write(&marker_path, marker_contents).map_err(WasmExecutionError::PrepareWarmPath)?;
    Ok(warmup_metrics_line(
        debug_enabled,
        true,
        "executed",
        import_cache,
        &resolved_module.specifier,
    ))
}

async fn prewarm_wasm_path_async(
    import_cache: &NodeImportCache,
    javascript_engine: &mut JavascriptExecutionEngine,
    javascript_context_id: &str,
    resolved_module: &ResolvedWasmModule,
    request: &StartWasmExecutionRequest,
    options: WasmPrewarmOptions<'_>,
) -> Result<Option<Vec<u8>>, WasmExecutionError> {
    let debug_enabled = env_flag_enabled(&request.env, WASM_WARMUP_DEBUG_ENV);
    let marker_contents = warmup_marker_contents(resolved_module);
    let marker_path = warmup_marker_path(
        import_cache.prewarm_marker_dir(),
        "wasm-runner-prewarm",
        WASM_WARMUP_MARKER_VERSION,
        &marker_contents,
    );

    if let Ok(metadata) = fs::metadata(&resolved_module.resolved_path) {
        if metadata.len() > MAX_SYNC_WASM_PREWARM_MODULE_BYTES {
            return Ok(warmup_metrics_line(
                debug_enabled,
                false,
                "skipped-large-module",
                import_cache,
                &resolved_module.specifier,
            ));
        }
    }

    if marker_path.exists() {
        return Ok(warmup_metrics_line(
            debug_enabled,
            false,
            "cached",
            import_cache,
            &resolved_module.specifier,
        ));
    }

    let mut prewarm_execution = start_wasm_javascript_execution(
        javascript_engine,
        options.runtime,
        import_cache,
        javascript_context_id,
        resolved_module,
        request,
        WasmJavascriptExecutionOptions {
            frozen_time_ms: options.frozen_time_ms,
            prewarm_only: true,
            warmup_metrics: None,
            defer_execute: false,
        },
    )
    .map_err(|error| match error {
        WasmExecutionError::Spawn(err) => WasmExecutionError::WarmupSpawn(err),
        other => other,
    })?;
    let mut internal_sync_rpc = WasmInternalSyncRpc {
        module_guest_paths: wasm_guest_module_paths(&resolved_module.specifier, &request.env),
        module_host_path: resolved_module.resolved_path.clone(),
        guest_cwd: wasm_guest_cwd(&request.env),
        host_cwd: request.cwd.clone(),
        sandbox_root: wasm_sandbox_root(&request.env),
        guest_path_mappings: wasm_guest_path_mappings(request),
        route_fs_through_sidecar: false,
        next_fd: 64,
        open_files: BTreeMap::new(),
        pending_events: WasmEventQueue::default(),
    };
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let started = Instant::now();

    loop {
        let poll_timeout = options.timeout.saturating_sub(started.elapsed());
        if poll_timeout.is_zero() {
            if let Err(error) = prewarm_execution.terminate() {
                eprintln!(
                    "ERR_AGENTOS_WASM_PREWARM_TERMINATE: timed-out prewarm did not terminate cleanly: {error}"
                );
            }
            return Err(WasmExecutionError::WarmupTimeout(options.timeout));
        }

        match prewarm_execution
            .poll_event(poll_timeout)
            .await
            .map_err(map_javascript_error)?
        {
            Some(JavascriptExecutionEvent::Stdout(chunk)) => {
                append_wasm_captured_output(&mut stdout, &chunk, "stdout")?;
            }
            Some(JavascriptExecutionEvent::Stderr(chunk)) => {
                append_wasm_captured_output(&mut stderr, &chunk, "stderr")?;
            }
            Some(JavascriptExecutionEvent::Exited(exit_code)) => {
                if exit_code != 0 {
                    return Err(WasmExecutionError::WarmupFailed {
                        exit_code,
                        stderr: String::from_utf8_lossy(&stderr).into_owned(),
                    });
                }
                break;
            }
            Some(JavascriptExecutionEvent::SyncRpcRequest(sync_request)) => {
                let handled = handle_internal_wasm_sync_rpc_request(
                    &mut prewarm_execution,
                    &mut internal_sync_rpc,
                    &sync_request,
                )?;
                if !handled {
                    return Err(WasmExecutionError::WarmupFailed {
                        exit_code: 1,
                        stderr: format!(
                            "unexpected WebAssembly prewarm sync RPC request {} {} {:?}",
                            sync_request.id, sync_request.method, sync_request.args
                        ),
                    });
                }
            }
            Some(JavascriptExecutionEvent::SignalState { .. }) => {}
            None => {
                if let Err(error) = prewarm_execution.terminate() {
                    eprintln!(
                        "ERR_AGENTOS_WASM_PREWARM_TERMINATE: timed-out prewarm did not terminate cleanly: {error}"
                    );
                }
                return Err(WasmExecutionError::WarmupTimeout(options.timeout));
            }
        }
    }

    let _ = stdout;
    fs::write(&marker_path, marker_contents).map_err(WasmExecutionError::PrepareWarmPath)?;
    Ok(warmup_metrics_line(
        debug_enabled,
        true,
        "executed",
        import_cache,
        &resolved_module.specifier,
    ))
}

fn wasm_guest_module_paths(specifier: &str, env: &BTreeMap<String, String>) -> Vec<String> {
    let mut candidates = Vec::new();
    candidates.push(specifier.to_owned());

    if specifier.starts_with('/') {
        candidates.push(normalize_guest_path(specifier));
        candidates.extend(mapped_guest_paths_for_host_path(Path::new(specifier), env));
    } else if !specifier.starts_with("file:") {
        let guest_cwd = wasm_guest_cwd(env);
        candidates.push(join_guest_path(&guest_cwd, specifier));
    }

    candidates.sort();
    candidates.dedup();
    candidates
}

fn wasm_guest_cwd(env: &BTreeMap<String, String>) -> String {
    env.get("PWD")
        .filter(|value| value.starts_with('/'))
        .cloned()
        .or_else(|| {
            env.get("HOME")
                .filter(|value| value.starts_with('/'))
                .cloned()
        })
        .unwrap_or_else(|| String::from(DEFAULT_WASM_GUEST_HOME))
}

fn mapped_guest_paths_for_host_path(
    host_path: &Path,
    env: &BTreeMap<String, String>,
) -> Vec<String> {
    if !host_path.is_absolute() {
        return Vec::new();
    }

    let mappings = env
        .get("AGENTOS_GUEST_PATH_MAPPINGS")
        .and_then(|value| serde_json::from_str::<Vec<Value>>(value).ok())
        .unwrap_or_default();

    let mut candidates = Vec::new();
    for mapping in mappings {
        let Some(guest_root) = mapping.get("guestPath").and_then(Value::as_str) else {
            continue;
        };
        let Some(host_root) = mapping.get("hostPath").and_then(Value::as_str) else {
            continue;
        };
        let host_root = Path::new(host_root);

        if let Ok(suffix) = host_path.strip_prefix(host_root) {
            candidates.push(join_guest_path(
                guest_root,
                &suffix.to_string_lossy().replace('\\', "/"),
            ));
            continue;
        }

        let Ok(real_host_root) = host_root.canonicalize() else {
            continue;
        };
        if let Ok(suffix) = host_path.strip_prefix(&real_host_root) {
            candidates.push(join_guest_path(
                guest_root,
                &suffix.to_string_lossy().replace('\\', "/"),
            ));
        }
    }

    candidates
}

fn normalize_guest_path(path: &str) -> String {
    join_guest_path("/", path)
}

fn join_guest_path(base: &str, suffix: &str) -> String {
    let mut segments = Vec::new();
    let mut absolute = false;
    for part in [base, suffix] {
        if part.starts_with('/') {
            absolute = true;
        }
        for segment in part.split('/') {
            match segment {
                "" | "." => {}
                ".." => {
                    let _ = segments.pop();
                }
                value => segments.push(value),
            }
        }
    }

    let joined = segments.join("/");
    if absolute {
        if joined.is_empty() {
            String::from("/")
        } else {
            format!("/{joined}")
        }
    } else if joined.is_empty() {
        String::from(".")
    } else {
        joined
    }
}

fn module_path(
    context: &WasmContext,
    request: &StartWasmExecutionRequest,
) -> Result<String, WasmExecutionError> {
    match context.module_path.as_deref() {
        Some(module_path) => Ok(module_path.to_owned()),
        None => request
            .argv
            .first()
            .cloned()
            .ok_or(WasmExecutionError::MissingModulePath),
    }
}

pub(super) fn guest_visible_wasm_env(env: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut guest_env = env
        .iter()
        .filter(|(key, _)| !is_internal_wasm_guest_env_key(key))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<BTreeMap<_, _>>();
    let guest_cwd = wasm_guest_cwd(env);
    let guest_home = guest_env
        .get("HOME")
        .filter(|value| value.starts_with('/'))
        .cloned()
        .unwrap_or_else(|| guest_cwd.clone());

    guest_env
        .entry(String::from("HOME"))
        .or_insert_with(|| guest_home.clone());
    guest_env
        .entry(String::from("PWD"))
        .or_insert_with(|| guest_cwd);
    guest_env
        .entry(String::from("USER"))
        .or_insert_with(|| String::from(DEFAULT_WASM_GUEST_USER));
    guest_env
        .entry(String::from("LOGNAME"))
        .or_insert_with(|| String::from(DEFAULT_WASM_GUEST_USER));
    guest_env
        .entry(String::from("SHELL"))
        .or_insert_with(|| String::from(DEFAULT_WASM_GUEST_SHELL));
    guest_env
        .entry(String::from("PATH"))
        .or_insert_with(|| String::from(DEFAULT_WASM_GUEST_PATH));
    guest_env
        .entry(String::from("TMPDIR"))
        .or_insert_with(|| String::from("/tmp"));
    guest_env
}

fn wasm_guest_path_mappings(request: &StartWasmExecutionRequest) -> Vec<WasmGuestPathMapping> {
    let guest_cwd = wasm_guest_cwd(&request.env);
    let mut mappings = request
        .env
        .get("AGENTOS_GUEST_PATH_MAPPINGS")
        .and_then(|value| serde_json::from_str::<Vec<Value>>(value).ok())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|mapping| {
            Some(WasmGuestPathMapping {
                guest_path: mapping.get("guestPath")?.as_str()?.to_owned(),
                host_path: PathBuf::from(mapping.get("hostPath")?.as_str()?),
                read_only: mapping
                    .get("readOnly")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            })
        })
        .collect::<Vec<_>>();

    if let Some(sandbox_root) = wasm_sandbox_root(&request.env) {
        push_wasm_guest_path_mapping(&mut mappings, String::from("/"), sandbox_root);
    }
    push_wasm_guest_path_mapping(&mut mappings, guest_cwd, request.cwd.clone());
    push_wasm_guest_path_mapping(
        &mut mappings,
        String::from("/workspace"),
        request.cwd.clone(),
    );
    mappings.sort_by_key(|mapping| std::cmp::Reverse(mapping.guest_path.len()));
    mappings
}

fn wasm_sandbox_root(env: &BTreeMap<String, String>) -> Option<PathBuf> {
    env.get(WASM_SANDBOX_ROOT_ENV)
        .filter(|value| Path::new(value.as_str()).is_absolute())
        .map(PathBuf::from)
}

fn push_wasm_guest_path_mapping(
    mappings: &mut Vec<WasmGuestPathMapping>,
    guest_path: String,
    host_path: PathBuf,
) {
    if guest_path.is_empty() || !guest_path.starts_with('/') {
        return;
    }
    if mappings
        .iter()
        .any(|mapping| mapping.guest_path == guest_path)
    {
        return;
    }
    mappings.push(WasmGuestPathMapping {
        guest_path,
        host_path,
        read_only: false,
    });
}

fn encode_wasm_guest_path_mappings(mappings: &[WasmGuestPathMapping]) -> String {
    serde_json::to_string(
        &mappings
            .iter()
            .map(|mapping| {
                json!({
                    "guestPath": mapping.guest_path,
                    "hostPath": mapping.host_path.to_string_lossy(),
                    "readOnly": mapping.read_only,
                })
            })
            .collect::<Vec<_>>(),
    )
    .unwrap_or_else(|_| String::from("[]"))
}

fn is_internal_wasm_guest_env_key(key: &str) -> bool {
    key.starts_with("AGENTOS_") || key.starts_with("NODE_SYNC_RPC_")
}

fn warmup_marker_contents(resolved_module: &ResolvedWasmModule) -> String {
    let module_fingerprint = file_fingerprint(&resolved_module.resolved_path);

    [
        env!("CARGO_PKG_NAME").to_string(),
        env!("CARGO_PKG_VERSION").to_string(),
        WASM_WARMUP_MARKER_VERSION.to_string(),
        resolved_module.specifier.clone(),
        resolved_module.resolved_path.display().to_string(),
        module_fingerprint,
    ]
    .join("\n")
}

fn warmup_metrics_line(
    debug_enabled: bool,
    executed: bool,
    reason: &str,
    import_cache: &NodeImportCache,
    module_specifier: &str,
) -> Option<Vec<u8>> {
    if !debug_enabled {
        return None;
    }

    Some(
        format!(
            "{WASM_WARMUP_METRICS_PREFIX}{{\"executed\":{},\"reason\":{},\"modulePath\":{},\"compileCacheDir\":{}}}\n",
            if executed { "true" } else { "false" },
            encode_json_string(reason),
            encode_json_string(module_specifier),
            encode_json_string(&import_cache.shared_compile_cache_dir().display().to_string()),
        )
        .into_bytes(),
    )
}

fn resolve_wasm_wall_clock_limit(
    request: &StartWasmExecutionRequest,
) -> Result<Option<Duration>, WasmExecutionError> {
    // Wall-clock time is an independent, opt-in elapsed deadline. With no
    // explicit value there is no outer timeout, matching interactive command
    // semantics.
    // The guest stays bounded by default anyway: the wasm module executes on
    // the runner isolate's thread, whose TRUE-CPU budget (the V8 CPU-time
    // watchdog, default 30s ACTIVE CPU) terminates an infinite-loop module
    // while letting an idle interactive guest (vim blocked in a kernel input
    // wait) live indefinitely, exactly like native Linux.
    Ok(request
        .limits
        .wall_clock_limit_ms
        .map(Duration::from_millis))
}

fn reject_v8_deterministic_fuel(
    request: &StartWasmExecutionRequest,
) -> Result<(), WasmExecutionError> {
    match request.limits.deterministic_fuel {
        Some(fuel) => Err(WasmExecutionError::DeterministicFuelUnsupported { fuel }),
        None => Ok(()),
    }
}

/// Resolve the per-execution WASM stack cap from the typed wire limit. The V8
/// runner currently has no enforceable per-module stack lever, so every configured
/// value fails closed with a typed error that names the requested bound.
fn resolve_wasm_stack_limit_bytes(
    request: &StartWasmExecutionRequest,
) -> Result<Option<u64>, WasmExecutionError> {
    match request.limits.max_stack_bytes {
        Some(0) => Err(WasmExecutionError::InvalidLimit(String::from(
            "wasm max stack bytes must be greater than zero",
        ))),
        Some(limit) => Err(WasmExecutionError::InvalidLimit(format!(
            "configured wasm max stack byte limit {limit} cannot be enforced by the V8 runner"
        ))),
        None => Ok(None),
    }
}

fn resolve_wasm_prewarm_timeout(
    request: &StartWasmExecutionRequest,
) -> Result<Duration, WasmExecutionError> {
    Ok(Duration::from_millis(
        request
            .limits
            .prewarm_timeout_ms
            .filter(|value| *value > 0)
            .unwrap_or(DEFAULT_WASM_PREWARM_TIMEOUT_MS),
    ))
}

fn resolve_wasm_module(
    context: &WasmContext,
    request: &StartWasmExecutionRequest,
) -> Result<ResolvedWasmModule, WasmExecutionError> {
    let specifier = module_path(context, request)?;
    let resolved_path = resolved_module_path(&specifier, &request.cwd);
    Ok(ResolvedWasmModule {
        specifier,
        resolved_path,
    })
}

fn resolved_module_path(specifier: &str, cwd: &Path) -> PathBuf {
    resolve_path_like_specifier(cwd, specifier)
        .map(|path| path.canonicalize().unwrap_or(path))
        .unwrap_or_else(|| PathBuf::from(specifier))
}

/// Sniff the first bytes of a resolved WebAssembly module and refuse to hand
/// non-`\0asm` content (such as `#!/bin/sh` shell shims) to `WebAssembly.compile`.
///
/// Without this guard, resolving a `node_modules/.bin/<cmd>` shell shim against
/// the WASM path produces an opaque `CompileError: WebAssembly.Module(): expected
/// magic word 00 61 73 6d, found 23 21 2f 62 @+0` during prewarm. That error
/// cascades through hundreds of downstream tests as `ERR_AGENTOS_NODE_SYNC_RPC:
/// WebAssembly warmup exited with status 1: CompileError`, which hides the real
/// command-resolution bug that fed the shim to the WASM engine in the first
/// place. A typed [`WasmExecutionError::NonWasmBinary`] instead names the resolved
/// path and preserves the header bytes so callers can route through the Node
/// dispatch path or surface a clear error.
fn verify_wasm_module_header(
    resolved_module: &ResolvedWasmModule,
) -> Result<(), WasmExecutionError> {
    let resolved_path = &resolved_module.resolved_path;
    let metadata = fs::metadata(resolved_path).map_err(|error| {
        WasmExecutionError::InvalidModule(format!(
            "failed to stat {}: {error}",
            resolved_path.display()
        ))
    })?;
    if metadata.len() > MAX_WASM_MODULE_FILE_BYTES {
        return Err(WasmExecutionError::InvalidModule(format!(
            "module file size of {} bytes exceeds the configured parser cap of {} bytes",
            metadata.len(),
            MAX_WASM_MODULE_FILE_BYTES
        )));
    }

    let mut file = fs::File::open(resolved_path).map_err(|error| {
        WasmExecutionError::InvalidModule(format!(
            "failed to open {}: {error}",
            resolved_path.display()
        ))
    })?;
    let mut header = [0u8; 4];
    let bytes_read = file.read(&mut header).map_err(|error| {
        WasmExecutionError::InvalidModule(format!(
            "failed to read header of {}: {error}",
            resolved_path.display()
        ))
    })?;
    let header = &header[..bytes_read];
    if header == b"\0asm" {
        return Ok(());
    }

    let shell_shim = header.len() >= 2 && &header[..2] == b"#!";
    if let Some(format) = detect_native_binary_format(header) {
        return Err(WasmExecutionError::NativeBinaryNotSupported {
            path: resolved_path.clone(),
            header: header.to_vec(),
            format,
        });
    }

    Err(WasmExecutionError::NonWasmBinary {
        path: resolved_path.clone(),
        header: header.to_vec(),
        shell_shim,
    })
}

fn validate_module_profile(resolved_module: &ResolvedWasmModule) -> Result<(), WasmExecutionError> {
    let bytes = cached_wasm_module_bytes(&resolved_module.resolved_path)?;
    profile::validate_locked_profile(bytes.as_slice()).map_err(WasmExecutionError::Host)
}

fn detect_native_binary_format(header: &[u8]) -> Option<NativeBinaryFormat> {
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
    if header.len() >= 4 && MACH_O_MAGICS.iter().any(|magic| header[..4] == magic[..]) {
        return Some(NativeBinaryFormat::MachO);
    }

    None
}

fn warmup_guest_argv(
    resolved_module: &ResolvedWasmModule,
    request: &StartWasmExecutionRequest,
) -> Vec<String> {
    if !request.argv.is_empty() {
        return request.argv.clone();
    }

    vec![resolved_module.specifier.clone()]
}

fn wasm_memory_limit_bytes(
    request: &StartWasmExecutionRequest,
) -> Result<Option<u64>, WasmExecutionError> {
    Ok(request.limits.max_memory_bytes)
}

fn wasm_stack_limit_bytes(
    request: &StartWasmExecutionRequest,
) -> Result<Option<u64>, WasmExecutionError> {
    resolve_wasm_stack_limit_bytes(request)
}

#[cfg(test)]
fn wasm_memory_limit_pages(memory_limit_bytes: u64) -> Result<u32, WasmExecutionError> {
    let pages = memory_limit_bytes / WASM_PAGE_BYTES;
    u32::try_from(pages).map_err(|_| {
        WasmExecutionError::InvalidLimit(format!(
            "{WASM_MAX_MEMORY_BYTES_ENV}={memory_limit_bytes}: exceeds V8's wasm page limit range"
        ))
    })
}

/// Resolve the wasm runner isolate's V8 heap cap (MB): the typed per-VM limit if
/// set to a positive value, else the bounded default.
fn wasm_runner_heap_limit_mb(request: &StartWasmExecutionRequest) -> u32 {
    request
        .limits
        .runner_heap_limit_mb
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_WASM_RUNNER_HEAP_LIMIT_MB)
}

fn v8_warm_worker_count() -> usize {
    std::env::var("AGENTOS_V8_WARM_ISOLATES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(4)
}

fn validate_module_limits(
    resolved_module: &ResolvedWasmModule,
    request: &StartWasmExecutionRequest,
) -> Result<(), WasmExecutionError> {
    // Read the wire stack cap on every execution and fail closed when configured;
    // the V8 runner cannot currently enforce a per-module stack byte bound.
    let _stack_limit = resolve_wasm_stack_limit_bytes(request)?;

    let Some(memory_limit) = wasm_memory_limit_bytes(request)? else {
        return Ok(());
    };

    let resolved_path = &resolved_module.resolved_path;
    let metadata = fs::metadata(resolved_path).map_err(|error| {
        WasmExecutionError::InvalidModule(format!(
            "failed to stat {}: {error}",
            resolved_path.display()
        ))
    })?;
    if metadata.len() > MAX_WASM_MODULE_FILE_BYTES {
        return Err(WasmExecutionError::InvalidModule(format!(
            "module file size of {} bytes exceeds the configured parser cap of {} bytes",
            metadata.len(),
            MAX_WASM_MODULE_FILE_BYTES
        )));
    }
    let bytes = fs::read(resolved_path).map_err(|error| {
        WasmExecutionError::InvalidModule(format!(
            "failed to read {}: {error}",
            resolved_path.display()
        ))
    })?;
    let module_limits = extract_wasm_module_limits(&bytes)?;

    if module_limits.imports_memory {
        return Err(WasmExecutionError::InvalidModule(String::from(
            "configured WebAssembly memory limit does not support imported memories yet",
        )));
    }

    if let Some(initial_bytes) = module_limits.initial_memory_bytes {
        if initial_bytes > memory_limit {
            warn_limit_exhausted(
                TrackedLimit::WasmMemoryBytes,
                usize_saturating_from_u64(initial_bytes),
                usize_saturating_from_u64(memory_limit),
            );
            return Err(WasmExecutionError::InvalidModule(format!(
                "initial WebAssembly memory of {initial_bytes} bytes exceeds the configured limit of {memory_limit} bytes"
            )));
        }
    }

    match module_limits.maximum_memory_bytes {
        Some(maximum_bytes) if maximum_bytes > memory_limit => {
            warn_limit_exhausted(
                TrackedLimit::WasmMemoryBytes,
                usize_saturating_from_u64(maximum_bytes),
                usize_saturating_from_u64(memory_limit),
            );
            Err(WasmExecutionError::InvalidModule(format!(
                "WebAssembly memory maximum of {maximum_bytes} bytes exceeds the configured limit of {memory_limit} bytes"
            )))
        }
        Some(_) => Ok(()),
        None => Ok(()),
    }
}

fn duration_millis_saturating_usize(duration: Duration) -> usize {
    usize::try_from(duration.as_millis()).unwrap_or(usize::MAX)
}

fn usize_saturating_from_u64(value: u64) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

#[derive(Debug, Default)]
struct WasmModuleLimits {
    imports_memory: bool,
    initial_memory_bytes: Option<u64>,
    maximum_memory_bytes: Option<u64>,
}

fn extract_wasm_module_limits(bytes: &[u8]) -> Result<WasmModuleLimits, WasmExecutionError> {
    if bytes.len() < 8 || &bytes[..4] != b"\0asm" {
        return Err(WasmExecutionError::InvalidModule(String::from(
            "module is not a valid WebAssembly binary",
        )));
    }

    let mut offset = 8;
    let mut limits = WasmModuleLimits::default();

    while offset < bytes.len() {
        let section_id = bytes[offset];
        offset += 1;
        let section_size = read_varuint_usize(bytes, &mut offset, "section size")?;
        let section_end = offset.checked_add(section_size).ok_or_else(|| {
            WasmExecutionError::InvalidModule(String::from("section size overflow"))
        })?;
        if section_end > bytes.len() {
            return Err(WasmExecutionError::InvalidModule(String::from(
                "section extends past end of module",
            )));
        }

        match section_id {
            2 => {
                let mut cursor = offset;
                let import_count = read_varuint_usize(bytes, &mut cursor, "import count")?;
                if import_count > MAX_WASM_IMPORT_SECTION_ENTRIES {
                    return Err(WasmExecutionError::InvalidModule(format!(
                        "import section contains {import_count} entries, which exceeds the parser cap of {MAX_WASM_IMPORT_SECTION_ENTRIES}"
                    )));
                }
                for _ in 0..import_count {
                    skip_name(bytes, &mut cursor)?;
                    skip_name(bytes, &mut cursor)?;
                    let kind = read_byte(bytes, &mut cursor)?;
                    match kind {
                        0x02 => {
                            let _ = read_memory_limits(bytes, &mut cursor)?;
                            limits.imports_memory = true;
                        }
                        0x00 => {
                            let _ = read_varuint(bytes, &mut cursor)?;
                        }
                        0x01 => {
                            skip_table_type(bytes, &mut cursor)?;
                        }
                        0x03 => {
                            let _ = read_byte(bytes, &mut cursor)?;
                            let _ = read_byte(bytes, &mut cursor)?;
                        }
                        other => {
                            return Err(WasmExecutionError::InvalidModule(format!(
                                "unsupported import kind {other}"
                            )));
                        }
                    }
                }
            }
            5 => {
                let mut cursor = offset;
                let memory_count = read_varuint_usize(bytes, &mut cursor, "memory count")?;
                if memory_count > MAX_WASM_MEMORY_SECTION_ENTRIES {
                    return Err(WasmExecutionError::InvalidModule(format!(
                        "memory section contains {memory_count} entries, which exceeds the parser cap of {MAX_WASM_MEMORY_SECTION_ENTRIES}"
                    )));
                }
                if memory_count > 0 {
                    let (initial_pages, maximum_pages) = read_memory_limits(bytes, &mut cursor)?;
                    limits.initial_memory_bytes =
                        Some(initial_pages.saturating_mul(WASM_PAGE_BYTES));
                    limits.maximum_memory_bytes =
                        maximum_pages.map(|pages| pages.saturating_mul(WASM_PAGE_BYTES));
                }
            }
            _ => {}
        }

        offset = section_end;
    }

    Ok(limits)
}

fn read_memory_limits(
    bytes: &[u8],
    offset: &mut usize,
) -> Result<(u64, Option<u64>), WasmExecutionError> {
    let flags = read_varuint(bytes, offset)?;
    let initial = read_varuint(bytes, offset)?;
    let maximum = if flags & 0x01 != 0 {
        Some(read_varuint(bytes, offset)?)
    } else {
        None
    };
    Ok((initial, maximum))
}

fn skip_name(bytes: &[u8], offset: &mut usize) -> Result<(), WasmExecutionError> {
    let length = read_varuint_usize(bytes, offset, "name length")?;
    let end = offset
        .checked_add(length)
        .ok_or_else(|| WasmExecutionError::InvalidModule(String::from("name length overflow")))?;
    if end > bytes.len() {
        return Err(WasmExecutionError::InvalidModule(String::from(
            "name extends past end of module",
        )));
    }
    *offset = end;
    Ok(())
}

fn skip_table_type(bytes: &[u8], offset: &mut usize) -> Result<(), WasmExecutionError> {
    let _ = read_byte(bytes, offset)?;
    let flags = read_varuint(bytes, offset)?;
    let _ = read_varuint(bytes, offset)?;
    if flags & 0x01 != 0 {
        let _ = read_varuint(bytes, offset)?;
    }
    Ok(())
}

fn read_byte(bytes: &[u8], offset: &mut usize) -> Result<u8, WasmExecutionError> {
    let Some(byte) = bytes.get(*offset).copied() else {
        return Err(WasmExecutionError::InvalidModule(String::from(
            "unexpected end of module",
        )));
    };
    *offset += 1;
    Ok(byte)
}

fn read_varuint(bytes: &[u8], offset: &mut usize) -> Result<u64, WasmExecutionError> {
    let mut shift = 0_u32;
    let mut value = 0_u64;
    let mut encoded_bytes = 0_usize;

    loop {
        let byte = read_byte(bytes, offset)?;
        encoded_bytes += 1;
        if encoded_bytes > MAX_WASM_VARUINT_BYTES {
            return Err(WasmExecutionError::InvalidModule(format!(
                "varuint exceeds the parser cap of {MAX_WASM_VARUINT_BYTES} bytes"
            )));
        }
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
        if encoded_bytes == MAX_WASM_VARUINT_BYTES {
            return Err(WasmExecutionError::InvalidModule(format!(
                "varuint exceeds the parser cap of {MAX_WASM_VARUINT_BYTES} bytes"
            )));
        }
        shift = shift.saturating_add(7);
        if shift >= 64 {
            return Err(WasmExecutionError::InvalidModule(String::from(
                "varuint is too large",
            )));
        }
    }
}

fn read_varuint_usize(
    bytes: &[u8],
    offset: &mut usize,
    label: &str,
) -> Result<usize, WasmExecutionError> {
    let value = read_varuint(bytes, offset)?;
    usize::try_from(value).map_err(|_| {
        WasmExecutionError::InvalidModule(format!(
            "{label} of {value} exceeds platform usize range"
        ))
    })
}

fn resolve_path_like_specifier(cwd: &Path, specifier: &str) -> Option<PathBuf> {
    if specifier.starts_with("file://") {
        return Some(PathBuf::from(specifier.trim_start_matches("file://")));
    }
    if specifier.starts_with("file:") {
        return Some(PathBuf::from(specifier.trim_start_matches("file:")));
    }
    if specifier.starts_with('/') {
        return Some(PathBuf::from(specifier));
    }
    if specifier.starts_with("./") || specifier.starts_with("../") {
        return Some(cwd.join(specifier));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::{
        build_wasm_internal_env, build_wasm_runner_bootstrap,
        build_wasm_snapshot_runner_inline_code, managed_kernel_host_bootstrap,
        max_cbor_byte_string_payload_bytes, open_wasm_guest_file, reject_v8_deterministic_fuel,
        resolve_wasm_prewarm_timeout, resolve_wasm_stack_limit_bytes,
        resolve_wasm_wall_clock_limit, resolved_module_path, translate_wasm_guest_path,
        translate_wasm_host_symlink_target, wasm_guest_module_paths, wasm_host_path_is_read_only,
        wasm_memory_limit_bytes, wasm_memory_limit_pages, wasm_mutation_touches_read_only_mapping,
        wasm_read_only_filesystem_error, wasm_runner_base_env, wasm_runner_heap_limit_mb,
        wasm_runner_javascript_limits, wasm_sandbox_root, wasm_snapshot_runner_base_env,
        wasm_sync_read_length, wasm_sync_rpc_error_code,
        wasm_sync_rpc_method_routes_through_sidecar_kernel, CreateWasmContextRequest,
        ExecutionSignalDispositionAction, ExecutionSignalHandlerRegistration, GuestRuntimeConfig,
        HostRpcRequest, ResolvedWasmModule, StartWasmExecutionRequest, Value, WasmEventQueue,
        WasmExecutionEngine, WasmExecutionError, WasmExecutionEvent, WasmExecutionLimits,
        WasmInternalSyncRpc, WasmPendingEventBudget, WasmPermissionTier, WasmSignalCheckpointInbox,
        DEFAULT_WASM_PREWARM_TIMEOUT_MS, DEFAULT_WASM_RUNNER_HEAP_LIMIT_MB,
        NODE_WASI_MODULE_SOURCE, WASM_CAPTURED_OUTPUT_LIMIT_BYTES,
        WASM_INTERNAL_MAX_STACK_BYTES_ENV, WASM_INTERNAL_SYNC_RPC_RESPONSE_LINE_BYTES_ENV,
        WASM_MAX_MEMORY_BYTES_ENV, WASM_MAX_MODULE_FILE_BYTES_ENV, WASM_MAX_SPAWN_FILE_ACTIONS_ENV,
        WASM_MAX_SPAWN_FILE_ACTION_BYTES_ENV, WASM_MAX_STACK_BYTES_ENV, WASM_PAGE_BYTES,
        WASM_PROCESS_SYNC_RPC_RESPONSE_BYTES, WASM_SANDBOX_ROOT_ENV,
        WASM_SIDECAR_ROUTED_FS_SYNC_METHODS, WASM_SYNC_READ_LIMIT_BYTES,
    };
    use std::collections::{BTreeMap, BTreeSet};
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::Duration;
    use tempfile::tempdir;

    use crate::backend::{ExecutionWakeIdentity, PublishedSignalCheckpoint};

    #[test]
    fn wasm_runner_forwards_vm_reactor_limits_to_javascript() {
        let limits = WasmExecutionLimits {
            reactor_work_quantum: Some(17),
            bridge_call_timeout_ms: Some(12_345),
            ..WasmExecutionLimits::default()
        };
        let javascript = wasm_runner_javascript_limits(&limits, 192);

        assert_eq!(javascript.v8_heap_limit_mb, Some(192));
        assert_eq!(javascript.reactor_work_quantum, Some(17));
        assert_eq!(javascript.bridge_call_timeout_ms, Some(12_345));
    }

    #[test]
    fn managed_kernel_host_mode_is_trusted_frozen_bootstrap_not_guest_env() {
        let runner = include_str!("../assets/runners/wasm-runner.mjs");
        assert!(runner.contains(
            "const SIDECAR_MANAGED_PROCESS = globalThis.__agentOSManagedKernelHost === true;"
        ));
        assert!(!runner.contains("typeof process?.env?.AGENTOS_SANDBOX_ROOT === 'string'"));

        let managed = managed_kernel_host_bootstrap(true);
        assert!(managed.contains("configurable: false"));
        assert!(managed.contains("writable: false"));
        assert!(managed.contains("value: true"));

        let standalone = managed_kernel_host_bootstrap(false);
        assert!(standalone.contains("value: false"));

        let snapshot = build_wasm_snapshot_runner_inline_code(None, true);
        let mode = snapshot
            .find("__agentOSManagedKernelHost")
            .expect("trusted mode bootstrap");
        let run = snapshot
            .find("__agentOSWasmRunnerRun")
            .expect("snapshotted runner invocation");
        assert!(
            mode < run,
            "mode must be frozen before runner code executes"
        );
    }

    #[test]
    fn managed_stdio_duplication_uses_kernel_descriptions() {
        let runner = include_str!("../assets/runners/wasm-runner.mjs");
        let helper = runner
            .find("function managedKernelFdForDuplicate(fd) {")
            .expect("managed duplicate helper");
        let dup = runner
            .find("        fd_dup(fd, retNewFdPtr) {")
            .expect("fd_dup import");
        let dup_min = runner
            .find("        fd_dup_min(fd, minFd, retNewFdPtr) {")
            .expect("fd_dup_min import");
        assert!(helper < dup);

        let dup_min_end = runner[dup_min..]
            .find("        fd_getfd(")
            .map(|offset| dup_min + offset)
            .expect("fd_dup_min terminator");
        for (section, rpc) in [
            (
                &runner[dup..dup_min],
                "callSyncRpc('process.fd_dup', [kernelSourceFd])",
            ),
            (
                &runner[dup_min..dup_min_end],
                "callSyncRpc('process.fd_dup_min', [kernelSourceFd, minimumFdNumber])",
            ),
        ] {
            let kernel_source = section
                .find("managedKernelFdForDuplicate(sourceFd)")
                .expect("managed stdio duplicate source");
            let kernel_dup = section.find(rpc).expect("kernel duplicate call");
            let local_clone = section
                .find("cloneFdHandle(sourceFd)")
                .expect("standalone duplicate fallback");
            assert!(kernel_source < kernel_dup);
            assert!(kernel_dup < local_clone);
        }
        assert!(
            runner[dup_min..dup_min_end].contains("authoritative\n              // RLIMIT_NOFILE")
        );
        assert!(runner[dup_min..dup_min_end].contains(
            "registerKernelDelegateFd(\n                callSyncRpc('process.fd_dup_min', [kernelSourceFd, minimumFdNumber]),\n                null,\n                minimumFdNumber,"
        ));

        let fdstat_get = runner
            .find("wasiImport.fd_fdstat_get = (fd, statPtr) => {")
            .expect("fdstat get override");
        let fdstat_set = runner
            .find("wasiImport.fd_fdstat_set_flags = (fd, flags) => {")
            .expect("fdstat set override");
        let get_section = &runner[fdstat_get..fdstat_set];
        assert!(
            get_section
                .find("callSyncRpc('process.fd_stat'")
                .expect("kernel fdstat projection")
                < get_section
                    .find("if (!SIDECAR_MANAGED_PROCESS && hostNetSocket")
                    .expect("standalone host-net fallback")
        );
        assert!(get_section.contains("BigInt(stat?.rightsBase ?? 0)"));

        let set_end = runner[fdstat_set..]
            .find("wasiImport.fd_filestat_get")
            .map(|offset| fdstat_set + offset)
            .expect("fdstat set terminator");
        let set_section = &runner[fdstat_set..set_end];
        assert!(
            set_section
                .find("callSyncRpc('process.fd_set_flags'")
                .expect("kernel status flag update")
                < set_section
                    .find("hostNetSocket.nonblock =")
                    .expect("managed transport metadata update")
        );
        assert!(set_section.contains("if (!SIDECAR_MANAGED_PROCESS && hostNetSocket"));

        let open_alias = runner
            .find("function kernelOpenPathForGuestPath(guestPath) {")
            .expect("managed proc-fd open translation");
        let open_alias_end = runner[open_alias..]
            .find("function fsOpenNumericFlagsForManagedPath")
            .map(|offset| open_alias + offset)
            .expect("managed proc-fd helper terminator");
        let open_alias = &runner[open_alias..open_alias_end];
        assert!(open_alias.contains("(?:proc\\/self\\/fd|dev\\/fd)"));
        assert!(open_alias.contains("lookupFdHandle(guestFd)"));
        assert!(open_alias.contains("handle.targetFd"));
        assert!(open_alias.contains("`dev/fd/${Number(handle.targetFd) >>> 0}`"));
        let path_open = &runner[runner
            .find("  wasiImport.path_open = (")
            .expect("path_open override")..];
        assert!(path_open.contains(
            "const managedOpenPath = SIDECAR_MANAGED_PROCESS\n      ? kernelOpenPathForGuestPath"
        ));
        assert!(path_open.matches("managedOpenPath,").count() >= 2);

        let write_start = runner
            .find("wasiImport.fd_write = (fd, iovs, iovsLen, nwrittenPtr) => {")
            .expect("fd_write override");
        let write_end = runner[write_start..]
            .find("wasiImport.fd_close = (fd) => {")
            .map(|offset| write_start + offset)
            .expect("fd_write terminator");
        let write = &runner[write_start..write_end];
        assert!(runner.contains("function kernelFdStdioStream(targetFd, descriptorPath) {"));
        assert!(runner.contains("callSyncRpc('__kernel_isatty', [targetFd])"));
        assert!(runner
            .contains("callSyncRpc('process.fd_description_identity', [targetFd])?.descriptionId"));
        let path_lookup = write
            .find("callSyncRpc('process.fd_path', [kernelFd])")
            .expect("kernel output aliases must resolve their authoritative path");
        let stdio_write = write
            .find("callSyncRpc('__kernel_stdio_write', [kernelFd, bytes])")
            .expect("stdout/stderr aliases must use the ordered output path");
        let ordinary_write = write
            .find("writeKernelFdCooperatively(kernelFd, bytes)")
            .expect("non-output descriptions retain the generic kernel write path");
        assert!(path_lookup < stdio_write);
        assert!(path_lookup < ordinary_write);
    }

    #[test]
    fn optional_posix_identity_ids_use_explicit_null_on_the_typed_bridge() {
        let runner = include_str!("../assets/runners/wasm-runner.mjs");
        assert!(runner.contains(
            "function hostUserOptionalId(value) {\n  const id = Number(value) >>> 0;\n  return id === 0xffffffff ? null : id;\n}"
        ));
        for method in ["setreuid", "setresuid", "setregid", "setresgid"] {
            assert!(
                runner.contains(&format!("callSyncRpc('process.{method}', [")),
                "{method} must use the typed identity bridge"
            );
        }
    }

    #[test]
    fn path_owner_accepts_libc_at_fdcwd_sentinel() {
        let runner = include_str!("../assets/runners/wasm-runner.mjs");
        let start = runner
            .find("  path_owner(fd, pathPtr, pathLen, followSymlinks, retUidPtr, retGidPtr) {")
            .expect("path-owner import");
        let section = &runner[start..];
        let end = section.find("  fd_owner(").expect("path-owner import end");
        let section = &section[..end];

        assert!(section.contains("const numericFd = Number(fd) >>> 0;"));
        assert!(runner.contains("const NODE_CWD_FD = 0xffffffff;"));
        assert!(section.contains("numericFd === NODE_CWD_FD"));
        assert!(section.contains("dirFd: NODE_CWD_FD"));
        assert!(section.contains("path.posix.resolve(HOST_FS_GUEST_CWD, rawTarget)"));
        assert!(section.contains("kernelPathOperand(numericFd, pathPtr, pathLen)"));

        let chown_start = runner
            .find("  path_chown(fd, pathPtr, pathLen, uid, gid, followSymlinks) {")
            .expect("path-chown import");
        let chown = &runner[chown_start..];
        let chown_end = chown.find("  fd_chown(").expect("path-chown import end");
        let chown = &chown[..chown_end];
        assert!(chown.contains("numericFd === NODE_CWD_FD"));
        assert!(chown.contains("dirFd: NODE_CWD_FD"));
    }

    #[test]
    fn proc_fd_readlink_rewrites_absolute_and_preopen_relative_guest_paths() {
        let runner = include_str!("../assets/runners/wasm-runner.mjs");
        let start = runner
            .find("function kernelProcFdPathForGuestPath(guestPath) {")
            .expect("proc-fd path rewrite");
        let section = &runner[start..];
        let end = section
            .find("function fsOpenNumericFlagsForManagedPath")
            .expect("proc-fd path rewrite end");
        let section = &section[..end];

        assert!(section.contains(r"/^(\/?)proc\/self\/fd\/(\d+)$/u"));
        assert!(section.contains("const guestFd = Number(match[2]);"));
        assert!(section.contains("`${match[1]}proc/self/fd/${Number(handle.targetFd) >>> 0}`"));
    }

    #[test]
    fn wasm_pending_event_queue_rejects_count_limit_without_leaking_reservation() {
        let budget = WasmPendingEventBudget::new(1, usize::MAX);
        let mut queue = WasmEventQueue::new(Arc::clone(&budget));

        queue
            .push_back(WasmExecutionEvent::Exited(0))
            .expect("event at count limit");
        let usage_at_limit = budget.usage();
        let error = queue
            .push_back(WasmExecutionEvent::Exited(1))
            .expect_err("event over count limit");

        assert!(matches!(
            error,
            WasmExecutionError::PendingEventLimit {
                limit_name: "limits.process.pendingEventCount",
                limit: 1,
                observed: 2,
            }
        ));
        assert_eq!(budget.usage(), usage_at_limit, "rejection must roll back");
        assert_eq!(queue.pop_front(), Some(WasmExecutionEvent::Exited(0)));
        assert_eq!(budget.usage(), (0, 0), "dequeue must release reservation");
    }

    #[test]
    fn wasm_pending_event_queue_rejects_byte_limit_without_leaking_reservation() {
        let event = WasmExecutionEvent::Stdout(vec![7; 8]);
        let retained_bytes = super::wasm_event_retained_bytes(&event);
        let budget = WasmPendingEventBudget::new(2, retained_bytes);
        let mut queue = WasmEventQueue::new(Arc::clone(&budget));

        queue.push_back(event.clone()).expect("event at byte limit");
        let error = queue
            .push_back(event.clone())
            .expect_err("event over byte limit");

        assert!(matches!(
            error,
            WasmExecutionError::PendingEventLimit {
                limit_name: "limits.process.pendingEventBytes",
                limit,
                observed,
            } if limit == retained_bytes && observed == retained_bytes * 2
        ));
        assert_eq!(budget.usage(), (1, retained_bytes));
        assert_eq!(queue.pop_front(), Some(event));
        assert_eq!(budget.usage(), (0, 0));
    }

    #[test]
    fn wasm_outer_and_internal_signal_events_share_one_budget() {
        let signal_event = WasmExecutionEvent::SignalState {
            signal: 15,
            registration: ExecutionSignalHandlerRegistration {
                action: ExecutionSignalDispositionAction::User,
                mask: vec![1, 2, 3],
                flags: 0,
            },
        };
        let retained_bytes = super::wasm_event_retained_bytes(&signal_event);
        let budget = WasmPendingEventBudget::new(1, retained_bytes * 2);
        let mut outer = WasmEventQueue::new(Arc::clone(&budget));
        let mut internal = WasmEventQueue::new(Arc::clone(&budget));

        outer
            .push_back(signal_event.clone())
            .expect("first expanded signal event");
        let error = internal
            .push_back(signal_event.clone())
            .expect_err("signal expansion cannot bypass the shared count bound");
        assert!(matches!(
            error,
            WasmExecutionError::PendingEventLimit {
                limit_name: "limits.process.pendingEventCount",
                limit: 1,
                observed: 2,
            }
        ));
        assert_eq!(budget.usage(), (1, retained_bytes));

        assert_eq!(outer.pop_front(), Some(signal_event.clone()));
        internal
            .push_back(signal_event.clone())
            .expect("released capacity is reusable by internal queue");
        assert_eq!(internal.pop_front(), Some(signal_event));
        assert_eq!(budget.usage(), (0, 0));
    }

    #[test]
    fn wasm_signal_checkpoint_inbox_is_bounded_and_generation_scoped() {
        let retained_bytes = std::mem::size_of::<super::QueuedSignalCheckpoint>();
        let budget = WasmPendingEventBudget::new(1, retained_bytes);
        let inbox = WasmSignalCheckpointInbox::new(Arc::clone(&budget));
        let identity = ExecutionWakeIdentity {
            generation: 7,
            pid: 42,
        };
        let delivery = PublishedSignalCheckpoint {
            signal: 15,
            delivery_token: 99,
            flags: 0x8000_0000,
            thread_id: 0,
        };

        inbox
            .publish(identity, delivery)
            .expect("first checkpoint fits the shared pending-event budget");
        let limit_error = inbox
            .publish(identity, delivery)
            .expect_err("second checkpoint must hit the count limit");
        assert_eq!(limit_error.code, "ERR_AGENTOS_RESOURCE_LIMIT");
        assert!(limit_error
            .message
            .contains("limits.process.pendingEventCount"));
        assert_eq!(budget.usage(), (1, retained_bytes));

        let stale_error = inbox
            .take(ExecutionWakeIdentity {
                generation: identity.generation + 1,
                ..identity
            })
            .expect_err("another execution generation cannot consume the checkpoint");
        assert_eq!(stale_error.code, "ESTALE");
        assert_eq!(budget.usage(), (1, retained_bytes));

        assert_eq!(
            inbox.take(identity).expect("matching execution identity"),
            Some(delivery)
        );
        assert_eq!(budget.usage(), (0, 0), "take must release the budget");
        assert_eq!(inbox.take(identity).expect("empty inbox"), None);

        inbox
            .publish(identity, delivery)
            .expect("checkpoint can be queued again after take");
        inbox
            .discard(identity, delivery.delivery_token)
            .expect("failed wake publication can roll back its checkpoint");
        assert_eq!(budget.usage(), (0, 0), "discard must release the budget");
        assert_eq!(inbox.take(identity).expect("discarded inbox"), None);

        inbox
            .publish(identity, delivery)
            .expect("old exec image checkpoint");
        inbox
            .discard_identity(identity)
            .expect("successful exec discards old-image checkpoints");
        assert_eq!(budget.usage(), (0, 0), "exec discard must release budget");
        assert_eq!(inbox.take(identity).expect("exec-discarded inbox"), None);
    }

    #[test]
    fn wasm_process_reads_fit_the_encoded_bridge_response_budget() {
        let raw_limit = max_cbor_byte_string_payload_bytes(WASM_PROCESS_SYNC_RPC_RESPONSE_BYTES);
        assert_eq!(raw_limit, 256 * 1024 - 5);
        assert_eq!(
            agentos_bridge::bridge_contract()
                .response_max_bytes
                .get("_processWasmSyncRpc")
                .copied(),
            Some(WASM_PROCESS_SYNC_RPC_RESPONSE_BYTES)
        );

        let bootstrap = build_wasm_runner_bootstrap(&BTreeMap::new(), None);
        assert!(bootstrap.contains(&format!(
            "const __agentOSWasmSyncRpcReadPayloadBytes = {raw_limit};"
        )));
        for method in [
            "process.exec_image_open",
            "process.exec_image_open_fd",
            "process.exec_image_read",
            "process.exec_image_close",
        ] {
            assert!(
                bootstrap.contains(&format!("case \"{method}\":")),
                "{method} must route through the V8 host-process bridge"
            );
        }
        let runner = include_str!("../assets/runners/wasm-runner.mjs");
        assert!(runner.contains("boundedWasmSyncRpcReadLength("));
        assert!(runner.contains("callSyncRpc('process.fd_read'"));
        assert!(runner.contains("callSyncRpc('process.fd_pread'"));
        assert!(runner.contains("callSyncRpc('process.exec_image_open'"));
        assert!(runner.contains("callSyncRpc('process.exec_image_open_fd'"));
        assert!(runner.contains("callSyncRpc('process.exec_image_read'"));
        assert!(runner.contains("callSyncRpc('process.exec_image_close'"));
        assert!(runner.contains("finally {"));
    }

    #[test]
    fn wasm_sync_rpc_response_line_limit_is_internal_config_and_source_guarded() {
        let mut request = request_with_typed_limits_and_misleading_env(WasmExecutionLimits {
            max_sync_rpc_response_line_bytes: Some(8_192),
            ..WasmExecutionLimits::default()
        });
        request.env.insert(
            WASM_INTERNAL_SYNC_RPC_RESPONSE_LINE_BYTES_ENV.to_string(),
            String::from("999999"),
        );
        let resolved_module = ResolvedWasmModule {
            specifier: String::from("./guest.wasm"),
            resolved_path: PathBuf::from("/tmp/guest.wasm"),
        };

        let internal_env =
            build_wasm_internal_env(&resolved_module, &request, 1_234, false).expect("env");
        assert_eq!(
            internal_env.get(WASM_INTERNAL_SYNC_RPC_RESPONSE_LINE_BYTES_ENV),
            Some(&String::from("8192")),
            "the typed limit must override an ambient internal env value"
        );
        assert!(
            !wasm_runner_base_env(&request)
                .contains_key(WASM_INTERNAL_SYNC_RPC_RESPONSE_LINE_BYTES_ENV),
            "the internal line cap must not remain guest-configurable"
        );

        let runner = include_str!("../assets/runners/wasm-runner.mjs");
        assert!(runner.contains("Math.ceil(maxSyncRpcResponseLineBytes * 0.8)"));
        assert!(runner.contains("ERR_AGENTOS_RESOURCE_LIMIT"));
        assert!(runner.contains("raise limits.reactor.maxBridgeResponseBytes"));
        assert!(runner.contains("remaining > 4095 ? 4096 : remaining + 1"));
    }

    #[test]
    fn wasm_sync_rpc_response_line_runtime_rejects_before_retaining_overflow() {
        let runner = include_str!("../assets/runners/wasm-runner.mjs");
        let marker_begin = "// AGENTOS_SYNC_RPC_RESPONSE_LIMIT_HELPERS_BEGIN";
        let marker_end = "// AGENTOS_SYNC_RPC_RESPONSE_LIMIT_HELPERS_END";
        let helper_start = runner.find(marker_begin).expect("helper begin marker");
        let helper_end = runner.find(marker_end).expect("helper end marker");
        let helpers = &runner[helper_start + marker_begin.len()..helper_end];
        let script = format!(
            r#"{helpers}
let retained = Buffer.alloc(0);
retained = appendSyncRpcResponseChunk(retained, Buffer.from('12345678'), 8).buffer;
let overflow;
try {{
  appendSyncRpcResponseChunk(retained, Buffer.from('9'), 8);
}} catch (error) {{
  overflow = {{
    code: error.code,
    details: error.details,
    message: error.message,
    retainedBytes: retained.byteLength,
  }};
}}
const exact = appendSyncRpcResponseChunk(Buffer.alloc(0), Buffer.from('12345678\n'), 8);
process.stdout.write(JSON.stringify({{ overflow, exactLine: exact.line, exactRetained: exact.buffer.byteLength }}));
"#
        );
        let output = std::process::Command::new("node")
            .args(["--input-type=module", "--eval", script.as_str()])
            .output()
            .expect("run node response-line helper");
        assert!(
            output.status.success(),
            "node helper failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let result: Value = serde_json::from_slice(&output.stdout).expect("helper JSON");
        assert_eq!(result["overflow"]["code"], "ERR_AGENTOS_RESOURCE_LIMIT");
        assert_eq!(
            result["overflow"]["details"]["limitName"],
            "limits.reactor.maxBridgeResponseBytes"
        );
        assert_eq!(result["overflow"]["details"]["limit"], 8);
        assert_eq!(result["overflow"]["details"]["observed"], 9);
        assert_eq!(result["overflow"]["retainedBytes"], 8);
        assert_eq!(result["exactLine"], "12345678");
        assert_eq!(result["exactRetained"], 0);
    }

    #[test]
    fn wasm_host_tty_read_prevalidates_guest_destination_before_kernel_read() {
        let runner = include_str!("../assets/runners/wasm-runner.mjs");
        let tty_start = runner
            .find("const hostTtyImport = {")
            .expect("host_tty import must exist");
        let tty_end = runner[tty_start..]
            .find("  isatty(fd) {")
            .map(|offset| tty_start + offset)
            .expect("host_tty read must precede isatty");
        let tty_read = &runner[tty_start..tty_end];
        let validation = tty_read
            .find("guestRangeIsValid(ptr, cap)")
            .expect("host_tty read must validate its full output range");
        let consuming_read = tty_read
            .find("callSyncRpc('__kernel_stdin_read'")
            .expect("host_tty read must call the kernel stdin RPC");

        assert!(
            validation < consuming_read,
            "guest output memory must be validated before PTY bytes are consumed"
        );
    }

    #[test]
    fn wasm_host_tty_size_returns_typed_native_errno_instead_of_throwing_rpc_errors() {
        let runner = include_str!("../assets/runners/wasm-runner.mjs");
        let size_start = runner
            .find("  get_size(fd, colsPtr, rowsPtr) {")
            .expect("host_tty get_size must exist");
        let size_end = runner[size_start..]
            .find("  set_size(fd, cols, rows) {")
            .map(|offset| size_start + offset)
            .expect("host_tty get_size must precede set_size");
        let get_size = &runner[size_start..size_end];

        assert!(get_size.contains("try {"));
        assert!(get_size.contains("if (error?.code === 'EBADF') return 9;"));
        assert!(get_size.contains("if (error?.code === 'ENOTTY') return 25;"));
        assert!(
            get_size.find("guestRangeIsValid(colsPtr, 2)").unwrap()
                < get_size.find("callSyncRpc('__kernel_tty_size'").unwrap(),
            "guest output pointers must be validated before the tty-size RPC"
        );
    }

    #[test]
    fn wasm_preview1_memory_is_prevalidated_before_host_side_effects() {
        let runner = include_str!("../assets/runners/wasm-runner.mjs");

        let assert_precedes = |start: &str, validation: &str, effect: &str| {
            let section_start = runner
                .rfind(start)
                .unwrap_or_else(|| panic!("missing {start}"));
            let section = &runner[section_start..];
            let validation_offset = section
                .find(validation)
                .unwrap_or_else(|| panic!("missing validation {validation} in {start}"));
            let effect_offset = section
                .find(effect)
                .unwrap_or_else(|| panic!("missing effect {effect} in {start}"));
            assert!(
                validation_offset < effect_offset,
                "{start} must validate guest memory before {effect}"
            );
        };

        assert!(runner.contains("const LINUX_IOV_MAX = 1024;"));
        assert_precedes(
            "wasiImport.fd_read =",
            "validateGuestIovRequest(iovs, iovsLen)",
            "getHostNetSocket(numericFd)",
        );
        assert_precedes(
            "wasiImport.fd_write =",
            "validateGuestIovRequest(iovs, iovsLen)",
            "getHostNetSocket(numericFd)",
        );
        assert_precedes(
            "wasiImport.fd_pread =",
            "guestRangeIsValid(nreadPtr, 4)",
            "callSyncRpc('process.fd_pread'",
        );
        assert_precedes(
            "wasiImport.fd_pwrite =",
            "guestRangeIsValid(nwrittenPtr, 4)",
            "callSyncRpc('process.fd_pwrite'",
        );
        assert_precedes(
            "wasiImport.fd_readdir =",
            "guestRangesAreValid([bufPtr, bufferLength], [bufUsedPtr, 4])",
            "lookupFdHandle(numericFd)",
        );
        assert_precedes(
            "wasiImport.fd_seek =",
            "guestRangeIsValid(newOffsetPtr, 8)",
            "callSyncRpc('process.fd_seek'",
        );
        assert_precedes(
            "wasiImport.fd_tell =",
            "guestRangeIsValid(offsetPtr, 8)",
            "callSyncRpc('process.fd_seek'",
        );
        assert_precedes(
            "wasiImport.fd_filestat_get =",
            "guestRangeIsValid(statPtr, 64)",
            "callSyncRpc('process.fd_filestat'",
        );
        assert_precedes(
            "wasiImport.poll_oneoff =",
            "guestRangesAreValid(",
            "new DataView(instanceMemory.buffer)",
        );

        let path_filestat = runner
            .find("  path_filestat_get(args) {")
            .expect("kernel path_filestat_get handler");
        let path_filestat = &runner[path_filestat..];
        assert!(
            path_filestat
                .find("guestRangesAreValid([args[2]")
                .expect("path_filestat_get validation")
                < path_filestat
                    .find("callSyncRpc('process.path_stat_at'")
                    .expect("path_filestat_get RPC")
        );

        for (start, end, ambient_io) in [
            (
                "wasiImport.fd_pread =",
                "wasiImport.fd_pwrite =",
                "fsModule.readSync(",
            ),
            (
                "wasiImport.fd_pwrite =",
                "wasiImport.fd_sync =",
                "fsModule.writeSync(",
            ),
        ] {
            let start = runner.rfind(start).expect("positioned I/O wrapper");
            let section = &runner[start..];
            let end = section.find(end).expect("positioned I/O wrapper end");
            let section = &section[..end];
            assert!(
                section
                    .find("if (SIDECAR_MANAGED_PROCESS) return WASI_ERRNO_BADF;")
                    .expect("managed ambient-I/O guard")
                    < section.find(ambient_io).expect("standalone ambient I/O"),
                "managed positioned I/O must stop before {ambient_io}"
            );
        }

        let path_open = runner
            .find("  wasiImport.path_open = (")
            .expect("path_open wrapper");
        let section = &runner[path_open..];
        assert!(
            section
                .find("guestRangesAreValid([pathPtr")
                .expect("path_open validation")
                < section
                    .find("pendingOpenCreateMode")
                    .expect("path_open state consumption")
        );
    }

    #[test]
    fn wasm_clock_and_system_outputs_are_prevalidated_before_process_rpc() {
        let runner = include_str!("../assets/runners/wasm-runner.mjs");
        for (start, end, validation, rpc) in [
            (
                "\nwasiImport.clock_time_get = (",
                "\nwasiImport.clock_res_get = (",
                "guestRangeIsValid(resultPtr, 8)",
                "callSyncRpc('process.clock_time'",
            ),
            (
                "\nwasiImport.clock_res_get = (",
                "// Managed VMs use the shared sidecar entropy source.",
                "guestRangeIsValid(resultPtr, 8)",
                "callSyncRpc('process.clock_resolution'",
            ),
            (
                "const hostSystemImport = {",
                "// Terminal event source",
                "guestRangeIsValid(bufferPtr, capacity)",
                "callSyncRpc('process.system_identity'",
            ),
        ] {
            let start_offset = runner.find(start).expect("provider start marker");
            let provider = &runner[start_offset..];
            let end_offset = provider.find(end).expect("provider end marker");
            let provider = &provider[..end_offset];
            let validation_offset = provider.find(validation).expect("output validation");
            let rpc_offset = provider.find(rpc).expect("shared process RPC");
            assert!(
                validation_offset < rpc_offset,
                "{start} must validate guest output before host work"
            );
        }

        let bootstrap = build_wasm_runner_bootstrap(&BTreeMap::new(), None);
        for method in [
            "process.clock_time",
            "process.clock_resolution",
            "process.system_identity",
        ] {
            assert!(bootstrap.contains(&format!("case \"{method}\":")));
        }
    }

    #[test]
    fn wasm_process_outputs_are_prevalidated_before_side_effects() {
        let runner = include_str!("../assets/runners/wasm-runner.mjs");
        let cases = [
            (
                "function returnWaitedChild(\n",
                "function returnLegacyWaitedChild(",
                "guestRangesAreValid(\n",
                "dispatchPendingWasmSignals()",
            ),
            (
                "function returnLegacyWaitedChild(",
                "function returnRawWaitedChild(",
                "guestRangesAreValid([retStatusPtr, 4], [retPidPtr, 4])",
                "dispatchPendingWasmSignals()",
            ),
            (
                "function returnRawWaitedChild(",
                "function processChildEvent(",
                "guestRangesAreValid([retStatusPtr, 4], [retPidPtr, 4])",
                "dispatchPendingWasmSignals()",
            ),
            (
                "        proc_spawn(\n",
                "        proc_spawn_v3(\n",
                "guestRangeIsValid(retPidPtr, 4)",
                "readGuestBytes(argvPtr, argvLen)",
            ),
            (
                "        proc_spawn_v3(\n",
                "        proc_spawn_v4(\n",
                "guestRangeIsValid(retPidPtr, 4)",
                "activeSpawnCallContext = {",
            ),
            (
                "        proc_spawn_v4(\n",
                "        proc_spawn_v2(\n",
                "guestRangeIsValid(retPidPtr, 4)",
                "activeSpawnCallContext = {",
            ),
            (
                "        proc_spawn_v2(\n",
                "        proc_exec(\n",
                "guestRangeIsValid(retPidPtr, 4)",
                "callSyncRpc('child_process.spawn'",
            ),
            (
                "        proc_waitpid(pid, options, retStatusPtr, retPidPtr) {",
                "        proc_waitpid_v2(\n",
                "guestRangesAreValid([retStatusPtr, 4], [retPidPtr, 4])",
                "pollChildEvent(record, 0)",
            ),
            (
                "        proc_waitpid_v2(\n",
                "        proc_waitpid_v3(",
                "guestRangesAreValid(\n",
                "pollChildEvent(record, 0)",
            ),
            (
                "        proc_waitpid_v3(",
                "        proc_kill(",
                "guestRangesAreValid([retStatusPtr, 4], [retPidPtr, 4])",
                "callSyncRpc('process.waitpid_transition'",
            ),
            (
                "        proc_getrlimit(",
                "        proc_setrlimit(",
                "guestRangesAreValid([retSoftPtr, 8], [retHardPtr, 8])",
                "callSyncRpc('process.getrlimit'",
            ),
            (
                "        proc_umask(",
                "        umask(",
                "guestRangeIsValid(retPreviousPtr, 4)",
                "callSyncRpc('process.umask'",
            ),
            (
                "        umask(",
                "        proc_itimer_real(",
                "guestRangeIsValid(retPreviousPtr, 4)",
                "callSyncRpc('process.umask'",
            ),
            (
                "        proc_itimer_real(",
                "        proc_getpgid(",
                "guestRangesAreValid([retRemainingUsPtr, 8], [retIntervalUsPtr, 8])",
                "callSyncRpc('process.itimer_real'",
            ),
            (
                "        proc_getpgid(",
                "        proc_setpgid(",
                "guestRangeIsValid(retPgidPtr, 4)",
                "callSyncRpc('process.getpgid'",
            ),
            (
                "        fd_pipe(",
                "        fd_dup(",
                "guestRangesAreValid([retReadFdPtr, 4], [retWriteFdPtr, 4])",
                "hasRunnerOpenFdCapacity(2)",
            ),
            (
                "        fd_dup(fd, retNewFdPtr) {",
                "        fd_dup2(",
                "guestRangeIsValid(retNewFdPtr, 4)",
                "allocateHostNetDuplicateFd(0)",
            ),
            (
                "        fd_dup_min(",
                "        fd_getfd(",
                "guestRangeIsValid(retNewFdPtr, 4)",
                "allocateHostNetDuplicateFd(minimumFdNumber)",
            ),
            (
                "        fd_getfd(",
                "        fd_setfd(",
                "guestRangeIsValid(retFlagsPtr, 4)",
                "callSyncRpc('process.fd_getfd'",
            ),
            (
                "        fd_socketpair(",
                "        fd_sendmsg_rights(",
                "guestRangeIsValid(retFirstPtr, 4)",
                "callSyncRpc('process.fd_socketpair'",
            ),
            (
                "        fd_sendmsg_rights(",
                "        fd_recvmsg_rights(",
                "guestRangeIsValid(retSentPtr, 4)",
                "callSyncRpc('process.fd_sendmsg_rights'",
            ),
            (
                "        proc_signal_mask_v2(",
                "        proc_ppoll_v1(",
                "guestRangesAreValid([retOldLoPtr, 4], [retOldHiPtr, 4])",
                "callSyncRpc('process.signal_mask'",
            ),
            (
                "        proc_ppoll_v1(",
                "\n};\n\nconst limitedHostProcessImport",
                "guestRangeIsValid(retReadyPtr, 4)",
                "return hostNetImport.net_poll(",
            ),
        ];

        for (start, end, validation, side_effect) in cases {
            let start_offset = runner
                .find(start)
                .unwrap_or_else(|| panic!("missing process import marker {start:?}"));
            let tail = &runner[start_offset..];
            let end_offset = tail
                .find(end)
                .unwrap_or_else(|| panic!("missing process import terminator {end:?}"));
            let import = &tail[..end_offset];
            let validation_offset = import
                .find(validation)
                .unwrap_or_else(|| panic!("{start:?} must validate {validation:?}"));
            let side_effect_offset = import
                .find(side_effect)
                .unwrap_or_else(|| panic!("{start:?} must retain {side_effect:?}"));
            assert!(
                validation_offset < side_effect_offset,
                "{start:?} must validate guest output before {side_effect:?}"
            );
        }
    }

    #[test]
    fn guest_byte_reads_use_typed_bounds_checks() {
        let runner = include_str!("../assets/runners/wasm-runner.mjs");
        let start = runner
            .find("function readGuestBytes(ptr, len) {")
            .expect("guest byte reader");
        let section = &runner[start..];
        let end = section
            .find("function readGuestString")
            .expect("guest byte reader end");
        let section = &section[..end];
        assert!(section.contains("const end = start + length;"));
        assert!(section.contains("end > instanceMemory.buffer.byteLength"));
        assert!(section.contains("throw execError('EFAULT'"));
        assert!(
            section.find("throw execError('EFAULT'").unwrap()
                < section.find("new Uint8Array(").unwrap()
        );
        assert_eq!(
            runner
                .matches("case 'EFAULT':\n      return WASI_ERRNO_FAULT;")
                .count(),
            2
        );
    }

    #[test]
    fn record_lock_query_prevalidates_its_atomic_copyout() {
        let runner = include_str!("../assets/runners/wasm-runner.mjs");
        let start = runner
            .find("        fd_record_lock(\n")
            .expect("record-lock import");
        let section = &runner[start..];
        let end = section
            .find("        proc_closefrom(")
            .expect("record-lock import end");
        let section = &section[..end];
        let validation = section
            .find("!guestRangesAreValid(\n              [retTypePtr, 4],")
            .expect("record-lock output validation");
        let host_query = section
            .find("callSyncRpc('process.fd_record_lock'")
            .expect("record-lock host query");
        assert!(validation < host_query);
    }

    #[test]
    fn managed_fexecve_snapshots_the_exact_kernel_description() {
        let runner = include_str!("../assets/runners/wasm-runner.mjs");
        let start = runner
            .find("function loadExecImageFromFd(fd, argv, closeFds) {")
            .expect("fd exec loader");
        let section = &runner[start..];
        let end = section
            .find("function traceHostProcess")
            .expect("fd exec loader end");
        let section = &section[..end];
        let managed = section.find("if (SIDECAR_MANAGED_PROCESS) {").unwrap();
        let projection = section
            .find("const handle = lookupFdHandle(descriptor);")
            .unwrap();
        assert!(managed < projection);
        assert!(section.contains("canonicalKernelFdForSpawnAction(descriptor)"));
        assert!(section.contains("callSyncRpc('process.exec_image_open_fd'"));
        assert!(section.contains("callSyncRpc('process.exec_image_read'"));
        assert!(section.contains("callSyncRpc('process.exec_image_close'"));
        assert!(!section[managed..projection].contains("fsModule."));
    }

    #[test]
    fn managed_closefrom_uses_one_bulk_kernel_mutation() {
        let runner = include_str!("../assets/runners/wasm-runner.mjs");
        let start = runner
            .find("        proc_closefrom(")
            .expect("closefrom import");
        let section = &runner[start..];
        let end = section
            .find("        fd_socketpair(")
            .expect("closefrom end");
        let section = &section[..end];
        assert_eq!(
            section
                .matches("callSyncRpc('process.fd_closefrom'")
                .count(),
            1
        );
        assert!(section.contains("exactKernelFds.add(Number(handle.targetFd) >>> 0)"));
        assert!(section.contains("[...exactKernelFds]"));
        assert!(section.contains("response?.closedFds"));
        assert!(section.contains("forgetSidecarClosedKernelTargetFd(fd)"));
        assert!(section.contains("handle?.internalPreopen === true"));
    }

    #[test]
    fn managed_fiemap_reads_one_indexed_extent() {
        let runner = include_str!("../assets/runners/wasm-runner.mjs");
        let start = runner.find("  fd_fiemap(").expect("fiemap import");
        let section = &runner[start..];
        let end = section.find("  fd_punch_hole(").expect("fiemap end");
        let section = &section[..end];
        assert!(section.contains("callSyncRpc('fs.fiemapAtSync'"));
        assert!(!section.contains("callSyncRpc('fs.fiemapSync'"));
        assert!(section.contains("return WASI_ERRNO_NODATA;"));
    }

    #[test]
    fn dispose_context_reclaims_wasm_and_nested_javascript_metadata() {
        let mut engine = WasmExecutionEngine::default();
        let baseline = (
            engine.context_count_for_test(),
            engine.javascript_context_count_for_test(),
        );
        let context = engine.create_context(CreateWasmContextRequest {
            vm_id: String::from("vm-wasm-context-dispose"),
            module_path: None,
        });
        assert_eq!(engine.context_count_for_test(), baseline.0 + 1);
        assert_eq!(engine.javascript_context_count_for_test(), baseline.1 + 1);

        assert!(engine.dispose_context(&context.context_id));
        assert_eq!(
            (
                engine.context_count_for_test(),
                engine.javascript_context_count_for_test(),
            ),
            baseline
        );
    }

    fn request_with_env(cwd: &Path, env: BTreeMap<String, String>) -> StartWasmExecutionRequest {
        // Translate the remaining runner-bootstrap `AGENTOS_WASM_*` limit env
        // keys these tests express into typed limits.
        let parse = |key: &str| env.get(key).and_then(|value| value.parse::<u64>().ok());
        let limits = WasmExecutionLimits {
            active_cpu_time_limit_ms: None,
            wall_clock_limit_ms: None,
            deterministic_fuel: None,
            max_memory_bytes: parse(WASM_MAX_MEMORY_BYTES_ENV),
            max_stack_bytes: parse(WASM_MAX_STACK_BYTES_ENV),
            max_module_file_bytes: None,
            max_spawn_file_actions: None,
            max_spawn_file_action_bytes: None,
            prewarm_timeout_ms: None,
            max_open_fds: None,
            max_sockets: None,
            max_blocking_read_ms: None,
            runner_heap_limit_mb: None,
            reactor_work_quantum: None,
            bridge_call_timeout_ms: None,
            max_sync_rpc_response_line_bytes: None,
            pending_event_count: None,
            pending_event_bytes: None,
            max_threads: None,
        };
        StartWasmExecutionRequest {
            limits,
            guest_runtime: GuestRuntimeConfig::default(),
            vm_id: String::from("vm-wasm"),
            context_id: String::from("ctx-wasm"),
            managed_kernel_host: false,
            argv: Vec::new(),
            env,
            cwd: cwd.to_path_buf(),
            permission_tier: WasmPermissionTier::Full,
        }
    }

    fn wasi_imports_from_source(source: &str) -> BTreeSet<String> {
        let table_start = source
            .find("this.wasiImport = {")
            .expect("WASI source should define a wasiImport table");
        let table_body = &source[table_start + "this.wasiImport = {".len()..];
        let table_end = table_body
            .find("\n      };")
            .expect("WASI source should close the wasiImport table");

        table_body[..table_end]
            .lines()
            .filter_map(|line| {
                let (name, _) = line.trim_start().split_once(':')?;
                name.chars()
                    .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
                    .then(|| name.to_string())
            })
            .collect()
    }

    fn wasm_sync_rpc_request(method: &str) -> HostRpcRequest {
        HostRpcRequest {
            id: 1,
            method: method.to_string(),
            args: Vec::new(),
            raw_bytes_args: Default::default(),
        }
    }

    /// Build a request whose typed limits and `AGENTOS_WASM_*` env disagree, so a
    /// reader that still consulted env would observe the (wrong) env value.
    fn request_with_typed_limits_and_misleading_env(
        limits: WasmExecutionLimits,
    ) -> StartWasmExecutionRequest {
        StartWasmExecutionRequest {
            limits,
            guest_runtime: GuestRuntimeConfig::default(),
            vm_id: String::from("vm-wasm"),
            context_id: String::from("ctx-wasm"),
            managed_kernel_host: false,
            argv: Vec::new(),
            // Deliberately huge remaining bootstrap env values: if any migrated
            // limit were still sourced from env, assertions would observe these.
            env: BTreeMap::from([
                (
                    String::from(WASM_MAX_MEMORY_BYTES_ENV),
                    String::from("999999"),
                ),
                (
                    String::from(WASM_MAX_STACK_BYTES_ENV),
                    String::from("999999"),
                ),
                (
                    String::from("AGENTOS_WASM_PREWARM_TIMEOUT_MS"),
                    String::from("999999"),
                ),
                (
                    String::from("AGENTOS_WASM_RUNNER_HEAP_LIMIT_MB"),
                    String::from("999999"),
                ),
                (
                    String::from(WASM_MAX_SPAWN_FILE_ACTIONS_ENV),
                    String::from("999999"),
                ),
                (
                    String::from(WASM_MAX_SPAWN_FILE_ACTION_BYTES_ENV),
                    String::from("999999"),
                ),
            ]),
            cwd: PathBuf::from("/tmp"),
            permission_tier: WasmPermissionTier::Full,
        }
    }

    #[test]
    fn wasm_limits_are_read_from_typed_fields_and_env_is_inert() {
        let request = request_with_typed_limits_and_misleading_env(WasmExecutionLimits {
            active_cpu_time_limit_ms: Some(1_234),
            wall_clock_limit_ms: Some(25),
            deterministic_fuel: None,
            max_memory_bytes: Some(65_536),
            max_stack_bytes: Some(131_072),
            max_module_file_bytes: Some(262_144),
            max_spawn_file_actions: Some(7),
            max_spawn_file_action_bytes: Some(321),
            prewarm_timeout_ms: Some(750),
            max_open_fds: None,
            max_sockets: None,
            max_blocking_read_ms: None,
            runner_heap_limit_mb: Some(512),
            reactor_work_quantum: Some(64),
            bridge_call_timeout_ms: Some(30_000),
            max_sync_rpc_response_line_bytes: None,
            pending_event_count: None,
            pending_event_bytes: None,
            max_threads: None,
        });

        assert_eq!(
            resolve_wasm_wall_clock_limit(&request).expect("wall-clock limit"),
            Some(Duration::from_millis(25)),
            "wall-clock time must come from the typed wire limit"
        );
        assert_eq!(
            wasm_memory_limit_bytes(&request).expect("memory limit"),
            Some(65_536),
            "memory must come from the typed wire limit, not AGENTOS_WASM_MAX_MEMORY_BYTES"
        );
        let stack_error = resolve_wasm_stack_limit_bytes(&request)
            .expect_err("an unenforceable stack limit must fail closed");
        assert!(
            stack_error.to_string().contains("131072"),
            "the typed error must name the configured stack limit: {stack_error}"
        );
        assert_eq!(
            resolve_wasm_prewarm_timeout(&request).expect("prewarm timeout"),
            Duration::from_millis(750),
            "prewarm timeout must come from the typed wire limit, not AGENTOS_WASM_PREWARM_TIMEOUT_MS"
        );
        assert_eq!(
            wasm_runner_heap_limit_mb(&request),
            512,
            "runner heap must come from the typed wire limit, not AGENTOS_WASM_RUNNER_HEAP_LIMIT_MB"
        );
        assert_eq!(
            wasm_runner_javascript_limits(&request.limits, wasm_runner_heap_limit_mb(&request))
                .cpu_time_limit_ms,
            Some(1_234),
        );
    }

    #[test]
    fn wasm_limits_leave_wall_clock_disabled_when_unset_even_with_env_present() {
        // Same misleading env, but no typed limits: no wall-clock timeout
        // (the runner's V8 TRUE-CPU budget bounds runaways), and memory and
        // stack limits remain absent.
        let request = request_with_typed_limits_and_misleading_env(WasmExecutionLimits::default());

        assert_eq!(
            resolve_wasm_wall_clock_limit(&request).expect("wall clock"),
            None
        );
        assert_eq!(wasm_memory_limit_bytes(&request).expect("memory"), None);
        assert_eq!(
            resolve_wasm_stack_limit_bytes(&request).expect("stack"),
            None
        );
        assert_eq!(
            resolve_wasm_prewarm_timeout(&request).expect("prewarm"),
            Duration::from_millis(DEFAULT_WASM_PREWARM_TIMEOUT_MS)
        );
        assert_eq!(
            wasm_runner_heap_limit_mb(&request),
            DEFAULT_WASM_RUNNER_HEAP_LIMIT_MB
        );
    }

    #[test]
    fn wasm_internal_env_scrubs_migrated_limit_env_keys() {
        let request = request_with_typed_limits_and_misleading_env(WasmExecutionLimits {
            active_cpu_time_limit_ms: Some(1_234),
            wall_clock_limit_ms: Some(25),
            deterministic_fuel: None,
            max_memory_bytes: Some(65_536),
            max_stack_bytes: Some(131_072),
            max_module_file_bytes: Some(262_144),
            max_spawn_file_actions: Some(7),
            max_spawn_file_action_bytes: Some(321),
            prewarm_timeout_ms: Some(750),
            max_open_fds: None,
            max_sockets: None,
            max_blocking_read_ms: None,
            runner_heap_limit_mb: Some(512),
            reactor_work_quantum: Some(64),
            bridge_call_timeout_ms: Some(30_000),
            max_sync_rpc_response_line_bytes: None,
            pending_event_count: None,
            pending_event_bytes: None,
            max_threads: None,
        });
        let resolved_module = ResolvedWasmModule {
            specifier: String::from("./guest.wasm"),
            resolved_path: PathBuf::from("/tmp/guest.wasm"),
        };

        let internal_env =
            build_wasm_internal_env(&resolved_module, &request, 1_234, false).expect("env");

        assert_eq!(
            internal_env.get(WASM_MAX_MEMORY_BYTES_ENV),
            Some(&String::from("65536"))
        );
        assert_eq!(
            internal_env.get(WASM_MAX_MODULE_FILE_BYTES_ENV),
            Some(&String::from("262144"))
        );
        assert_eq!(
            internal_env.get(WASM_MAX_SPAWN_FILE_ACTIONS_ENV),
            Some(&String::from("7"))
        );
        assert_eq!(
            internal_env.get(WASM_MAX_SPAWN_FILE_ACTION_BYTES_ENV),
            Some(&String::from("321"))
        );
        assert_eq!(
            internal_env.get(WASM_INTERNAL_MAX_STACK_BYTES_ENV),
            Some(&String::from("131072"))
        );
        assert!(!internal_env.contains_key(WASM_MAX_STACK_BYTES_ENV));
        assert!(!internal_env.contains_key("AGENTOS_WASM_PREWARM_TIMEOUT_MS"));
        assert!(!internal_env.contains_key("AGENTOS_WASM_RUNNER_HEAP_LIMIT_MB"));
    }

    #[test]
    fn wasm_runner_base_env_scrubs_migrated_limit_env_keys() {
        let mut request = request_with_typed_limits_and_misleading_env(WasmExecutionLimits {
            active_cpu_time_limit_ms: Some(1_234),
            wall_clock_limit_ms: Some(25),
            deterministic_fuel: None,
            max_memory_bytes: Some(65_536),
            max_stack_bytes: Some(131_072),
            max_module_file_bytes: Some(262_144),
            max_spawn_file_actions: Some(7),
            max_spawn_file_action_bytes: Some(321),
            prewarm_timeout_ms: Some(750),
            max_open_fds: None,
            max_sockets: None,
            max_blocking_read_ms: None,
            runner_heap_limit_mb: Some(512),
            reactor_work_quantum: Some(64),
            bridge_call_timeout_ms: Some(30_000),
            max_sync_rpc_response_line_bytes: None,
            pending_event_count: None,
            pending_event_bytes: None,
            max_threads: None,
        });
        request
            .env
            .insert(String::from("USER_VISIBLE"), String::from("kept"));
        request
            .env
            .insert(String::from("AGENTOS_TRACE_ID"), String::from("kept"));

        let env = wasm_runner_base_env(&request);

        assert_eq!(env.get("USER_VISIBLE"), Some(&String::from("kept")));
        assert_eq!(env.get("AGENTOS_TRACE_ID"), Some(&String::from("kept")));
        assert!(!env.contains_key(WASM_MAX_MEMORY_BYTES_ENV));
        assert!(!env.contains_key(WASM_MAX_MODULE_FILE_BYTES_ENV));
        assert!(!env.contains_key(WASM_MAX_STACK_BYTES_ENV));
        assert!(!env.contains_key("AGENTOS_WASM_PREWARM_TIMEOUT_MS"));
        assert!(!env.contains_key("AGENTOS_WASM_RUNNER_HEAP_LIMIT_MB"));
    }

    #[test]
    fn wasm_snapshot_runner_base_env_scrubs_internal_and_migrated_limit_env_keys() {
        let mut request = request_with_typed_limits_and_misleading_env(WasmExecutionLimits {
            active_cpu_time_limit_ms: Some(1_234),
            wall_clock_limit_ms: Some(25),
            deterministic_fuel: None,
            max_memory_bytes: Some(65_536),
            max_stack_bytes: Some(131_072),
            max_module_file_bytes: Some(262_144),
            max_spawn_file_actions: Some(7),
            max_spawn_file_action_bytes: Some(321),
            prewarm_timeout_ms: Some(750),
            max_open_fds: None,
            max_sockets: None,
            max_blocking_read_ms: None,
            runner_heap_limit_mb: Some(512),
            reactor_work_quantum: Some(64),
            bridge_call_timeout_ms: Some(30_000),
            max_sync_rpc_response_line_bytes: None,
            pending_event_count: None,
            pending_event_bytes: None,
            max_threads: None,
        });
        request
            .env
            .insert(String::from("USER_VISIBLE"), String::from("kept"));
        request.env.insert(
            String::from("NODE_SYNC_RPC_WAIT_TIMEOUT_MS"),
            String::from("999"),
        );

        let env = wasm_snapshot_runner_base_env(&request);

        assert_eq!(env.get("USER_VISIBLE"), Some(&String::from("kept")));
        assert!(!env.contains_key("NODE_SYNC_RPC_WAIT_TIMEOUT_MS"));
        assert!(!env.contains_key(WASM_MAX_MEMORY_BYTES_ENV));
        assert!(!env.contains_key(WASM_MAX_STACK_BYTES_ENV));
        assert!(!env.contains_key("AGENTOS_WASM_PREWARM_TIMEOUT_MS"));
        assert!(!env.contains_key("AGENTOS_WASM_RUNNER_HEAP_LIMIT_MB"));
    }

    #[test]
    fn wasm_stack_limit_of_zero_is_rejected() {
        let request = request_with_typed_limits_and_misleading_env(WasmExecutionLimits {
            max_stack_bytes: Some(0),
            ..WasmExecutionLimits::default()
        });

        assert!(
            resolve_wasm_stack_limit_bytes(&request).is_err(),
            "a zero stack cap must fail closed rather than be silently dropped"
        );
    }

    #[test]
    fn resolved_module_path_canonicalizes_path_like_specifiers() {
        let temp = tempdir().expect("create temp dir");
        let real = temp.path().join("real.wasm");
        let alias = temp.path().join("alias.wasm");
        fs::write(&real, b"\0asm\x01\0\0\0").expect("write wasm file");
        symlink(&real, &alias).expect("create wasm symlink");

        let resolved = resolved_module_path("./alias.wasm", temp.path());

        assert_eq!(
            resolved,
            real.canonicalize().expect("canonicalize wasm target")
        );
    }

    #[test]
    fn wasm_prewarm_timeout_is_separate_from_wall_clock_limit() {
        let temp = tempdir().expect("create temp dir");
        let mut request = request_with_env(temp.path(), BTreeMap::new());
        request.limits.wall_clock_limit_ms = Some(25);
        request.limits.prewarm_timeout_ms = Some(750);

        assert_eq!(
            resolve_wasm_wall_clock_limit(&request).expect("wall-clock limit"),
            Some(Duration::from_millis(25))
        );
        assert_eq!(
            resolve_wasm_prewarm_timeout(&request).expect("prewarm timeout"),
            Duration::from_millis(750)
        );
    }

    // No explicit wall-clock budget means no wasm-specific elapsed timeout. Runaway
    // wasm stays bounded by the runner isolate's active-CPU watchdog, so idle
    // interactive guests are not killed on wall time.
    #[test]
    fn wasm_wall_clock_limit_is_unset_by_default() {
        let temp = tempdir().expect("create temp dir");
        let request = request_with_env(temp.path(), BTreeMap::new());

        let timeout =
            resolve_wasm_wall_clock_limit(&request).expect("wall-clock limit resolves when unset");

        assert_eq!(
            timeout, None,
            "no explicit wall-clock limit means no elapsed timeout; the runner \
             isolate's TRUE-CPU budget (default 30s active CPU) is the bound \
             that terminates an infinite-loop module (F-004), so an idle \
             interactive guest is not killed on wall time"
        );
    }

    #[test]
    fn v8_rejects_explicit_deterministic_fuel_with_typed_error() {
        let temp = tempdir().expect("create temp dir");
        let mut request = request_with_env(temp.path(), BTreeMap::new());
        request.limits.deterministic_fuel = Some(123_456);

        let error =
            reject_v8_deterministic_fuel(&request).expect_err("V8 cannot meter deterministic fuel");
        assert!(matches!(
            error,
            WasmExecutionError::DeterministicFuelUnsupported { fuel: 123_456 }
        ));
    }

    #[test]
    fn wasm_captured_output_rejects_output_over_limit() {
        let mut stdout = vec![b'x'; WASM_CAPTURED_OUTPUT_LIMIT_BYTES - 1];
        super::append_wasm_captured_output(&mut stdout, b"y", "stdout").expect("fill to limit");
        assert_eq!(stdout.len(), WASM_CAPTURED_OUTPUT_LIMIT_BYTES);

        let error = super::append_wasm_captured_output(&mut stdout, b"z", "stdout")
            .expect_err("captured output over limit should fail");
        assert!(matches!(
            error,
            WasmExecutionError::OutputBufferExceeded {
                stream: "stdout",
                limit: WASM_CAPTURED_OUTPUT_LIMIT_BYTES,
            }
        ));
    }

    #[test]
    fn wasm_sync_read_length_rejects_oversized_guest_lengths() {
        assert_eq!(
            wasm_sync_read_length(Some(WASM_SYNC_READ_LIMIT_BYTES as u64))
                .expect("max read length should be accepted"),
            WASM_SYNC_READ_LIMIT_BYTES
        );

        let error = wasm_sync_read_length(Some(WASM_SYNC_READ_LIMIT_BYTES as u64 + 1))
            .expect_err("oversized read length should fail before allocation");
        assert!(
            matches!(error, WasmExecutionError::InvalidLimit(message) if message.contains("fs.readSync length"))
        );
    }

    #[test]
    fn wasm_bytes_arg_rejects_payloads_over_limit_before_decode() {
        let mut payload = serde_json::Map::new();
        payload.insert(
            String::from("base64"),
            Value::String(String::from("YWJjZA==")),
        );

        let error =
            super::decode_wasm_bytes_arg(Some(&Value::Object(payload)), "fs.writeSync bytes", 3)
                .expect_err("decoded bytes over limit should fail before allocation");

        assert!(matches!(
            error,
            WasmExecutionError::OutputBufferExceeded {
                stream: "fs.writeSync bytes",
                limit: 3,
            }
        ));
    }

    #[test]
    fn wasm_runner_bootstrap_caps_wasi_iov_lengths_before_allocation() {
        let bootstrap = build_wasm_runner_bootstrap(&BTreeMap::new(), None);

        // The read cap now comes from the per-backend host seam, with the native
        // build-substituted constant as the fallback; assert the constant is
        // defined and the placeholder was substituted to the value.
        assert!(bootstrap.contains("const __agentOSWasmSyncReadLimitBytes ="));
        assert!(bootstrap.contains(&format!(": {WASM_SYNC_READ_LIMIT_BYTES};")));
        assert!(!bootstrap.contains("__AGENTOS_WASM_SYNC_READ_LIMIT_BYTES__"));
        assert!(bootstrap.contains("_boundedIovLength(iovs, iovsLen)"));
        assert!(bootstrap.contains("const totalLength = this._boundedIovLength(iovs, iovsLen);\n      const view = this._memoryView();"));
        assert!(bootstrap.contains("return Buffer.concat(chunks, totalLength);"));
        assert!(bootstrap.contains("const totalLength = this._boundedIovLength(iovs, iovsLen);"));
        assert!(!bootstrap.contains("const totalLength = (() => {"));
    }

    #[test]
    fn wasi_preview1_import_manifest_matches_native_runner() {
        let manifest: Value = serde_json::from_str(include_str!("../assets/agentos-wasm-abi.json"))
            .expect("parse AgentOS WASM ABI manifest");
        let expected = manifest["imports"]
            .as_array()
            .expect("ABI imports array")
            .iter()
            .filter(|entry| {
                entry["module"] == "wasi_snapshot_preview1" && entry["status"] != "compatibility"
            })
            .map(|entry| entry["name"].as_str().expect("ABI import name").to_string())
            .collect::<BTreeSet<_>>();

        assert_eq!(expected, wasi_imports_from_source(NODE_WASI_MODULE_SOURCE));
    }

    #[test]
    fn wasm_guest_module_paths_include_mapped_guest_paths_for_host_specifiers() {
        let temp = tempdir().expect("create temp dir");
        let command_root = temp.path().join("commands");
        let module = command_root.join("hello");
        fs::create_dir_all(&command_root).expect("create command root");
        fs::write(&module, b"\0asm\x01\0\0\0").expect("write wasm file");

        let candidates = wasm_guest_module_paths(
            module.to_string_lossy().as_ref(),
            &BTreeMap::from([(
                String::from("AGENTOS_GUEST_PATH_MAPPINGS"),
                format!(
                    "[{{\"guestPath\":\"/__agentos/commands/0\",\"hostPath\":\"{}\"}}]",
                    command_root.display()
                ),
            )]),
        );

        assert!(candidates.contains(&module.to_string_lossy().into_owned()));
        assert!(candidates.contains(&String::from("/__agentos/commands/0/hello")));
    }

    #[test]
    fn translate_wasm_guest_path_uses_sandbox_root_for_absolute_paths() {
        let temp = tempdir().expect("create temp dir");
        let sandbox_root = temp.path().join("shadow-root");
        let cwd = sandbox_root.join("workspace");
        fs::create_dir_all(cwd.join("project")).expect("create host cwd");

        let internal_sync_rpc = WasmInternalSyncRpc {
            module_guest_paths: Vec::new(),
            module_host_path: sandbox_root.join("module.wasm"),
            guest_cwd: String::from("/workspace"),
            host_cwd: cwd.clone(),
            sandbox_root: Some(sandbox_root.clone()),
            guest_path_mappings: Vec::new(),
            route_fs_through_sidecar: false,
            next_fd: 64,
            open_files: Default::default(),
            pending_events: WasmEventQueue::default(),
        };

        assert_eq!(
            translate_wasm_guest_path("/tmp/redir.txt", &internal_sync_rpc),
            Some(sandbox_root.join("tmp/redir.txt"))
        );
        assert_eq!(
            translate_wasm_guest_path("project/output.txt", &internal_sync_rpc),
            Some(cwd.join("project/output.txt"))
        );
    }

    #[test]
    fn translate_wasm_host_symlink_target_returns_guest_path_for_mapped_targets() {
        let temp = tempdir().expect("create temp dir");
        let sandbox_root = temp.path().join("shadow-root");
        let cwd = sandbox_root.join("workspace");
        fs::create_dir_all(cwd.join("project")).expect("create host cwd");

        let internal_sync_rpc = WasmInternalSyncRpc {
            module_guest_paths: Vec::new(),
            module_host_path: sandbox_root.join("module.wasm"),
            guest_cwd: String::from("/workspace"),
            host_cwd: cwd.clone(),
            sandbox_root: Some(sandbox_root.clone()),
            guest_path_mappings: vec![super::WasmGuestPathMapping {
                guest_path: String::from("/"),
                host_path: sandbox_root.clone(),
                read_only: false,
            }],
            route_fs_through_sidecar: false,
            next_fd: 64,
            open_files: Default::default(),
            pending_events: WasmEventQueue::default(),
        };

        assert_eq!(
            translate_wasm_host_symlink_target(
                &sandbox_root.join("tmp/sc/pdir/r.txt"),
                &internal_sync_rpc
            ),
            Some(String::from("/tmp/sc/pdir/r.txt"))
        );
        assert_eq!(
            translate_wasm_host_symlink_target(Path::new("relative-target"), &internal_sync_rpc),
            None
        );
    }

    #[test]
    fn translate_wasm_guest_path_recovers_root_collapsed_relative_paths_from_guest_cwd() {
        let temp = tempdir().expect("create temp dir");
        let sandbox_root = temp.path().join("shadow-root");
        let cwd = temp.path().join("mounted-workspace");
        fs::create_dir_all(&sandbox_root).expect("create sandbox root");
        fs::create_dir_all(&cwd).expect("create mounted workspace");
        fs::write(cwd.join("note.txt"), b"hello").expect("write mounted file");

        let internal_sync_rpc = WasmInternalSyncRpc {
            module_guest_paths: Vec::new(),
            module_host_path: sandbox_root.join("module.wasm"),
            guest_cwd: String::from("/workspace"),
            host_cwd: cwd.clone(),
            sandbox_root: Some(sandbox_root.clone()),
            guest_path_mappings: vec![super::WasmGuestPathMapping {
                guest_path: String::from("/workspace"),
                host_path: cwd.clone(),
                read_only: false,
            }],
            route_fs_through_sidecar: false,
            next_fd: 64,
            open_files: Default::default(),
            pending_events: WasmEventQueue::default(),
        };

        assert_eq!(
            translate_wasm_guest_path("/note.txt", &internal_sync_rpc),
            Some(cwd.join("note.txt"))
        );
    }

    #[test]
    fn translate_wasm_guest_path_accepts_host_absolute_paths_within_known_roots() {
        let temp = tempdir().expect("create temp dir");
        let sandbox_root = temp.path().join("shadow-root");
        let cwd = temp.path().join("mounted-workspace");
        let mapped_root = temp.path().join("mounted-commands");
        fs::create_dir_all(&sandbox_root).expect("create sandbox root");
        fs::create_dir_all(cwd.join("subdir")).expect("create cwd");
        fs::create_dir_all(&mapped_root).expect("create mapped root");

        let internal_sync_rpc = WasmInternalSyncRpc {
            module_guest_paths: vec![String::from("/workspace/guest.wasm")],
            module_host_path: cwd.join("guest.wasm"),
            guest_cwd: String::from("/workspace"),
            host_cwd: cwd.clone(),
            sandbox_root: Some(sandbox_root.clone()),
            guest_path_mappings: vec![
                super::WasmGuestPathMapping {
                    guest_path: String::from("/workspace"),
                    host_path: cwd.clone(),
                    read_only: false,
                },
                super::WasmGuestPathMapping {
                    guest_path: String::from("/__agentos/commands/0"),
                    host_path: mapped_root.clone(),
                    read_only: false,
                },
            ],
            route_fs_through_sidecar: false,
            next_fd: 64,
            open_files: Default::default(),
            pending_events: WasmEventQueue::default(),
        };

        assert_eq!(
            translate_wasm_guest_path(cwd.to_string_lossy().as_ref(), &internal_sync_rpc),
            Some(cwd.clone())
        );
        assert_eq!(
            translate_wasm_guest_path(
                cwd.join("subdir/output.txt").to_string_lossy().as_ref(),
                &internal_sync_rpc
            ),
            Some(cwd.join("subdir/output.txt"))
        );
        assert_eq!(
            translate_wasm_guest_path(
                mapped_root.join("tool.wasm").to_string_lossy().as_ref(),
                &internal_sync_rpc
            ),
            Some(mapped_root.join("tool.wasm"))
        );
        assert_eq!(
            translate_wasm_guest_path(
                sandbox_root
                    .join("tmp/runtime.sock")
                    .to_string_lossy()
                    .as_ref(),
                &internal_sync_rpc
            ),
            Some(sandbox_root.join("tmp/runtime.sock"))
        );
    }

    #[test]
    fn translate_wasm_guest_path_rejects_symlink_escape_from_sandbox_root() {
        let temp = tempdir().expect("create temp dir");
        let sandbox_root = temp.path().join("shadow-root");
        let outside = temp.path().join("outside");
        fs::create_dir_all(&sandbox_root).expect("create sandbox root");
        fs::create_dir_all(&outside).expect("create outside root");
        fs::write(outside.join("secret.txt"), b"host secret").expect("write outside file");
        symlink(&outside, sandbox_root.join("escape")).expect("create escape symlink");

        let internal_sync_rpc = WasmInternalSyncRpc {
            module_guest_paths: Vec::new(),
            module_host_path: sandbox_root.join("module.wasm"),
            guest_cwd: String::from("/"),
            host_cwd: sandbox_root.clone(),
            sandbox_root: Some(sandbox_root.clone()),
            guest_path_mappings: vec![super::WasmGuestPathMapping {
                guest_path: String::from("/"),
                host_path: sandbox_root,
                read_only: false,
            }],
            route_fs_through_sidecar: false,
            next_fd: 64,
            open_files: Default::default(),
            pending_events: WasmEventQueue::default(),
        };

        assert_eq!(
            translate_wasm_guest_path("/escape/secret.txt", &internal_sync_rpc),
            None
        );
        assert_eq!(
            translate_wasm_guest_path("/escape/new.txt", &internal_sync_rpc),
            None
        );
    }

    #[test]
    fn wasm_read_only_mapping_blocks_mutating_host_paths() {
        let temp = tempdir().expect("create temp dir");
        let sandbox_root = temp.path().join("shadow-root");
        let readonly_root = temp.path().join("readonly");
        fs::create_dir_all(&sandbox_root).expect("create sandbox root");
        fs::create_dir_all(&readonly_root).expect("create readonly root");
        fs::write(readonly_root.join("package.json"), b"{}").expect("write readonly file");

        let internal_sync_rpc = WasmInternalSyncRpc {
            module_guest_paths: Vec::new(),
            module_host_path: sandbox_root.join("module.wasm"),
            guest_cwd: String::from("/workspace"),
            host_cwd: sandbox_root.clone(),
            sandbox_root: Some(sandbox_root),
            guest_path_mappings: vec![super::WasmGuestPathMapping {
                guest_path: String::from("/node_modules"),
                host_path: readonly_root.clone(),
                read_only: true,
            }],
            route_fs_through_sidecar: false,
            next_fd: 64,
            open_files: Default::default(),
            pending_events: WasmEventQueue::default(),
        };

        let host_path = translate_wasm_guest_path("/node_modules/package.json", &internal_sync_rpc)
            .expect("read path should resolve");
        assert_eq!(host_path, readonly_root.join("package.json"));
        assert!(wasm_host_path_is_read_only(&host_path, &internal_sync_rpc));
        assert!(wasm_host_path_is_read_only(
            &readonly_root.join("new-package.json"),
            &internal_sync_rpc
        ));
        assert_eq!(
            wasm_sync_rpc_error_code(&wasm_read_only_filesystem_error("/node_modules")),
            "EROFS"
        );
    }

    #[test]
    fn wasm_open_guest_file_errors_remain_sync_rpc_errors() {
        let temp = tempdir().expect("create temp dir");
        let missing_path = temp.path().join("missing.txt");

        let error = open_wasm_guest_file(&missing_path, &Value::from(0))
            .expect_err("missing file should return an open error");

        assert_eq!(wasm_sync_rpc_error_code(&error), "ENOENT");
    }

    #[test]
    fn wasm_hard_links_are_rejected_when_either_side_is_read_only() {
        let temp = tempdir().expect("create temp dir");
        let readonly_root = temp.path().join("readonly");
        let writable_root = temp.path().join("writable");
        fs::create_dir_all(&readonly_root).expect("create readonly root");
        fs::create_dir_all(&writable_root).expect("create writable root");
        let readonly_file = readonly_root.join("package.json");
        let writable_file = writable_root.join("source.txt");
        fs::write(&readonly_file, b"readonly").expect("write readonly source");
        fs::write(&writable_file, b"writable").expect("write writable source");

        let internal_sync_rpc = WasmInternalSyncRpc {
            module_guest_paths: Vec::new(),
            module_host_path: writable_root.join("module.wasm"),
            guest_cwd: String::from("/workspace"),
            host_cwd: writable_root.clone(),
            sandbox_root: Some(writable_root.clone()),
            guest_path_mappings: vec![
                super::WasmGuestPathMapping {
                    guest_path: String::from("/node_modules"),
                    host_path: readonly_root.clone(),
                    read_only: true,
                },
                super::WasmGuestPathMapping {
                    guest_path: String::from("/workspace"),
                    host_path: writable_root.clone(),
                    read_only: false,
                },
            ],
            route_fs_through_sidecar: false,
            next_fd: 64,
            open_files: Default::default(),
            pending_events: WasmEventQueue::default(),
        };

        assert!(wasm_mutation_touches_read_only_mapping(
            &readonly_file,
            &writable_root.join("alias-from-readonly.json"),
            &internal_sync_rpc
        ));
        assert!(wasm_mutation_touches_read_only_mapping(
            &writable_file,
            &readonly_root.join("alias-into-readonly.txt"),
            &internal_sync_rpc
        ));
        assert!(!wasm_mutation_touches_read_only_mapping(
            &writable_file,
            &writable_root.join("alias.txt"),
            &internal_sync_rpc
        ));

        let raw_alias = writable_root.join("raw-alias.json");
        fs::hard_link(&readonly_file, &raw_alias).expect("host hard link would otherwise succeed");
        fs::write(&raw_alias, b"mutated").expect("write through host hard link alias");
        assert_eq!(
            fs::read(&readonly_file).expect("read readonly source"),
            b"mutated"
        );
    }

    #[test]
    fn translate_wasm_guest_path_preserves_real_root_paths_before_guest_cwd_fallback() {
        let temp = tempdir().expect("create temp dir");
        let sandbox_root = temp.path().join("shadow-root");
        let cwd = temp.path().join("mounted-workspace");
        fs::create_dir_all(&sandbox_root).expect("create sandbox root");
        fs::create_dir_all(&cwd).expect("create mounted workspace");
        fs::write(sandbox_root.join("note.txt"), b"root").expect("write root file");
        fs::write(cwd.join("note.txt"), b"cwd").expect("write cwd file");

        let internal_sync_rpc = WasmInternalSyncRpc {
            module_guest_paths: Vec::new(),
            module_host_path: sandbox_root.join("module.wasm"),
            guest_cwd: String::from("/workspace"),
            host_cwd: cwd.clone(),
            sandbox_root: Some(sandbox_root.clone()),
            guest_path_mappings: vec![super::WasmGuestPathMapping {
                guest_path: String::from("/workspace"),
                host_path: cwd,
                read_only: false,
            }],
            route_fs_through_sidecar: false,
            next_fd: 64,
            open_files: Default::default(),
            pending_events: WasmEventQueue::default(),
        };

        assert_eq!(
            translate_wasm_guest_path("/note.txt", &internal_sync_rpc),
            Some(sandbox_root.join("note.txt"))
        );
    }

    #[test]
    fn wasm_sandbox_root_reads_absolute_env_only() {
        let sandbox_root = wasm_sandbox_root(&BTreeMap::from([(
            String::from(WASM_SANDBOX_ROOT_ENV),
            String::from("/tmp/agentos-shadow"),
        )]));
        assert_eq!(sandbox_root, Some(PathBuf::from("/tmp/agentos-shadow")));

        let relative = wasm_sandbox_root(&BTreeMap::from([(
            String::from(WASM_SANDBOX_ROOT_ENV),
            String::from("relative/shadow"),
        )]));
        assert_eq!(relative, None);
    }

    #[test]
    fn wasm_sidecar_managed_fs_methods_route_to_kernel_sync_rpc() {
        let mut standalone = WasmInternalSyncRpc {
            module_guest_paths: Vec::new(),
            module_host_path: PathBuf::from("/tmp/module.wasm"),
            guest_cwd: String::from("/"),
            host_cwd: PathBuf::from("/tmp"),
            sandbox_root: None,
            guest_path_mappings: Vec::new(),
            route_fs_through_sidecar: false,
            next_fd: 64,
            open_files: Default::default(),
            pending_events: WasmEventQueue::default(),
        };
        let sidecar_managed = WasmInternalSyncRpc {
            module_guest_paths: Vec::new(),
            module_host_path: PathBuf::from("/tmp/module.wasm"),
            guest_cwd: String::from("/"),
            host_cwd: PathBuf::from("/tmp"),
            sandbox_root: Some(PathBuf::from("/tmp/agentos-shadow")),
            guest_path_mappings: Vec::new(),
            route_fs_through_sidecar: true,
            next_fd: 64,
            open_files: Default::default(),
            pending_events: WasmEventQueue::default(),
        };

        for method in WASM_SIDECAR_ROUTED_FS_SYNC_METHODS {
            let request = wasm_sync_rpc_request(method);
            assert!(
                wasm_sync_rpc_method_routes_through_sidecar_kernel(&request, &sidecar_managed),
                "{method} should route through the sidecar kernel for managed WASI executions"
            );
            assert!(
                !wasm_sync_rpc_method_routes_through_sidecar_kernel(&request, &standalone),
                "{method} should stay host-direct for standalone/prewarm WASI execution"
            );
        }

        standalone.route_fs_through_sidecar = true;
        let non_fs_request = wasm_sync_rpc_request("child_process.spawn");
        assert!(!wasm_sync_rpc_method_routes_through_sidecar_kernel(
            &non_fs_request,
            &standalone
        ));
    }

    #[test]
    fn wasm_guest_path_mappings_mount_root_to_sandbox_root() {
        let temp = tempdir().expect("create temp dir");
        let sandbox_root = temp.path().join("shadow-root");
        let host_cwd = sandbox_root.join("workspace");
        fs::create_dir_all(&host_cwd).expect("create host cwd");

        let mappings = super::wasm_guest_path_mappings(&request_with_env(
            &host_cwd,
            BTreeMap::from([
                (String::from("PWD"), String::from("/workspace")),
                (
                    String::from(WASM_SANDBOX_ROOT_ENV),
                    sandbox_root.to_string_lossy().into_owned(),
                ),
            ]),
        ));

        assert!(mappings
            .iter()
            .any(|mapping| { mapping.guest_path == "/" && mapping.host_path == sandbox_root }));
        assert!(mappings.iter().any(|mapping| {
            mapping.guest_path == "/workspace" && mapping.host_path == host_cwd
        }));
    }

    #[test]
    fn wasm_runner_bootstrap_keeps_root_preopens_rooted() {
        let bootstrap = build_wasm_runner_bootstrap(&BTreeMap::new(), None);

        assert!(bootstrap.contains("if (guestPath === \".\") {"));
        assert!(!bootstrap.contains("if (guestPath === \".\" || guestPath === \"/\") {"));
    }

    #[test]
    fn wasm_runner_bootstrap_exposes_unix_socket_sync_rpcs() {
        let bootstrap = build_wasm_runner_bootstrap(&BTreeMap::new(), None);

        for (method, bridge) in [
            ("net.bind_unix", "_netBindUnixRaw.applySync"),
            (
                "net.bind_connected_unix",
                "_netBindConnectedUnixRaw.applySync",
            ),
            ("net.server_close", "_netServerCloseSyncRaw.applySync"),
            (
                "net.socket_wait_connect",
                "_netSocketWaitConnectSyncRaw.applySync",
            ),
            ("net.write", "_netSocketWriteSyncRaw.applySync"),
        ] {
            assert!(
                bootstrap.contains(&format!("case \"{method}\":")),
                "missing WASM sync RPC case for {method}"
            );
            assert!(
                bootstrap.contains(bridge),
                "missing synchronous V8 bridge call for {method}"
            );
        }
    }

    #[test]
    fn wasm_runner_bootstrap_reports_dot_preopen_to_wasi() {
        let bootstrap = build_wasm_runner_bootstrap(&BTreeMap::new(), None);

        // The dot preopen must resolve through the guest cwd, never surface as a
        // literal "." (restructured into _currentGuestCwd/_descriptorPreopenName
        // by the wasi-shim stat-path rework).
        assert!(bootstrap.contains("_currentGuestCwd()"));
        assert!(!bootstrap.contains("preopens['.'] = createPreopen(HOST_CWD, cwdReadOnly);"));
        assert!(bootstrap.contains("_descriptorPreopenName(entry)"));
        assert!(bootstrap.contains(
            "if (guestPath === \".\") {\n        return this._descriptorGuestPath(entry);"
        ));
        assert!(bootstrap.contains("const guestPath = this._descriptorPreopenName(entry);"));
    }

    #[test]
    fn wasm_runner_path_open_uses_guest_mapping_for_absolute_paths() {
        let bootstrap = build_wasm_runner_bootstrap(&BTreeMap::new(), None);

        assert!(bootstrap
            .contains("const resolved = this._resolveDescriptorPath(fd, pathPtr, pathLen, {"));
        assert!(
            !bootstrap.contains("const hostPath = __agentOSPath().resolve(baseHostPath, target);")
        );
    }

    #[test]
    fn wasm_runner_root_preopen_relative_paths_preserve_cwd_fallback() {
        let bootstrap = build_wasm_runner_bootstrap(&BTreeMap::new(), None);

        assert!(bootstrap
            .contains("const rootGuestPath = __agentOSPath().posix.resolve(\"/\", target);"));
        assert!(bootstrap.contains(
            "const cwdGuestTarget = __agentOSPath().posix.resolve(cwdGuestPath, target);"
        ));
        assert!(bootstrap.contains("_rootRelativeTargetPrefersCwd(target)"));
        assert!(bootstrap.contains("_mappedPathExists(cwdGuestTarget, cwdHostTarget)"));
        assert!(bootstrap.contains("_mappedPathExists(rootGuestPath, rootHostPath)"));
        assert!(bootstrap
            .contains("__agentOSWasiSyncRpc().callSync(\"fs.statSync\", [sidecarGuestPath])"));
        assert!(bootstrap.contains("_rootRelativeTargetMatchesAbsoluteArg(target)"));
        assert!(bootstrap.contains("__agentOSPath().posix.normalize(arg) === rootGuestPath"));
        assert!(bootstrap.contains("_createParentExists(guestPath, hostPath)"));
        assert!(bootstrap.contains(
            "preferCreateParent &&\n              !this._rootRelativeTargetIsWithinAbsoluteArg(target)"
        ));
        assert!(bootstrap.contains("this._createParentExists(cwdGuestTarget, cwdHostTarget)"));
    }

    #[test]
    fn wasm_runner_readdir_uses_guest_preopen_path_in_sidecar() {
        let bootstrap = build_wasm_runner_bootstrap(&BTreeMap::new(), None);

        assert!(bootstrap.contains("const fsPath = this._descriptorDirectoryFsPath(entry);"));
        assert!(
            bootstrap.contains("(entry?.kind === \"preopen\" || entry?.kind === \"directory\")")
        );
    }

    #[test]
    fn wasm_runner_blocks_read_only_fd_write_paths() {
        let bootstrap = build_wasm_runner_bootstrap(&BTreeMap::new(), None);

        assert!(bootstrap.contains("readOnly: entry.readOnly === true,"));
        assert!(bootstrap.contains(
            "if (handle.readOnly === true) {\n            return __agentOSWasiErrnoRofs;\n          }"
        ));
        assert!(bootstrap.contains(
            "if (entry.readOnly === true) {\n          return __agentOSWasiErrnoRofs;\n        }\n        const written = __agentOSFs().writeSync("
        ));
    }

    #[test]
    fn managed_host_network_fds_use_kernel_description_authority() {
        let runner = include_str!("../assets/runners/wasm-runner.mjs");

        assert!(runner.contains("const HOST_NET_SOCK_NONBLOCK = 0x4000;"));
        assert!(runner.contains("const HOST_NET_SOCK_CLOEXEC = 0x2000;"));
        assert!(runner.contains("const standaloneHostNetSockets = new Map();"));
        assert!(!runner.contains("const managedHostNetDescriptions = new Map();"));
        assert!(!runner.contains("const hostNetSockets = new Map();"));
        assert!(runner.contains("callSyncRpc('process.hostnet_fd_open'"));
        assert!(runner.contains("callSyncRpc('process.hostnet_validate'"));
        assert!(runner.contains("validateHostNetSocketDescriptor(fd, true)"));
        assert!(!runner.contains("callSyncRpc('process.fd_description_alias_count'"));
        assert!(runner.contains("callSyncRpc('process.fd_dup'"));
        assert!(runner.contains("attachManagedHostNetDescription(guestFd, descriptionId);"));
        assert!(runner.contains("descriptionId: handle?.hostNetDescriptionId ?? null"));
        assert!(runner.contains("fd = registerKernelDelegateFd(received.fd);"));
        assert!(runner.contains("const managedHostNetKernelGuestFds = new Set("));
        assert!(runner.contains(
            "initialKernelGuestFds.has(guestFd) && !managedHostNetKernelGuestFds.has(guestFd)"
        ));
    }

    #[test]
    fn managed_runner_keeps_only_bounded_process_and_fd_projections() {
        let runner = include_str!("../assets/runners/wasm-runner.mjs");

        for obsolete in [
            "const spawnedChildren =",
            "const spawnedChildrenById =",
            "const syntheticFdEntries =",
            "runnerCloexecFds",
            "delegateManagedFdRefCounts",
        ] {
            assert!(
                !runner.contains(obsolete),
                "obsolete authority map: {obsolete}"
            );
        }
        for required in [
            "const childCorrelationsByPid = new Map();",
            "const childCorrelationsById = new Map();",
            "const managedKernelFdProjections = new Map();",
            "const standaloneSyntheticFdEntries = new Map();",
            "const standaloneCloexecFds = new Set();",
            "const standaloneDelegateFdRefCounts = new Map();",
            "managed descriptor projections must reference a kernel fd",
            "managed execution cannot retain a Node-WASI delegate fd",
            "callSyncRpc('process.fd_getfd'",
            "callSyncRpc('process.fd_path'",
            "callSyncRpc('process.fd_move'",
            "callSyncRpc('process.waitpid'",
            "record.terminalEventObserved = true;",
            "function collectStaleManagedChildCorrelations()",
            "...(SIDECAR_MANAGED_PROCESS ? {} : { processGroup })",
        ] {
            assert!(
                runner.contains(required),
                "missing managed-mode guard: {required}"
            );
        }
        assert!(runner
            .contains("const handle = {\n    kind: 'kernel-fd',\n    targetFd: kernelFd,\n  };"));
        assert!(!runner.contains("openedHandle.guestPath ="));
        assert!(!runner.contains("callSyncRpc('child_process.kill'"));
        assert!(runner.contains("collectStaleManagedChildCorrelations();\n    return false;"));
    }

    #[test]
    fn managed_guest_fd_reuse_does_not_close_or_replace_hidden_preopen_backing_descriptors() {
        let runner = include_str!("../assets/runners/wasm-runner.mjs");
        assert!(runner.contains(
            "if (handle.kind === 'kernel-fd') {\n    // Managed preopens are hidden capability roots"
        ));
        assert!(runner.contains(
            "if (handle.internalPreopen === true) {\n      return;\n    }\n    callSyncRpc('process.fd_close'"
        ));
        assert!(runner.contains("function kernelFdMappingsForSpawn()"));
        assert!(runner.contains("handle.open !== false &&\n      handle.internalPreopen !== true"));
        let start = runner
            .find("wasiImport.fd_renumber = (from, to) => {")
            .expect("fd_renumber implementation");
        let end = runner[start..]
            .find("wasiImport.poll_oneoff =")
            .map(|offset| start + offset)
            .expect("fd_renumber implementation end");
        let implementation = &runner[start..end];
        assert!(implementation.contains(
            "managedTarget?.internalPreopen === true && hiddenPreopenHandles.has(targetFd)"
        ));
        assert!(implementation
            .contains("managedTarget?.kind === 'kernel-fd' && !shadowsInternalPreopen"));
        assert!(implementation.contains("shadowsInternalPreopen,"));
    }

    #[test]
    fn host_process_errno_mapping_uses_only_typed_error_codes() {
        let runner = include_str!("../assets/runners/wasm-runner.mjs");
        let start = runner
            .find("function mapHostProcessError(error) {")
            .expect("host process errno mapper");
        let end = runner[start..]
            .find("function seekGuestFileHandle")
            .map(|offset| start + offset)
            .expect("host process errno mapper end");
        let mapper = &runner[start..end];
        assert!(mapper.contains("case 'ENOENT':\n      return WASI_ERRNO_NOENT;"));
        assert!(mapper.contains("case 'EINTR':\n      return WASI_ERRNO_INTR;"));
        assert!(mapper.contains("default:\n      return WASI_ERRNO_FAULT;"));
        assert!(!mapper.contains("command not found"));
        assert!(!mapper.contains("error?.message"));
    }

    #[test]
    fn managed_signal_dispatch_drains_the_published_delivery_without_double_claiming() {
        let runner = include_str!("../assets/runners/wasm-runner.mjs");
        let start = runner
            .find("function dispatchPendingWasmSignals(")
            .expect("signal dispatch helper");
        let helper = &runner[start..];
        let end = helper
            .find("Object.defineProperty(globalThis, '__secureExecWasmSignalDispatch'")
            .expect("signal dispatch helper end");
        let helper = &helper[..end];

        assert_eq!(
            helper.matches("callSyncRpc('process.take_signal'").count(),
            1
        );
        assert!(!helper.contains("callSyncRpc('process.signal_begin'"));
        assert!(runner.contains("callSyncRpc('process.signal_end', [token])"));
        assert!(runner.contains("function pumpSpawnedChildrenOrWaitRestartable(waitMs)"));
        assert!(runner.contains("return dispatchPendingWasmSignals(true);"));
        assert!(runner.contains("numericSignal === 0 || numericSignal > 64"));
        assert!(runner.contains(
            "registration.action === 'user' &&\n              typeof instance?.exports?.__wasi_signal_trampoline !== 'function'"
        ));
    }

    #[test]
    fn managed_wait_uses_bounded_kernel_authoritative_direct_replies() {
        let runner = include_str!("../assets/runners/wasm-runner.mjs");
        let start = runner
            .find("function takeManagedWaitTransition(")
            .expect("managed wait helper");
        let end = runner[start..]
            .find("function reapManagedChildCorrelation(")
            .map(|offset| start + offset)
            .expect("managed wait helper end");
        let helper = &runner[start..end];
        assert_eq!(helper.matches("callSyncRpc('process.waitpid'").count(), 1);
        assert!(helper.contains("KERNEL_WAIT_SLICE_MS"));
        assert!(helper.contains("transition == null && blocking"));
        assert!(!helper.contains("process.waitpid_transition"));
        assert!(!helper.contains("pumpSpawnedChildren"));
        assert!(!helper.contains("Atomics.wait"));
        assert!(helper.contains("dispatchPendingWasmSignals(true)"));
        assert!(helper.contains("error?.code === 'EINTR' && !mustInterrupt"));
        assert!(helper.contains("continue;"));
        assert!(runner.contains("case 'ECHILD':\n      return WASI_ERRNO_CHILD;"));

        let pump = runner
            .find("function pumpSpawnedChildren(waitMs) {")
            .expect("child pump");
        let pump = &runner[pump..];
        let pump_end = pump
            .find("function pumpSpawnedChildrenOrWait")
            .expect("child pump end");
        assert!(pump[..pump_end]
            .contains("if (SIDECAR_MANAGED_PROCESS) {\n    // Managed child lifecycle"));
        assert!(pump[pump_end..].contains("callSyncRpc('process.sleep', [boundedWaitMs])"));
        let sleep = runner
            .find("        sleep_ms(milliseconds) {")
            .expect("sleep import");
        let sleep = &runner[sleep..];
        let sleep_end = sleep.find("        pty_open(").expect("sleep import end");
        let sleep = &sleep[..sleep_end];
        assert!(sleep.contains("if (!SIDECAR_MANAGED_PROCESS)"));
        assert!(sleep.contains("Atomics.wait(syntheticWaitArray, 0, 0, durationMs)"));
        assert!(sleep.contains("callSyncRpc('process.sleep', [durationMs])"));
    }

    #[test]
    fn wasm_memory_limit_pages_floor_to_whole_wasm_pages() {
        assert_eq!(
            wasm_memory_limit_pages(WASM_PAGE_BYTES + 123).expect("page limit"),
            1
        );
        assert_eq!(
            wasm_memory_limit_pages(2 * WASM_PAGE_BYTES).expect("page limit"),
            2
        );
    }

    #[test]
    fn wasm_memory_limit_no_longer_requires_declared_module_maximum() {
        let temp = tempdir().expect("create temp dir");
        let request = request_with_env(
            temp.path(),
            BTreeMap::from([(
                String::from(WASM_MAX_MEMORY_BYTES_ENV),
                (2 * WASM_PAGE_BYTES).to_string(),
            )]),
        );

        assert!(
            super::validate_module_limits(
                &super::ResolvedWasmModule {
                    specifier: String::from("./guest.wasm"),
                    resolved_path: {
                        let path = temp.path().join("guest.wasm");
                        fs::write(
                            &path,
                            wat::parse_str(
                                r#"
(module
  (memory (export "memory") 1)
  (func (export "_start"))
)
"#,
                            )
                            .expect("compile wasm fixture"),
                        )
                        .expect("write wasm fixture");
                        path
                    },
                },
                &request,
            )
            .is_ok(),
            "runtime memory cap should allow modules without a declared maximum"
        );
    }
}
