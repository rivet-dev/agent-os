use crate::backend::{
    DirectHostReplyHandle, DirectHostReplyTarget, ExecutionBackend, ExecutionBackendKind,
    ExecutionExit, ExecutionWakeHandle, ExecutionWakeIdentity, HostCallIdentity, HostCallReply,
    HostServiceError, PayloadLimit, ShutdownOutcome, ShutdownReason, SignalCheckpointOutcome,
};
use crate::common::{encode_json_string, frozen_time_ms};
use crate::host::{
    BoundedBytes, BoundedProcessLaunchRequest, BoundedString, BoundedUsize, BoundedVec,
    DnsAddressFamily, FilesystemOperation, HostOperation, HttpHeader, ManagedTcpEndpoint,
    ManagedUdpFamily, NetworkOperation, PathAttributeUpdate, ProcessLaunchOptions,
    ProcessLaunchRequest, ProcessOperation,
};
use crate::javascript::{
    CreateJavascriptContextRequest, GuestRuntimeConfig, HostRpcRequest, JavascriptExecution,
    JavascriptExecutionEngine, JavascriptExecutionError, JavascriptExecutionEvent,
    JavascriptExecutionLimits, JavascriptSyncRpcResponder, StartJavascriptExecutionRequest,
};
use crate::node_import_cache::{NodeImportCache, NODE_IMPORT_CACHE_ASSET_ROOT_ENV};
use crate::runtime_support::{
    env_flag_enabled, file_fingerprint, resolve_execution_path, warmup_marker_path,
    NODE_DISABLE_COMPILE_CACHE_ENV, NODE_FROZEN_TIME_ENV,
};
use crate::v8_runtime;
use agentos_bridge::queue_tracker::{register_queue, QueueGauge, TrackedLimit};
use agentos_runtime_tokio::accounting::ResourceClass;
use agentos_runtime_tokio::RuntimeContext;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::Notify;
const NODE_ALLOW_PROCESS_BINDINGS_ENV: &str = "AGENTOS_ALLOW_PROCESS_BINDINGS";
const NODE_GUEST_ENTRYPOINT_MODULE_MODE_ENV: &str = "AGENTOS_GUEST_ENTRYPOINT_MODULE_MODE";
const NODE_GUEST_PATH_MAPPINGS_ENV: &str = "AGENTOS_GUEST_PATH_MAPPINGS";
const NODE_SYNC_RPC_DATA_BYTES_ENV: &str = "AGENTOS_NODE_SYNC_RPC_DATA_BYTES";
const PYODIDE_INDEX_URL_ENV: &str = "AGENTOS_PYODIDE_INDEX_URL";
const PYODIDE_PACKAGE_BASE_URL_ENV: &str = "AGENTOS_PYODIDE_PACKAGE_BASE_URL";
const PYODIDE_PACKAGE_CACHE_DIR_ENV: &str = "AGENTOS_PYODIDE_PACKAGE_CACHE_DIR";
const PYODIDE_GUEST_ROOT: &str = "/__agentos_pyodide";
const PYODIDE_CACHE_GUEST_ROOT: &str = "/__agentos_pyodide_cache";
const PYTHON_CODE_ENV: &str = "AGENTOS_PYTHON_CODE";
const PYTHON_FILE_ENV: &str = "AGENTOS_PYTHON_FILE";
const PYTHON_PREWARM_ONLY_ENV: &str = "AGENTOS_PYTHON_PREWARM_ONLY";
const PYTHON_WARMUP_DEBUG_ENV: &str = "AGENTOS_PYTHON_WARMUP_DEBUG";
const PYTHON_WARMUP_METRICS_PREFIX: &str = "__AGENTOS_PYTHON_WARMUP_METRICS__:";
const PYTHON_WARMUP_MARKER_VERSION: &str = "2";
const DEFAULT_PYTHON_OUTPUT_BUFFER_MAX_BYTES: usize = 1024 * 1024;
const DEFAULT_PYTHON_EXECUTION_TIMEOUT_MS: u64 = 5 * 60 * 1000;
const DEFAULT_PYTHON_MAX_OLD_SPACE_MB: usize = 0;
const DEFAULT_PYTHON_VFS_RPC_TIMEOUT_MS: u64 = 30_000;
const DEFAULT_PYTHON_PENDING_VFS_RPCS: usize = 512;
const PYTHON_SYNC_RPC_DATA_BYTES: usize = 20 * 1024 * 1024;
const PYTHON_SYNC_RPC_WAIT_TIMEOUT_MS: u64 = 120_000;
const PYTHON_PREWARM_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum PythonVfsRpcMethod {
    Read,
    Write,
    Stat,
    Lstat,
    ReadDir,
    Mkdir,
    Unlink,
    Rmdir,
    Rename,
    Symlink,
    ReadLink,
    Setattr,
    HttpRequest,
    DnsLookup,
    SubprocessRun,
    SocketConnect,
    SocketSend,
    SocketRecv,
    SocketClose,
    UdpCreate,
    UdpSendto,
    UdpRecvfrom,
}

impl PythonVfsRpcMethod {
    fn from_wire(value: &str) -> Option<Self> {
        match value {
            "fsRead" => Some(Self::Read),
            "fsWrite" => Some(Self::Write),
            "fsStat" => Some(Self::Stat),
            "fsLstat" => Some(Self::Lstat),
            "fsReaddir" => Some(Self::ReadDir),
            "fsMkdir" => Some(Self::Mkdir),
            "fsUnlink" => Some(Self::Unlink),
            "fsRmdir" => Some(Self::Rmdir),
            "fsRename" => Some(Self::Rename),
            "fsSymlink" => Some(Self::Symlink),
            "fsReadlink" => Some(Self::ReadLink),
            "fsSetattr" => Some(Self::Setattr),
            "httpRequest" => Some(Self::HttpRequest),
            "dnsLookup" => Some(Self::DnsLookup),
            "subprocessRun" => Some(Self::SubprocessRun),
            "socketConnect" => Some(Self::SocketConnect),
            "socketSend" => Some(Self::SocketSend),
            "socketRecv" => Some(Self::SocketRecv),
            "socketClose" => Some(Self::SocketClose),
            "udpCreate" => Some(Self::UdpCreate),
            "udpSendto" => Some(Self::UdpSendto),
            "udpRecvfrom" => Some(Self::UdpRecvfrom),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PythonVfsRpcRequest {
    pub id: u64,
    pub method: PythonVfsRpcMethod,
    pub path: String,
    /// Second path for `Rename` (the destination); `None` for other methods.
    pub destination: Option<String>,
    /// Symlink target (the path the link points at), for `Symlink`.
    pub target: Option<String>,
    /// `Setattr` metadata fields (each applied only when present).
    pub mode: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub atime_ms: Option<u64>,
    pub mtime_ms: Option<u64>,
    pub content_base64: Option<String>,
    pub recursive: bool,
    pub url: Option<String>,
    pub http_method: Option<String>,
    pub headers: BTreeMap<String, String>,
    pub body_base64: Option<String>,
    pub hostname: Option<String>,
    pub family: Option<u8>,
    /// Port for socket connect/sendto.
    pub port: Option<u16>,
    /// Socket handle for send/recv/close/sendto/recvfrom.
    pub socket_id: Option<u64>,
    pub command: Option<String>,
    pub args: Vec<String>,
    /// Explicit child argv[0], kept separate from the executable path.
    pub argv0: Option<String>,
    pub cwd: Option<String>,
    pub env: BTreeMap<String, String>,
    pub shell: bool,
    pub max_buffer: Option<usize>,
    /// Maximum time one socket receive operation may wait for shared
    /// readiness. `Some(0)` preserves nonblocking socket behavior.
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PythonVfsRpcStat {
    pub mode: u32,
    pub size: u64,
    pub is_directory: bool,
    pub is_symbolic_link: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PythonVfsRpcResponsePayload {
    Empty,
    Read {
        content_base64: String,
    },
    Stat {
        stat: PythonVfsRpcStat,
    },
    ReadDir {
        entries: Vec<String>,
    },
    Http {
        status: u16,
        reason: String,
        url: String,
        headers: BTreeMap<String, Vec<String>>,
        body_base64: String,
    },
    DnsLookup {
        addresses: Vec<String>,
    },
    SubprocessRun {
        exit_code: i32,
        stdout: String,
        stderr: String,
        max_buffer_exceeded: bool,
    },
    SocketCreated {
        socket_id: u64,
    },
    SocketSent {
        bytes_sent: usize,
    },
    SocketReceived {
        data_base64: String,
        closed: bool,
        timed_out: bool,
    },
    UdpReceived {
        data_base64: String,
        host: String,
        port: u16,
        timed_out: bool,
    },
    SymlinkTarget {
        target: String,
    },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PythonVfsBridgeRequestWire {
    method: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    destination: Option<String>,
    #[serde(default)]
    target: Option<String>,
    // JS numbers cross the bridge as f64; accept that and narrow below.
    #[serde(default)]
    mode: Option<f64>,
    #[serde(default)]
    uid: Option<f64>,
    #[serde(default)]
    gid: Option<f64>,
    #[serde(default, rename = "atimeMs")]
    atime_ms: Option<f64>,
    #[serde(default, rename = "mtimeMs")]
    mtime_ms: Option<f64>,
    #[serde(default)]
    content_base64: Option<String>,
    #[serde(default)]
    recursive: bool,
    #[serde(default)]
    url: Option<String>,
    #[serde(default, rename = "httpMethod")]
    http_method: Option<String>,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    #[serde(default, rename = "bodyBase64")]
    body_base64: Option<String>,
    #[serde(default)]
    hostname: Option<String>,
    #[serde(default)]
    family: Option<u8>,
    #[serde(default)]
    port: Option<u16>,
    #[serde(default, rename = "socketId")]
    socket_id: Option<u64>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    argv0: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    shell: bool,
    #[serde(default, rename = "maxBuffer")]
    max_buffer: Option<usize>,
    #[serde(default, rename = "timeoutMs")]
    timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct PythonGuestPathMappingWire {
    guest_path: String,
    host_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatePythonContextRequest {
    pub vm_id: String,
    pub pyodide_dist_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PythonContext {
    pub context_id: String,
    pub vm_id: String,
    pub pyodide_dist_path: PathBuf,
}

/// Per-execution Python runtime limits, carried as typed fields rather than
/// `AGENTOS_*` env vars. Populated by the sidecar from the per-VM `VmLimits`
/// (originating from `CreateVmConfig` on the BARE wire); `None` selects the
/// engine default. See the env-vs-wire rule in `crates/sidecar/CLAUDE.md`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PythonExecutionLimits {
    /// Captured-output buffer cap in bytes. `None` keeps the engine default.
    pub output_buffer_max_bytes: Option<usize>,
    /// Execution wall-clock cap in ms. `None` keeps the engine default;
    /// `Some(0)` disables the timeout.
    pub execution_timeout_ms: Option<u64>,
    /// Pyodide V8 old-space cap in MB (`0` keeps the V8 default). `None` keeps
    /// the engine default.
    pub max_old_space_mb: Option<usize>,
    /// VFS sync-RPC wait ceiling in ms. `None` keeps the engine default.
    pub vfs_rpc_timeout_ms: Option<u64>,
    /// VM readiness work bound forwarded unchanged to the Pyodide V8 runner.
    pub reactor_work_quantum: Option<usize>,
    /// Per-call host bridge deadline forwarded unchanged to the Pyodide V8 runner.
    pub bridge_call_timeout_ms: Option<u64>,
    /// Maximum host-direct descriptors retained for managed Pyodide assets.
    /// `None` keeps the execution engine's bounded fallback. The native sidecar
    /// always supplies the VM kernel's configured descriptor limit.
    pub max_open_fds: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartPythonExecutionRequest {
    pub vm_id: String,
    pub context_id: String,
    pub code: String,
    pub file_path: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
    pub cwd: PathBuf,
    /// Per-execution runtime limits (see [`PythonExecutionLimits`]).
    pub limits: PythonExecutionLimits,
    /// Per-execution guest-runtime config, forwarded to the Pyodide runner's JS
    /// execution (see [`GuestRuntimeConfig`]).
    pub guest_runtime: GuestRuntimeConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PythonExecutionEvent {
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
    HostRpcRequest(HostRpcRequest),
    VfsRpcRequest(Box<PythonVfsRpcRequest>),
    Exited(i32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PythonExecutionResult {
    pub execution_id: String,
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[derive(Debug)]
pub enum PythonExecutionError {
    MissingContext(String),
    InvalidLimit(String),
    VmMismatch {
        expected: String,
        found: String,
    },
    /// Guest Python is unavailable because this build was compiled without the
    /// bundled Pyodide runtime assets (the published crate excludes them).
    RuntimeUnavailable,
    PrepareRuntime(std::io::Error),
    PrepareWarmPath(std::io::Error),
    WarmupFailed {
        exit_code: i32,
        stderr: String,
    },
    Spawn(std::io::Error),
    StdinClosed,
    Stdin(std::io::Error),
    Kill(std::io::Error),
    Control(std::io::Error),
    TimedOut(Duration),
    PendingVfsRpcRequest(u64),
    PendingVfsRpcLimit {
        limit: usize,
        observed: usize,
    },
    RpcResponse(String),
    OutputBufferExceeded {
        stream: &'static str,
        limit: usize,
    },
    EventChannelClosed,
}

impl fmt::Display for PythonExecutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingContext(context_id) => {
                write!(f, "unknown guest Python context: {context_id}")
            }
            Self::InvalidLimit(message) => write!(f, "invalid Python limit: {message}"),
            Self::VmMismatch { expected, found } => {
                write!(
                    f,
                    "guest Python context belongs to vm {expected}, not {found}"
                )
            }
            Self::RuntimeUnavailable => write!(
                f,
                "guest Python execution is unavailable: this build of agentos-execution \
                 was compiled without the bundled Pyodide runtime assets"
            ),
            Self::PrepareRuntime(err) => {
                write!(f, "failed to prepare guest Python runtime assets: {err}")
            }
            Self::PrepareWarmPath(err) => {
                write!(f, "failed to prepare guest Python warm path: {err}")
            }
            Self::WarmupFailed { exit_code, stderr } => {
                if stderr.trim().is_empty() {
                    write!(f, "guest Python warmup exited with status {exit_code}")
                } else {
                    write!(
                        f,
                        "guest Python warmup exited with status {exit_code}: {}",
                        stderr.trim()
                    )
                }
            }
            Self::Spawn(err) => write!(f, "failed to start guest Python runtime: {err}"),
            Self::StdinClosed => f.write_str("guest Python stdin is already closed"),
            Self::Stdin(err) => write!(f, "failed to write guest stdin: {err}"),
            Self::Kill(err) => write!(f, "failed to kill guest Python runtime: {err}"),
            Self::Control(err) => write!(f, "failed to control guest Python runtime: {err}"),
            Self::TimedOut(timeout) => write!(
                f,
                "guest Python runtime timed out after {}ms",
                timeout.as_millis()
            ),
            Self::PendingVfsRpcRequest(id) => {
                write!(
                    f,
                    "guest Python execution requires servicing pending VFS RPC request {id}"
                )
            }
            Self::PendingVfsRpcLimit { limit, observed } => write!(
                f,
                "ERR_AGENTOS_RESOURCE_LIMIT: pending Python VFS RPC calls observed {observed}, exceeding limits.reactor.maxBridgeCalls ({limit})"
            ),
            Self::RpcResponse(message) => {
                write!(
                    f,
                    "failed to reply to guest Python VFS RPC request: {message}"
                )
            }
            Self::OutputBufferExceeded { stream, limit } => {
                write!(
                    f,
                    "guest Python {stream} exceeded the captured output limit of {limit} bytes"
                )
            }
            Self::EventChannelClosed => {
                f.write_str("guest Python event channel closed unexpectedly")
            }
        }
    }
}

impl std::error::Error for PythonExecutionError {}

/// Returns an error when this build was compiled without the bundled Pyodide
/// runtime assets (the published crate excludes them; see `build.rs`). In the
/// workspace build the in-tree assets are present and this is a no-op.
fn ensure_pyodide_available() -> Result<(), PythonExecutionError> {
    #[cfg(agentos_pyodide_unavailable)]
    {
        return Err(PythonExecutionError::RuntimeUnavailable);
    }
    #[cfg(not(agentos_pyodide_unavailable))]
    {
        Ok(())
    }
}

#[derive(Debug)]
pub struct PythonExecution {
    runtime: RuntimeContext,
    execution_id: String,
    child_pid: u32,
    inner: JavascriptExecution,
    pyodide_dist_path: PathBuf,
    managed_host_files: PythonManagedHostFiles,
    pending_vfs_rpc: Arc<Mutex<PendingVfsRpcRegistry>>,
    managed_network: Arc<Mutex<PythonManagedNetworkState>>,
    v8_session: crate::v8_host::V8SessionHandle,
    output_buffer_max_bytes: usize,
    execution_timeout: Option<Duration>,
    vfs_rpc_timeout: Duration,
}

/// Cloneable response lane for a pending Python VFS RPC.
///
/// Socket operations complete on the shared Tokio runtime after the sidecar
/// dispatcher has returned to its event loop.  Keeping only the pending-RPC
/// state and V8 session handle here lets those tasks reply directly without
/// parking the dispatcher or borrowing the process table across an await.
#[derive(Debug, Clone)]
pub struct PythonVfsRpcResponder {
    pending_vfs_rpc: Arc<Mutex<PendingVfsRpcRegistry>>,
    managed_network: Arc<Mutex<PythonManagedNetworkState>>,
    javascript_responder: JavascriptSyncRpcResponder,
}

/// One Python adapter request normalized to the common host-service boundary.
#[derive(Debug)]
pub struct PythonHostCall {
    pub operation: HostOperation,
    pub reply: DirectHostReplyHandle,
}

#[derive(Debug, Clone)]
enum PythonHostReplyKind {
    Empty,
    FileRead,
    Stat,
    ReadDirectory,
    ReadLink,
    RunCaptured,
    Http,
    Dns,
    SocketCreated(PythonManagedSocketReservation),
    SocketSent,
    SocketReceived,
    SocketClosed(u64),
    UdpReceived,
}

impl PythonHostReplyKind {
    fn rollback_socket_reservation(&self) {
        if let Self::SocketCreated(reservation) = self {
            reservation.rollback();
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PythonManagedSocketKind {
    Tcp,
    Udp,
}

#[derive(Debug, Clone)]
struct PythonManagedSocket {
    kind: PythonManagedSocketKind,
    host_socket_id: String,
}

#[derive(Debug)]
enum PythonManagedSocketSlot {
    Reserved(PythonManagedSocketKind),
    Live(PythonManagedSocket),
}

#[derive(Debug)]
struct PythonManagedSocketReservationInner {
    socket_id: u64,
    kind: PythonManagedSocketKind,
    state: Arc<Mutex<PythonManagedNetworkState>>,
}

impl Drop for PythonManagedSocketReservationInner {
    fn drop(&mut self) {
        let mut state = self.state.lock().unwrap_or_else(|poisoned| {
            eprintln!(
                "ERR_AGENTOS_PYTHON_NETWORK_STATE_POISONED: recovering reserved socket {}",
                self.socket_id
            );
            poisoned.into_inner()
        });
        if matches!(
            state.sockets.get(&self.socket_id),
            Some(PythonManagedSocketSlot::Reserved(kind)) if *kind == self.kind
        ) {
            state.sockets.remove(&self.socket_id);
        }
    }
}

#[derive(Debug, Clone)]
struct PythonManagedSocketReservation(Arc<PythonManagedSocketReservationInner>);

impl PythonManagedSocketReservation {
    fn commit(&self, host_socket_id: String) -> Result<u64, HostServiceError> {
        let mut state = self.0.state.lock().unwrap_or_else(|poisoned| {
            eprintln!(
                "ERR_AGENTOS_PYTHON_NETWORK_STATE_POISONED: recovering socket reservation {} during commit",
                self.0.socket_id
            );
            poisoned.into_inner()
        });
        match state.sockets.get(&self.0.socket_id) {
            Some(PythonManagedSocketSlot::Reserved(kind)) if *kind == self.0.kind => {}
            _ => {
                return Err(HostServiceError::new(
                    "ESTALE",
                    format!(
                        "Python socket reservation {} is no longer live",
                        self.0.socket_id
                    ),
                ))
            }
        }
        state.sockets.insert(
            self.0.socket_id,
            PythonManagedSocketSlot::Live(PythonManagedSocket {
                kind: self.0.kind,
                host_socket_id,
            }),
        );
        Ok(self.0.socket_id)
    }

    fn rollback(&self) {
        let mut state = self.0.state.lock().unwrap_or_else(|poisoned| {
            eprintln!(
                "ERR_AGENTOS_PYTHON_NETWORK_STATE_POISONED: recovering socket reservation {}",
                self.0.socket_id
            );
            poisoned.into_inner()
        });
        if matches!(
            state.sockets.get(&self.0.socket_id),
            Some(PythonManagedSocketSlot::Reserved(kind)) if *kind == self.0.kind
        ) {
            state.sockets.remove(&self.0.socket_id);
        }
    }
}

#[derive(Debug)]
struct PythonManagedNetworkState {
    next_socket_id: u64,
    maximum: usize,
    sockets: BTreeMap<u64, PythonManagedSocketSlot>,
}

impl PythonManagedNetworkState {
    fn new(maximum: usize) -> Self {
        Self {
            next_socket_id: 1,
            maximum,
            sockets: BTreeMap::new(),
        }
    }

    fn reserve(
        state: &Arc<Mutex<Self>>,
        kind: PythonManagedSocketKind,
    ) -> Result<PythonManagedSocketReservation, HostServiceError> {
        let mut locked = state.lock().map_err(|_| {
            HostServiceError::new(
                "ERR_AGENTOS_PYTHON_NETWORK_STATE_POISONED",
                "Python managed-network state lock poisoned",
            )
        })?;
        let observed = locked.sockets.len().saturating_add(1);
        if observed > locked.maximum {
            return Err(HostServiceError::limit(
                "ERR_AGENTOS_RESOURCE_LIMIT",
                "limits.resources.maxOpenFds",
                locked.maximum as u64,
                observed as u64,
            ));
        }
        let socket_id = locked.next_socket_id;
        let next_socket_id = locked.next_socket_id.checked_add(1).ok_or_else(|| {
            HostServiceError::new("EOVERFLOW", "Python managed socket id space exhausted")
        })?;
        locked.next_socket_id = next_socket_id;
        locked
            .sockets
            .insert(socket_id, PythonManagedSocketSlot::Reserved(kind));
        drop(locked);
        Ok(PythonManagedSocketReservation(Arc::new(
            PythonManagedSocketReservationInner {
                socket_id,
                kind,
                state: Arc::clone(state),
            },
        )))
    }

    fn socket(
        &self,
        socket_id: u64,
        expected: Option<PythonManagedSocketKind>,
    ) -> Result<PythonManagedSocket, HostServiceError> {
        let socket = self.sockets.get(&socket_id).ok_or_else(|| {
            HostServiceError::new("EBADF", format!("unknown Python socket {socket_id}"))
                .with_details(json!({ "socketId": socket_id }))
        })?;
        let PythonManagedSocketSlot::Live(socket) = socket else {
            return Err(HostServiceError::new(
                "EBUSY",
                format!("Python socket {socket_id} is still being created"),
            )
            .with_details(json!({ "socketId": socket_id })));
        };
        let socket = socket.clone();
        if let Some(expected) = expected {
            if socket.kind != expected {
                return Err(HostServiceError::new(
                    "EINVAL",
                    format!("Python socket {socket_id} has the wrong socket kind"),
                )
                .with_details(json!({
                    "socketId": socket_id,
                    "expected": format!("{expected:?}"),
                    "actual": format!("{:?}", socket.kind),
                })));
            }
        }
        Ok(socket)
    }
}

#[derive(Debug)]
struct PythonHostReplyTarget {
    responder: PythonVfsRpcResponder,
    kind: PythonHostReplyKind,
}

#[derive(Debug)]
struct PendingVfsRpc {
    state: PendingVfsRpcState,
    timeout_abort: Option<tokio::task::AbortHandle>,
}

#[derive(Debug)]
struct PendingVfsRpcRegistry {
    entries: BTreeMap<u64, PendingVfsRpc>,
    maximum: usize,
    gauge: Arc<QueueGauge>,
}

impl PendingVfsRpcRegistry {
    fn new(maximum: usize) -> Self {
        let maximum = maximum.max(1);
        Self {
            entries: BTreeMap::new(),
            maximum,
            gauge: register_queue(TrackedLimit::PendingPythonVfsRpcCalls, maximum),
        }
    }

    fn observe_depth(&self) {
        self.gauge.observe_depth(self.entries.len());
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingVfsRpcState {
    Pending(u64),
    TimedOut(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingVfsRpcResolution {
    Pending,
    TimedOut,
    Missing,
}

impl PythonExecution {
    pub fn vfs_rpc_responder(&self) -> PythonVfsRpcResponder {
        PythonVfsRpcResponder {
            pending_vfs_rpc: Arc::clone(&self.pending_vfs_rpc),
            managed_network: Arc::clone(&self.managed_network),
            javascript_responder: self.inner.sync_rpc_responder(),
        }
    }

    pub fn javascript_sync_rpc_responder(&self) -> crate::JavascriptSyncRpcResponder {
        self.inner.sync_rpc_responder()
    }

    pub fn execution_id(&self) -> &str {
        &self.execution_id
    }

    pub fn native_process_id(&self) -> Option<u32> {
        (self.child_pid != 0).then_some(self.child_pid)
    }

    /// Run another sidecar-managed operation in the retained Pyodide
    /// interpreter owned by this execution.
    pub fn execute_retained(&mut self, source: String) -> Result<(), PythonExecutionError> {
        let source = serde_json::to_string(&source).map_err(|error| {
            PythonExecutionError::Control(std::io::Error::other(format!(
                "failed to encode retained Python source: {error}"
            )))
        })?;
        let runner = format!(
            "process.exitCode = 0; (async () => {{ const pyodide = globalThis.__agentOSRetainedPyodide; if (!pyodide) throw new Error('retained Python interpreter is unavailable'); await pyodide.runPythonAsync({source}); }})()"
        );
        self.inner
            .execute_retained(runner, String::from("/[agentos-python-retained]"), false)
            .map_err(map_javascript_error)
    }

    pub fn start_prepared(&mut self) -> Result<(), PythonExecutionError> {
        self.inner.start_prepared().map_err(map_javascript_error)
    }

    #[doc(hidden)]
    pub fn is_prepared_for_start(&self) -> bool {
        self.inner.is_prepared_for_start()
    }

    pub fn write_stdin(&mut self, chunk: &[u8]) -> Result<(), PythonExecutionError> {
        self.inner
            .write_kernel_stdin_only(chunk)
            .map_err(map_javascript_error)
    }

    pub fn close_stdin(&mut self) -> Result<(), PythonExecutionError> {
        self.inner.close_kernel_stdin_only();
        Ok(())
    }

    pub fn cancel(&mut self) -> Result<(), PythonExecutionError> {
        self.kill()
    }

    pub fn kill(&mut self) -> Result<(), PythonExecutionError> {
        self.close_stdin()?;
        self.inner.terminate().map_err(map_javascript_error)
    }

    pub fn pause(&self) -> Result<(), PythonExecutionError> {
        self.inner.pause().map_err(map_javascript_error)
    }

    pub fn resume(&self) -> Result<(), PythonExecutionError> {
        self.inner.resume().map_err(map_javascript_error)
    }

    pub fn respond_vfs_rpc_success(
        &mut self,
        id: u64,
        payload: PythonVfsRpcResponsePayload,
    ) -> Result<(), PythonExecutionError> {
        self.vfs_rpc_responder().respond_success(id, payload)
    }

    pub fn respond_vfs_rpc_error(
        &mut self,
        id: u64,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<(), PythonExecutionError> {
        self.vfs_rpc_responder().respond_error(id, code, message)
    }

    pub fn respond_javascript_sync_rpc_success(
        &mut self,
        id: u64,
        result: Value,
    ) -> Result<(), PythonExecutionError> {
        self.inner
            .respond_sync_rpc_success(id, result)
            .map_err(map_javascript_error)
    }

    pub fn claim_javascript_sync_rpc_response(
        &mut self,
        id: u64,
    ) -> Result<bool, PythonExecutionError> {
        self.inner
            .claim_sync_rpc_response(id)
            .map_err(map_javascript_error)
    }

    pub fn respond_claimed_javascript_sync_rpc_success(
        &mut self,
        id: u64,
        result: Value,
    ) -> Result<(), PythonExecutionError> {
        self.inner
            .respond_claimed_sync_rpc_success(id, result)
            .map_err(map_javascript_error)
    }

    pub fn respond_javascript_sync_rpc_error(
        &mut self,
        id: u64,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<(), PythonExecutionError> {
        self.inner
            .respond_sync_rpc_error(id, code, message)
            .map_err(map_javascript_error)
    }

    pub fn respond_claimed_javascript_sync_rpc_error(
        &mut self,
        id: u64,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<(), PythonExecutionError> {
        self.inner
            .respond_claimed_sync_rpc_error(id, code, message)
            .map_err(map_javascript_error)
    }

    pub async fn poll_event(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<PythonExecutionEvent>, PythonExecutionError> {
        self.poll_event_until(Some(timeout)).await
    }

    pub fn try_poll_event(&mut self) -> Result<Option<PythonExecutionEvent>, PythonExecutionError> {
        loop {
            let Some(event) = self.inner.try_poll_event().map_err(map_javascript_error)? else {
                return Ok(None);
            };
            if let Some(event) = self.translate_javascript_event(event)? {
                return Ok(Some(event));
            }
        }
    }

    async fn poll_event_until(
        &mut self,
        timeout: Option<Duration>,
    ) -> Result<Option<PythonExecutionEvent>, PythonExecutionError> {
        let started = Instant::now();
        loop {
            let remaining = timeout.map(|timeout| {
                if timeout.is_zero() {
                    Duration::ZERO
                } else {
                    timeout.saturating_sub(started.elapsed())
                }
            });
            match self
                .inner
                .poll_event_until(remaining)
                .await
                .map_err(map_javascript_error)?
            {
                Some(event) => {
                    if let Some(event) = self.translate_javascript_event(event)? {
                        return Ok(Some(event));
                    }
                }
                None => return Ok(None),
            }
        }
    }

    /// Service a module-resolution JS sync RPC host-directly via the underlying
    /// JS execution's translator. For consumers driving `poll_event_blocking`
    /// manually without a kernel/service loop.
    pub fn try_service_standalone_module_sync_rpc(
        &mut self,
        request: &HostRpcRequest,
    ) -> Result<bool, PythonExecutionError> {
        self.inner
            .try_service_standalone_module_sync_rpc(request)
            .map_err(map_javascript_error)
    }

    /// Service the local stdin bridge for consumers that drive a standalone
    /// Python execution without the sidecar's sync-RPC dispatcher.
    #[doc(hidden)]
    pub fn try_service_standalone_stdin_sync_rpc(
        &mut self,
        request: &HostRpcRequest,
    ) -> Result<bool, PythonExecutionError> {
        self.inner
            .handle_kernel_stdin_sync_rpc(request)
            .map_err(map_javascript_error)
    }

    pub fn poll_event_blocking(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<PythonExecutionEvent>, PythonExecutionError> {
        let deadline = checked_python_poll_deadline(timeout)?;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self
                .inner
                .poll_event_blocking(remaining)
                .map_err(map_javascript_error)?
            {
                Some(event) => {
                    if let Some(event) = self.translate_javascript_event(event)? {
                        return Ok(Some(event));
                    }
                }
                None => {
                    if Instant::now() >= deadline {
                        return Ok(None);
                    }
                }
            }
        }
    }

    fn next_event_blocking(&mut self) -> Result<PythonExecutionEvent, PythonExecutionError> {
        loop {
            let event = self
                .inner
                .next_event_blocking()
                .map_err(map_javascript_error)?;
            if let Some(event) = self.translate_javascript_event(event)? {
                return Ok(event);
            }
        }
    }

    pub fn wait(
        mut self,
        timeout: Option<Duration>,
    ) -> Result<PythonExecutionResult, PythonExecutionError> {
        self.close_stdin()?;

        let mut stdout = PythonOutputBuffer::new(self.output_buffer_max_bytes);
        let mut stderr = PythonOutputBuffer::new(self.output_buffer_max_bytes);
        let started = Instant::now();
        let timeout = match (timeout, self.execution_timeout) {
            (Some(requested), Some(configured)) => Some(requested.min(configured)),
            (Some(requested), None) => Some(requested),
            (None, Some(configured)) => Some(configured),
            (None, None) => None,
        };
        loop {
            let poll_timeout = python_wait_remaining(timeout, started);
            let event = match poll_timeout {
                Some(timeout) => self.poll_event_blocking(timeout)?,
                None => Some(self.next_event_blocking()?),
            };

            match event {
                Some(PythonExecutionEvent::Stdout(chunk)) => stdout.extend(&chunk),
                Some(PythonExecutionEvent::Stderr(chunk)) => stderr.extend(&chunk),
                Some(PythonExecutionEvent::HostRpcRequest(request)) => {
                    // Module-resolution sync RPCs are serviced host-directly via
                    // the JS execution's own translator (the standalone Python
                    // wait loop runs without a kernel/service loop).
                    if self
                        .inner
                        .try_service_standalone_module_sync_rpc(&request)
                        .map_err(map_javascript_error)?
                    {
                        continue;
                    }
                    if let Some((code, message)) = python_javascript_sync_rpc_error(&request) {
                        self.inner
                            .respond_sync_rpc_error(request.id, code, message)
                            .map_err(map_javascript_error)?;
                        continue;
                    }
                    return Err(PythonExecutionError::RpcResponse(format!(
                        "guest Python execution requires servicing pending JavaScript sync RPC request {} {} {:?}",
                        request.id, request.method, request.args
                    )));
                }
                Some(PythonExecutionEvent::VfsRpcRequest(request)) => {
                    if let Some((code, message)) = python_vfs_rpc_standalone_error(request.method) {
                        self.respond_vfs_rpc_error(request.id, code, message)?;
                        continue;
                    }
                    return Err(PythonExecutionError::PendingVfsRpcRequest(request.id));
                }
                Some(PythonExecutionEvent::Exited(exit_code)) => {
                    return Ok(PythonExecutionResult {
                        execution_id: self.execution_id.clone(),
                        exit_code,
                        stdout: stdout.into_inner(),
                        stderr: stderr.into_inner(),
                    });
                }
                None => {}
            }

            if let Some(limit) = timeout {
                if started.elapsed() >= limit {
                    self.kill()?;
                    return Err(PythonExecutionError::TimedOut(limit));
                }
            }
        }
    }

    fn translate_javascript_event(
        &mut self,
        event: JavascriptExecutionEvent,
    ) -> Result<Option<PythonExecutionEvent>, PythonExecutionError> {
        match event {
            JavascriptExecutionEvent::Stdout(chunk) => {
                Ok(Some(PythonExecutionEvent::Stdout(chunk)))
            }
            JavascriptExecutionEvent::Stderr(chunk) => {
                Ok(Some(PythonExecutionEvent::Stderr(chunk)))
            }
            JavascriptExecutionEvent::Exited(code) => Ok(Some(PythonExecutionEvent::Exited(code))),
            JavascriptExecutionEvent::SignalState { .. } => Ok(None),
            JavascriptExecutionEvent::SyncRpcRequest(request) => {
                if request.method == "_pythonRpc" {
                    let request = parse_python_bridge_sync_rpc_request(&request)?;
                    if let Err(error) = set_pending_vfs_rpc_state(&self.pending_vfs_rpc, request.id)
                    {
                        self.inner
                            .sync_rpc_responder()
                            .respond_host_error(request.id, python_vfs_rpc_admission_error(error))
                            .map_err(map_javascript_error)?;
                        return Ok(None);
                    }
                    spawn_python_vfs_rpc_timeout(
                        &self.runtime,
                        request.id,
                        self.vfs_rpc_timeout,
                        self.pending_vfs_rpc.clone(),
                        self.inner.sync_rpc_responder(),
                    )?;
                    Ok(Some(PythonExecutionEvent::VfsRpcRequest(Box::new(request))))
                } else {
                    if self.try_service_standalone_module_sync_rpc(&request)? {
                        return Ok(None);
                    }
                    if let Some(action) = python_javascript_sync_rpc_action(
                        &self.pyodide_dist_path,
                        &mut self.managed_host_files,
                        &request,
                    )? {
                        respond_python_javascript_sync_rpc_action(
                            &mut self.inner,
                            request.id,
                            action,
                        )?;
                        Ok(None)
                    } else {
                        Ok(Some(PythonExecutionEvent::HostRpcRequest(request)))
                    }
                }
            }
        }
    }
}

fn checked_python_poll_deadline(timeout: Duration) -> Result<Instant, PythonExecutionError> {
    Instant::now().checked_add(timeout).ok_or_else(|| {
        PythonExecutionError::InvalidLimit(format!(
            "blocking poll timeout of {}ms exceeds the host clock range",
            timeout.as_millis()
        ))
    })
}

fn validate_python_captured_reply_limit(
    max_buffer: usize,
    max_reply_bytes: usize,
) -> Result<(), HostServiceError> {
    let worst_case_reply_bytes = max_buffer
        .saturating_mul(2)
        .saturating_mul(6)
        .saturating_add(512);
    if worst_case_reply_bytes > max_reply_bytes {
        return Err(HostServiceError::limit(
            "ERR_AGENTOS_RESOURCE_LIMIT",
            "limits.reactor.maxBridgeResponseBytes",
            max_reply_bytes as u64,
            worst_case_reply_bytes as u64,
        ));
    }
    Ok(())
}

impl ExecutionBackend for PythonExecution {
    fn kind(&self) -> ExecutionBackendKind {
        ExecutionBackendKind::Python
    }

    fn native_process_id(&self) -> Option<u32> {
        PythonExecution::native_process_id(self)
    }

    fn wake_handle(&self, identity: ExecutionWakeIdentity) -> Option<ExecutionWakeHandle> {
        Some(ExecutionWakeHandle::new(
            identity,
            Arc::new(self.v8_session.clone()),
        ))
    }

    fn is_prepared_for_start(&self) -> bool {
        PythonExecution::is_prepared_for_start(self)
    }

    fn start_prepared(&mut self) -> Result<(), HostServiceError> {
        PythonExecution::start_prepared(self).map_err(|error| {
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
            // Shared Python runs inside V8 and therefore has no OS wait status
            // from which to recover the terminating signal.
            self.kill().map_err(|error| {
                HostServiceError::new("ERR_AGENTOS_EXECUTION_SHUTDOWN", error.to_string())
            })?;
            return Ok(ShutdownOutcome::Exited(ExecutionExit::Signaled {
                signal,
                core_dumped: false,
            }));
        }
        self.kill().map_err(|error| {
            HostServiceError::new("ERR_AGENTOS_EXECUTION_SHUTDOWN", error.to_string())
        })?;
        Ok(ShutdownOutcome::AwaitExit)
    }

    fn set_paused(&self, paused: bool) -> Result<(), HostServiceError> {
        let result = if paused {
            PythonExecution::pause(self)
        } else {
            PythonExecution::resume(self)
        };
        result.map_err(|error| {
            HostServiceError::new("ERR_AGENTOS_EXECUTION_CONTROL", error.to_string())
        })
    }

    fn write_stdin(&mut self, _bytes: &[u8]) -> Result<(), HostServiceError> {
        // Sidecar-managed Python reads fd 0 from the kernel pipe.
        Ok(())
    }

    fn close_stdin(&mut self) -> Result<(), HostServiceError> {
        PythonExecution::close_stdin(self).map_err(|error| {
            HostServiceError::new("ERR_AGENTOS_EXECUTION_STDIN", error.to_string())
        })
    }

    fn deliver_signal_checkpoint(
        &self,
        identity: ExecutionWakeIdentity,
        signal: i32,
        delivery_token: u64,
        _flags: u32,
        _thread_id: u32,
    ) -> Result<SignalCheckpointOutcome, HostServiceError> {
        let Some(wake) = self.wake_handle(identity) else {
            return Ok(if let Some(process_id) = self.native_process_id() {
                SignalCheckpointOutcome::ForwardToProcess { process_id }
            } else {
                SignalCheckpointOutcome::Unsupported
            });
        };
        wake.publish_signal(signal, delivery_token)
            .map_err(|error| HostServiceError::new(error.code(), error.to_string()))?;
        Ok(SignalCheckpointOutcome::Published)
    }
}

fn python_wait_remaining(timeout: Option<Duration>, started: Instant) -> Option<Duration> {
    timeout.map(|limit| limit.saturating_sub(started.elapsed()))
}

impl PythonVfsRpcResponder {
    pub fn try_host_call(
        &self,
        request: PythonVfsRpcRequest,
        identity: HostCallIdentity,
        max_request_bytes: usize,
        max_reply_bytes: usize,
    ) -> Result<Option<PythonHostCall>, HostServiceError> {
        const CWD_FD: u32 = u32::MAX;
        const MAX_PATH_BYTES: usize = 4096;
        const MAX_HOST_BYTES: usize = 253;
        const MAX_HTTP_URL_BYTES: usize = 8 * 1024;
        const MAX_HTTP_METHOD_BYTES: usize = 32;
        const MAX_HTTP_HEADERS: usize = 256;
        const MAX_HTTP_HEADER_BYTES: usize = 64 * 1024;
        const MAX_DNS_RESULTS: usize = 64;
        const PYTHON_SOCKET_DEFAULT_RECV: usize = 65_536;
        const PYTHON_SOCKET_MAX_RECV: usize = 4 * 1024 * 1024;

        let request_limit =
            PayloadLimit::new("limits.reactor.maxBridgeRequestBytes", max_request_bytes)?;
        let reply_limit =
            PayloadLimit::new("limits.reactor.maxBridgeResponseBytes", max_reply_bytes)?;
        let path_limit = PayloadLimit::new("runtime.filesystem.maxPathBytes", MAX_PATH_BYTES)?;
        let host_limit = PayloadLimit::new("runtime.network.maxHostnameBytes", MAX_HOST_BYTES)?;
        let socket_id_limit =
            PayloadLimit::new("limits.reactor.maxBridgeRequestBytes", max_request_bytes)?;
        let bounded_host = |host: String, label: &str| {
            if host.is_empty() {
                return Err(HostServiceError::new(
                    "EINVAL",
                    format!("{label} must not be empty"),
                ));
            }
            BoundedString::try_new(host, &host_limit)
        };
        let decode_body = |encoded: Option<&str>, label: &str| {
            let Some(encoded) = encoded else {
                return Ok(Vec::new());
            };
            base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|error| {
                    HostServiceError::new("EINVAL", format!("{label} is invalid: {error}"))
                })
        };
        let managed_socket = |socket_id: Option<u64>, expected| {
            let socket_id = socket_id.ok_or_else(|| {
                HostServiceError::new("EINVAL", "Python socket operation requires socketId")
            })?;
            let state = self.managed_network.lock().map_err(|_| {
                HostServiceError::new(
                    "ERR_AGENTOS_PYTHON_NETWORK_STATE_POISONED",
                    "Python managed-network state lock poisoned",
                )
            })?;
            Ok((socket_id, state.socket(socket_id, expected)?))
        };
        let bounded_path = |path: String, label: &str| {
            if !path.starts_with('/') {
                return Err(HostServiceError::new(
                    "EINVAL",
                    format!("{label} must be an absolute guest path"),
                )
                .with_details(json!({ "path": path })));
            }
            BoundedString::try_new(path, &path_limit)
        };

        let path = || bounded_path(request.path.clone(), "Python filesystem path");
        let (operation, kind) = match request.method {
            PythonVfsRpcMethod::Read => {
                // Account for base64 expansion and the small Python response
                // envelope before any file body is allocated by the kernel.
                let body_limit = max_reply_bytes.saturating_sub(128) / 4 * 3;
                (
                    HostOperation::Filesystem(FilesystemOperation::ReadFileAt {
                        dir_fd: CWD_FD,
                        path: path()?,
                        max_bytes: BoundedUsize::try_new(body_limit, &reply_limit)?,
                    }),
                    PythonHostReplyKind::FileRead,
                )
            }
            PythonVfsRpcMethod::Write => {
                let encoded = request.content_base64.as_deref().ok_or_else(|| {
                    HostServiceError::new("EINVAL", "Python fsWrite requires contentBase64")
                })?;
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .map_err(|error| {
                        HostServiceError::new(
                            "EINVAL",
                            format!("Python fsWrite contentBase64 is invalid: {error}"),
                        )
                    })?;
                (
                    HostOperation::Filesystem(FilesystemOperation::WriteFileAt {
                        dir_fd: CWD_FD,
                        path: path()?,
                        bytes: BoundedBytes::try_new(bytes, &request_limit)?,
                        mode: request.mode,
                    }),
                    PythonHostReplyKind::Empty,
                )
            }
            PythonVfsRpcMethod::Stat => (
                HostOperation::Filesystem(FilesystemOperation::NodeStatAt {
                    dir_fd: CWD_FD,
                    path: path()?,
                }),
                PythonHostReplyKind::Stat,
            ),
            PythonVfsRpcMethod::Lstat => (
                HostOperation::Filesystem(FilesystemOperation::NodeLstatAt {
                    dir_fd: CWD_FD,
                    path: path()?,
                }),
                PythonHostReplyKind::Stat,
            ),
            PythonVfsRpcMethod::ReadDir => (
                HostOperation::Filesystem(FilesystemOperation::ReadDirectoryAt {
                    dir_fd: CWD_FD,
                    path: path()?,
                    max_entries: BoundedUsize::try_new(
                        4096,
                        &PayloadLimit::new("runtime.filesystem.maxReaddirEntries", 4096)?,
                    )?,
                    max_reply_bytes: BoundedUsize::try_new(max_reply_bytes, &reply_limit)?,
                }),
                PythonHostReplyKind::ReadDirectory,
            ),
            PythonVfsRpcMethod::Mkdir => (
                HostOperation::Filesystem(if request.recursive {
                    FilesystemOperation::CreateDirectoriesAt {
                        dir_fd: CWD_FD,
                        path: path()?,
                        mode: request.mode,
                    }
                } else {
                    FilesystemOperation::CreateDirectoryAt {
                        dir_fd: CWD_FD,
                        path: path()?,
                        mode: request.mode.unwrap_or(0o777),
                    }
                }),
                PythonHostReplyKind::Empty,
            ),
            PythonVfsRpcMethod::Unlink | PythonVfsRpcMethod::Rmdir => (
                HostOperation::Filesystem(FilesystemOperation::UnlinkAt {
                    dir_fd: CWD_FD,
                    path: path()?,
                    remove_directory: request.method == PythonVfsRpcMethod::Rmdir,
                }),
                PythonHostReplyKind::Empty,
            ),
            PythonVfsRpcMethod::Rename => {
                let destination = request.destination.clone().ok_or_else(|| {
                    HostServiceError::new("EINVAL", "Python fsRename requires destination")
                })?;
                (
                    HostOperation::Filesystem(FilesystemOperation::RenameAt {
                        old_dir_fd: CWD_FD,
                        old_path: path()?,
                        new_dir_fd: CWD_FD,
                        new_path: bounded_path(destination, "Python rename destination")?,
                        flags: 0,
                    }),
                    PythonHostReplyKind::Empty,
                )
            }
            PythonVfsRpcMethod::Symlink => {
                let target = request.target.clone().ok_or_else(|| {
                    HostServiceError::new("EINVAL", "Python fsSymlink requires target")
                })?;
                (
                    HostOperation::Filesystem(FilesystemOperation::SymlinkAt {
                        target: BoundedString::try_new(target, &path_limit)?,
                        dir_fd: CWD_FD,
                        path: path()?,
                    }),
                    PythonHostReplyKind::Empty,
                )
            }
            PythonVfsRpcMethod::ReadLink => (
                HostOperation::Filesystem(FilesystemOperation::ReadLinkAt {
                    dir_fd: CWD_FD,
                    path: path()?,
                    max_bytes: BoundedUsize::try_new(
                        MAX_PATH_BYTES.min(max_reply_bytes),
                        &reply_limit,
                    )?,
                }),
                PythonHostReplyKind::ReadLink,
            ),
            PythonVfsRpcMethod::Setattr => (
                HostOperation::Filesystem(FilesystemOperation::SetAttributesAt {
                    dir_fd: CWD_FD,
                    path: path()?,
                    update: PathAttributeUpdate {
                        mode: request.mode,
                        uid: request.uid,
                        gid: request.gid,
                        atime_ms: request.atime_ms,
                        mtime_ms: request.mtime_ms,
                    },
                    follow_symlinks: true,
                }),
                PythonHostReplyKind::Empty,
            ),
            PythonVfsRpcMethod::SubprocessRun => {
                let command = request.command.clone().ok_or_else(|| {
                    HostServiceError::new("EINVAL", "Python subprocessRun requires a command")
                })?;
                let launch = ProcessLaunchRequest {
                    command,
                    args: request.args.clone(),
                    options: ProcessLaunchOptions {
                        argv0: request.argv0.clone(),
                        cwd: request.cwd.clone(),
                        env: request.env.clone(),
                        shell: request.shell,
                        stdio: vec![
                            String::from("pipe"),
                            String::from("pipe"),
                            String::from("pipe"),
                        ],
                        ..ProcessLaunchOptions::default()
                    },
                };
                let max_buffer = request.max_buffer.unwrap_or(1024 * 1024);
                // The child service may retain maxBuffer independently for
                // stdout and stderr. Each arbitrary input byte can require up
                // to six JSON bytes (for example a control-character escape),
                // plus the fixed result envelope. Reject before spawn when the
                // final direct reply cannot be admitted.
                validate_python_captured_reply_limit(max_buffer, max_reply_bytes)?;
                (
                    HostOperation::Process(ProcessOperation::RunCaptured {
                        request: BoundedProcessLaunchRequest::try_new(launch, &request_limit)?,
                        max_buffer: BoundedUsize::try_new(max_buffer, &reply_limit)?,
                    }),
                    PythonHostReplyKind::RunCaptured,
                )
            }
            PythonVfsRpcMethod::HttpRequest => {
                let url = request.url.clone().ok_or_else(|| {
                    HostServiceError::new("EINVAL", "Python httpRequest requires a url")
                })?;
                let url_limit =
                    PayloadLimit::new("runtime.network.maxHttpUrlBytes", MAX_HTTP_URL_BYTES)?;
                let method_limit =
                    PayloadLimit::new("runtime.network.maxHttpMethodBytes", MAX_HTTP_METHOD_BYTES)?;
                let header_count_limit =
                    PayloadLimit::new("runtime.network.maxHttpHeaders", MAX_HTTP_HEADERS)?;
                let mut headers = Vec::with_capacity(request.headers.len());
                for (name, value) in &request.headers {
                    headers.push(HttpHeader {
                        name: BoundedString::try_new(name.clone(), &request_limit)?,
                        value: BoundedString::try_new(value.clone(), &request_limit)?,
                    });
                }
                let body = decode_body(request.body_base64.as_deref(), "Python HTTP bodyBase64")?;
                let response_body_max = max_reply_bytes.saturating_sub(512) / 4 * 3;
                let header_max = MAX_HTTP_HEADER_BYTES.min(max_reply_bytes.saturating_sub(256));
                (
                    HostOperation::Network(NetworkOperation::HttpRequest {
                        url: BoundedString::try_new(url, &url_limit)?,
                        method: BoundedString::try_new(
                            request
                                .http_method
                                .clone()
                                .unwrap_or_else(|| String::from("GET")),
                            &method_limit,
                        )?,
                        headers: BoundedVec::try_new(headers, &header_count_limit)?,
                        body: BoundedBytes::try_new(body, &request_limit)?,
                        max_response_bytes: BoundedUsize::try_new(max_reply_bytes, &reply_limit)?,
                        max_header_bytes: BoundedUsize::try_new(header_max, &reply_limit)?,
                        max_body_bytes: BoundedUsize::try_new(response_body_max, &reply_limit)?,
                    }),
                    PythonHostReplyKind::Http,
                )
            }
            PythonVfsRpcMethod::DnsLookup => {
                let host = request.hostname.clone().ok_or_else(|| {
                    HostServiceError::new("EINVAL", "Python dnsLookup requires a hostname")
                })?;
                let family = match request.family.unwrap_or(0) {
                    0 => DnsAddressFamily::Any,
                    4 => DnsAddressFamily::Inet4,
                    6 => DnsAddressFamily::Inet6,
                    family => {
                        return Err(HostServiceError::new(
                            "EINVAL",
                            format!("unsupported Python DNS address family {family}"),
                        ))
                    }
                };
                let maximum = MAX_DNS_RESULTS.min(max_reply_bytes / 64);
                (
                    HostOperation::Network(NetworkOperation::ResolveDns {
                        host: bounded_host(host, "Python DNS hostname")?,
                        port: None,
                        family,
                        max_results: BoundedUsize::try_new(
                            maximum,
                            &PayloadLimit::new("runtime.network.maxDnsResults", MAX_DNS_RESULTS)?,
                        )?,
                    }),
                    PythonHostReplyKind::Dns,
                )
            }
            PythonVfsRpcMethod::SocketConnect => {
                let host = request.hostname.clone().ok_or_else(|| {
                    HostServiceError::new("EINVAL", "Python socketConnect requires a hostname")
                })?;
                let port = request.port.ok_or_else(|| {
                    HostServiceError::new("EINVAL", "Python socketConnect requires a port")
                })?;
                let host = bounded_host(host, "Python TCP hostname")?;
                let reservation = PythonManagedNetworkState::reserve(
                    &self.managed_network,
                    PythonManagedSocketKind::Tcp,
                )?;
                (
                    HostOperation::Network(NetworkOperation::ManagedConnect {
                        endpoint: ManagedTcpEndpoint {
                            host: Some(host),
                            port: Some(port),
                            unix: None,
                            bound_server_id: None,
                            local_address: None,
                            local_port: None,
                            local_reservation: None,
                            backlog: None,
                        },
                    }),
                    PythonHostReplyKind::SocketCreated(reservation),
                )
            }
            PythonVfsRpcMethod::SocketSend => {
                let (_, socket) =
                    managed_socket(request.socket_id, Some(PythonManagedSocketKind::Tcp))?;
                let body = decode_body(request.body_base64.as_deref(), "Python TCP bodyBase64")?;
                (
                    HostOperation::Network(NetworkOperation::ManagedWrite {
                        socket_id: BoundedString::try_new(socket.host_socket_id, &socket_id_limit)?,
                        bytes: BoundedBytes::try_new(body, &request_limit)?,
                    }),
                    PythonHostReplyKind::SocketSent,
                )
            }
            PythonVfsRpcMethod::SocketRecv => {
                let (_, socket) =
                    managed_socket(request.socket_id, Some(PythonManagedSocketKind::Tcp))?;
                let requested = request
                    .max_buffer
                    .unwrap_or(PYTHON_SOCKET_DEFAULT_RECV)
                    .clamp(1, PYTHON_SOCKET_MAX_RECV);
                let maximum = requested.min(max_reply_bytes.saturating_sub(128) / 4 * 3);
                (
                    HostOperation::Network(NetworkOperation::ManagedRead {
                        socket_id: BoundedString::try_new(socket.host_socket_id, &socket_id_limit)?,
                        max_bytes: maximum as u64,
                        peek: false,
                        wait_ms: request
                            .timeout_ms
                            .unwrap_or(DEFAULT_PYTHON_VFS_RPC_TIMEOUT_MS),
                    }),
                    PythonHostReplyKind::SocketReceived,
                )
            }
            PythonVfsRpcMethod::SocketClose => {
                let (socket_id, socket) = managed_socket(request.socket_id, None)?;
                let operation = match socket.kind {
                    PythonManagedSocketKind::Tcp => NetworkOperation::ManagedDestroy {
                        socket_id: BoundedString::try_new(socket.host_socket_id, &socket_id_limit)?,
                    },
                    PythonManagedSocketKind::Udp => NetworkOperation::ManagedUdpClose {
                        socket_id: BoundedString::try_new(socket.host_socket_id, &socket_id_limit)?,
                    },
                };
                (
                    HostOperation::Network(operation),
                    PythonHostReplyKind::SocketClosed(socket_id),
                )
            }
            PythonVfsRpcMethod::UdpCreate => {
                let reservation = PythonManagedNetworkState::reserve(
                    &self.managed_network,
                    PythonManagedSocketKind::Udp,
                )?;
                (
                    HostOperation::Network(NetworkOperation::ManagedUdpCreate {
                        family: ManagedUdpFamily::Inet4,
                    }),
                    PythonHostReplyKind::SocketCreated(reservation),
                )
            }
            PythonVfsRpcMethod::UdpSendto => {
                let (_, socket) =
                    managed_socket(request.socket_id, Some(PythonManagedSocketKind::Udp))?;
                let host = request.hostname.clone().ok_or_else(|| {
                    HostServiceError::new("EINVAL", "Python udpSendto requires a hostname")
                })?;
                let port = request.port.ok_or_else(|| {
                    HostServiceError::new("EINVAL", "Python udpSendto requires a port")
                })?;
                let body = decode_body(request.body_base64.as_deref(), "Python UDP bodyBase64")?;
                (
                    HostOperation::Network(NetworkOperation::ManagedUdpSend {
                        socket_id: BoundedString::try_new(socket.host_socket_id, &socket_id_limit)?,
                        bytes: BoundedBytes::try_new(body, &request_limit)?,
                        host: Some(bounded_host(host, "Python UDP hostname")?),
                        port: Some(port),
                    }),
                    PythonHostReplyKind::SocketSent,
                )
            }
            PythonVfsRpcMethod::UdpRecvfrom => {
                let (_, socket) =
                    managed_socket(request.socket_id, Some(PythonManagedSocketKind::Udp))?;
                let requested = request
                    .max_buffer
                    .unwrap_or(PYTHON_SOCKET_DEFAULT_RECV)
                    .clamp(1, PYTHON_SOCKET_MAX_RECV);
                let maximum = requested.min(max_reply_bytes.saturating_sub(256) / 4 * 3);
                (
                    HostOperation::Network(NetworkOperation::ManagedUdpPoll {
                        socket_id: BoundedString::try_new(socket.host_socket_id, &socket_id_limit)?,
                        wait_ms: request
                            .timeout_ms
                            .unwrap_or(DEFAULT_PYTHON_VFS_RPC_TIMEOUT_MS),
                        peek: false,
                        max_bytes: Some(BoundedUsize::try_new(maximum, &reply_limit)?),
                    }),
                    PythonHostReplyKind::UdpReceived,
                )
            }
        };
        let target = Arc::new(PythonHostReplyTarget {
            responder: self.clone(),
            kind,
        });
        let reply = DirectHostReplyHandle::new(identity, target, max_reply_bytes)?;
        Ok(Some(PythonHostCall { operation, reply }))
    }

    pub fn respond_success(
        &self,
        id: u64,
        payload: PythonVfsRpcResponsePayload,
    ) -> Result<(), PythonExecutionError> {
        match clear_pending_vfs_rpc(&self.pending_vfs_rpc, id)? {
            PendingVfsRpcResolution::Pending => {}
            PendingVfsRpcResolution::TimedOut | PendingVfsRpcResolution::Missing => {
                return Err(PythonExecutionError::RpcResponse(format!(
                    "VFS RPC request {id} is no longer pending"
                )));
            }
        }

        let result = match payload {
            PythonVfsRpcResponsePayload::Empty => json!({}),
            PythonVfsRpcResponsePayload::Read { content_base64 } => {
                json!({ "contentBase64": content_base64 })
            }
            PythonVfsRpcResponsePayload::Stat { stat } => json!({
                "stat": {
                    "mode": stat.mode,
                    "size": stat.size,
                    "isDirectory": stat.is_directory,
                    "isSymbolicLink": stat.is_symbolic_link,
                }
            }),
            PythonVfsRpcResponsePayload::ReadDir { entries } => {
                json!({ "entries": entries })
            }
            PythonVfsRpcResponsePayload::Http {
                status,
                reason,
                url,
                headers,
                body_base64,
            } => json!({
                "status": status,
                "reason": reason,
                "url": url,
                "headers": headers,
                "bodyBase64": body_base64,
            }),
            PythonVfsRpcResponsePayload::DnsLookup { addresses } => {
                json!({ "addresses": addresses })
            }
            PythonVfsRpcResponsePayload::SubprocessRun {
                exit_code,
                stdout,
                stderr,
                max_buffer_exceeded,
            } => json!({
                "exitCode": exit_code,
                "stdout": stdout,
                "stderr": stderr,
                "maxBufferExceeded": max_buffer_exceeded,
            }),
            PythonVfsRpcResponsePayload::SocketCreated { socket_id } => json!({
                "socketId": socket_id,
            }),
            PythonVfsRpcResponsePayload::SocketSent { bytes_sent } => json!({
                "bytesSent": bytes_sent,
            }),
            PythonVfsRpcResponsePayload::SocketReceived {
                data_base64,
                closed,
                timed_out,
            } => json!({
                "dataBase64": data_base64,
                "closed": closed,
                "timedOut": timed_out,
            }),
            PythonVfsRpcResponsePayload::UdpReceived {
                data_base64,
                host,
                port,
                timed_out,
            } => json!({
                "dataBase64": data_base64,
                "host": host,
                "port": port,
                "timedOut": timed_out,
            }),
            PythonVfsRpcResponsePayload::SymlinkTarget { target } => json!({
                "target": target,
            }),
        };

        self.javascript_responder
            .respond_success(id, result)
            .map_err(map_javascript_error)
    }

    pub fn respond_error(
        &self,
        id: u64,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<(), PythonExecutionError> {
        match clear_pending_vfs_rpc(&self.pending_vfs_rpc, id)? {
            PendingVfsRpcResolution::Pending => {}
            PendingVfsRpcResolution::TimedOut | PendingVfsRpcResolution::Missing => {
                return Err(PythonExecutionError::RpcResponse(format!(
                    "VFS RPC request {id} is no longer pending"
                )));
            }
        }

        self.javascript_responder
            .respond_host_error(id, HostServiceError::new(code.into(), message.into()))
            .map_err(map_javascript_error)
    }

    pub fn respond_host_error(
        &self,
        id: u64,
        error: HostServiceError,
    ) -> Result<(), PythonExecutionError> {
        match clear_pending_vfs_rpc(&self.pending_vfs_rpc, id)? {
            PendingVfsRpcResolution::Pending => {}
            PendingVfsRpcResolution::TimedOut | PendingVfsRpcResolution::Missing => {
                return Err(PythonExecutionError::RpcResponse(format!(
                    "VFS RPC request {id} is no longer pending"
                )));
            }
        }
        self.javascript_responder
            .respond_host_error(id, error)
            .map_err(map_javascript_error)
    }
}

impl DirectHostReplyTarget for PythonHostReplyTarget {
    fn claim(&self, call_id: u64) -> Result<bool, HostServiceError> {
        let claimed = claim_pending_vfs_rpc(&self.responder.pending_vfs_rpc, call_id, || {
            self.responder
                .javascript_responder
                .claim(call_id)
                .map_err(crate::javascript::host_reply_adapter_error)
        })?;
        if !claimed {
            self.kind.rollback_socket_reservation();
        }
        Ok(claimed)
    }

    fn respond(
        &self,
        call_id: u64,
        claimed: bool,
        result: Result<HostCallReply, HostServiceError>,
    ) -> Result<(), HostServiceError> {
        if !claimed {
            match clear_pending_vfs_rpc(&self.responder.pending_vfs_rpc, call_id)
                .map_err(python_host_reply_adapter_error)?
            {
                PendingVfsRpcResolution::Pending => {}
                PendingVfsRpcResolution::TimedOut | PendingVfsRpcResolution::Missing => {
                    return Err(HostServiceError::new(
                        "ESTALE",
                        format!("Python host call {call_id} is no longer pending"),
                    )
                    .with_details(json!({ "callId": call_id })));
                }
            }
        }

        if result.is_err() {
            self.kind.rollback_socket_reservation();
        }
        let response = match result {
            Ok(reply) => match map_python_host_reply(
                &self.responder.managed_network,
                self.kind.clone(),
                reply,
            ) {
                Ok(value) if claimed => self
                    .responder
                    .javascript_responder
                    .respond_claimed_success(call_id, value),
                Ok(value) => self
                    .responder
                    .javascript_responder
                    .respond_success(call_id, value),
                Err(error) if claimed => self
                    .responder
                    .javascript_responder
                    .respond_claimed_host_error(call_id, error),
                Err(error) => self
                    .responder
                    .javascript_responder
                    .respond_host_error(call_id, error),
            },
            Err(error) if claimed => self
                .responder
                .javascript_responder
                .respond_claimed_host_error(call_id, error),
            Err(error) => self
                .responder
                .javascript_responder
                .respond_host_error(call_id, error),
        };
        crate::javascript::map_host_reply_adapter_response(response)
    }
}

fn python_host_reply_adapter_error(error: PythonExecutionError) -> HostServiceError {
    match error {
        PythonExecutionError::PendingVfsRpcRequest(call_id) => HostServiceError::new(
            "EBUSY",
            format!("Python host call {call_id} is already pending"),
        )
        .with_details(json!({ "callId": call_id })),
        PythonExecutionError::PendingVfsRpcLimit { limit, observed } => HostServiceError::limit(
            "ERR_AGENTOS_RESOURCE_LIMIT",
            "limits.reactor.maxBridgeCalls",
            limit as u64,
            observed as u64,
        ),
        other => HostServiceError::new("ERR_AGENTOS_PYTHON_ADAPTER_REPLY", other.to_string()),
    }
}

fn map_python_host_reply(
    managed_network: &Arc<Mutex<PythonManagedNetworkState>>,
    kind: PythonHostReplyKind,
    reply: HostCallReply,
) -> Result<Value, HostServiceError> {
    let reply_kind = format!("{kind:?}");
    let protocol_error = |expected: &'static str| {
        HostServiceError::new("EPROTO", format!("Python host reply expected {expected}"))
            .with_details(json!({ "replyKind": reply_kind }))
    };
    match (kind, reply) {
        (PythonHostReplyKind::Empty, HostCallReply::Empty)
        | (PythonHostReplyKind::Empty, HostCallReply::Json(Value::Null)) => Ok(json!({})),
        (PythonHostReplyKind::FileRead, HostCallReply::Raw(bytes)) => Ok(json!({
            "contentBase64": base64::engine::general_purpose::STANDARD.encode(bytes),
        })),
        (PythonHostReplyKind::Stat, HostCallReply::Json(Value::Object(stat))) => Ok(json!({
            "stat": {
                "mode": stat.get("mode").cloned().unwrap_or(Value::Null),
                "size": stat.get("size").cloned().unwrap_or(Value::Null),
                "isDirectory": stat.get("isDirectory").cloned().unwrap_or(Value::Bool(false)),
                "isSymbolicLink": stat.get("isSymbolicLink").cloned().unwrap_or(Value::Bool(false)),
            }
        })),
        (PythonHostReplyKind::ReadDirectory, HostCallReply::Json(Value::Array(entries))) => {
            Ok(json!({ "entries": entries }))
        }
        (PythonHostReplyKind::ReadLink, HostCallReply::Json(Value::String(target))) => {
            Ok(json!({ "target": target }))
        }
        (PythonHostReplyKind::RunCaptured, HostCallReply::Json(Value::Object(result))) => {
            Ok(json!({
                "exitCode": result.get("code").cloned().unwrap_or(Value::from(1)),
                "stdout": result.get("stdout").cloned().unwrap_or(Value::String(String::new())),
                "stderr": result.get("stderr").cloned().unwrap_or(Value::String(String::new())),
                "maxBufferExceeded": result.get("maxBufferExceeded").cloned().unwrap_or(Value::Bool(false)),
            }))
        }
        (PythonHostReplyKind::Http, HostCallReply::Json(Value::Object(result))) => {
            Ok(Value::Object(result))
        }
        (PythonHostReplyKind::Dns, HostCallReply::Json(Value::Array(addresses))) => {
            let addresses = addresses
                .into_iter()
                .map(|entry| {
                    entry
                        .get("address")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .ok_or_else(|| protocol_error("DNS address objects"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(json!({ "addresses": addresses }))
        }
        (
            PythonHostReplyKind::SocketCreated(reservation),
            HostCallReply::Json(Value::Object(created)),
        ) => {
            let host_socket_id = created
                .get("socketId")
                .and_then(Value::as_str)
                .ok_or_else(|| protocol_error("a managed socket object"))?
                .to_owned();
            let socket_id = reservation.commit(host_socket_id)?;
            Ok(json!({ "socketId": socket_id }))
        }
        (PythonHostReplyKind::SocketSent, HostCallReply::Json(Value::Number(written))) => {
            Ok(json!({ "bytesSent": written }))
        }
        (PythonHostReplyKind::SocketSent, HostCallReply::Json(Value::Object(result))) => {
            let written = result
                .get("bytes")
                .and_then(Value::as_u64)
                .ok_or_else(|| protocol_error("a managed socket byte count"))?;
            Ok(json!({ "bytesSent": written }))
        }
        (PythonHostReplyKind::SocketReceived, HostCallReply::Raw(bytes)) => Ok(json!({
            "dataBase64": base64::engine::general_purpose::STANDARD.encode(bytes),
            "closed": false,
            "timedOut": false,
        })),
        (PythonHostReplyKind::SocketReceived, HostCallReply::Json(Value::Null)) => Ok(json!({
            "dataBase64": "",
            "closed": true,
            "timedOut": false,
        })),
        (PythonHostReplyKind::SocketReceived, HostCallReply::Json(Value::String(timeout)))
            if timeout == "__agentos_net_timeout__" =>
        {
            Ok(json!({
                "dataBase64": "",
                "closed": false,
                "timedOut": true,
            }))
        }
        (
            PythonHostReplyKind::SocketClosed(socket_id),
            HostCallReply::Empty | HostCallReply::Json(Value::Null),
        ) => {
            managed_network
                .lock()
                .map_err(|_| {
                    HostServiceError::new(
                        "ERR_AGENTOS_PYTHON_NETWORK_STATE_POISONED",
                        "Python managed-network state lock poisoned",
                    )
                })?
                .sockets
                .remove(&socket_id);
            Ok(json!({}))
        }
        (PythonHostReplyKind::UdpReceived, HostCallReply::Json(Value::Null)) => Ok(json!({
            "dataBase64": "",
            "host": "",
            "port": 0,
            "timedOut": true,
        })),
        (PythonHostReplyKind::UdpReceived, HostCallReply::Json(Value::Object(message))) => {
            if message.get("type").and_then(Value::as_str) == Some("error") {
                return Err(HostServiceError::new(
                    message.get("code").and_then(Value::as_str).unwrap_or("EIO"),
                    message
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("managed UDP receive failed"),
                ));
            }
            let encoded = message
                .get("data")
                .and_then(Value::as_object)
                .ok_or_else(|| protocol_error("a managed UDP datagram"))?;
            if encoded.get("__agentOSType").and_then(Value::as_str) != Some("bytes") {
                return Err(protocol_error("encoded managed UDP datagram bytes"));
            }
            let data_base64 = encoded
                .get("base64")
                .and_then(Value::as_str)
                .ok_or_else(|| protocol_error("encoded managed UDP datagram bytes"))?;
            base64::engine::general_purpose::STANDARD
                .decode(data_base64)
                .map_err(|_| protocol_error("valid managed UDP datagram base64"))?;
            let host = message
                .get("remoteAddress")
                .and_then(Value::as_str)
                .ok_or_else(|| protocol_error("a managed UDP remote address"))?;
            let port = message
                .get("remotePort")
                .and_then(Value::as_u64)
                .and_then(|port| u16::try_from(port).ok())
                .ok_or_else(|| protocol_error("a managed UDP remote port"))?;
            Ok(json!({
                "dataBase64": data_base64,
                "host": host,
                "port": port,
                "timedOut": false,
            }))
        }
        (PythonHostReplyKind::Empty, _) => Err(protocol_error("an empty reply")),
        (PythonHostReplyKind::FileRead, _) => Err(protocol_error("raw file bytes")),
        (PythonHostReplyKind::Stat, _) => Err(protocol_error("a stat object")),
        (PythonHostReplyKind::ReadDirectory, _) => Err(protocol_error("a directory-entry array")),
        (PythonHostReplyKind::ReadLink, _) => Err(protocol_error("a symlink target")),
        (PythonHostReplyKind::RunCaptured, _) => Err(protocol_error("a captured-process result")),
        (PythonHostReplyKind::Http, _) => Err(protocol_error("an HTTP response object")),
        (PythonHostReplyKind::Dns, _) => Err(protocol_error("a DNS address array")),
        (PythonHostReplyKind::SocketCreated(_), _) => {
            Err(protocol_error("a managed socket object"))
        }
        (PythonHostReplyKind::SocketSent, _) => Err(protocol_error("a socket byte count")),
        (PythonHostReplyKind::SocketReceived, _) => Err(protocol_error("a TCP receive reply")),
        (PythonHostReplyKind::SocketClosed(_), _) => Err(protocol_error("an empty close reply")),
        (PythonHostReplyKind::UdpReceived, _) => Err(protocol_error("a UDP receive reply")),
    }
}

fn claim_pending_vfs_rpc(
    pending_vfs_rpc: &Arc<Mutex<PendingVfsRpcRegistry>>,
    id: u64,
    downstream_claim: impl FnOnce() -> Result<bool, HostServiceError>,
) -> Result<bool, HostServiceError> {
    let mut pending = pending_vfs_rpc.lock().map_err(|_| {
        HostServiceError::new(
            "ERR_AGENTOS_PYTHON_ADAPTER_REPLY",
            "Python pending host-call registry lock poisoned",
        )
    })?;
    let Some(rpc) = pending.entries.get(&id) else {
        return Ok(false);
    };
    if rpc.state == PendingVfsRpcState::TimedOut(id) {
        let rpc = pending
            .entries
            .remove(&id)
            .expect("timed-out call remains registered");
        pending.observe_depth();
        if let Some(timeout_abort) = rpc.timeout_abort {
            timeout_abort.abort();
        }
        return Ok(false);
    }
    let claimed = downstream_claim()?;
    let rpc = pending
        .entries
        .remove(&id)
        .expect("pending call remains registered while its registry lock is held");
    pending.observe_depth();
    if let Some(timeout_abort) = rpc.timeout_abort {
        timeout_abort.abort();
    }
    Ok(claimed)
}

fn clear_pending_vfs_rpc(
    pending_vfs_rpc: &Arc<Mutex<PendingVfsRpcRegistry>>,
    id: u64,
) -> Result<PendingVfsRpcResolution, PythonExecutionError> {
    let mut pending = pending_vfs_rpc
        .lock()
        .map_err(|_| PythonExecutionError::EventChannelClosed)?;
    let Some(rpc) = pending.entries.remove(&id) else {
        return Ok(PendingVfsRpcResolution::Missing);
    };
    let resolution = match rpc.state {
        PendingVfsRpcState::Pending(_) => PendingVfsRpcResolution::Pending,
        PendingVfsRpcState::TimedOut(_) => PendingVfsRpcResolution::TimedOut,
    };
    if let Some(timeout_abort) = rpc.timeout_abort {
        timeout_abort.abort();
    }
    pending.observe_depth();
    Ok(resolution)
}

fn cancel_pending_vfs_rpc(pending_vfs_rpc: &Arc<Mutex<PendingVfsRpcRegistry>>) {
    let pending = pending_vfs_rpc
        .lock()
        .map(|mut pending| {
            let entries = std::mem::take(&mut pending.entries);
            pending.observe_depth();
            entries
        })
        .unwrap_or_else(|poisoned| {
            eprintln!("ERR_AGENTOS_PYTHON_VFS_RPC_STATE_POISONED: cancelling pending timeouts");
            let mut pending = poisoned.into_inner();
            let entries = std::mem::take(&mut pending.entries);
            pending.observe_depth();
            entries
        });
    for rpc in pending.into_values() {
        if let Some(timeout_abort) = rpc.timeout_abort {
            timeout_abort.abort();
        }
    }
}

impl Drop for PythonExecution {
    fn drop(&mut self) {
        cancel_pending_vfs_rpc(&self.pending_vfs_rpc);
        if let Err(error) = self.close_stdin() {
            eprintln!("ERR_AGENTOS_PYTHON_STDIN_CLOSE: {error}");
        }
        if let Err(error) = self.inner.terminate() {
            eprintln!("ERR_AGENTOS_PYTHON_TERMINATE: {error}");
        }
    }
}

#[derive(Debug)]
pub struct PythonExecutionEngine {
    runtime: Option<RuntimeContext>,
    next_context_id: usize,
    next_execution_id: usize,
    contexts: BTreeMap<String, PythonContext>,
    import_caches: BTreeMap<String, NodeImportCache>,
    javascript_context_ids: BTreeMap<String, String>,
    javascript_engine: JavascriptExecutionEngine,
}

impl Default for PythonExecutionEngine {
    fn default() -> Self {
        let runtime = default_python_test_runtime_context();
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
        }
    }
}

#[cfg(test)]
fn default_python_test_runtime_context() -> Option<RuntimeContext> {
    agentos_runtime_tokio::SidecarRuntime::process(&agentos_runtime_tokio::RuntimeConfig::default())
        .ok()
        .map(agentos_runtime_tokio::SidecarRuntime::context)
}

#[cfg(not(test))]
fn default_python_test_runtime_context() -> Option<RuntimeContext> {
    None
}

impl PythonExecutionEngine {
    pub fn new(runtime: RuntimeContext) -> Self {
        Self {
            runtime: Some(runtime.clone()),
            next_context_id: 0,
            next_execution_id: 0,
            contexts: BTreeMap::new(),
            import_caches: BTreeMap::new(),
            javascript_context_ids: BTreeMap::new(),
            javascript_engine: JavascriptExecutionEngine::new(runtime),
        }
    }

    pub fn set_runtime_context(&mut self, runtime: RuntimeContext) {
        self.javascript_engine.set_runtime_context(runtime.clone());
        self.runtime = Some(runtime);
    }

    fn runtime_context(&self) -> Result<&RuntimeContext, PythonExecutionError> {
        self.runtime.as_ref().ok_or_else(|| {
            PythonExecutionError::Spawn(std::io::Error::other(
                "ERR_AGENTOS_RUNTIME_NOT_INJECTED: PythonExecutionEngine requires a process RuntimeContext; construct it with PythonExecutionEngine::new(runtime)",
            ))
        })
    }

    pub fn set_event_notify(&mut self, notify: Option<Arc<Notify>>) {
        self.javascript_engine.set_event_notify(notify);
    }

    pub fn bundled_pyodide_dist_path_for_vm(
        &mut self,
        vm_id: &str,
    ) -> Result<PathBuf, PythonExecutionError> {
        ensure_pyodide_available()?;
        let runtime = self.runtime_context()?.clone();
        let import_cache = self.import_caches.entry(vm_id.to_owned()).or_default();
        import_cache
            .ensure_materialized_with_runtime(&runtime)
            .map_err(PythonExecutionError::PrepareRuntime)?;
        Ok(import_cache.pyodide_dist_path().to_path_buf())
    }

    pub async fn bundled_pyodide_dist_path_for_vm_async(
        &mut self,
        vm_id: &str,
        runtime: &RuntimeContext,
    ) -> Result<PathBuf, PythonExecutionError> {
        ensure_pyodide_available()?;
        let import_cache = self.import_caches.entry(vm_id.to_owned()).or_default();
        import_cache
            .ensure_materialized_with_timeout_and_runtime_async(runtime, PYTHON_PREWARM_TIMEOUT)
            .await
            .map_err(PythonExecutionError::PrepareRuntime)?;
        Ok(import_cache.pyodide_dist_path().to_path_buf())
    }

    pub fn create_context(&mut self, request: CreatePythonContextRequest) -> PythonContext {
        self.next_context_id += 1;
        self.import_caches.entry(request.vm_id.clone()).or_default();
        let javascript_context =
            self.javascript_engine
                .create_context(CreateJavascriptContextRequest {
                    vm_id: request.vm_id.clone(),
                    bootstrap_module: None,
                    compile_cache_root: None,
                });

        let context = PythonContext {
            context_id: format!("python-ctx-{}", self.next_context_id),
            vm_id: request.vm_id,
            pyodide_dist_path: request.pyodide_dist_path,
        };
        self.javascript_context_ids
            .insert(context.context_id.clone(), javascript_context.context_id);
        self.contexts
            .insert(context.context_id.clone(), context.clone());
        context
    }

    /// Dispose the Python context and its private JavaScript bridge context.
    /// Live executions retain their own bridge session and are unaffected.
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
        request: StartPythonExecutionRequest,
    ) -> Result<PythonExecution, PythonExecutionError> {
        let runtime = self.runtime_context()?.clone();
        self.create_execution_with_runtime(request, runtime, false)
    }

    pub fn prepare_execution(
        &mut self,
        request: StartPythonExecutionRequest,
    ) -> Result<PythonExecution, PythonExecutionError> {
        let runtime = self.runtime_context()?.clone();
        self.create_execution_with_runtime(request, runtime, true)
    }

    pub fn start_execution_with_runtime(
        &mut self,
        request: StartPythonExecutionRequest,
        runtime: RuntimeContext,
    ) -> Result<PythonExecution, PythonExecutionError> {
        self.create_execution_with_runtime(request, runtime, false)
    }

    fn create_execution_with_runtime(
        &mut self,
        request: StartPythonExecutionRequest,
        runtime: RuntimeContext,
        defer_execute: bool,
    ) -> Result<PythonExecution, PythonExecutionError> {
        ensure_pyodide_available()?;
        let context = self
            .contexts
            .get(&request.context_id)
            .cloned()
            .ok_or_else(|| PythonExecutionError::MissingContext(request.context_id.clone()))?;

        if context.vm_id != request.vm_id {
            return Err(PythonExecutionError::VmMismatch {
                expected: context.vm_id,
                found: request.vm_id,
            });
        }

        let frozen_time_ms = frozen_time_ms();
        let javascript_context_id = self
            .javascript_context_ids
            .get(&context.context_id)
            .cloned()
            .ok_or_else(|| PythonExecutionError::MissingContext(context.context_id.clone()))?;
        let warmup_metrics = {
            let import_cache = self.import_caches.entry(context.vm_id.clone()).or_default();
            import_cache
                .ensure_materialized_with_runtime(&runtime)
                .map_err(PythonExecutionError::PrepareRuntime)?;
            prewarm_python_path(
                import_cache,
                &mut self.javascript_engine,
                &javascript_context_id,
                &context,
                &request,
                frozen_time_ms,
                &runtime,
            )?
        };

        self.finish_start_execution(
            request,
            runtime,
            &context,
            javascript_context_id,
            frozen_time_ms,
            warmup_metrics,
            defer_execute,
        )
    }

    /// Start Python from an async sidecar dispatch without parking a trusted
    /// Tokio worker during import-cache materialization or V8 prewarm.
    pub async fn start_execution_with_runtime_async(
        &mut self,
        request: StartPythonExecutionRequest,
        runtime: RuntimeContext,
    ) -> Result<PythonExecution, PythonExecutionError> {
        ensure_pyodide_available()?;
        let context = self
            .contexts
            .get(&request.context_id)
            .cloned()
            .ok_or_else(|| PythonExecutionError::MissingContext(request.context_id.clone()))?;

        if context.vm_id != request.vm_id {
            return Err(PythonExecutionError::VmMismatch {
                expected: context.vm_id,
                found: request.vm_id,
            });
        }

        let frozen_time_ms = frozen_time_ms();
        let javascript_context =
            self.javascript_engine
                .create_context(CreateJavascriptContextRequest {
                    vm_id: request.vm_id.clone(),
                    bootstrap_module: None,
                    compile_cache_root: None,
                });
        let javascript_context_id = javascript_context.context_id.clone();
        self.javascript_context_ids
            .insert(context.context_id.clone(), javascript_context_id.clone());
        let warmup_metrics = {
            let import_cache = self.import_caches.entry(context.vm_id.clone()).or_default();
            import_cache
                .ensure_materialized_with_timeout_and_runtime_async(
                    &runtime,
                    PYTHON_PREWARM_TIMEOUT,
                )
                .await
                .map_err(PythonExecutionError::PrepareRuntime)?;
            prewarm_python_path_async(
                import_cache,
                &mut self.javascript_engine,
                &javascript_context_id,
                &context,
                &request,
                frozen_time_ms,
                &runtime,
            )
            .await?
        };

        self.finish_start_execution(
            request,
            runtime,
            &context,
            javascript_context_id,
            frozen_time_ms,
            warmup_metrics,
            false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_start_execution(
        &mut self,
        request: StartPythonExecutionRequest,
        runtime: RuntimeContext,
        context: &PythonContext,
        javascript_context_id: String,
        frozen_time_ms: u128,
        warmup_metrics: Option<Vec<u8>>,
        defer_execute: bool,
    ) -> Result<PythonExecution, PythonExecutionError> {
        self.next_execution_id += 1;
        let execution_id = format!("exec-{}", self.next_execution_id);
        let import_cache = self
            .import_caches
            .get(&context.vm_id)
            .expect("vm import cache should exist after materialization");
        let pyodide_dist_path =
            resolved_pyodide_dist_path(&context.pyodide_dist_path, &request.cwd);
        let javascript_execution = start_python_javascript_execution(
            &mut self.javascript_engine,
            &runtime,
            import_cache,
            &javascript_context_id,
            context,
            &request,
            PythonJavascriptExecutionOptions {
                frozen_time_ms,
                prewarm_only: false,
                warmup_metrics: warmup_metrics.as_deref(),
                defer_execute,
            },
        )?;
        let max_pending_vfs_rpcs = runtime
            .resources()
            .configured_limit(ResourceClass::BridgeCalls)
            .map_or(DEFAULT_PYTHON_PENDING_VFS_RPCS, |limit| limit.maximum);
        let pending_vfs_rpc =
            Arc::new(Mutex::new(PendingVfsRpcRegistry::new(max_pending_vfs_rpcs)));
        let managed_network = Arc::new(Mutex::new(PythonManagedNetworkState::new(
            python_managed_host_file_limit(&request),
        )));
        let vfs_rpc_timeout = python_vfs_rpc_timeout(&request);

        Ok(PythonExecution {
            runtime,
            execution_id,
            child_pid: javascript_execution.native_process_id().unwrap_or_default(),
            v8_session: javascript_execution.v8_session_handle(),
            inner: javascript_execution,
            pyodide_dist_path,
            managed_host_files: PythonManagedHostFiles::new(python_managed_host_file_limit(
                &request,
            )),
            pending_vfs_rpc,
            managed_network,
            output_buffer_max_bytes: python_output_buffer_max_bytes(&request),
            execution_timeout: python_execution_timeout(&request),
            vfs_rpc_timeout,
        })
    }

    pub fn dispose_vm(&mut self, vm_id: &str) {
        self.contexts.retain(|_, context| context.vm_id != vm_id);
        self.javascript_context_ids
            .retain(|python_context_id, _| self.contexts.contains_key(python_context_id));
        self.import_caches.remove(vm_id);
        self.javascript_engine.dispose_vm(vm_id);
    }
}

fn set_pending_vfs_rpc_state(
    pending_vfs_rpc: &Arc<Mutex<PendingVfsRpcRegistry>>,
    id: u64,
) -> Result<(), PythonExecutionError> {
    let mut pending = pending_vfs_rpc
        .lock()
        .map_err(|_| PythonExecutionError::EventChannelClosed)?;
    if pending.entries.contains_key(&id) {
        return Err(PythonExecutionError::PendingVfsRpcRequest(id));
    }
    let observed = pending.entries.len().saturating_add(1);
    if observed > pending.maximum {
        return Err(PythonExecutionError::PendingVfsRpcLimit {
            limit: pending.maximum,
            observed,
        });
    }
    pending.entries.insert(
        id,
        PendingVfsRpc {
            state: PendingVfsRpcState::Pending(id),
            timeout_abort: None,
        },
    );
    pending.observe_depth();
    Ok(())
}

fn python_vfs_rpc_admission_error(error: PythonExecutionError) -> HostServiceError {
    match error {
        PythonExecutionError::PendingVfsRpcRequest(id) => HostServiceError::new(
            "EBUSY",
            PythonExecutionError::PendingVfsRpcRequest(id).to_string(),
        )
        .with_details(json!({ "callId": id })),
        PythonExecutionError::PendingVfsRpcLimit { limit, observed } => HostServiceError::limit(
            "ERR_AGENTOS_RESOURCE_LIMIT",
            "limits.reactor.maxBridgeCalls",
            limit as u64,
            observed as u64,
        ),
        other => HostServiceError::new("ERR_AGENTOS_PYTHON_VFS_RPC", other.to_string()),
    }
}

fn map_javascript_error(error: JavascriptExecutionError) -> PythonExecutionError {
    match error {
        JavascriptExecutionError::EmptyArgv => PythonExecutionError::Spawn(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "guest Python bootstrap requires a JavaScript entrypoint",
        )),
        JavascriptExecutionError::InvalidLimit(message) => {
            PythonExecutionError::InvalidLimit(message)
        }
        JavascriptExecutionError::MissingContext(context_id) => {
            PythonExecutionError::MissingContext(context_id)
        }
        JavascriptExecutionError::VmMismatch { expected, found } => {
            PythonExecutionError::VmMismatch { expected, found }
        }
        JavascriptExecutionError::PrepareImportCache(error) => {
            PythonExecutionError::PrepareRuntime(error)
        }
        JavascriptExecutionError::Spawn(error) => PythonExecutionError::Spawn(error),
        JavascriptExecutionError::PendingSyncRpcRequest(id) => {
            PythonExecutionError::PendingVfsRpcRequest(id)
        }
        JavascriptExecutionError::PendingSyncRpcLimit { limit, observed } => {
            PythonExecutionError::RpcResponse(format!(
                "ERR_AGENTOS_RESOURCE_LIMIT: pending sync RPC calls observed {observed}, exceeding limits.reactor.maxBridgeCalls ({limit})"
            ))
        }
        JavascriptExecutionError::ExpiredSyncRpcRequest(id) => {
            PythonExecutionError::RpcResponse(format!("VFS RPC request {id} is no longer pending"))
        }
        JavascriptExecutionError::RpcResponse(message) => {
            PythonExecutionError::RpcResponse(message)
        }
        JavascriptExecutionError::BridgeSettlement(error) => {
            PythonExecutionError::RpcResponse(error.to_string())
        }
        JavascriptExecutionError::Terminate(error) => PythonExecutionError::Kill(error),
        JavascriptExecutionError::Control(error) => PythonExecutionError::Control(error),
        JavascriptExecutionError::StdinClosed => PythonExecutionError::StdinClosed,
        JavascriptExecutionError::Stdin(error) => PythonExecutionError::Stdin(error),
        JavascriptExecutionError::OutputBufferExceeded { stream, limit } => {
            PythonExecutionError::OutputBufferExceeded { stream, limit }
        }
        JavascriptExecutionError::EventChannelClosed => PythonExecutionError::EventChannelClosed,
    }
}

struct PythonJavascriptExecutionOptions<'a> {
    frozen_time_ms: u128,
    prewarm_only: bool,
    warmup_metrics: Option<&'a [u8]>,
    defer_execute: bool,
}

fn start_python_javascript_execution(
    javascript_engine: &mut JavascriptExecutionEngine,
    runtime: &RuntimeContext,
    import_cache: &NodeImportCache,
    javascript_context_id: &str,
    context: &PythonContext,
    request: &StartPythonExecutionRequest,
    options: PythonJavascriptExecutionOptions<'_>,
) -> Result<JavascriptExecution, PythonExecutionError> {
    let internal_env = build_python_internal_env(
        import_cache,
        context,
        request,
        options.frozen_time_ms,
        options.prewarm_only,
    );
    let inline_code =
        build_python_runner_module_source(import_cache, &internal_env, options.warmup_metrics)?;
    let mut env = request.env.clone();
    env.extend(internal_env);
    // The trusted Python runner is always an `.mjs` module and uses top-level
    // await. A JavaScript parent can otherwise pass its own internal CommonJS
    // selector through an inherited `process.env`, incorrectly forcing this
    // nested runner through `require()`.
    enforce_python_runner_module_mode(&mut env);

    // The Pyodide runner is itself a V8 execution. Its heap cap (the Python
    // `maxOldSpaceMb` knob) and sync-RPC wait ceiling ride the typed runner
    // limits, not env — the JS engine reads them from `limits`, not `AGENTOS_*`.
    let max_old_space_mb = python_max_old_space_mb(request);
    let runner_limits = python_runner_javascript_limits(&request.limits, max_old_space_mb);

    let javascript_request = StartJavascriptExecutionRequest {
        vm_id: request.vm_id.clone(),
        context_id: javascript_context_id.to_owned(),
        argv: vec![import_cache.python_runner_path().display().to_string()],
        argv0: None,
        env,
        cwd: request.cwd.clone(),
        limits: runner_limits,
        // Forward the guest-runtime identity so the runner's shim sets
        // process.* from typed config rather than env.
        guest_runtime: request.guest_runtime.clone(),
        wasm_module_bytes: None,
        inline_code: Some(inline_code),
    };
    if options.defer_execute {
        javascript_engine.prepare_execution_with_runtime(javascript_request, runtime.clone())
    } else {
        javascript_engine.start_execution_with_runtime(javascript_request, runtime.clone())
    }
    .map_err(map_javascript_error)
}

fn enforce_python_runner_module_mode(env: &mut BTreeMap<String, String>) {
    env.insert(
        String::from(NODE_GUEST_ENTRYPOINT_MODULE_MODE_ENV),
        String::from("1"),
    );
}

fn python_runner_javascript_limits(
    limits: &PythonExecutionLimits,
    max_old_space_mb: usize,
) -> JavascriptExecutionLimits {
    JavascriptExecutionLimits {
        v8_heap_limit_mb: (max_old_space_mb > 0).then_some(max_old_space_mb as u32),
        sync_rpc_wait_timeout_ms: Some(PYTHON_SYNC_RPC_WAIT_TIMEOUT_MS),
        reactor_work_quantum: limits.reactor_work_quantum,
        bridge_call_timeout_ms: limits.bridge_call_timeout_ms,
        ..JavascriptExecutionLimits::default()
    }
}

fn build_python_internal_env(
    import_cache: &NodeImportCache,
    context: &PythonContext,
    request: &StartPythonExecutionRequest,
    frozen_time_ms: u128,
    prewarm_only: bool,
) -> BTreeMap<String, String> {
    let mut internal_env = request
        .env
        .iter()
        .filter(|(key, _)| key.starts_with("AGENTOS_"))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<BTreeMap<_, _>>();
    let pyodide_dist_path = resolved_pyodide_dist_path(&context.pyodide_dist_path, &request.cwd);

    add_python_guest_path_mapping(&mut internal_env, &pyodide_dist_path);

    internal_env.insert(
        PYODIDE_INDEX_URL_ENV.to_string(),
        String::from(PYODIDE_GUEST_ROOT),
    );
    internal_env.insert(
        PYODIDE_PACKAGE_BASE_URL_ENV.to_string(),
        request
            .env
            .get(PYODIDE_PACKAGE_BASE_URL_ENV)
            .cloned()
            .unwrap_or_else(|| String::from(PYODIDE_GUEST_ROOT)),
    );
    internal_env.insert(
        PYODIDE_PACKAGE_CACHE_DIR_ENV.to_string(),
        String::from(PYODIDE_CACHE_GUEST_ROOT),
    );
    internal_env.insert(
        NODE_IMPORT_CACHE_ASSET_ROOT_ENV.to_string(),
        import_cache.asset_root().display().to_string(),
    );
    internal_env.insert(
        NODE_ALLOW_PROCESS_BINDINGS_ENV.to_string(),
        String::from("1"),
    );
    internal_env.insert(
        NODE_SYNC_RPC_DATA_BYTES_ENV.to_string(),
        PYTHON_SYNC_RPC_DATA_BYTES.to_string(),
    );
    internal_env.insert(
        NODE_DISABLE_COMPILE_CACHE_ENV.to_string(),
        String::from("1"),
    );
    // The runner's V8 heap cap and sync-RPC wait timeout are carried as typed
    // `JavascriptExecutionLimits` on the runner request (see the launch site),
    // not as `AGENTOS_V8_HEAP_LIMIT_MB` / `AGENTOS_NODE_SYNC_RPC_WAIT_TIMEOUT_MS`
    // env knobs, which the JS engine no longer reads.
    internal_env.insert(PYTHON_CODE_ENV.to_string(), request.code.clone());
    internal_env.insert(NODE_FROZEN_TIME_ENV.to_string(), frozen_time_ms.to_string());
    if prewarm_only {
        internal_env.insert(PYTHON_PREWARM_ONLY_ENV.to_string(), String::from("1"));
    } else {
        internal_env.insert(String::from("AGENTOS_KEEP_STDIN_OPEN"), String::from("1"));
        internal_env.insert(
            String::from("AGENTOS_FORWARD_KERNEL_STDIN_RPC"),
            String::from("1"),
        );
        internal_env.remove(PYTHON_PREWARM_ONLY_ENV);
    }
    if let Some(file_path) = &request.file_path {
        internal_env.insert(PYTHON_FILE_ENV.to_string(), file_path.display().to_string());
    } else {
        internal_env.remove(PYTHON_FILE_ENV);
    }

    internal_env
}

fn add_python_guest_path_mapping(
    internal_env: &mut BTreeMap<String, String>,
    pyodide_dist_path: &Path,
) {
    let pyodide_cache_path = pyodide_cache_path(pyodide_dist_path);
    let mut mappings = internal_env
        .get(NODE_GUEST_PATH_MAPPINGS_ENV)
        .and_then(|value| serde_json::from_str::<Vec<PythonGuestPathMappingWire>>(value).ok())
        .unwrap_or_default();

    mappings.retain(|mapping| {
        mapping.guest_path != PYODIDE_GUEST_ROOT && mapping.guest_path != PYODIDE_CACHE_GUEST_ROOT
    });
    mappings.push(PythonGuestPathMappingWire {
        guest_path: String::from(PYODIDE_GUEST_ROOT),
        host_path: pyodide_dist_path.display().to_string(),
    });
    mappings.push(PythonGuestPathMappingWire {
        guest_path: String::from(PYODIDE_CACHE_GUEST_ROOT),
        host_path: pyodide_cache_path.display().to_string(),
    });

    let serialized = serde_json::to_string(&mappings).unwrap_or_else(|_| String::from("[]"));
    internal_env.insert(String::from(NODE_GUEST_PATH_MAPPINGS_ENV), serialized);
}

fn pyodide_cache_path(pyodide_dist_path: &Path) -> PathBuf {
    let base = pyodide_dist_path
        .parent()
        .and_then(|parent| {
            if parent.file_name().is_some_and(|name| name == "assets") {
                parent.parent()
            } else {
                Some(parent)
            }
        })
        .unwrap_or(pyodide_dist_path);

    base.join("pyodide-package-cache")
}

fn build_python_runner_module_source(
    import_cache: &NodeImportCache,
    internal_env: &BTreeMap<String, String>,
    warmup_metrics: Option<&[u8]>,
) -> Result<String, PythonExecutionError> {
    let runner_source = fs::read_to_string(import_cache.python_runner_path())
        .map_err(PythonExecutionError::PrepareRuntime)?;
    let bootstrap = build_python_runner_bootstrap(internal_env, warmup_metrics);
    Ok(insert_python_runner_bootstrap(&runner_source, &bootstrap))
}

fn build_python_runner_bootstrap(
    internal_env: &BTreeMap<String, String>,
    warmup_metrics: Option<&[u8]>,
) -> String {
    let internal_env_json =
        serde_json::to_string(internal_env).unwrap_or_else(|_| String::from("{}"));
    let warmup_metrics_json = warmup_metrics.map(|bytes| {
        serde_json::to_string(&String::from_utf8_lossy(bytes).to_string())
            .unwrap_or_else(|_| String::from("\"\""))
    });

    match warmup_metrics_json {
        Some(warmup_metrics_json) => format!(
            "globalThis.__agentOSPythonInternalEnv = {internal_env_json};\n\
if (typeof process !== 'undefined') {{\n  process.env = {{ ...(process.env || {{}}), ...globalThis.__agentOSPythonInternalEnv }};\n}}\n\
if (typeof process?.stderr?.write === 'function') {{\n  process.stderr.write({warmup_metrics_json});\n}}\n"
        ),
        None => format!(
            "globalThis.__agentOSPythonInternalEnv = {internal_env_json};\n\
if (typeof process !== 'undefined') {{\n  process.env = {{ ...(process.env || {{}}), ...globalThis.__agentOSPythonInternalEnv }};\n}}\n"
        ),
    }
}

fn insert_python_runner_bootstrap(source: &str, bootstrap: &str) -> String {
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

fn parse_python_bridge_sync_rpc_request(
    request: &HostRpcRequest,
) -> Result<PythonVfsRpcRequest, PythonExecutionError> {
    if request.method != "_pythonRpc" {
        return Err(PythonExecutionError::RpcResponse(format!(
            "unexpected JavaScript sync RPC method for guest Python runtime: {}",
            request.method
        )));
    }

    let payload = request.args.first().ok_or_else(|| {
        PythonExecutionError::RpcResponse(String::from(
            "guest Python bridge call did not include a request payload",
        ))
    })?;

    let wire: PythonVfsBridgeRequestWire =
        serde_json::from_value(payload.clone()).map_err(|error| {
            PythonExecutionError::RpcResponse(format!(
                "invalid guest Python bridge request payload: {error}"
            ))
        })?;

    let method = PythonVfsRpcMethod::from_wire(&wire.method).ok_or_else(|| {
        PythonExecutionError::RpcResponse(format!(
            "unsupported agentos python rpc method {} for {}",
            wire.method, request.id
        ))
    })?;

    Ok(PythonVfsRpcRequest {
        id: request.id,
        method,
        path: wire.path,
        destination: wire.destination,
        target: wire.target,
        mode: wire.mode.map(|value| value as u32),
        uid: wire.uid.map(|value| value as u32),
        gid: wire.gid.map(|value| value as u32),
        atime_ms: wire.atime_ms.map(|value| value as u64),
        mtime_ms: wire.mtime_ms.map(|value| value as u64),
        content_base64: wire.content_base64,
        recursive: wire.recursive,
        url: wire.url,
        http_method: wire.http_method,
        headers: wire.headers,
        body_base64: wire.body_base64,
        hostname: wire.hostname,
        family: wire.family,
        port: wire.port,
        socket_id: wire.socket_id,
        command: wire.command,
        args: wire.args,
        argv0: wire.argv0,
        cwd: wire.cwd,
        env: wire.env,
        shell: wire.shell,
        max_buffer: wire.max_buffer,
        timeout_ms: wire.timeout_ms,
    })
}

#[derive(Debug)]
struct PythonOutputBuffer {
    bytes: Vec<u8>,
    max_bytes: usize,
}

impl PythonOutputBuffer {
    fn new(max_bytes: usize) -> Self {
        Self {
            bytes: Vec::new(),
            max_bytes,
        }
    }

    fn extend(&mut self, chunk: &[u8]) {
        if self.bytes.len() >= self.max_bytes {
            return;
        }

        let remaining = self.max_bytes - self.bytes.len();
        let take = remaining.min(chunk.len());
        self.bytes.extend_from_slice(&chunk[..take]);
    }

    fn into_inner(self) -> Vec<u8> {
        self.bytes
    }
}

fn python_output_buffer_max_bytes(request: &StartPythonExecutionRequest) -> usize {
    request
        .limits
        .output_buffer_max_bytes
        .unwrap_or(DEFAULT_PYTHON_OUTPUT_BUFFER_MAX_BYTES)
}

fn python_execution_timeout(request: &StartPythonExecutionRequest) -> Option<Duration> {
    match request.limits.execution_timeout_ms {
        // `Some(0)` explicitly disables the timeout.
        Some(0) => None,
        Some(value) => Some(Duration::from_millis(value)),
        None => Some(Duration::from_millis(DEFAULT_PYTHON_EXECUTION_TIMEOUT_MS)),
    }
}

fn python_max_old_space_mb(request: &StartPythonExecutionRequest) -> usize {
    request
        .limits
        .max_old_space_mb
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_PYTHON_MAX_OLD_SPACE_MB)
}

fn python_vfs_rpc_timeout(request: &StartPythonExecutionRequest) -> Duration {
    Duration::from_millis(
        request
            .limits
            .vfs_rpc_timeout_ms
            .filter(|value| *value > 0)
            .unwrap_or(DEFAULT_PYTHON_VFS_RPC_TIMEOUT_MS),
    )
}

fn spawn_python_vfs_rpc_timeout(
    runtime: &RuntimeContext,
    id: u64,
    timeout: Duration,
    pending: Arc<Mutex<PendingVfsRpcRegistry>>,
    javascript_responder: JavascriptSyncRpcResponder,
) -> Result<(), PythonExecutionError> {
    let cancellation = runtime.clone();
    let pending_for_task = Arc::clone(&pending);
    let handle = runtime
        .spawn(agentos_runtime_tokio::TaskClass::Timer, async move {
            tokio::select! {
                _ = tokio::time::sleep(timeout) => {}
                _ = cancellation.admission_closed() => {
                    let mut guard = pending_for_task.lock().unwrap_or_else(|poisoned| {
                        eprintln!(
                            "ERR_AGENTOS_PYTHON_VFS_RPC_STATE_POISONED: recovering request {id} during runtime shutdown"
                        );
                        poisoned.into_inner()
                    });
                    if guard.entries.get(&id).map(|rpc| rpc.state)
                        == Some(PendingVfsRpcState::Pending(id))
                    {
                        guard.entries.remove(&id);
                        guard.observe_depth();
                    }
                    return;
                }
            }

        let mut guard = pending_for_task.lock().unwrap_or_else(|poisoned| {
            eprintln!(
                "ERR_AGENTOS_PYTHON_VFS_RPC_STATE_POISONED: recovering request {id} while delivering its timeout"
            );
            poisoned.into_inner()
        });
        let should_timeout = if let Some(rpc) = guard.entries.get_mut(&id) {
            if rpc.state == PendingVfsRpcState::Pending(id) {
                rpc.state = PendingVfsRpcState::TimedOut(id);
                rpc.timeout_abort = None;
                true
            } else {
                false
            }
        } else {
            false
        };
        drop(guard);

        if !should_timeout {
            return;
        }

        if let Err(error) = javascript_responder.respond_error(
            id,
            "ERR_AGENTOS_PYTHON_VFS_RPC_TIMEOUT",
            format!(
                "guest Python VFS RPC request {id} timed out after {}ms",
                timeout.as_millis()
            ),
        ) {
            eprintln!(
                "ERR_AGENTOS_PYTHON_VFS_RPC_TIMEOUT_DELIVERY: could not deliver timeout for request {id}: {error}"
            );
        }
        })
        .map_err(|error| {
            PythonExecutionError::RpcResponse(format!(
                "could not arm Python VFS RPC timeout for request {id}: {error}"
            ))
        })?;

    let timeout_abort = handle.abort_handle();
    let mut guard = pending
        .lock()
        .map_err(|_| PythonExecutionError::EventChannelClosed)?;
    if let Some(rpc) = guard.entries.get_mut(&id) {
        if rpc.state == PendingVfsRpcState::Pending(id) {
            rpc.timeout_abort = Some(timeout_abort);
            return Ok(());
        }
    }
    timeout_abort.abort();
    Ok(())
}

fn resolved_pyodide_dist_path(path: &Path, cwd: &Path) -> PathBuf {
    resolve_execution_path(path, cwd)
}

struct PythonPrewarmOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    sync_rpc_log: Vec<String>,
    managed_host_files: PythonManagedHostFiles,
}

impl PythonPrewarmOutput {
    fn new(max_open_fds: usize) -> Self {
        Self {
            stdout: Vec::new(),
            stderr: Vec::new(),
            sync_rpc_log: Vec::new(),
            managed_host_files: PythonManagedHostFiles::new(max_open_fds),
        }
    }
}

fn handle_python_prewarm_event(
    prewarm_execution: &mut JavascriptExecution,
    context: &PythonContext,
    request: &StartPythonExecutionRequest,
    event: Option<JavascriptExecutionEvent>,
    output: &mut PythonPrewarmOutput,
) -> Result<Option<PythonExecutionResult>, PythonExecutionError> {
    match event {
        Some(JavascriptExecutionEvent::Stdout(chunk)) => output.stdout.extend(chunk),
        Some(JavascriptExecutionEvent::Stderr(chunk)) => output.stderr.extend(chunk),
        Some(JavascriptExecutionEvent::Exited(exit_code)) => {
            return Ok(Some(PythonExecutionResult {
                execution_id: String::from("python-prewarm"),
                exit_code,
                stdout: std::mem::take(&mut output.stdout),
                stderr: std::mem::take(&mut output.stderr),
            }));
        }
        Some(JavascriptExecutionEvent::SignalState { .. }) => {}
        Some(JavascriptExecutionEvent::SyncRpcRequest(sync_request)) => {
            output.sync_rpc_log.push(format!(
                "{} {} {:?}",
                sync_request.id, sync_request.method, sync_request.args
            ));
            // The Python runner module imports node builtins and the pyodide
            // ESM entry; module-resolution sync RPCs are serviced host-directly
            // because this prewarm has no kernel/service-loop consumer.
            if prewarm_execution
                .try_service_standalone_module_sync_rpc(&sync_request)
                .map_err(map_javascript_error)?
            {
                output
                    .sync_rpc_log
                    .push(format!("responded {} (module)", sync_request.id));
                return Ok(None);
            }
            let pyodide_dist_path =
                resolved_pyodide_dist_path(&context.pyodide_dist_path, &request.cwd);
            if let Some(action) = python_javascript_sync_rpc_action(
                &pyodide_dist_path,
                &mut output.managed_host_files,
                &sync_request,
            )? {
                respond_python_javascript_sync_rpc_action(
                    prewarm_execution,
                    sync_request.id,
                    action,
                )?;
                output
                    .sync_rpc_log
                    .push(format!("responded {}", sync_request.id));
                return Ok(None);
            }
            if let Some((code, message)) = python_javascript_sync_rpc_error(&sync_request) {
                prewarm_execution
                    .respond_sync_rpc_error(sync_request.id, code, message)
                    .map_err(map_javascript_error)?;
                output
                    .sync_rpc_log
                    .push(format!("errored {}", sync_request.id));
                return Ok(None);
            }
            if sync_request.method == "_pythonRpc" {
                let request = parse_python_bridge_sync_rpc_request(&sync_request)?;
                return Err(PythonExecutionError::WarmupFailed {
                    exit_code: 1,
                    stderr: format!(
                        "unexpected Python prewarm VFS RPC request {} {} {:?}",
                        request.id, request.path, request.method
                    ),
                });
            }
            return Err(PythonExecutionError::WarmupFailed {
                exit_code: 1,
                stderr: format!(
                    "unexpected Python prewarm JavaScript sync RPC request {} {} {:?}",
                    sync_request.id, sync_request.method, sync_request.args
                ),
            });
        }
        None => {
            return Err(PythonExecutionError::WarmupFailed {
                exit_code: 1,
                stderr: format!(
                    "python prewarm timed out after {}s\nstdout:\n{}\nstderr:\n{}\nsync rpc:\n{}",
                    PYTHON_PREWARM_TIMEOUT.as_secs(),
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr),
                    output.sync_rpc_log.join("\n"),
                ),
            });
        }
    }
    Ok(None)
}

fn prewarm_python_path(
    import_cache: &NodeImportCache,
    javascript_engine: &mut JavascriptExecutionEngine,
    javascript_context_id: &str,
    context: &PythonContext,
    request: &StartPythonExecutionRequest,
    frozen_time_ms: u128,
    runtime: &RuntimeContext,
) -> Result<Option<Vec<u8>>, PythonExecutionError> {
    let debug_enabled = python_warmup_metrics_enabled(request);
    let marker_contents = warmup_marker_contents(import_cache, context, request);
    let marker_path = warmup_marker_path(
        import_cache.prewarm_marker_dir(),
        "python-runner-prewarm",
        PYTHON_WARMUP_MARKER_VERSION,
        &marker_contents,
    );
    let marker_exists = marker_path.exists();

    let started = Instant::now();
    let mut prewarm_execution = start_python_javascript_execution(
        javascript_engine,
        runtime,
        import_cache,
        javascript_context_id,
        context,
        request,
        PythonJavascriptExecutionOptions {
            frozen_time_ms,
            prewarm_only: true,
            warmup_metrics: None,
            defer_execute: false,
        },
    )?;
    let mut output = PythonPrewarmOutput::new(python_managed_host_file_limit(request));
    let result = loop {
        let event = prewarm_execution
            .poll_event_blocking(PYTHON_PREWARM_TIMEOUT)
            .map_err(map_javascript_error)?;
        if let Some(result) = handle_python_prewarm_event(
            &mut prewarm_execution,
            context,
            request,
            event,
            &mut output,
        )? {
            break result;
        }
    };
    let duration_ms = started.elapsed().as_secs_f64() * 1000.0;

    if result.exit_code != 0 {
        return Err(PythonExecutionError::WarmupFailed {
            exit_code: result.exit_code,
            stderr: String::from_utf8_lossy(&result.stderr).into_owned(),
        });
    }

    if marker_exists {
        return Ok(warmup_metrics_line(
            debug_enabled,
            false,
            "cached",
            0.0,
            import_cache,
            context,
            request,
        ));
    }

    fs::write(&marker_path, marker_contents).map_err(PythonExecutionError::PrepareWarmPath)?;
    Ok(warmup_metrics_line(
        debug_enabled,
        true,
        "executed",
        duration_ms,
        import_cache,
        context,
        request,
    ))
}

async fn prewarm_python_path_async(
    import_cache: &NodeImportCache,
    javascript_engine: &mut JavascriptExecutionEngine,
    javascript_context_id: &str,
    context: &PythonContext,
    request: &StartPythonExecutionRequest,
    frozen_time_ms: u128,
    runtime: &RuntimeContext,
) -> Result<Option<Vec<u8>>, PythonExecutionError> {
    let debug_enabled = python_warmup_metrics_enabled(request);
    let marker_contents = warmup_marker_contents(import_cache, context, request);
    let marker_path = warmup_marker_path(
        import_cache.prewarm_marker_dir(),
        "python-runner-prewarm",
        PYTHON_WARMUP_MARKER_VERSION,
        &marker_contents,
    );
    let marker_exists = marker_path.exists();

    let started = Instant::now();
    let mut prewarm_execution = start_python_javascript_execution(
        javascript_engine,
        runtime,
        import_cache,
        javascript_context_id,
        context,
        request,
        PythonJavascriptExecutionOptions {
            frozen_time_ms,
            prewarm_only: true,
            warmup_metrics: None,
            defer_execute: false,
        },
    )?;
    let mut output = PythonPrewarmOutput::new(python_managed_host_file_limit(request));
    let result = loop {
        let event = prewarm_execution
            .poll_event(PYTHON_PREWARM_TIMEOUT)
            .await
            .map_err(map_javascript_error)?;
        if let Some(result) = handle_python_prewarm_event(
            &mut prewarm_execution,
            context,
            request,
            event,
            &mut output,
        )? {
            break result;
        }
    };
    let duration_ms = started.elapsed().as_secs_f64() * 1000.0;

    if result.exit_code != 0 {
        return Err(PythonExecutionError::WarmupFailed {
            exit_code: result.exit_code,
            stderr: String::from_utf8_lossy(&result.stderr).into_owned(),
        });
    }

    if marker_exists {
        return Ok(warmup_metrics_line(
            debug_enabled,
            false,
            "cached",
            0.0,
            import_cache,
            context,
            request,
        ));
    }

    fs::write(&marker_path, marker_contents).map_err(PythonExecutionError::PrepareWarmPath)?;
    Ok(warmup_metrics_line(
        debug_enabled,
        true,
        "executed",
        duration_ms,
        import_cache,
        context,
        request,
    ))
}

#[derive(Debug)]
enum PythonJavascriptSyncRpcAction {
    Success(Value),
    RawSuccess(Vec<u8>),
    Error { code: &'static str, message: String },
}

#[derive(Debug)]
struct PythonManagedHostFiles {
    next_fd: u64,
    max_files: usize,
    files: BTreeMap<u64, fs::File>,
}

// Standalone execution callers do not have a VM kernel from which to source
// policy. Keep their fallback bounded; production sidecar launches always pass
// `vm.kernel.resource_limits().max_open_fds` in `PythonExecutionLimits`.
const DEFAULT_PYTHON_MANAGED_HOST_FILE_LIMIT: usize = 256;

impl Default for PythonManagedHostFiles {
    fn default() -> Self {
        Self::new(DEFAULT_PYTHON_MANAGED_HOST_FILE_LIMIT)
    }
}

impl PythonManagedHostFiles {
    fn new(max_files: usize) -> Self {
        Self {
            // Keep host-direct descriptors disjoint from ordinary guest and
            // stdio descriptors. They never cross the JavaScript execution.
            next_fd: 0x4000_0000,
            max_files,
            files: BTreeMap::new(),
        }
    }
}

fn python_managed_host_file_limit(request: &StartPythonExecutionRequest) -> usize {
    request
        .limits
        .max_open_fds
        .unwrap_or(DEFAULT_PYTHON_MANAGED_HOST_FILE_LIMIT)
}

fn python_javascript_sync_rpc_action(
    pyodide_dist_path: &Path,
    managed_host_files: &mut PythonManagedHostFiles,
    request: &HostRpcRequest,
) -> Result<Option<PythonJavascriptSyncRpcAction>, PythonExecutionError> {
    if matches!(request.method.as_str(), "fs.readSync" | "_fsReadRaw") {
        let Some(fd) = request.args.first().and_then(Value::as_u64) else {
            return Ok(None);
        };
        let Some(file) = managed_host_files.files.get_mut(&fd) else {
            return Ok(None);
        };
        let length = request
            .args
            .get(1)
            .and_then(Value::as_u64)
            .and_then(|length| usize::try_from(length).ok())
            .ok_or_else(|| {
                PythonExecutionError::RpcResponse(String::from(
                    "managed fs.readSync length must fit within usize",
                ))
            })?;
        if length > PYTHON_SYNC_RPC_DATA_BYTES {
            return Err(PythonExecutionError::RpcResponse(format!(
                "managed fs.readSync length {length} exceeds {PYTHON_SYNC_RPC_DATA_BYTES} bytes"
            )));
        }
        if let Some(position) = request.args.get(2).and_then(Value::as_u64) {
            file.seek(SeekFrom::Start(position))
                .map_err(PythonExecutionError::PrepareRuntime)?;
        }
        let mut bytes = vec![0; length];
        let bytes_read = file
            .read(&mut bytes)
            .map_err(PythonExecutionError::PrepareRuntime)?;
        bytes.truncate(bytes_read);
        return Ok(Some(if request.raw_bytes_args.contains_key(&usize::MAX) {
            PythonJavascriptSyncRpcAction::RawSuccess(bytes)
        } else {
            PythonJavascriptSyncRpcAction::Success(Value::String(v8_runtime::base64_encode_pub(
                &bytes,
            )))
        }));
    }

    if request.method == "fs.closeSync" {
        let Some(fd) = request.args.first().and_then(Value::as_u64) else {
            return Ok(None);
        };
        if managed_host_files.files.remove(&fd).is_none() {
            return Ok(None);
        }
        return Ok(Some(PythonJavascriptSyncRpcAction::Success(Value::Null)));
    }

    if request.method == "fs.fstatSync" {
        let Some(fd) = request.args.first().and_then(Value::as_u64) else {
            return Ok(None);
        };
        let Some(file) = managed_host_files.files.get(&fd) else {
            return Ok(None);
        };
        let metadata = file
            .metadata()
            .map_err(PythonExecutionError::PrepareRuntime)?;
        return Ok(Some(PythonJavascriptSyncRpcAction::Success(
            python_host_stat_value(&metadata),
        )));
    }

    let Some(path) = request.args.first().and_then(Value::as_str) else {
        return Ok(None);
    };
    let path_kind = python_managed_path_kind(pyodide_dist_path, path);
    let Some(host_path) = path_kind.host_path() else {
        return Ok(None);
    };

    Ok(Some(match request.method.as_str() {
        "fs.openSync" => {
            let flags = request.args.get(1).unwrap_or(&Value::Null);
            let read_only = matches!(flags.as_str(), Some("r"))
                || flags.as_u64().is_some_and(|flags| flags == 0);
            if !read_only {
                PythonJavascriptSyncRpcAction::Error {
                    code: "EROFS",
                    message: format!(
                        "EROFS: managed Python runtime assets are read-only, open '{path}'"
                    ),
                }
            } else if managed_host_files.files.len() >= managed_host_files.max_files {
                PythonJavascriptSyncRpcAction::Error {
                    code: "EMFILE",
                    message: format!(
                        "EMFILE: managed Python host descriptor limit {} reached (limits.resources.maxOpenFds); raise limits.resources.maxOpenFds",
                        managed_host_files.max_files
                    ),
                }
            } else {
                match fs::File::open(&host_path) {
                    Ok(file) => {
                        let fd = managed_host_files.next_fd;
                        managed_host_files.next_fd =
                            managed_host_files.next_fd.checked_add(1).ok_or_else(|| {
                                PythonExecutionError::RpcResponse(String::from(
                                    "managed Python host descriptor ids exhausted",
                                ))
                            })?;
                        managed_host_files.files.insert(fd, file);
                        PythonJavascriptSyncRpcAction::Success(json!(fd))
                    }
                    Err(error) => {
                        return python_sync_rpc_fs_action_error(path, "open", error).map(Some);
                    }
                }
            }
        }
        "fs.promises.readFile" | "fs.readFileSync" => {
            let bytes = match fs::read(&host_path) {
                Ok(bytes) => bytes,
                Err(error) => {
                    return python_sync_rpc_fs_action_error(path, "open", error).map(Some);
                }
            };
            let encoding = python_prewarm_sync_rpc_encoding(&request.args);
            match encoding.as_deref() {
                Some("utf8") | Some("utf-8") => PythonJavascriptSyncRpcAction::Success(
                    Value::String(String::from_utf8_lossy(&bytes).into_owned()),
                ),
                _ => PythonJavascriptSyncRpcAction::Success(json!({
                    "__agentOSType": "bytes",
                    "base64": v8_runtime::base64_encode_pub(&bytes),
                })),
            }
        }
        "fs.statSync" | "fs.promises.stat" => match fs::metadata(&host_path) {
            Ok(metadata) => {
                PythonJavascriptSyncRpcAction::Success(python_host_stat_value(&metadata))
            }
            Err(error) => return python_sync_rpc_fs_action_error(path, "stat", error).map(Some),
        },
        "fs.lstatSync" | "fs.promises.lstat" => match fs::symlink_metadata(&host_path) {
            Ok(metadata) => {
                PythonJavascriptSyncRpcAction::Success(python_host_stat_value(&metadata))
            }
            Err(error) => return python_sync_rpc_fs_action_error(path, "lstat", error).map(Some),
        },
        "fs.existsSync" => PythonJavascriptSyncRpcAction::Success(Value::Bool(host_path.exists())),
        "fs.accessSync" | "fs.promises.access" => match fs::metadata(&host_path) {
            Ok(_) => PythonJavascriptSyncRpcAction::Success(Value::Null),
            Err(error) => return python_sync_rpc_fs_action_error(path, "access", error).map(Some),
        },
        "fs.readdirSync" | "fs.promises.readdir" => match fs::read_dir(&host_path) {
            Ok(entries) => PythonJavascriptSyncRpcAction::Success(python_readdir_value(
                entries
                    .filter_map(|entry| entry.ok())
                    .filter_map(|entry| entry.file_name().into_string().ok())
                    .collect(),
            )),
            Err(error) => return python_sync_rpc_fs_action_error(path, "scandir", error).map(Some),
        },
        "fs.mkdirSync" | "fs.promises.mkdir" => {
            let recursive = python_sync_rpc_recursive_flag(&request.args);
            if recursive {
                fs::create_dir_all(&host_path).map_err(PythonExecutionError::PrepareRuntime)?;
            } else {
                match fs::create_dir(&host_path) {
                    Ok(()) => {}
                    Err(error) => {
                        return python_sync_rpc_fs_action_error(path, "mkdir", error).map(Some);
                    }
                }
            }
            PythonJavascriptSyncRpcAction::Success(Value::Null)
        }
        "fs.writeFileSync" | "fs.promises.writeFile" => {
            let contents = python_sync_rpc_bytes_arg(&request.args, 1)?;
            if let Some(parent) = host_path.parent() {
                fs::create_dir_all(parent).map_err(PythonExecutionError::PrepareRuntime)?;
            }
            fs::write(&host_path, contents).map_err(PythonExecutionError::PrepareRuntime)?;
            PythonJavascriptSyncRpcAction::Success(Value::Null)
        }
        "fs.realpathSync" | "fs.realpathSync.native" => match fs::canonicalize(&host_path) {
            Ok(canonical) => PythonJavascriptSyncRpcAction::Success(Value::String(
                path_kind.render_path(pyodide_dist_path, &canonical, path),
            )),
            Err(error) => {
                return python_sync_rpc_fs_action_error(path, "realpath", error).map(Some);
            }
        },
        _ => return Ok(None),
    }))
}

fn python_sync_rpc_fs_action_error(
    path: &str,
    syscall: &str,
    error: std::io::Error,
) -> Result<PythonJavascriptSyncRpcAction, PythonExecutionError> {
    let action = match error.kind() {
        std::io::ErrorKind::NotFound => PythonJavascriptSyncRpcAction::Error {
            code: "ENOENT",
            message: format!("ENOENT: no such file or directory, {syscall} '{path}'"),
        },
        std::io::ErrorKind::AlreadyExists => PythonJavascriptSyncRpcAction::Error {
            code: "EEXIST",
            message: format!("EEXIST: file already exists, {syscall} '{path}'"),
        },
        std::io::ErrorKind::PermissionDenied => PythonJavascriptSyncRpcAction::Error {
            code: "EACCES",
            message: format!("EACCES: permission denied, {syscall} '{path}'"),
        },
        _ => {
            return Err(PythonExecutionError::PrepareRuntime(std::io::Error::new(
                error.kind(),
                error.to_string(),
            )));
        }
    };
    Ok(action)
}

fn respond_python_javascript_sync_rpc_action(
    execution: &mut JavascriptExecution,
    id: u64,
    action: PythonJavascriptSyncRpcAction,
) -> Result<(), PythonExecutionError> {
    match action {
        PythonJavascriptSyncRpcAction::Success(value) => execution
            .respond_sync_rpc_success(id, value)
            .map_err(map_javascript_error),
        PythonJavascriptSyncRpcAction::RawSuccess(bytes) => execution
            .respond_sync_rpc_raw_success(id, bytes)
            .map_err(map_javascript_error),
        PythonJavascriptSyncRpcAction::Error { code, message } => execution
            .respond_sync_rpc_error(id, code, message)
            .map_err(map_javascript_error),
    }
}

#[derive(Debug, Clone)]
enum PythonManagedPathKind {
    GuestPyodide,
    GuestCache,
    HostManaged,
    Unmanaged,
}

impl PythonManagedPathKind {
    fn render_path(&self, pyodide_dist_path: &Path, canonical: &Path, original: &str) -> String {
        match self {
            Self::GuestPyodide | Self::GuestCache => {
                python_host_path_to_guest(pyodide_dist_path, canonical)
                    .unwrap_or_else(|| original.to_owned())
            }
            Self::HostManaged => canonical.display().to_string(),
            Self::Unmanaged => original.to_owned(),
        }
    }
}

fn python_managed_path_kind(pyodide_dist_path: &Path, path: &str) -> PythonManagedResolvedPath {
    let cache_path = pyodide_cache_path(pyodide_dist_path);

    if let Some(normalized) = strip_guest_managed_root(path, PYODIDE_GUEST_ROOT) {
        let root = canonicalize_existing_or_self(pyodide_dist_path);
        let relative = normalize_relative_guest_suffix(normalized);
        let host_path = if relative.as_os_str().is_empty() {
            root.clone()
        } else {
            root.join(relative)
        };
        if confined_managed_path(&host_path, &root) {
            return PythonManagedResolvedPath {
                kind: PythonManagedPathKind::GuestPyodide,
                host_path: Some(host_path),
            };
        }
        return PythonManagedResolvedPath {
            kind: PythonManagedPathKind::Unmanaged,
            host_path: None,
        };
    }

    if let Some(normalized) = strip_guest_managed_root(path, PYODIDE_CACHE_GUEST_ROOT) {
        let root = canonicalize_existing_or_self(&cache_path);
        let relative = normalize_relative_guest_suffix(normalized);
        let host_path = if relative.as_os_str().is_empty() {
            root.clone()
        } else {
            root.join(relative)
        };
        if confined_managed_path(&host_path, &root) {
            return PythonManagedResolvedPath {
                kind: PythonManagedPathKind::GuestCache,
                host_path: Some(host_path),
            };
        }
        return PythonManagedResolvedPath {
            kind: PythonManagedPathKind::Unmanaged,
            host_path: None,
        };
    }

    let candidate = PathBuf::from(path);
    let pyodide_root = canonicalize_existing_or_self(pyodide_dist_path);
    let cache_root = canonicalize_existing_or_self(&cache_path);
    if candidate.is_absolute()
        && !path_has_parent_or_prefix_component(&candidate)
        && confined_managed_path(&candidate, &pyodide_root)
    {
        return PythonManagedResolvedPath {
            kind: PythonManagedPathKind::HostManaged,
            host_path: Some(candidate),
        };
    }
    if candidate.is_absolute()
        && !path_has_parent_or_prefix_component(&candidate)
        && confined_managed_path(&candidate, &cache_root)
    {
        return PythonManagedResolvedPath {
            kind: PythonManagedPathKind::HostManaged,
            host_path: Some(candidate),
        };
    }

    PythonManagedResolvedPath {
        kind: PythonManagedPathKind::Unmanaged,
        host_path: None,
    }
}

#[derive(Debug, Clone)]
struct PythonManagedResolvedPath {
    kind: PythonManagedPathKind,
    host_path: Option<PathBuf>,
}

impl PythonManagedResolvedPath {
    fn host_path(&self) -> Option<PathBuf> {
        self.host_path.clone()
    }

    fn render_path(&self, pyodide_dist_path: &Path, canonical: &Path, original: &str) -> String {
        self.kind
            .render_path(pyodide_dist_path, canonical, original)
    }
}

fn strip_guest_managed_root<'a>(path: &'a str, root: &str) -> Option<&'a str> {
    if path == root {
        return Some("");
    }
    path.strip_prefix(root)?.strip_prefix('/')
}

fn normalize_relative_guest_suffix(suffix: &str) -> PathBuf {
    let mut normalized = PathBuf::new();
    for segment in suffix.trim_start_matches('/').split('/') {
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment == ".." {
            normalized.pop();
        } else {
            normalized.push(segment);
        }
    }
    normalized
}

fn path_has_parent_or_prefix_component(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
}

fn canonicalize_existing_or_self(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn confined_managed_path(path: &Path, root: &Path) -> bool {
    let canonical_root = canonicalize_existing_or_self(root);
    let Some(canonical_path) = canonicalize_managed_candidate(path) else {
        return false;
    };

    canonical_path == canonical_root || canonical_path.starts_with(canonical_root)
}

fn canonicalize_managed_candidate(path: &Path) -> Option<PathBuf> {
    let mut missing_components = Vec::new();
    let mut current = path;
    loop {
        match fs::canonicalize(current) {
            Ok(mut canonical) => {
                for component in missing_components.iter().rev() {
                    canonical.push(component);
                }
                return Some(canonical);
            }
            Err(_) => {
                let file_name = current.file_name()?.to_owned();
                if Path::new(&file_name)
                    .components()
                    .any(|component| !matches!(component, Component::Normal(_)))
                {
                    return None;
                }
                missing_components.push(file_name);
                current = current.parent()?;
            }
        }
    }
}

fn python_host_path_to_guest(pyodide_dist_path: &Path, host_path: &Path) -> Option<String> {
    if let Ok(relative) = host_path.strip_prefix(pyodide_dist_path) {
        let suffix = relative.to_string_lossy().replace('\\', "/");
        return Some(if suffix.is_empty() {
            String::from(PYODIDE_GUEST_ROOT)
        } else {
            format!("{PYODIDE_GUEST_ROOT}/{suffix}")
        });
    }

    let cache_path = pyodide_cache_path(pyodide_dist_path);
    let relative = host_path.strip_prefix(cache_path).ok()?;
    let suffix = relative.to_string_lossy().replace('\\', "/");
    Some(if suffix.is_empty() {
        String::from(PYODIDE_CACHE_GUEST_ROOT)
    } else {
        format!("{PYODIDE_CACHE_GUEST_ROOT}/{suffix}")
    })
}

fn python_host_stat_value(metadata: &fs::Metadata) -> Value {
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

fn python_readdir_value(entries: Vec<String>) -> Value {
    json!(entries
        .into_iter()
        .filter(|entry| entry != "." && entry != "..")
        .collect::<Vec<_>>())
}

fn python_sync_rpc_recursive_flag(args: &[Value]) -> bool {
    args.get(1)
        .and_then(|value| {
            value
                .as_bool()
                .or_else(|| value.get("recursive").and_then(Value::as_bool))
        })
        .unwrap_or(false)
}

fn python_sync_rpc_bytes_arg(
    args: &[Value],
    index: usize,
) -> Result<Vec<u8>, PythonExecutionError> {
    let Some(value) = args.get(index) else {
        return Err(PythonExecutionError::RpcResponse(format!(
            "sync RPC argument {index} is required"
        )));
    };

    if let Some(text) = value.as_str() {
        return Ok(text.as_bytes().to_vec());
    }

    let Some(base64_value) = value
        .get("__agentOSType")
        .and_then(Value::as_str)
        .filter(|kind| *kind == "bytes")
        .and_then(|_| value.get("base64"))
        .and_then(Value::as_str)
    else {
        return Err(PythonExecutionError::RpcResponse(format!(
            "sync RPC argument {index} must be a string or encoded bytes payload"
        )));
    };

    base64::engine::general_purpose::STANDARD
        .decode(base64_value)
        .map_err(|error| {
            PythonExecutionError::RpcResponse(format!(
                "sync RPC argument {index} contains invalid base64: {error}"
            ))
        })
}

fn python_prewarm_sync_rpc_encoding(args: &[Value]) -> Option<String> {
    args.get(1).and_then(|value| {
        value.as_str().map(str::to_owned).or_else(|| {
            value
                .get("encoding")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
    })
}

fn python_javascript_sync_rpc_error(request: &HostRpcRequest) -> Option<(&'static str, String)> {
    if matches!(
        request.method.as_str(),
        "net.connect"
            | "net.createConnection"
            | "dns.lookup"
            | "dns.resolve"
            | "dns.resolve4"
            | "dns.resolve6"
            | "dns.reverse"
            | "dgram.send"
            | "http.request"
            | "https.request"
            | "tls.connect"
    ) {
        return Some((
            "ERR_ACCESS_DENIED",
            String::from(
                "network access is not available during standalone guest Python execution",
            ),
        ));
    }

    None
}

fn python_vfs_rpc_standalone_error(method: PythonVfsRpcMethod) -> Option<(&'static str, String)> {
    if matches!(
        method,
        PythonVfsRpcMethod::HttpRequest
            | PythonVfsRpcMethod::DnsLookup
            | PythonVfsRpcMethod::SocketConnect
            | PythonVfsRpcMethod::SocketSend
            | PythonVfsRpcMethod::SocketRecv
            | PythonVfsRpcMethod::SocketClose
            | PythonVfsRpcMethod::UdpCreate
            | PythonVfsRpcMethod::UdpSendto
            | PythonVfsRpcMethod::UdpRecvfrom
    ) {
        return Some((
            "ERR_ACCESS_DENIED",
            String::from(
                "network access is not available during standalone guest Python execution",
            ),
        ));
    }

    None
}

fn warmup_marker_contents(
    import_cache: &NodeImportCache,
    context: &PythonContext,
    request: &StartPythonExecutionRequest,
) -> String {
    let pyodide_dist_path = resolved_pyodide_dist_path(&context.pyodide_dist_path, &request.cwd);
    let compile_cache_dir = import_cache.shared_compile_cache_dir();

    [
        env!("CARGO_PKG_NAME").to_string(),
        env!("CARGO_PKG_VERSION").to_string(),
        PYTHON_WARMUP_MARKER_VERSION.to_string(),
        String::from("agentos-v8"),
        python_max_old_space_mb(request).to_string(),
        compile_cache_dir.display().to_string(),
        pyodide_dist_path.display().to_string(),
        file_fingerprint(&pyodide_dist_path.join("pyodide.mjs")),
        file_fingerprint(&pyodide_dist_path.join("pyodide-lock.json")),
        file_fingerprint(&pyodide_dist_path.join("pyodide.asm.js")),
        file_fingerprint(&pyodide_dist_path.join("pyodide.asm.wasm")),
        file_fingerprint(&pyodide_dist_path.join("python_stdlib.zip")),
    ]
    .join("\n")
}

fn python_warmup_metrics_enabled(request: &StartPythonExecutionRequest) -> bool {
    env_flag_enabled(&request.env, PYTHON_WARMUP_DEBUG_ENV)
}

fn warmup_metrics_line(
    debug_enabled: bool,
    executed: bool,
    reason: &str,
    duration_ms: f64,
    import_cache: &NodeImportCache,
    context: &PythonContext,
    request: &StartPythonExecutionRequest,
) -> Option<Vec<u8>> {
    if !debug_enabled {
        return None;
    }

    let compile_cache_dir = import_cache.shared_compile_cache_dir();
    let pyodide_dist_path = resolved_pyodide_dist_path(&context.pyodide_dist_path, &request.cwd);

    Some(
        format!(
            "{PYTHON_WARMUP_METRICS_PREFIX}{{\"phase\":\"prewarm\",\"executed\":{},\"reason\":{},\"durationMs\":{duration_ms:.3},\"heapLimitMb\":{},\"compileCacheDir\":{},\"pyodideDistPath\":{}}}\n",
            if executed { "true" } else { "false" },
            encode_json_string(reason),
            python_max_old_space_mb(request),
            encode_json_string(&compile_cache_dir.display().to_string()),
            encode_json_string(&pyodide_dist_path.display().to_string()),
        )
        .into_bytes(),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        cancel_pending_vfs_rpc, clear_pending_vfs_rpc, enforce_python_runner_module_mode,
        python_javascript_sync_rpc_action, python_managed_path_kind,
        python_runner_javascript_limits, python_vfs_rpc_admission_error, python_wait_remaining,
        set_pending_vfs_rpc_state, CreatePythonContextRequest, HostRpcRequest, PendingVfsRpc,
        PendingVfsRpcRegistry, PendingVfsRpcResolution, PendingVfsRpcState, PythonExecutionEngine,
        PythonExecutionError, PythonExecutionLimits, PythonHostReplyKind,
        PythonJavascriptSyncRpcAction, PythonManagedHostFiles, PythonManagedPathKind,
        NODE_GUEST_ENTRYPOINT_MODULE_MODE_ENV, PYODIDE_CACHE_GUEST_ROOT, PYODIDE_GUEST_ROOT,
    };
    use crate::backend::HostCallReply;
    use std::collections::{BTreeMap, HashMap};
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::symlink;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};
    use tempfile::tempdir;

    #[test]
    fn python_runner_forwards_vm_reactor_limits_to_javascript() {
        let limits = PythonExecutionLimits {
            reactor_work_quantum: Some(23),
            bridge_call_timeout_ms: Some(54_321),
            ..PythonExecutionLimits::default()
        };
        let javascript = python_runner_javascript_limits(&limits, 256);

        assert_eq!(javascript.v8_heap_limit_mb, Some(256));
        assert_eq!(javascript.reactor_work_quantum, Some(23));
        assert_eq!(javascript.bridge_call_timeout_ms, Some(54_321));
    }

    #[test]
    fn python_runner_module_mode_overrides_inherited_commonjs_selector() {
        let mut env = BTreeMap::from([(
            String::from(NODE_GUEST_ENTRYPOINT_MODULE_MODE_ENV),
            String::from("0"),
        )]);

        enforce_python_runner_module_mode(&mut env);

        assert_eq!(
            env.get(NODE_GUEST_ENTRYPOINT_MODULE_MODE_ENV)
                .map(String::as_str),
            Some("1")
        );
    }

    #[test]
    fn common_host_replies_keep_the_python_wire_shape() {
        let network = Arc::new(Mutex::new(super::PythonManagedNetworkState::new(8)));
        assert_eq!(
            super::map_python_host_reply(
                &network,
                PythonHostReplyKind::RunCaptured,
                HostCallReply::Json(serde_json::json!({
                    "pid": 17,
                    "code": 3,
                    "stdout": "out",
                    "stderr": "err",
                    "maxBufferExceeded": true,
                })),
            )
            .expect("map captured process reply"),
            serde_json::json!({
                "exitCode": 3,
                "stdout": "out",
                "stderr": "err",
                "maxBufferExceeded": true,
            })
        );
        assert_eq!(
            super::map_python_host_reply(
                &network,
                PythonHostReplyKind::FileRead,
                HostCallReply::Raw(b"shared-kernel".to_vec()),
            )
            .expect("map file read reply"),
            serde_json::json!({ "contentBase64": "c2hhcmVkLWtlcm5lbA==" })
        );
    }

    #[test]
    fn canonical_udp_host_reply_maps_to_python_socket_shape() {
        let network = Arc::new(Mutex::new(super::PythonManagedNetworkState::new(8)));
        let reply = super::map_python_host_reply(
            &network,
            PythonHostReplyKind::UdpReceived,
            HostCallReply::Json(serde_json::json!({
                "type": "message",
                "data": {
                    "__agentOSType": "bytes",
                    "base64": "cGluZyB1ZHA=",
                },
                "remoteAddress": "127.0.0.1",
                "remotePort": 43123,
                "remoteFamily": "IPv4",
            })),
        )
        .expect("map canonical UDP reply");

        assert_eq!(
            reply,
            serde_json::json!({
                "dataBase64": "cGluZyB1ZHA=",
                "host": "127.0.0.1",
                "port": 43123,
                "timedOut": false,
            })
        );
    }

    #[test]
    fn mismatched_common_host_reply_is_a_typed_protocol_error() {
        let network = Arc::new(Mutex::new(super::PythonManagedNetworkState::new(8)));
        let error = super::map_python_host_reply(
            &network,
            PythonHostReplyKind::ReadDirectory,
            HostCallReply::Empty,
        )
        .expect_err("reject mismatched host reply");
        assert_eq!(error.code, "EPROTO");
        assert_eq!(
            error.details.expect("protocol error details")["replyKind"],
            "ReadDirectory"
        );
    }

    #[test]
    fn managed_socket_reservations_admit_exactly_the_configured_cap() {
        let state = Arc::new(Mutex::new(super::PythonManagedNetworkState::new(1)));
        let reservation =
            super::PythonManagedNetworkState::reserve(&state, super::PythonManagedSocketKind::Tcp)
                .expect("exact cap reservation");
        let error =
            super::PythonManagedNetworkState::reserve(&state, super::PythonManagedSocketKind::Udp)
                .expect_err("cap plus one is rejected before a host operation exists");
        assert_eq!(error.code, "ERR_AGENTOS_RESOURCE_LIMIT");
        assert_eq!(state.lock().expect("state").sockets.len(), 1);
        drop(reservation);
        assert!(state.lock().expect("state").sockets.is_empty());
    }

    #[test]
    fn managed_socket_id_exhaustion_leaves_no_reserved_operation() {
        let state = Arc::new(Mutex::new(super::PythonManagedNetworkState::new(1)));
        state.lock().expect("state").next_socket_id = u64::MAX;
        let error =
            super::PythonManagedNetworkState::reserve(&state, super::PythonManagedSocketKind::Tcp)
                .expect_err("id exhaustion");
        assert_eq!(error.code, "EOVERFLOW");
        assert!(state.lock().expect("state").sockets.is_empty());
    }

    #[test]
    fn managed_socket_reply_commits_the_pre_reserved_adapter_handle() {
        let state = Arc::new(Mutex::new(super::PythonManagedNetworkState::new(1)));
        let reservation =
            super::PythonManagedNetworkState::reserve(&state, super::PythonManagedSocketKind::Tcp)
                .expect("reservation");
        let socket_id = reservation.0.socket_id;
        let reply = super::map_python_host_reply(
            &state,
            PythonHostReplyKind::SocketCreated(reservation),
            HostCallReply::Json(serde_json::json!({ "socketId": "tcp-7" })),
        )
        .expect("commit host socket");
        assert_eq!(reply, serde_json::json!({ "socketId": socket_id }));
        let socket = state
            .lock()
            .expect("state")
            .socket(socket_id, Some(super::PythonManagedSocketKind::Tcp))
            .expect("live socket");
        assert_eq!(socket.host_socket_id, "tcp-7");
    }

    #[test]
    fn downstream_claim_error_preserves_python_pending_state() {
        let pending = Arc::new(Mutex::new(PendingVfsRpcRegistry::new(4)));
        pending.lock().expect("pending").entries.insert(
            42,
            PendingVfsRpc {
                state: PendingVfsRpcState::Pending(42),
                timeout_abort: None,
            },
        );
        let error = super::claim_pending_vfs_rpc(&pending, 42, || {
            Err(crate::backend::HostServiceError::new(
                "EIO",
                "downstream claim failed",
            ))
        })
        .expect_err("claim error");
        assert_eq!(error.code, "EIO");
        assert!(pending.lock().expect("pending").entries.contains_key(&42));
    }

    #[test]
    fn captured_process_reply_bound_is_enforced_before_spawn() {
        super::validate_python_captured_reply_limit(10, 632).expect("exact bound");
        let error = super::validate_python_captured_reply_limit(11, 632)
            .expect_err("reply expansion exceeds bridge bound");
        assert_eq!(error.code, "ERR_AGENTOS_RESOURCE_LIMIT");
        assert_eq!(
            error.details.expect("details")["configPath"],
            "limits.reactor.maxBridgeResponseBytes"
        );
    }

    #[test]
    fn blocking_poll_rejects_an_unrepresentable_deadline() {
        let error = super::checked_python_poll_deadline(Duration::MAX)
            .expect_err("Duration::MAX cannot fit in Instant");
        assert!(matches!(error, PythonExecutionError::InvalidLimit(_)));
    }

    #[test]
    fn dispose_context_reclaims_python_and_nested_javascript_metadata() {
        let mut engine = PythonExecutionEngine::default();
        let baseline = (
            engine.context_count_for_test(),
            engine.javascript_context_count_for_test(),
        );
        let temp = tempdir().expect("create Pyodide fixture root");
        let context = engine.create_context(CreatePythonContextRequest {
            vm_id: String::from("vm-python-context-dispose"),
            pyodide_dist_path: temp.path().to_path_buf(),
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

    #[test]
    fn idle_wait_uses_readiness_instead_of_turn_polling() {
        let started = Instant::now();
        assert_eq!(python_wait_remaining(None, started), None);

        let remaining = python_wait_remaining(Some(Duration::from_secs(1)), started)
            .expect("finite wait keeps one deadline");
        assert!(remaining > Duration::from_millis(900));
        assert!(remaining <= Duration::from_secs(1));
    }

    #[test]
    fn stale_python_vfs_completion_has_no_pending_waiter() {
        let pending = Arc::new(Mutex::new(PendingVfsRpcRegistry::new(2)));

        assert_eq!(
            clear_pending_vfs_rpc(&pending, 41).expect("inspect pending request"),
            PendingVfsRpcResolution::Missing
        );
    }

    #[test]
    fn timed_out_python_vfs_completion_is_consumed_as_stale() {
        let mut registry = PendingVfsRpcRegistry::new(2);
        registry.entries.insert(
            42,
            PendingVfsRpc {
                state: PendingVfsRpcState::TimedOut(42),
                timeout_abort: None,
            },
        );
        registry.observe_depth();
        let pending = Arc::new(Mutex::new(registry));

        assert_eq!(
            clear_pending_vfs_rpc(&pending, 42).expect("clear timed-out request"),
            PendingVfsRpcResolution::TimedOut
        );
        assert!(pending
            .lock()
            .expect("pending request lock")
            .entries
            .is_empty());
    }

    #[test]
    fn pending_python_vfs_rpc_registry_is_bounded_keyed_and_nonlossy() {
        let pending = Arc::new(Mutex::new(PendingVfsRpcRegistry::new(2)));
        set_pending_vfs_rpc_state(&pending, 41).expect("first concurrent VFS waiter");
        set_pending_vfs_rpc_state(&pending, 42).expect("second concurrent VFS waiter");

        let duplicate = set_pending_vfs_rpc_state(&pending, 41)
            .expect_err("duplicate call ID must not replace its waiter");
        assert!(matches!(
            duplicate,
            PythonExecutionError::PendingVfsRpcRequest(41)
        ));
        let duplicate_host_error = python_vfs_rpc_admission_error(duplicate);
        assert_eq!(duplicate_host_error.code, "EBUSY");
        assert_eq!(duplicate_host_error.details.as_ref().unwrap()["callId"], 41);

        let over_limit = set_pending_vfs_rpc_state(&pending, 43)
            .expect_err("third distinct VFS waiter exceeds admission");
        assert!(matches!(
            over_limit,
            PythonExecutionError::PendingVfsRpcLimit {
                limit: 2,
                observed: 3,
            }
        ));
        let limit_host_error = python_vfs_rpc_admission_error(over_limit);
        assert_eq!(limit_host_error.code, "ERR_AGENTOS_RESOURCE_LIMIT");
        assert_eq!(
            limit_host_error.details.as_ref().unwrap()["limitName"],
            "limits.reactor.maxBridgeCalls"
        );

        assert_eq!(
            clear_pending_vfs_rpc(&pending, 42).expect("settle exact second waiter"),
            PendingVfsRpcResolution::Pending
        );
        assert_eq!(
            pending
                .lock()
                .expect("pending request lock")
                .entries
                .get(&41)
                .map(|rpc| rpc.state),
            Some(PendingVfsRpcState::Pending(41))
        );

        cancel_pending_vfs_rpc(&pending);
        assert!(pending
            .lock()
            .expect("pending request lock")
            .entries
            .is_empty());
    }

    #[test]
    fn python_managed_asset_descriptor_reads_use_raw_bounded_responses() {
        let temp = tempdir().expect("create temp dir");
        let pyodide = temp.path().join("pyodide");
        fs::create_dir_all(&pyodide).expect("create pyodide root");
        fs::write(pyodide.join("python_stdlib.zip"), b"stdlib-bytes").expect("write managed asset");
        let mut files = PythonManagedHostFiles::default();
        let open = HostRpcRequest {
            id: 1,
            method: String::from("fs.openSync"),
            args: vec![
                serde_json::Value::String(format!("{PYODIDE_GUEST_ROOT}/python_stdlib.zip")),
                serde_json::json!(0),
                serde_json::Value::Null,
            ],
            raw_bytes_args: HashMap::new(),
        };
        let fd = match python_javascript_sync_rpc_action(&pyodide, &mut files, &open)
            .expect("route managed open")
            .expect("managed open action")
        {
            PythonJavascriptSyncRpcAction::Success(value) => {
                value.as_u64().expect("managed descriptor")
            }
            other => panic!("unexpected managed open action: {other:?}"),
        };

        let read = HostRpcRequest {
            id: 2,
            method: String::from("fs.readSync"),
            args: vec![
                serde_json::json!(fd),
                serde_json::json!(64),
                serde_json::Value::Null,
            ],
            raw_bytes_args: HashMap::from([(usize::MAX, Vec::new())]),
        };
        match python_javascript_sync_rpc_action(&pyodide, &mut files, &read)
            .expect("route managed read")
            .expect("managed read action")
        {
            PythonJavascriptSyncRpcAction::RawSuccess(bytes) => {
                assert_eq!(bytes, b"stdlib-bytes")
            }
            other => panic!("unexpected managed read action: {other:?}"),
        }

        let close = HostRpcRequest {
            id: 3,
            method: String::from("fs.closeSync"),
            args: vec![serde_json::json!(fd)],
            raw_bytes_args: HashMap::new(),
        };
        assert!(matches!(
            python_javascript_sync_rpc_action(&pyodide, &mut files, &close)
                .expect("route managed close"),
            Some(PythonJavascriptSyncRpcAction::Success(
                serde_json::Value::Null
            ))
        ));
        assert!(files.files.is_empty());
    }

    #[test]
    fn python_managed_asset_descriptors_enforce_limit_and_reuse_capacity_after_close() {
        let temp = tempdir().expect("create temp dir");
        let pyodide = temp.path().join("pyodide");
        fs::create_dir_all(&pyodide).expect("create pyodide root");
        fs::write(pyodide.join("python_stdlib.zip"), b"stdlib-bytes").expect("write managed asset");
        let mut files = PythonManagedHostFiles::new(2);
        let open = |id| HostRpcRequest {
            id,
            method: String::from("fs.openSync"),
            args: vec![
                serde_json::Value::String(format!("{PYODIDE_GUEST_ROOT}/python_stdlib.zip")),
                serde_json::json!(0),
                serde_json::Value::Null,
            ],
            raw_bytes_args: HashMap::new(),
        };

        let opened_fd = |action: PythonJavascriptSyncRpcAction| match action {
            PythonJavascriptSyncRpcAction::Success(value) => {
                value.as_u64().expect("managed descriptor")
            }
            other => panic!("unexpected managed open action: {other:?}"),
        };
        let first = opened_fd(
            python_javascript_sync_rpc_action(&pyodide, &mut files, &open(1))
                .expect("route first open")
                .expect("first open action"),
        );
        let _second = opened_fd(
            python_javascript_sync_rpc_action(&pyodide, &mut files, &open(2))
                .expect("route second open")
                .expect("second open action"),
        );
        assert_eq!(files.files.len(), 2);

        assert!(matches!(
            python_javascript_sync_rpc_action(&pyodide, &mut files, &open(3))
                .expect("route saturated open"),
            Some(PythonJavascriptSyncRpcAction::Error { code: "EMFILE", message })
                if message.contains("limits.resources.maxOpenFds")
        ));
        assert_eq!(files.files.len(), 2);

        let close = HostRpcRequest {
            id: 4,
            method: String::from("fs.closeSync"),
            args: vec![serde_json::json!(first)],
            raw_bytes_args: HashMap::new(),
        };
        assert!(matches!(
            python_javascript_sync_rpc_action(&pyodide, &mut files, &close).expect("route close"),
            Some(PythonJavascriptSyncRpcAction::Success(
                serde_json::Value::Null
            ))
        ));
        let _replacement = opened_fd(
            python_javascript_sync_rpc_action(&pyodide, &mut files, &open(5))
                .expect("route replacement open")
                .expect("replacement open action"),
        );
        assert_eq!(files.files.len(), 2);
    }

    #[test]
    fn python_managed_guest_paths_normalize_dot_dot_inside_root() {
        let temp = tempdir().expect("create temp dir");
        let pyodide = temp.path().join("pyodide");
        fs::create_dir_all(pyodide.join("lib")).expect("create pyodide lib");

        let resolved = python_managed_path_kind(
            &pyodide,
            &format!("{PYODIDE_GUEST_ROOT}/lib/../pyodide.mjs"),
        );

        assert!(matches!(resolved.kind, PythonManagedPathKind::GuestPyodide));
        assert_eq!(
            resolved.host_path().expect("host path"),
            pyodide.join("pyodide.mjs")
        );
    }

    #[test]
    fn python_managed_guest_paths_clamp_dot_dot_escape_to_root() {
        let temp = tempdir().expect("create temp dir");
        let pyodide = temp.path().join("pyodide");
        fs::create_dir_all(&pyodide).expect("create pyodide root");

        let resolved =
            python_managed_path_kind(&pyodide, &format!("{PYODIDE_GUEST_ROOT}/../../outside.txt"));

        assert!(matches!(resolved.kind, PythonManagedPathKind::GuestPyodide));
        assert_eq!(
            resolved.host_path().expect("host path"),
            pyodide.join("outside.txt")
        );
    }

    #[cfg(unix)]
    #[test]
    fn python_managed_guest_paths_reject_symlink_escape() {
        let temp = tempdir().expect("create temp dir");
        let pyodide = temp.path().join("pyodide");
        let outside = temp.path().join("outside");
        fs::create_dir_all(&pyodide).expect("create pyodide root");
        fs::create_dir_all(&outside).expect("create outside dir");
        symlink(&outside, pyodide.join("escape")).expect("create escape symlink");

        let resolved =
            python_managed_path_kind(&pyodide, &format!("{PYODIDE_GUEST_ROOT}/escape/file.txt"));

        assert!(matches!(resolved.kind, PythonManagedPathKind::Unmanaged));
        assert!(resolved.host_path().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn python_managed_guest_paths_reject_symlink_escape_to_missing_descendant() {
        let temp = tempdir().expect("create temp dir");
        let pyodide = temp.path().join("pyodide");
        let outside = temp.path().join("outside");
        fs::create_dir_all(&pyodide).expect("create pyodide root");
        fs::create_dir_all(&outside).expect("create outside dir");
        symlink(&outside, pyodide.join("escape")).expect("create escape symlink");

        let resolved = python_managed_path_kind(
            &pyodide,
            &format!("{PYODIDE_GUEST_ROOT}/escape/missing/file.txt"),
        );

        assert!(matches!(resolved.kind, PythonManagedPathKind::Unmanaged));
        assert!(resolved.host_path().is_none());
    }

    #[test]
    fn python_managed_host_paths_accept_canonical_root_descendants() {
        let temp = tempdir().expect("create temp dir");
        let pyodide = temp.path().join("pyodide");
        fs::create_dir_all(pyodide.join("pkg")).expect("create pyodide package dir");
        let candidate = pyodide.join("pkg/module.py");

        let resolved = python_managed_path_kind(&pyodide, &candidate.display().to_string());

        assert!(matches!(resolved.kind, PythonManagedPathKind::HostManaged));
        assert_eq!(resolved.host_path().expect("host path"), candidate);
    }

    #[test]
    fn python_managed_host_paths_reject_unresolved_dot_dot_escape() {
        let temp = tempdir().expect("create temp dir");
        let pyodide = temp.path().join("pyodide");
        fs::create_dir_all(&pyodide).expect("create pyodide root");
        let candidate = pyodide.join("missing/../../outside.txt");

        let resolved = python_managed_path_kind(&pyodide, &candidate.display().to_string());

        assert!(matches!(resolved.kind, PythonManagedPathKind::Unmanaged));
        assert!(resolved.host_path().is_none());
    }

    #[test]
    fn python_managed_cache_guest_paths_resolve_inside_cache_root() {
        let temp = tempdir().expect("create temp dir");
        let pyodide = temp.path().join("pyodide");
        fs::create_dir_all(&pyodide).expect("create pyodide root");

        let resolved = python_managed_path_kind(
            &pyodide,
            &format!("{PYODIDE_CACHE_GUEST_ROOT}/wheels/pkg.whl"),
        );
        let host_path = resolved.host_path().expect("host path");

        assert!(matches!(resolved.kind, PythonManagedPathKind::GuestCache));
        assert!(host_path.ends_with("pyodide-package-cache/wheels/pkg.whl"));
    }
}
