use crate::common::{encode_json_string, frozen_time_ms, stable_hash64};
use crate::node_import_cache::{
    NodeImportCache, NodeImportCacheCleanup, NODE_IMPORT_CACHE_ASSET_ROOT_ENV,
};
use crate::node_process::{
    apply_guest_env, configure_node_control_channel, create_node_control_channel,
    encode_json_string_array, env_builtin_enabled, harden_node_command, node_binary,
    node_resolution_read_paths, resolve_path_like_specifier, spawn_node_control_reader,
    spawn_stream_reader, spawn_waiter, ExportedChildFds, LinePrefixFilter, NodeControlMessage,
    NodeSignalHandlerRegistration,
};
use crate::runtime_support::{
    configure_compile_cache, env_flag_enabled, import_cache_root, sandbox_root, warmup_marker_path,
    NODE_COMPILE_CACHE_ENV, NODE_DISABLE_COMPILE_CACHE_ENV, NODE_FROZEN_TIME_ENV,
    NODE_SANDBOX_ROOT_ENV,
};
use crate::v8_host::{V8RuntimeHost, V8SessionHandle};
use crate::v8_ipc::BinaryFrame;
use crate::v8_runtime;
use nix::fcntl::OFlag;
use nix::unistd::pipe2;
use serde::Deserialize;
use serde_json::{from_str, json, Value};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::{
    mpsc::{self, Receiver, SyncSender, TrySendError},
    Arc, Mutex,
};
use std::thread;
use std::time::{Duration, Instant};
use tokio::sync::mpsc::{
    error::TryRecvError as TokioTryRecvError, unbounded_channel, UnboundedReceiver,
};
use tokio::time;

const NODE_ENTRYPOINT_ENV: &str = "AGENT_OS_ENTRYPOINT";
const NODE_BOOTSTRAP_ENV: &str = "AGENT_OS_BOOTSTRAP_MODULE";
const NODE_GUEST_ARGV_ENV: &str = "AGENT_OS_GUEST_ARGV";
const NODE_PREWARM_IMPORTS_ENV: &str = "AGENT_OS_NODE_PREWARM_IMPORTS";
const NODE_WARMUP_DEBUG_ENV: &str = "AGENT_OS_NODE_WARMUP_DEBUG";
const NODE_WARMUP_METRICS_PREFIX: &str = "__AGENT_OS_NODE_WARMUP_METRICS__:";
const NODE_IMPORT_COMPILE_CACHE_NAMESPACE_VERSION: &str = "3";
const NODE_IMPORT_CACHE_LOADER_PATH_ENV: &str = "AGENT_OS_NODE_IMPORT_CACHE_LOADER_PATH";
const NODE_IMPORT_CACHE_PATH_ENV: &str = "AGENT_OS_NODE_IMPORT_CACHE_PATH";
const NODE_KEEP_STDIN_OPEN_ENV: &str = "AGENT_OS_KEEP_STDIN_OPEN";
const NODE_GUEST_ENTRYPOINT_ENV: &str = "AGENT_OS_GUEST_ENTRYPOINT";
const NODE_GUEST_PATH_MAPPINGS_ENV: &str = "AGENT_OS_GUEST_PATH_MAPPINGS";
const NODE_VIRTUAL_PROCESS_EXEC_PATH_ENV: &str = "AGENT_OS_VIRTUAL_PROCESS_EXEC_PATH";
const NODE_VIRTUAL_PROCESS_PID_ENV: &str = "AGENT_OS_VIRTUAL_PROCESS_PID";
const NODE_VIRTUAL_PROCESS_PPID_ENV: &str = "AGENT_OS_VIRTUAL_PROCESS_PPID";
const NODE_VIRTUAL_PROCESS_UID_ENV: &str = "AGENT_OS_VIRTUAL_PROCESS_UID";
const NODE_VIRTUAL_PROCESS_GID_ENV: &str = "AGENT_OS_VIRTUAL_PROCESS_GID";
const NODE_PARENT_ALLOW_CHILD_PROCESS_ENV: &str = "AGENT_OS_PARENT_NODE_ALLOW_CHILD_PROCESS";
const NODE_PARENT_ALLOW_WORKER_ENV: &str = "AGENT_OS_PARENT_NODE_ALLOW_WORKER";
const NODE_EXTRA_FS_READ_PATHS_ENV: &str = "AGENT_OS_EXTRA_FS_READ_PATHS";
const NODE_EXTRA_FS_WRITE_PATHS_ENV: &str = "AGENT_OS_EXTRA_FS_WRITE_PATHS";
const NODE_ALLOWED_BUILTINS_ENV: &str = "AGENT_OS_ALLOWED_NODE_BUILTINS";
const NODE_LOOPBACK_EXEMPT_PORTS_ENV: &str = "AGENT_OS_LOOPBACK_EXEMPT_PORTS";
const NODE_SYNC_RPC_ENABLE_ENV: &str = "AGENT_OS_NODE_SYNC_RPC_ENABLE";
const NODE_SYNC_RPC_REQUEST_FD_ENV: &str = "AGENT_OS_NODE_SYNC_RPC_REQUEST_FD";
const NODE_SYNC_RPC_RESPONSE_FD_ENV: &str = "AGENT_OS_NODE_SYNC_RPC_RESPONSE_FD";
const NODE_SYNC_RPC_DATA_BYTES_ENV: &str = "AGENT_OS_NODE_SYNC_RPC_DATA_BYTES";
const NODE_SYNC_RPC_WAIT_TIMEOUT_MS_ENV: &str = "AGENT_OS_NODE_SYNC_RPC_WAIT_TIMEOUT_MS";
const NODE_SYNC_RPC_DEFAULT_DATA_BYTES: usize = 4 * 1024 * 1024;
const NODE_SYNC_RPC_DEFAULT_WAIT_TIMEOUT_MS: u64 = 30_000;
const NODE_SYNC_RPC_RESPONSE_QUEUE_CAPACITY: usize = 1;
const NODE_WARMUP_MARKER_VERSION: &str = "1";
const NODE_WARMUP_SPECIFIERS: &[&str] = &[
    "agent-os:builtin/path",
    "agent-os:builtin/url",
    "agent-os:builtin/fs-promises",
    "agent-os:polyfill/path",
];
const CONTROLLED_STDERR_PREFIXES: &[&str] =
    &[crate::node_import_cache::NODE_IMPORT_CACHE_METRICS_PREFIX];
const RESERVED_NODE_ENV_KEYS: &[&str] = &[
    NODE_BOOTSTRAP_ENV,
    NODE_COMPILE_CACHE_ENV,
    NODE_DISABLE_COMPILE_CACHE_ENV,
    NODE_ENTRYPOINT_ENV,
    NODE_EXTRA_FS_READ_PATHS_ENV,
    NODE_EXTRA_FS_WRITE_PATHS_ENV,
    NODE_SANDBOX_ROOT_ENV,
    NODE_FROZEN_TIME_ENV,
    NODE_GUEST_ENTRYPOINT_ENV,
    NODE_GUEST_ARGV_ENV,
    NODE_GUEST_PATH_MAPPINGS_ENV,
    NODE_VIRTUAL_PROCESS_EXEC_PATH_ENV,
    NODE_VIRTUAL_PROCESS_PID_ENV,
    NODE_VIRTUAL_PROCESS_PPID_ENV,
    NODE_VIRTUAL_PROCESS_UID_ENV,
    NODE_VIRTUAL_PROCESS_GID_ENV,
    NODE_PARENT_ALLOW_CHILD_PROCESS_ENV,
    NODE_PARENT_ALLOW_WORKER_ENV,
    NODE_IMPORT_CACHE_ASSET_ROOT_ENV,
    NODE_IMPORT_CACHE_LOADER_PATH_ENV,
    NODE_IMPORT_CACHE_PATH_ENV,
    NODE_KEEP_STDIN_OPEN_ENV,
    NODE_ALLOWED_BUILTINS_ENV,
    NODE_LOOPBACK_EXEMPT_PORTS_ENV,
    NODE_SYNC_RPC_ENABLE_ENV,
    NODE_SYNC_RPC_REQUEST_FD_ENV,
    NODE_SYNC_RPC_RESPONSE_FD_ENV,
    NODE_SYNC_RPC_DATA_BYTES_ENV,
    NODE_SYNC_RPC_WAIT_TIMEOUT_MS_ENV,
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JavascriptSyncRpcRequest {
    pub id: u64,
    pub method: String,
    pub args: Vec<Value>,
}

#[derive(Debug, Deserialize)]
struct JavascriptSyncRpcRequestWire {
    id: u64,
    method: String,
    #[serde(default)]
    args: Vec<Value>,
}

struct JavascriptSyncRpcChannels {
    parent_request_reader: File,
    parent_response_writer: File,
    child_request_writer: OwnedFd,
    child_response_reader: OwnedFd,
}

#[derive(Debug)]
struct JavascriptSyncRpcResponseWriter {
    sender: SyncSender<Vec<u8>>,
    timeout: Duration,
}

impl JavascriptSyncRpcResponseWriter {
    fn new(writer: File, timeout: Duration) -> Self {
        let (sender, receiver) = mpsc::sync_channel(NODE_SYNC_RPC_RESPONSE_QUEUE_CAPACITY);
        spawn_javascript_sync_rpc_response_writer(writer, receiver);
        Self { sender, timeout }
    }

    fn send(&self, payload: Vec<u8>) -> Result<(), JavascriptExecutionError> {
        let started = Instant::now();
        let mut payload = Some(payload);

        loop {
            match self
                .sender
                .try_send(payload.take().expect("payload should be present"))
            {
                Ok(()) => return Ok(()),
                Err(TrySendError::Disconnected(_)) => {
                    return Err(JavascriptExecutionError::RpcResponse(String::from(
                        "JavaScript sync RPC response channel closed unexpectedly",
                    )));
                }
                Err(TrySendError::Full(returned_payload)) => {
                    if started.elapsed() >= self.timeout {
                        return Err(JavascriptExecutionError::RpcResponse(format!(
                            "timed out after {}ms while queueing JavaScript sync RPC response",
                            self.timeout.as_millis()
                        )));
                    }
                    payload = Some(returned_payload);
                    thread::sleep(Duration::from_millis(5));
                }
            }
        }
    }
}

impl Clone for JavascriptSyncRpcResponseWriter {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            timeout: self.timeout,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingSyncRpcState {
    Pending(u64),
    TimedOut(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingSyncRpcResolution {
    Pending,
    TimedOut,
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateJavascriptContextRequest {
    pub vm_id: String,
    pub bootstrap_module: Option<String>,
    pub compile_cache_root: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JavascriptContext {
    pub context_id: String,
    pub vm_id: String,
    pub bootstrap_module: Option<String>,
    pub compile_cache_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartJavascriptExecutionRequest {
    pub vm_id: String,
    pub context_id: String,
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: PathBuf,
    /// Optional inline JavaScript code to execute directly in the V8 isolate.
    /// When set, this code is passed as user_code instead of generating a
    /// require() call for the entrypoint. Used by the sidecar to pass
    /// entrypoint content read from the VFS.
    pub inline_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JavascriptExecutionEvent {
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
    SyncRpcRequest(JavascriptSyncRpcRequest),
    SignalState {
        signal: u32,
        registration: NodeSignalHandlerRegistration,
    },
    Exited(i32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum JavascriptProcessEvent {
    Stdout(Vec<u8>),
    RawStderr(Vec<u8>),
    SyncRpcRequest(JavascriptSyncRpcRequest),
    Control(NodeControlMessage),
    Exited(i32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JavascriptExecutionResult {
    pub execution_id: String,
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GuestPathMapping {
    guest_path: String,
    host_path: PathBuf,
}

#[derive(Debug, Deserialize)]
struct GuestPathMappingWire {
    #[serde(rename = "guestPath")]
    guest_path: String,
    #[serde(rename = "hostPath")]
    host_path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ModuleResolveMode {
    Require,
    Import,
}

#[derive(Debug, Clone, Default)]
struct LocalModuleResolutionCache {
    resolve_results: HashMap<(String, String, ModuleResolveMode), Option<String>>,
    package_json_results: HashMap<String, Option<LocalPackageJson>>,
    exists_results: HashMap<String, bool>,
    stat_results: HashMap<String, Option<bool>>,
}

#[derive(Debug, Clone, Default)]
struct LocalBridgeState {
    translator: GuestPathTranslator,
    resolution_cache: LocalModuleResolutionCache,
    handle_descriptions: HashMap<String, String>,
    next_timer_id: u64,
    timers: Arc<Mutex<HashMap<u64, LocalTimerEntry>>>,
    v8_session: Option<V8SessionHandle>,
}

#[derive(Debug, Clone, Default)]
struct GuestPathTranslator {
    implicit_guest_cwd: String,
    implicit_host_cwd: PathBuf,
    mappings: Vec<GuestPathMapping>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct LocalPackageJson {
    #[serde(default)]
    main: Option<String>,
    #[serde(default)]
    #[serde(rename = "type")]
    package_type: Option<String>,
    #[serde(default)]
    exports: Option<Value>,
    #[serde(default)]
    imports: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LocalTimerEntry {
    delay_ms: u64,
    generation: u64,
    repeat: bool,
}

#[derive(Debug, Clone, PartialEq)]
enum LocalBridgeCallResult {
    Immediate(Value),
    Deferred,
}

fn timer_delay_ms(value: Option<&Value>) -> u64 {
    let delay = match value {
        Some(Value::Number(number)) => number.as_f64().unwrap_or(0.0),
        Some(Value::String(text)) => text.parse::<f64>().unwrap_or(0.0),
        _ => 0.0,
    };

    if !delay.is_finite() || delay <= 0.0 {
        0
    } else {
        delay.floor().min(u64::MAX as f64) as u64
    }
}

impl GuestPathTranslator {
    fn from_request(request: &StartJavascriptExecutionRequest) -> Self {
        let implicit_guest_cwd = request
            .env
            .get("HOME")
            .filter(|value| value.starts_with('/'))
            .cloned()
            .unwrap_or_else(|| String::from("/root"));
        let mut mappings = parse_guest_path_mappings(request)
            .into_iter()
            .filter(|mapping| mapping.guest_path.starts_with('/'))
            .collect::<Vec<_>>();

        if !mappings.iter().any(|mapping| {
            mapping.host_path == request.cwd && mapping.guest_path == implicit_guest_cwd
        }) {
            mappings.push(GuestPathMapping {
                guest_path: implicit_guest_cwd.clone(),
                host_path: request.cwd.clone(),
            });
        }

        mappings.sort_by(|left, right| {
            right
                .host_path
                .components()
                .count()
                .cmp(&left.host_path.components().count())
                .then_with(|| right.guest_path.len().cmp(&left.guest_path.len()))
        });

        Self {
            implicit_guest_cwd,
            implicit_host_cwd: request.cwd.clone(),
            mappings,
        }
    }

    fn guest_cwd(&self) -> &str {
        &self.implicit_guest_cwd
    }

    fn resolve_host_entrypoint(&self, cwd: &Path, entrypoint: &str) -> PathBuf {
        if entrypoint == "-e" || entrypoint == "--eval" {
            return PathBuf::from(entrypoint);
        }

        let path = Path::new(entrypoint);
        if path.is_absolute() {
            self.guest_to_host(entrypoint)
                .unwrap_or_else(|| path.to_path_buf())
        } else {
            cwd.join(path)
        }
    }

    fn host_to_guest_string(&self, host_path: &Path) -> String {
        if !host_path.is_absolute() {
            return normalize_guest_path(&host_path.to_string_lossy());
        }

        for mapping in &self.mappings {
            if let Ok(stripped) = host_path.strip_prefix(&mapping.host_path) {
                return join_guest_path(
                    &mapping.guest_path,
                    &stripped.to_string_lossy().replace('\\', "/"),
                );
            }
        }

        if let Ok(stripped) = host_path.strip_prefix(&self.implicit_host_cwd) {
            return join_guest_path(
                &self.implicit_guest_cwd,
                &stripped.to_string_lossy().replace('\\', "/"),
            );
        }

        let basename = host_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("unknown");
        join_guest_path("/unknown", basename)
    }

    fn guest_to_host(&self, guest_path: &str) -> Option<PathBuf> {
        let normalized = normalize_guest_path(guest_path);

        for mapping in &self.mappings {
            if let Some(suffix) = strip_guest_prefix(&normalized, &mapping.guest_path) {
                return Some(join_host_path(&mapping.host_path, suffix));
            }
        }

        if let Some(suffix) = strip_guest_prefix(&normalized, &self.implicit_guest_cwd) {
            return Some(join_host_path(&self.implicit_host_cwd, suffix));
        }

        let path = PathBuf::from(&normalized);
        if path.is_absolute() {
            Some(path)
        } else {
            None
        }
    }
}

fn parse_guest_path_mappings(request: &StartJavascriptExecutionRequest) -> Vec<GuestPathMapping> {
    request
        .env
        .get(NODE_GUEST_PATH_MAPPINGS_ENV)
        .and_then(|value| serde_json::from_str::<Vec<GuestPathMappingWire>>(value).ok())
        .into_iter()
        .flatten()
        .map(|mapping| GuestPathMapping {
            guest_path: normalize_guest_path(&mapping.guest_path),
            host_path: PathBuf::from(mapping.host_path),
        })
        .collect()
}

fn normalize_guest_path(path: &str) -> String {
    let mut segments = Vec::new();
    let absolute = path.starts_with('/');
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            other => segments.push(other),
        }
    }
    if !absolute {
        return segments.join("/");
    }
    if segments.is_empty() {
        String::from("/")
    } else {
        format!("/{}", segments.join("/"))
    }
}

fn join_guest_path(base: &str, suffix: &str) -> String {
    if suffix.is_empty() || suffix == "." {
        return normalize_guest_path(base);
    }
    let trimmed = suffix.trim_start_matches('/');
    normalize_guest_path(&format!("{}/{}", base.trim_end_matches('/'), trimmed))
}

fn strip_guest_prefix<'a>(path: &'a str, prefix: &str) -> Option<&'a str> {
    if path == prefix {
        return Some("");
    }
    path.strip_prefix(prefix)
        .and_then(|suffix| suffix.strip_prefix('/'))
}

fn join_host_path(base: &Path, suffix: &str) -> PathBuf {
    if suffix.is_empty() {
        return base.to_path_buf();
    }
    let mut joined = base.to_path_buf();
    for segment in suffix.split('/') {
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment == ".." {
            joined.pop();
        } else {
            joined.push(segment);
        }
    }
    joined
}

fn translate_v8_bridge_value_to_legacy(value: &Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(
            values
                .iter()
                .map(translate_v8_bridge_value_to_legacy)
                .collect(),
        ),
        Value::Object(map) if map.get("__type").and_then(Value::as_str) == Some("Buffer") => {
            json!({
                "__agentOsType": "bytes",
                "base64": map.get("data").cloned().unwrap_or(Value::String(String::new())),
            })
        }
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), translate_v8_bridge_value_to_legacy(value)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn translate_request_args_for_legacy(method: &str, args: &[Value]) -> Vec<Value> {
    let mut translated = args
        .iter()
        .map(translate_v8_bridge_value_to_legacy)
        .collect::<Vec<_>>();

    if matches!(method, "fs.writeFileSync" | "fs.promises.writeFile") {
        if let Some(Value::String(data)) = translated.get(1) {
            translated[1] = json!({
                "__agentOsType": "bytes",
                "base64": data,
            });
        }
    }

    translated
}

fn translate_legacy_bridge_value_to_v8(value: &Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(
            values
                .iter()
                .map(translate_legacy_bridge_value_to_v8)
                .collect(),
        ),
        Value::Object(map) if map.get("__agentOsType").and_then(Value::as_str) == Some("bytes") => {
            json!({
                "__type": "Buffer",
                "data": map.get("base64").cloned().unwrap_or(Value::String(String::new())),
            })
        }
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), translate_legacy_bridge_value_to_v8(value)))
                .collect(),
        ),
        other => other.clone(),
    }
}

#[derive(Debug)]
pub enum JavascriptExecutionError {
    EmptyArgv,
    MissingContext(String),
    VmMismatch { expected: String, found: String },
    MissingChildStream(&'static str),
    PrepareImportCache(std::io::Error),
    WarmupSpawn(std::io::Error),
    WarmupFailed { exit_code: i32, stderr: String },
    Spawn(std::io::Error),
    PendingSyncRpcRequest(u64),
    ExpiredSyncRpcRequest(u64),
    RpcChannel(String),
    RpcResponse(String),
    Terminate(std::io::Error),
    StdinClosed,
    Stdin(std::io::Error),
    EventChannelClosed,
}

impl fmt::Display for JavascriptExecutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyArgv => f.write_str("guest JavaScript execution requires argv[0]"),
            Self::MissingContext(context_id) => {
                write!(f, "unknown guest JavaScript context: {context_id}")
            }
            Self::VmMismatch { expected, found } => {
                write!(
                    f,
                    "guest JavaScript context belongs to vm {expected}, not {found}"
                )
            }
            Self::MissingChildStream(name) => write!(f, "node child missing {name} pipe"),
            Self::PrepareImportCache(err) => {
                write!(
                    f,
                    "failed to prepare sidecar-scoped Node import cache: {err}"
                )
            }
            Self::WarmupSpawn(err) => {
                write!(f, "failed to start Node import warmup process: {err}")
            }
            Self::WarmupFailed { exit_code, stderr } => {
                if stderr.trim().is_empty() {
                    write!(f, "Node import warmup exited with status {exit_code}")
                } else {
                    write!(
                        f,
                        "Node import warmup exited with status {exit_code}: {}",
                        stderr.trim()
                    )
                }
            }
            Self::Spawn(err) => write!(f, "failed to start guest JavaScript runtime: {err}"),
            Self::PendingSyncRpcRequest(id) => {
                write!(
                    f,
                    "guest JavaScript execution requires servicing pending sync RPC request {id}"
                )
            }
            Self::ExpiredSyncRpcRequest(id) => {
                write!(f, "sync RPC request {id} is no longer pending")
            }
            Self::RpcChannel(message) => {
                write!(
                    f,
                    "failed to configure guest JavaScript sync RPC channel: {message}"
                )
            }
            Self::RpcResponse(message) => {
                write!(
                    f,
                    "failed to reply to guest JavaScript sync RPC request: {message}"
                )
            }
            Self::Terminate(err) => {
                write!(f, "failed to terminate guest JavaScript runtime: {err}")
            }
            Self::StdinClosed => f.write_str("guest JavaScript stdin is already closed"),
            Self::Stdin(err) => write!(f, "failed to write guest stdin: {err}"),
            Self::EventChannelClosed => {
                f.write_str("guest JavaScript event channel closed unexpectedly")
            }
        }
    }
}

impl std::error::Error for JavascriptExecutionError {}

#[derive(Debug)]
pub struct JavascriptExecution {
    execution_id: String,
    child_pid: u32,
    stdin: Option<ChildStdin>,
    events: RefCell<UnboundedReceiver<JavascriptExecutionEvent>>,
    pending_sync_rpc: Arc<Mutex<Option<PendingSyncRpcState>>>,
    sync_rpc_responses: Option<JavascriptSyncRpcResponseWriter>,
    _import_cache_guard: Arc<NodeImportCacheCleanup>,
    /// V8 session handle for sending bridge responses (None for legacy node mode).
    v8_session: Option<V8SessionHandle>,
}

impl JavascriptExecution {
    pub fn execution_id(&self) -> &str {
        &self.execution_id
    }

    pub fn child_pid(&self) -> u32 {
        self.child_pid
    }

    pub fn write_stdin(&mut self, chunk: &[u8]) -> Result<(), JavascriptExecutionError> {
        // V8 stdin via stream event
        if let Some(session) = &self.v8_session {
            // CBOR-encode the stdin data
            let payload = v8_runtime::json_to_cbor_payload(&Value::String(
                String::from_utf8_lossy(chunk).into_owned(),
            ))
            .map_err(|e| JavascriptExecutionError::Stdin(e))?;
            session
                .send_stream_event("stdin", payload)
                .map_err(|e| JavascriptExecutionError::Stdin(e))?;
            return Ok(());
        }

        // Legacy node stdin pipe
        let stdin = self
            .stdin
            .as_mut()
            .ok_or(JavascriptExecutionError::StdinClosed)?;
        stdin
            .write_all(chunk)
            .and_then(|()| stdin.flush())
            .map_err(JavascriptExecutionError::Stdin)
    }

    pub fn close_stdin(&mut self) -> Result<(), JavascriptExecutionError> {
        // V8 stdin end via stream event
        if let Some(session) = &self.v8_session {
            let _ = session.send_stream_event("stdin_end", vec![]);
            return Ok(());
        }

        // Legacy node stdin pipe
        if let Some(stdin) = self.stdin.take() {
            drop(stdin);
        }
        Ok(())
    }

    pub fn terminate(&self) -> Result<(), JavascriptExecutionError> {
        if let Some(session) = &self.v8_session {
            return session
                .terminate()
                .map_err(JavascriptExecutionError::Terminate);
        }

        Ok(())
    }

    pub fn respond_sync_rpc_success(
        &mut self,
        id: u64,
        result: Value,
    ) -> Result<(), JavascriptExecutionError> {
        match self.clear_pending_sync_rpc(id)? {
            PendingSyncRpcResolution::Pending => {}
            PendingSyncRpcResolution::TimedOut => {
                return Err(JavascriptExecutionError::ExpiredSyncRpcRequest(id));
            }
            PendingSyncRpcResolution::Missing => {}
        }

        // V8 bridge response path
        if let Some(session) = &self.v8_session {
            let payload = translate_legacy_bridge_value_to_v8(&result);
            let payload = v8_runtime::json_to_cbor_payload(&payload)
                .map_err(|e| JavascriptExecutionError::RpcResponse(e.to_string()))?;
            session
                .send_bridge_response(id, 0, payload)
                .map_err(|e| JavascriptExecutionError::RpcResponse(e.to_string()))?;
            return Ok(());
        }

        // Legacy node pipe-based response path
        let Some(writer) = &self.sync_rpc_responses else {
            return Err(JavascriptExecutionError::RpcResponse(String::from(
                "no sync RPC channel is active for this JavaScript execution",
            )));
        };
        write_javascript_sync_rpc_response(
            writer,
            json!({
                "id": id,
                "ok": true,
                "result": result,
            }),
        )
    }

    pub fn respond_sync_rpc_error(
        &mut self,
        id: u64,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<(), JavascriptExecutionError> {
        match self.clear_pending_sync_rpc(id)? {
            PendingSyncRpcResolution::Pending => {}
            PendingSyncRpcResolution::TimedOut => {
                return Err(JavascriptExecutionError::ExpiredSyncRpcRequest(id));
            }
            PendingSyncRpcResolution::Missing => {}
        }

        // V8 bridge response path
        if let Some(session) = &self.v8_session {
            let error_msg = message.into();
            let payload = error_msg.into_bytes();
            session
                .send_bridge_response(id, 1, payload)
                .map_err(|e| JavascriptExecutionError::RpcResponse(e.to_string()))?;
            return Ok(());
        }

        // Legacy node pipe-based response path
        let Some(writer) = &self.sync_rpc_responses else {
            return Err(JavascriptExecutionError::RpcResponse(String::from(
                "no sync RPC channel is active for this JavaScript execution",
            )));
        };
        write_javascript_sync_rpc_response(
            writer,
            json!({
                "id": id,
                "ok": false,
                "error": {
                    "code": code.into(),
                    "message": message.into(),
                },
            }),
        )
    }

    pub async fn poll_event(
        &self,
        timeout: Duration,
    ) -> Result<Option<JavascriptExecutionEvent>, JavascriptExecutionError> {
        if timeout.is_zero() {
            return match self.events.borrow_mut().try_recv() {
                Ok(event) => Ok(Some(event)),
                Err(TokioTryRecvError::Empty) => Ok(None),
                Err(TokioTryRecvError::Disconnected) => {
                    Err(JavascriptExecutionError::EventChannelClosed)
                }
            };
        }

        let mut events = self.events.borrow_mut();
        match time::timeout(timeout, events.recv()).await {
            Ok(Some(event)) => Ok(Some(event)),
            Ok(None) => Err(JavascriptExecutionError::EventChannelClosed),
            Err(_) => Ok(None),
        }
    }

    pub fn poll_event_blocking(
        &self,
        timeout: Duration,
    ) -> Result<Option<JavascriptExecutionEvent>, JavascriptExecutionError> {
        let deadline = Instant::now() + timeout;
        loop {
            match self.events.borrow_mut().try_recv() {
                Ok(event) => return Ok(Some(event)),
                Err(TokioTryRecvError::Disconnected) => {
                    return Err(JavascriptExecutionError::EventChannelClosed)
                }
                Err(TokioTryRecvError::Empty) => {
                    if Instant::now() >= deadline {
                        return Ok(None);
                    }
                    thread::sleep(Duration::from_millis(1));
                }
            }
        }
    }

    pub fn wait(mut self) -> Result<JavascriptExecutionResult, JavascriptExecutionError> {
        self.close_stdin()?;
        let mut events = self.events.into_inner();

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        loop {
            match events.blocking_recv() {
                Some(JavascriptExecutionEvent::Stdout(chunk)) => stdout.extend(chunk),
                Some(JavascriptExecutionEvent::Stderr(chunk)) => stderr.extend(chunk),
                Some(JavascriptExecutionEvent::SyncRpcRequest(request)) => {
                    return Err(JavascriptExecutionError::PendingSyncRpcRequest(request.id));
                }
                Some(JavascriptExecutionEvent::SignalState { .. }) => {}
                Some(JavascriptExecutionEvent::Exited(exit_code)) => {
                    return Ok(JavascriptExecutionResult {
                        execution_id: self.execution_id,
                        exit_code,
                        stdout,
                        stderr,
                    });
                }
                None => return Err(JavascriptExecutionError::EventChannelClosed),
            }
        }
    }

    fn clear_pending_sync_rpc(
        &self,
        id: u64,
    ) -> Result<PendingSyncRpcResolution, JavascriptExecutionError> {
        let mut pending = self.pending_sync_rpc.lock().map_err(|_| {
            JavascriptExecutionError::RpcResponse(String::from(
                "sync RPC pending-request state lock poisoned",
            ))
        })?;
        match *pending {
            Some(PendingSyncRpcState::Pending(current)) if current == id => {
                *pending = None;
                Ok(PendingSyncRpcResolution::Pending)
            }
            Some(PendingSyncRpcState::TimedOut(current)) if current == id => {
                Ok(PendingSyncRpcResolution::TimedOut)
            }
            _ => Ok(PendingSyncRpcResolution::Missing),
        }
    }
}

pub struct JavascriptExecutionEngine {
    next_context_id: usize,
    next_execution_id: usize,
    contexts: BTreeMap<String, JavascriptContext>,
    import_caches: BTreeMap<String, NodeImportCache>,
    v8_host: Option<V8RuntimeHost>,
}

impl Default for JavascriptExecutionEngine {
    fn default() -> Self {
        Self {
            next_context_id: 0,
            next_execution_id: 0,
            contexts: BTreeMap::new(),
            import_caches: BTreeMap::new(),
            v8_host: None,
        }
    }
}

impl std::fmt::Debug for JavascriptExecutionEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JavascriptExecutionEngine")
            .field("next_context_id", &self.next_context_id)
            .field("next_execution_id", &self.next_execution_id)
            .field("contexts", &self.contexts)
            .field("v8_host", &self.v8_host.is_some())
            .finish()
    }
}

impl JavascriptExecutionEngine {
    #[doc(hidden)]
    pub fn set_import_cache_base_dir(&mut self, vm_id: impl Into<String>, base_dir: PathBuf) {
        self.import_caches
            .insert(vm_id.into(), NodeImportCache::new_in(base_dir));
    }

    pub fn create_context(&mut self, request: CreateJavascriptContextRequest) -> JavascriptContext {
        self.next_context_id += 1;
        self.import_caches.entry(request.vm_id.clone()).or_default();

        let context = JavascriptContext {
            context_id: format!("js-ctx-{}", self.next_context_id),
            vm_id: request.vm_id,
            bootstrap_module: request.bootstrap_module,
            compile_cache_dir: request
                .compile_cache_root
                .map(resolve_node_import_compile_cache_dir),
        };
        self.contexts
            .insert(context.context_id.clone(), context.clone());
        context
    }

    pub fn start_execution(
        &mut self,
        request: StartJavascriptExecutionRequest,
    ) -> Result<JavascriptExecution, JavascriptExecutionError> {
        let context = self
            .contexts
            .get(&request.context_id)
            .cloned()
            .ok_or_else(|| JavascriptExecutionError::MissingContext(request.context_id.clone()))?;

        if context.vm_id != request.vm_id {
            return Err(JavascriptExecutionError::VmMismatch {
                expected: context.vm_id,
                found: request.vm_id,
            });
        }

        if request.argv.is_empty() {
            return Err(JavascriptExecutionError::EmptyArgv);
        }

        // Ensure import cache is materialized (still needed for module resolution)
        let import_cache = self.import_caches.entry(context.vm_id.clone()).or_default();
        import_cache
            .ensure_materialized()
            .map_err(JavascriptExecutionError::PrepareImportCache)?;
        let import_cache_guard = import_cache.cleanup_guard();

        self.next_execution_id += 1;
        let execution_id = format!("exec-{}", self.next_execution_id);
        let sync_rpc_timeout = javascript_sync_rpc_timeout(&request);

        // Lazily spawn the V8 runtime host
        if self.v8_host.is_none() {
            self.v8_host = Some(V8RuntimeHost::spawn().map_err(JavascriptExecutionError::Spawn)?);
        }
        let v8_host = self.v8_host.as_ref().unwrap();

        // Create a V8 session
        let session_id = format!("v8-{execution_id}");
        let frame_receiver = v8_host.register_session(&session_id);

        v8_host
            .send_frame(&BinaryFrame::CreateSession {
                session_id: session_id.clone(),
                heap_limit_mb: 0, // no limit for now
                cpu_time_limit_ms: 0,
            })
            .map_err(JavascriptExecutionError::Spawn)?;

        // Build user code: prefer inline code, fall back to entrypoint-based
        let translator = GuestPathTranslator::from_request(&request);
        let host_entrypoint = translator.resolve_host_entrypoint(&request.cwd, &request.argv[0]);
        let guest_entrypoint = if request.argv[0] == "-e" || request.argv[0] == "--eval" {
            request.argv[0].clone()
        } else {
            translator.host_to_guest_string(&host_entrypoint)
        };
        let process_argv = std::iter::once(String::from("node"))
            .chain(std::iter::once(guest_entrypoint.clone()))
            .chain(request.argv.iter().skip(1).cloned())
            .collect::<Vec<_>>();
        let use_module_mode = request.inline_code.is_none()
            && matches!(
                host_entrypoint.extension().and_then(|ext| ext.to_str()),
                Some("mjs" | "mts")
            );
        let user_code = if let Some(inline_code) = request.inline_code.clone() {
            inline_code
        } else if use_module_mode {
            fs::read_to_string(&host_entrypoint)
                .map_err(JavascriptExecutionError::PrepareImportCache)?
        } else {
            build_v8_user_code(&guest_entrypoint, &request.env)
        };
        let user_code = prepend_v8_runtime_shim(
            user_code,
            &guest_entrypoint,
            &process_argv,
            translator.guest_cwd(),
            &request.env,
        );

        // Execute bridge code + user code in the V8 isolate
        v8_host
            .send_frame(&BinaryFrame::Execute {
                session_id: session_id.clone(),
                mode: if use_module_mode { 1 } else { 0 },
                file_path: guest_entrypoint.clone(),
                bridge_code: V8RuntimeHost::bridge_code().to_owned(),
                post_restore_script: String::new(),
                user_code,
            })
            .map_err(JavascriptExecutionError::Spawn)?;

        // Create session handle for sending bridge responses
        let v8_session = V8SessionHandle::new(session_id.clone(), v8_host.writer_handle());

        // Spawn V8 event bridge thread that converts BinaryFrame → JavascriptExecutionEvent
        let pending_sync_rpc = Arc::new(Mutex::new(None));
        let events = spawn_v8_event_bridge(
            frame_receiver,
            pending_sync_rpc.clone(),
            sync_rpc_timeout,
            v8_session.clone(),
            LocalBridgeState {
                translator,
                v8_session: Some(v8_session.clone()),
                ..Default::default()
            },
        );

        Ok(JavascriptExecution {
            execution_id,
            child_pid: 0, // V8 isolate has no host PID
            stdin: None,
            events: RefCell::new(events),
            pending_sync_rpc,
            sync_rpc_responses: None,
            _import_cache_guard: import_cache_guard,
            v8_session: Some(v8_session),
        })
    }

    pub fn dispose_vm(&mut self, vm_id: &str) {
        self.contexts.retain(|_, context| context.vm_id != vm_id);
        self.import_caches.remove(vm_id);
    }

    #[doc(hidden)]
    #[allow(dead_code)]
    pub fn materialize_import_cache_for_vm(
        &mut self,
        vm_id: &str,
    ) -> Result<&std::path::Path, std::io::Error> {
        let import_cache = self.import_caches.entry(vm_id.to_owned()).or_default();
        import_cache.ensure_materialized()?;
        Ok(import_cache.cache_path())
    }

    #[doc(hidden)]
    #[allow(dead_code)]
    pub fn import_cache_path_for_vm(&self, vm_id: &str) -> Option<&std::path::Path> {
        self.import_caches
            .get(vm_id)
            .map(NodeImportCache::cache_path)
    }
}

fn spawn_javascript_event_bridge(
    receiver: Receiver<JavascriptProcessEvent>,
    stderr_filter: Arc<Mutex<LinePrefixFilter>>,
    pending_sync_rpc: Arc<Mutex<Option<PendingSyncRpcState>>>,
    sync_rpc_responses: Option<JavascriptSyncRpcResponseWriter>,
    sync_rpc_timeout: Duration,
) -> UnboundedReceiver<JavascriptExecutionEvent> {
    let (sender, forwarded) = unbounded_channel();
    thread::spawn(move || {
        while let Ok(event) = receiver.recv() {
            let forwarded_event = match event {
                JavascriptProcessEvent::Stdout(chunk) => {
                    Some(JavascriptExecutionEvent::Stdout(chunk))
                }
                JavascriptProcessEvent::RawStderr(chunk) => {
                    let mut filter = match stderr_filter.lock() {
                        Ok(filter) => filter,
                        Err(_) => break,
                    };
                    let filtered = filter.filter_chunk(&chunk, CONTROLLED_STDERR_PREFIXES);
                    if filtered.is_empty() {
                        None
                    } else {
                        Some(JavascriptExecutionEvent::Stderr(filtered))
                    }
                }
                JavascriptProcessEvent::SyncRpcRequest(request) => {
                    if set_pending_sync_rpc_state(&pending_sync_rpc, request.id).is_err() {
                        break;
                    }
                    spawn_javascript_sync_rpc_timeout(
                        request.id,
                        sync_rpc_timeout,
                        pending_sync_rpc.clone(),
                        sync_rpc_responses.clone(),
                    );
                    Some(JavascriptExecutionEvent::SyncRpcRequest(request))
                }
                JavascriptProcessEvent::Control(NodeControlMessage::NodeImportCacheMetrics {
                    metrics,
                }) => Some(JavascriptExecutionEvent::Stderr(
                    format!(
                        "{}{}\n",
                        crate::node_import_cache::NODE_IMPORT_CACHE_METRICS_PREFIX,
                        serde_json::to_string(&metrics).unwrap_or_else(|_| String::from("{}"))
                    )
                    .into_bytes(),
                )),
                JavascriptProcessEvent::Control(NodeControlMessage::SignalState {
                    signal,
                    registration,
                }) => Some(JavascriptExecutionEvent::SignalState {
                    signal,
                    registration,
                }),
                JavascriptProcessEvent::Control(_) => None,
                JavascriptProcessEvent::Exited(code) => {
                    Some(JavascriptExecutionEvent::Exited(code))
                }
            };

            if let Some(event) = forwarded_event {
                if sender.send(event).is_err() {
                    break;
                }
            }
        }
    });
    forwarded
}

fn set_pending_sync_rpc_state(
    pending_sync_rpc: &Arc<Mutex<Option<PendingSyncRpcState>>>,
    id: u64,
) -> Result<(), JavascriptExecutionError> {
    let mut pending = pending_sync_rpc.lock().map_err(|_| {
        JavascriptExecutionError::RpcResponse(String::from(
            "sync RPC pending-request state lock poisoned",
        ))
    })?;
    *pending = Some(PendingSyncRpcState::Pending(id));
    Ok(())
}

#[cfg(feature = "legacy-js-tests")]
fn prewarm_node_import_path(
    import_cache: &NodeImportCache,
    context: &JavascriptContext,
    request: &StartJavascriptExecutionRequest,
    frozen_time_ms: u128,
) -> Result<Option<Vec<u8>>, JavascriptExecutionError> {
    let debug_enabled = env_flag_enabled(&request.env, NODE_WARMUP_DEBUG_ENV);

    let Some(_compile_cache_dir) = &context.compile_cache_dir else {
        return Ok(warmup_metrics_line(
            debug_enabled,
            false,
            "compile-cache-disabled",
            import_cache,
        ));
    };

    let marker_path = warmup_marker_path(
        import_cache.prewarm_marker_dir(),
        "node-import-prewarm",
        NODE_WARMUP_MARKER_VERSION,
        &warmup_marker_contents(),
    );
    if marker_path.exists() {
        return Ok(warmup_metrics_line(
            debug_enabled,
            false,
            "cached",
            import_cache,
        ));
    }

    let warmup_imports = NODE_WARMUP_SPECIFIERS
        .iter()
        .map(|specifier| (*specifier).to_string())
        .collect::<Vec<_>>();

    let mut command = Command::new(node_binary());
    configure_node_sandbox(&mut command, import_cache, context, request)?;
    command
        .arg("--import")
        .arg(import_cache.register_path())
        .arg("--import")
        .arg(import_cache.timing_bootstrap_path())
        .arg(import_cache.prewarm_path())
        .current_dir(&request.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env(
            NODE_PREWARM_IMPORTS_ENV,
            encode_json_string_array(&warmup_imports),
        );
    configure_node_command(&mut command, import_cache, context, frozen_time_ms)?;

    let output = command
        .output()
        .map_err(JavascriptExecutionError::WarmupSpawn)?;
    if !output.status.success() {
        return Err(JavascriptExecutionError::WarmupFailed {
            exit_code: output.status.code().unwrap_or(1),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }

    fs::write(&marker_path, warmup_marker_contents())
        .map_err(JavascriptExecutionError::PrepareImportCache)?;

    Ok(warmup_metrics_line(
        debug_enabled,
        true,
        "executed",
        import_cache,
    ))
}

#[cfg(feature = "legacy-js-tests")]
fn create_node_child(
    import_cache: &NodeImportCache,
    context: &JavascriptContext,
    request: &StartJavascriptExecutionRequest,
    frozen_time_ms: u128,
    control_fd: &std::os::fd::OwnedFd,
    sync_rpc_channels: Option<JavascriptSyncRpcChannels>,
) -> Result<
    (
        std::process::Child,
        Option<File>,
        Option<JavascriptSyncRpcResponseWriter>,
    ),
    JavascriptExecutionError,
> {
    let guest_argv = encode_json_string_array(&request.argv[1..]);
    let mut command = Command::new(node_binary());
    configure_node_sandbox(&mut command, import_cache, context, request)?;
    command
        .arg("--import")
        .arg(import_cache.register_path())
        .arg("--import")
        .arg(import_cache.timing_bootstrap_path())
        .arg(import_cache.runner_path())
        .current_dir(&request.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env(NODE_ENTRYPOINT_ENV, &request.argv[0]);

    apply_guest_env(&mut command, &request.env, RESERVED_NODE_ENV_KEYS);
    command.env(NODE_GUEST_ARGV_ENV, guest_argv);
    for key in [
        NODE_ALLOWED_BUILTINS_ENV,
        NODE_EXTRA_FS_READ_PATHS_ENV,
        NODE_EXTRA_FS_WRITE_PATHS_ENV,
        NODE_GUEST_ENTRYPOINT_ENV,
        NODE_GUEST_PATH_MAPPINGS_ENV,
        NODE_KEEP_STDIN_OPEN_ENV,
        NODE_LOOPBACK_EXEMPT_PORTS_ENV,
        NODE_VIRTUAL_PROCESS_EXEC_PATH_ENV,
        NODE_VIRTUAL_PROCESS_PID_ENV,
        NODE_VIRTUAL_PROCESS_PPID_ENV,
        NODE_VIRTUAL_PROCESS_UID_ENV,
        NODE_VIRTUAL_PROCESS_GID_ENV,
    ] {
        if let Some(value) = request.env.get(key) {
            command.env(key, value);
        }
    }
    command.env(
        NODE_PARENT_ALLOW_CHILD_PROCESS_ENV,
        if inherited_node_permission_enabled(&request.env, NODE_PARENT_ALLOW_CHILD_PROCESS_ENV)
            .unwrap_or_else(|| env_builtin_enabled(&request.env, "child_process"))
        {
            "1"
        } else {
            "0"
        },
    );
    command.env(
        NODE_PARENT_ALLOW_WORKER_ENV,
        if inherited_node_permission_enabled(&request.env, NODE_PARENT_ALLOW_WORKER_ENV)
            .unwrap_or_else(|| env_builtin_enabled(&request.env, "worker_threads"))
        {
            "1"
        } else {
            "0"
        },
    );

    if let Some(bootstrap_module) = &context.bootstrap_module {
        command.env(NODE_BOOTSTRAP_ENV, bootstrap_module);
    }

    let channels = sync_rpc_channels.expect("JavaScript sync RPC channels should be configured");
    let mut exported_fds = ExportedChildFds::default();
    command
        .env(NODE_SYNC_RPC_ENABLE_ENV, "1")
        .env(
            NODE_SYNC_RPC_DATA_BYTES_ENV,
            NODE_SYNC_RPC_DEFAULT_DATA_BYTES.to_string(),
        )
        .env(
            NODE_SYNC_RPC_WAIT_TIMEOUT_MS_ENV,
            javascript_sync_rpc_timeout(request).as_millis().to_string(),
        );
    exported_fds
        .export(
            &mut command,
            NODE_SYNC_RPC_REQUEST_FD_ENV,
            &channels.child_request_writer,
        )
        .map_err(|error| JavascriptExecutionError::RpcChannel(error.to_string()))?;
    exported_fds
        .export(
            &mut command,
            NODE_SYNC_RPC_RESPONSE_FD_ENV,
            &channels.child_response_reader,
        )
        .map_err(|error| JavascriptExecutionError::RpcChannel(error.to_string()))?;
    let (sync_rpc_request_reader, sync_rpc_response_writer) = (
        Some(channels.parent_request_reader),
        Some(JavascriptSyncRpcResponseWriter::new(
            channels.parent_response_writer,
            javascript_sync_rpc_timeout(request),
        )),
    );

    configure_node_control_channel(&mut command, control_fd, &mut exported_fds)
        .map_err(JavascriptExecutionError::Spawn)?;
    configure_node_command(&mut command, import_cache, context, frozen_time_ms)?;

    let child = command.spawn().map_err(JavascriptExecutionError::Spawn)?;
    Ok((child, sync_rpc_request_reader, sync_rpc_response_writer))
}

#[cfg(feature = "legacy-js-tests")]
fn configure_node_sandbox(
    command: &mut Command,
    import_cache: &NodeImportCache,
    context: &JavascriptContext,
    request: &StartJavascriptExecutionRequest,
) -> Result<(), JavascriptExecutionError> {
    let sandbox_root = sandbox_root(&request.env, &request.cwd);
    let cache_root = import_cache_root(import_cache, import_cache.asset_root());
    let mut read_paths = vec![cache_root.clone()];
    let mut write_paths = vec![cache_root, sandbox_root.clone()];

    if let Some(entrypoint_path) = resolve_path_like_specifier(&request.cwd, &request.argv[0]) {
        read_paths.push(entrypoint_path.clone());
        if let Some(parent) = entrypoint_path.parent() {
            read_paths.push(parent.to_path_buf());
        }
    }

    if let Some(bootstrap_module) = &context.bootstrap_module {
        if let Some(bootstrap_path) = resolve_path_like_specifier(&request.cwd, bootstrap_module) {
            read_paths.push(bootstrap_path);
        }
    }

    read_paths.extend(node_resolution_read_paths(
        std::iter::once(request.cwd.clone())
            .chain(
                resolve_path_like_specifier(&request.cwd, &request.argv[0])
                    .and_then(|path| path.parent().map(PathBuf::from)),
            )
            .chain(
                context
                    .bootstrap_module
                    .as_ref()
                    .and_then(|module| resolve_path_like_specifier(&request.cwd, module))
                    .and_then(|path| path.parent().map(PathBuf::from)),
            ),
    ));

    if let Some(compile_cache_dir) = &context.compile_cache_dir {
        read_paths.push(compile_cache_dir.clone());
        write_paths.push(compile_cache_dir.clone());
    }

    read_paths.extend(parse_env_path_list(
        &request.env,
        NODE_EXTRA_FS_READ_PATHS_ENV,
    ));
    write_paths.extend(parse_env_path_list(
        &request.env,
        NODE_EXTRA_FS_WRITE_PATHS_ENV,
    ));

    harden_node_command(
        command,
        &sandbox_root,
        &read_paths,
        &write_paths,
        true,
        false,
        inherited_node_permission_enabled(&request.env, NODE_PARENT_ALLOW_WORKER_ENV)
            .unwrap_or(true),
        inherited_node_permission_enabled(&request.env, NODE_PARENT_ALLOW_CHILD_PROCESS_ENV)
            .unwrap_or_else(|| env_builtin_enabled(&request.env, "child_process")),
    );
    Ok(())
}

#[cfg(feature = "legacy-js-tests")]
fn inherited_node_permission_enabled(env: &BTreeMap<String, String>, key: &str) -> Option<bool> {
    env.get(key).and_then(|value| match value.as_str() {
        "1" | "true" => Some(true),
        "0" | "false" => Some(false),
        _ => None,
    })
}

#[cfg(feature = "legacy-js-tests")]
fn parse_env_path_list(env: &BTreeMap<String, String>, key: &str) -> Vec<PathBuf> {
    env.get(key)
        .and_then(|value| from_str::<Vec<String>>(value).ok())
        .into_iter()
        .flatten()
        .map(PathBuf::from)
        .collect()
}

#[cfg(feature = "legacy-js-tests")]
fn configure_node_command(
    command: &mut Command,
    import_cache: &NodeImportCache,
    context: &JavascriptContext,
    frozen_time_ms: u128,
) -> Result<(), JavascriptExecutionError> {
    command
        .env(
            NODE_IMPORT_CACHE_LOADER_PATH_ENV,
            import_cache.loader_path(),
        )
        .env(NODE_IMPORT_CACHE_PATH_ENV, import_cache.cache_path())
        .env(NODE_IMPORT_CACHE_ASSET_ROOT_ENV, import_cache.asset_root())
        .env(NODE_FROZEN_TIME_ENV, frozen_time_ms.to_string());

    if let Some(compile_cache_dir) = &context.compile_cache_dir {
        configure_compile_cache(command, compile_cache_dir)
            .map_err(JavascriptExecutionError::PrepareImportCache)?;
    }

    Ok(())
}

#[cfg(feature = "legacy-js-tests")]
fn warmup_marker_contents() -> String {
    [
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION"),
        NODE_WARMUP_MARKER_VERSION,
        NODE_IMPORT_COMPILE_CACHE_NAMESPACE_VERSION,
    ]
    .into_iter()
    .chain(NODE_WARMUP_SPECIFIERS.iter().copied())
    .collect::<Vec<_>>()
    .join("\n")
}

#[cfg(feature = "legacy-js-tests")]
fn warmup_metrics_line(
    debug_enabled: bool,
    executed: bool,
    reason: &str,
    import_cache: &NodeImportCache,
) -> Option<Vec<u8>> {
    if !debug_enabled {
        return None;
    }

    Some(
        format!(
            "{NODE_WARMUP_METRICS_PREFIX}{{\"executed\":{},\"reason\":{},\"importCount\":{},\"assetRoot\":{}}}\n",
            if executed { "true" } else { "false" },
            encode_json_string(reason),
            NODE_WARMUP_SPECIFIERS.len(),
            encode_json_string(&import_cache.asset_root().display().to_string()),
        )
        .into_bytes(),
    )
}

fn resolve_node_import_compile_cache_dir(root_dir: PathBuf) -> PathBuf {
    root_dir.join(format!(
        "node-imports-v{NODE_IMPORT_COMPILE_CACHE_NAMESPACE_VERSION}-{:016x}",
        stable_compile_cache_namespace_hash()
    ))
}

fn stable_compile_cache_namespace_hash() -> u64 {
    stable_hash64(
        [
            env!("CARGO_PKG_NAME"),
            env!("CARGO_PKG_VERSION"),
            NODE_ENTRYPOINT_ENV,
            NODE_BOOTSTRAP_ENV,
            NODE_GUEST_ARGV_ENV,
            NODE_PREWARM_IMPORTS_ENV,
            NODE_WARMUP_MARKER_VERSION,
        ]
        .into_iter()
        .chain(NODE_WARMUP_SPECIFIERS.iter().copied())
        .collect::<Vec<_>>()
        .join("\n")
        .as_bytes(),
    )
}

fn create_javascript_sync_rpc_channels(
) -> Result<JavascriptSyncRpcChannels, JavascriptExecutionError> {
    let fd_reservations = (0..64)
        .map(|_| File::open("/dev/null"))
        .collect::<Result<Vec<_>, _>>()
        .map_err(JavascriptExecutionError::PrepareImportCache)?;
    let (parent_request_reader, child_request_writer) = pipe2(OFlag::O_CLOEXEC)
        .map_err(|error| JavascriptExecutionError::RpcChannel(error.to_string()))?;
    let (child_response_reader, parent_response_writer) = pipe2(OFlag::O_CLOEXEC)
        .map_err(|error| JavascriptExecutionError::RpcChannel(error.to_string()))?;
    drop(fd_reservations);

    Ok(JavascriptSyncRpcChannels {
        parent_request_reader: File::from(parent_request_reader),
        parent_response_writer: File::from(parent_response_writer),
        child_request_writer,
        child_response_reader,
    })
}

fn javascript_sync_rpc_timeout(request: &StartJavascriptExecutionRequest) -> Duration {
    let timeout_ms = request
        .env
        .get(NODE_SYNC_RPC_WAIT_TIMEOUT_MS_ENV)
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(NODE_SYNC_RPC_DEFAULT_WAIT_TIMEOUT_MS);
    Duration::from_millis(timeout_ms)
}

fn spawn_javascript_sync_rpc_timeout(
    id: u64,
    timeout: Duration,
    pending_state: Arc<Mutex<Option<PendingSyncRpcState>>>,
    responses: Option<JavascriptSyncRpcResponseWriter>,
) {
    let Some(responses) = responses else {
        return;
    };

    thread::spawn(move || {
        thread::sleep(timeout);

        let should_timeout = match pending_state.lock() {
            Ok(mut guard) if *guard == Some(PendingSyncRpcState::Pending(id)) => {
                *guard = Some(PendingSyncRpcState::TimedOut(id));
                true
            }
            Ok(_) => false,
            Err(_) => false,
        };

        if !should_timeout {
            return;
        }

        let _ = write_javascript_sync_rpc_response(
            &responses,
            json!({
                "id": id,
                "ok": false,
                "error": {
                    "code": "ERR_AGENT_OS_NODE_SYNC_RPC_TIMEOUT",
                    "message": format!(
                        "guest JavaScript sync RPC request {id} timed out after {}ms",
                        timeout.as_millis()
                    ),
                },
            }),
        );
    });
}

fn spawn_javascript_sync_rpc_reader(
    reader: File,
    sender: mpsc::Sender<JavascriptProcessEvent>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        let mut line = String::new();

        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => return,
                Ok(_) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }

                    match parse_javascript_sync_rpc_request(trimmed) {
                        Ok(request) => {
                            if sender
                                .send(JavascriptProcessEvent::SyncRpcRequest(request))
                                .is_err()
                            {
                                return;
                            }
                        }
                        Err(message) => {
                            if sender
                                .send(JavascriptProcessEvent::RawStderr(
                                    format!("{message}\n").into_bytes(),
                                ))
                                .is_err()
                            {
                                return;
                            }
                        }
                    }
                }
                Err(error) => {
                    let _ = sender.send(JavascriptProcessEvent::RawStderr(
                        format!("failed to read JavaScript sync RPC request: {error}\n")
                            .into_bytes(),
                    ));
                    return;
                }
            }
        }
    })
}

fn parse_javascript_sync_rpc_request(line: &str) -> Result<JavascriptSyncRpcRequest, String> {
    let wire: JavascriptSyncRpcRequestWire =
        serde_json::from_str(line).map_err(|error| error.to_string())?;
    Ok(JavascriptSyncRpcRequest {
        id: wire.id,
        method: wire.method,
        args: wire.args,
    })
}

fn write_javascript_sync_rpc_response(
    writer: &JavascriptSyncRpcResponseWriter,
    response: Value,
) -> Result<(), JavascriptExecutionError> {
    let mut payload = serde_json::to_vec(&response)
        .map_err(|error| JavascriptExecutionError::RpcResponse(error.to_string()))?;
    payload.push(b'\n');
    writer.send(payload)
}

fn spawn_javascript_sync_rpc_response_writer(
    writer: File,
    receiver: Receiver<Vec<u8>>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut writer = BufWriter::new(writer);
        while let Ok(payload) = receiver.recv() {
            if writer
                .write_all(&payload)
                .and_then(|()| writer.flush())
                .is_err()
            {
                return;
            }
        }
    })
}

/// Build the user code wrapper for V8 execution.
/// This wraps the entrypoint in a way that the V8 bridge can execute it.
fn build_v8_user_code(entrypoint: &str, env: &BTreeMap<String, String>) -> String {
    // The bridge code (polyfills) sets up the module system and globals.
    // User code is executed after the bridge completes.
    // For file-based entrypoints, we load and execute them through the module system.
    // For inline code (-e flag), we execute directly.
    if entrypoint == "-e" || entrypoint == "--eval" {
        // Inline code from NODE_EVAL or similar
        env.get("AGENT_OS_NODE_EVAL").cloned().unwrap_or_default()
    } else {
        // Module entrypoint - use require to load it
        format!(
            "require({});",
            serde_json::to_string(entrypoint).unwrap_or_else(|_| format!("\"{}\"", entrypoint))
        )
    }
}

fn resolve_v8_entrypoint(cwd: &Path, entrypoint: &str) -> String {
    if entrypoint == "-e" || entrypoint == "--eval" {
        return entrypoint.to_owned();
    }

    let path = Path::new(entrypoint);
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    resolved.to_string_lossy().into_owned()
}

fn prepend_v8_runtime_shim(
    user_code: String,
    entrypoint: &str,
    argv: &[String],
    cwd: &str,
    env: &BTreeMap<String, String>,
) -> String {
    let argv_json = serde_json::to_string(argv).unwrap_or_else(|_| String::from("[\"node\"]"));
    let entry_json =
        serde_json::to_string(entrypoint).unwrap_or_else(|_| String::from("\"/<entry>\""));
    let cwd_json = serde_json::to_string(cwd).unwrap_or_else(|_| String::from("\"/\""));
    let env_json = serde_json::to_string(env).unwrap_or_else(|_| String::from("{}"));

    format!(
        r#"(function () {{
  const nextArgv = {argv_json};
  const entryFile = {entry_json};
  const nextCwd = {cwd_json};
  const nextEnv = {env_json};
  const visibleEnv = Object.fromEntries(
    Object.entries(nextEnv).filter(([key]) => !key.startsWith("AGENT_OS_"))
  );

  if (typeof process !== "undefined") {{
    process.argv = nextArgv;
    process.argv0 = nextArgv[0] || "node";
    process.env = {{
      ...(process.env || {{}}),
      ...visibleEnv,
    }};
    const nextPid = Number(nextEnv.AGENT_OS_VIRTUAL_PROCESS_PID);
    if (Number.isFinite(nextPid) && nextPid > 0) {{
      process.pid = nextPid;
    }}
    const nextPpid = Number(nextEnv.AGENT_OS_VIRTUAL_PROCESS_PPID);
    if (Number.isFinite(nextPpid) && nextPpid >= 0) {{
      process.ppid = nextPpid;
    }}
    if (typeof nextEnv.AGENT_OS_VIRTUAL_PROCESS_EXEC_PATH === "string" && nextEnv.AGENT_OS_VIRTUAL_PROCESS_EXEC_PATH.length > 0) {{
      process.execPath = nextEnv.AGENT_OS_VIRTUAL_PROCESS_EXEC_PATH;
    }}
    process.cwd = () => nextCwd;
    process._cwd = nextCwd;
    if (typeof process.getBuiltinModule !== "function") {{
      process.getBuiltinModule = function(specifier) {{
        return globalThis.require ? globalThis.require(specifier) : undefined;
      }};
    }}
  }}

  globalThis.__runtimeStreamStdin = nextEnv.AGENT_OS_KEEP_STDIN_OPEN === "1";

  if (
    typeof globalThis.require === "undefined" &&
    typeof globalThis._moduleModule?.createRequire === "function"
  ) {{
    globalThis.require = globalThis._moduleModule.createRequire(entryFile);
  }}
}})();
{user_code}"#
    )
}

/// Spawn a V8 event bridge thread that converts V8 BinaryFrame messages
/// into JavascriptExecutionEvent for the sidecar event loop.
///
/// Internal bridge calls (module loading, logging, timers) are handled locally
/// by the event bridge. Kernel operations (fs, net, child_process, dns) are
/// forwarded to the sidecar via SyncRpcRequest events.
fn spawn_v8_event_bridge(
    frame_receiver: mpsc::Receiver<BinaryFrame>,
    pending_sync_rpc: Arc<Mutex<Option<PendingSyncRpcState>>>,
    _sync_rpc_timeout: Duration,
    v8_session: V8SessionHandle,
    mut local_bridge: LocalBridgeState,
) -> UnboundedReceiver<JavascriptExecutionEvent> {
    let (sender, receiver) = unbounded_channel();

    thread::spawn(move || {
        while let Ok(frame) = frame_receiver.recv() {
            let event = match frame {
                BinaryFrame::BridgeCall {
                    call_id,
                    method,
                    payload,
                    ..
                } => {
                    // Convert CBOR payload to JSON args
                    let args = v8_runtime::cbor_payload_to_json_args(&payload).unwrap_or_default();

                    // Check if this is an internal bridge call we handle locally
                    if let Some(response) =
                        local_bridge.handle_internal_bridge_call(call_id, &method, &args)
                    {
                        if let LocalBridgeCallResult::Immediate(response) = response {
                            let cbor_payload =
                                v8_runtime::json_to_cbor_payload(&response).unwrap_or_default();
                            let _ = v8_session.send_bridge_response(call_id, 0, cbor_payload);
                        }
                        continue;
                    }

                    // Handle logging locally (produce stdout/stderr events)
                    if method == "_log" || method == "_error" {
                        let msg = args
                            .iter()
                            .map(|a| match a {
                                Value::String(s) => s.clone(),
                                other => other.to_string(),
                            })
                            .collect::<Vec<_>>()
                            .join(" ");
                        let msg_with_newline = format!("{msg}\n");
                        // Respond to the bridge call
                        let _ = v8_session.send_bridge_response(
                            call_id,
                            0,
                            v8_runtime::json_to_cbor_payload(&Value::Null).unwrap_or_default(),
                        );
                        if method == "_log" {
                            let _ = sender.send(JavascriptExecutionEvent::Stdout(
                                msg_with_newline.into_bytes(),
                            ));
                        } else {
                            let _ = sender.send(JavascriptExecutionEvent::Stderr(
                                msg_with_newline.into_bytes(),
                            ));
                        }
                        continue;
                    }

                    // Map the bridge method name to the sidecar sync RPC method name
                    let (sidecar_method, _needs_translation) =
                        v8_runtime::map_bridge_method(&method);

                    // Track pending sync RPC
                    if let Ok(mut pending) = pending_sync_rpc.lock() {
                        *pending = Some(PendingSyncRpcState::Pending(call_id));
                    }

                    Some(JavascriptExecutionEvent::SyncRpcRequest(
                        JavascriptSyncRpcRequest {
                            id: call_id,
                            method: sidecar_method.to_owned(),
                            args: translate_request_args_for_legacy(sidecar_method, &args),
                        },
                    ))
                }
                BinaryFrame::Log {
                    channel, message, ..
                } => {
                    if channel == 0 {
                        Some(JavascriptExecutionEvent::Stdout(message.into_bytes()))
                    } else {
                        Some(JavascriptExecutionEvent::Stderr(message.into_bytes()))
                    }
                }
                BinaryFrame::ExecutionResult {
                    exit_code, error, ..
                } => {
                    if let Some(err) = &error {
                        let error_msg = if err.stack.is_empty() {
                            format!("{}: {}\n", err.error_type, err.message)
                        } else {
                            format!("{}\n", err.stack)
                        };
                        let _ =
                            sender.send(JavascriptExecutionEvent::Stderr(error_msg.into_bytes()));
                    }
                    Some(JavascriptExecutionEvent::Exited(exit_code))
                }
                BinaryFrame::StreamCallback { .. } => None,
                _ => None,
            };

            if let Some(event) = event {
                if sender.send(event).is_err() {
                    break;
                }
            }
        }
    });

    receiver
}

/// Handle internal bridge calls that don't need to go to the sidecar.
/// Returns Some(response) if handled locally, None if it should be forwarded.
impl LocalBridgeState {
    fn handle_internal_bridge_call(
        &mut self,
        call_id: u64,
        method: &str,
        args: &[Value],
    ) -> Option<LocalBridgeCallResult> {
        match method {
            "_resolveModule" | "_resolveModuleSync" => {
                let specifier = args.first().and_then(Value::as_str).unwrap_or("");
                let parent = args.get(1).and_then(Value::as_str).unwrap_or("/");
                let mode = match args.get(2).and_then(Value::as_str) {
                    Some("import") => ModuleResolveMode::Import,
                    Some("require") => ModuleResolveMode::Require,
                    _ if method == "_resolveModule" => ModuleResolveMode::Import,
                    _ => ModuleResolveMode::Require,
                };
                Some(LocalBridgeCallResult::Immediate(
                    self.resolve_module(specifier, parent, mode)
                        .map(Value::String)
                        .unwrap_or(Value::Null),
                ))
            }
            "_loadFile" | "_loadFileSync" => Some(LocalBridgeCallResult::Immediate(
                self.load_file(args.first().and_then(Value::as_str).unwrap_or(""))
                    .map(Value::String)
                    .unwrap_or(Value::Null),
            )),
            "_batchResolveModules" => Some(LocalBridgeCallResult::Immediate(
                self.batch_resolve_modules(args),
            )),
            "_loadPolyfill" => Some(LocalBridgeCallResult::Immediate(
                self.handle_polyfill_dispatch(args),
            )),
            "_cryptoRandomFill" => {
                let size = args.first().and_then(Value::as_u64).unwrap_or(16) as usize;
                let mut bytes = vec![0u8; size];
                let seed = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos();
                for (i, byte) in bytes.iter_mut().enumerate() {
                    *byte = ((seed >> (i % 16 * 8)) & 0xFF) as u8 ^ (i as u8);
                }
                Some(LocalBridgeCallResult::Immediate(json!({
                    "__type": "Buffer",
                    "data": v8_runtime::base64_encode_pub(&bytes)
                })))
            }
            "_cryptoRandomUUID" => {
                let seed = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos();
                Some(LocalBridgeCallResult::Immediate(Value::String(format!(
                    "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
                    (seed >> 96) as u32,
                    (seed >> 80) as u16,
                    (seed >> 64) as u16 & 0x0FFF,
                    ((seed >> 48) as u16 & 0x3FFF) | 0x8000,
                    seed as u64 & 0xFFFFFFFFFFFF,
                ))))
            }
            "_scheduleTimer" => {
                self.schedule_bridge_timer_response(call_id, timer_delay_ms(args.first()));
                Some(LocalBridgeCallResult::Deferred)
            }
            "_ptySetRawMode" => Some(LocalBridgeCallResult::Immediate(Value::Null)),
            _ => None,
        }
    }

    fn handle_polyfill_dispatch(&mut self, args: &[Value]) -> Value {
        let Some(dispatch) = args.first().and_then(Value::as_str) else {
            return Value::String(String::new());
        };
        if !dispatch.starts_with("__bd:") {
            return Value::String(String::new());
        }
        let (dispatch_method, payload_json) = dispatch
            .strip_prefix("__bd:")
            .and_then(|value| value.split_once(':'))
            .unwrap_or(("", "[]"));
        let payload = serde_json::from_str::<Value>(payload_json).unwrap_or_else(|_| json!([]));
        let args = payload.as_array().cloned().unwrap_or_default();
        let result = match dispatch_method {
            "kernelHandleRegister" => {
                if let (Some(id), Some(description)) = (
                    args.first().and_then(Value::as_str),
                    args.get(1).and_then(Value::as_str),
                ) {
                    self.handle_descriptions
                        .insert(id.to_owned(), description.to_owned());
                }
                Value::Null
            }
            "kernelHandleUnregister" => {
                if let Some(id) = args.first().and_then(Value::as_str) {
                    self.handle_descriptions.remove(id);
                }
                Value::Null
            }
            "kernelHandleList" => Value::Array(
                self.handle_descriptions
                    .iter()
                    .map(|(id, description)| {
                        json!({
                            "id": id,
                            "description": description,
                        })
                    })
                    .collect(),
            ),
            "kernelTimerCreate" => {
                let delay_ms = timer_delay_ms(args.first());
                let repeat = args.get(1).and_then(Value::as_bool).unwrap_or(false);
                json!(self.create_kernel_timer(delay_ms, repeat))
            }
            "kernelTimerArm" => {
                if let Some(timer_id) = args.first().and_then(Value::as_u64) {
                    self.arm_kernel_timer(timer_id);
                }
                Value::Null
            }
            "kernelTimerClear" => {
                if let Some(timer_id) = args.first().and_then(Value::as_u64) {
                    self.clear_kernel_timer(timer_id);
                }
                Value::Null
            }
            _ => json!({
                "__bd_error": {
                    "name": "Error",
                    "message": format!("No handler: {dispatch_method}"),
                }
            }),
        };

        if dispatch_method.starts_with("kernel") {
            Value::String(
                serde_json::to_string(&json!({ "__bd_result": result }))
                    .unwrap_or_else(|_| String::from("{\"__bd_result\":null}")),
            )
        } else {
            Value::String(
                serde_json::to_string(&json!({
                    "__bd_error": {
                        "name": "Error",
                        "message": format!("No handler: {dispatch_method}"),
                    }
                }))
                .unwrap_or_else(|_| {
                    String::from(
                        "{\"__bd_error\":{\"name\":\"Error\",\"message\":\"dispatch failed\"}}",
                    )
                }),
            )
        }
    }

    fn create_kernel_timer(&mut self, delay_ms: u64, repeat: bool) -> u64 {
        self.next_timer_id += 1;
        if let Ok(mut timers) = self.timers.lock() {
            timers.insert(
                self.next_timer_id,
                LocalTimerEntry {
                    delay_ms,
                    generation: 0,
                    repeat,
                },
            );
        }
        self.next_timer_id
    }

    fn arm_kernel_timer(&self, timer_id: u64) {
        let Some(session) = self.v8_session.clone() else {
            return;
        };

        let Some((delay_ms, generation, timers)) =
            self.timers.lock().ok().and_then(|mut timers| {
                let entry = timers.get_mut(&timer_id)?;
                entry.generation = entry.generation.wrapping_add(1);
                Some((entry.delay_ms, entry.generation, self.timers.clone()))
            })
        else {
            return;
        };

        thread::spawn(move || {
            if delay_ms > 0 {
                thread::sleep(Duration::from_millis(delay_ms));
            }

            let should_fire = timers
                .lock()
                .ok()
                .and_then(|mut timers| {
                    let (current_generation, repeat) = timers
                        .get(&timer_id)
                        .map(|entry| (entry.generation, entry.repeat))?;
                    if current_generation != generation {
                        return Some(false);
                    }
                    if !repeat {
                        timers.remove(&timer_id);
                    }
                    Some(true)
                })
                .unwrap_or(false);
            if !should_fire {
                return;
            }

            let payload = v8_runtime::json_to_cbor_payload(&json!(timer_id)).unwrap_or_default();
            let _ = session.send_stream_event("timer", payload);
        });
    }

    fn clear_kernel_timer(&self, timer_id: u64) {
        if let Ok(mut timers) = self.timers.lock() {
            timers.remove(&timer_id);
        }
    }

    fn schedule_bridge_timer_response(&self, call_id: u64, delay_ms: u64) {
        let Some(session) = self.v8_session.clone() else {
            return;
        };

        thread::spawn(move || {
            if delay_ms > 0 {
                thread::sleep(Duration::from_millis(delay_ms));
            }
            let _ = session.send_bridge_response(call_id, 0, Vec::new());
        });
    }

    fn batch_resolve_modules(&mut self, args: &[Value]) -> Value {
        let requests = args
            .first()
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Value::Array(
            requests
                .into_iter()
                .map(|request| {
                    let pair = request.as_array().cloned().unwrap_or_default();
                    let specifier = pair.first().and_then(Value::as_str).unwrap_or("");
                    let referrer = pair.get(1).and_then(Value::as_str).unwrap_or("/");
                    self.resolve_module(specifier, referrer, ModuleResolveMode::Import)
                        .and_then(|resolved| {
                            self.load_file(&resolved).map(|source| {
                                json!({
                                    "resolved": resolved,
                                    "source": source,
                                })
                            })
                        })
                        .unwrap_or(Value::Null)
                })
                .collect(),
        )
    }

    fn resolve_module(
        &mut self,
        specifier: &str,
        from_dir: &str,
        mode: ModuleResolveMode,
    ) -> Option<String> {
        let normalized_from = normalize_module_resolve_context(from_dir);
        let cache_key = (specifier.to_owned(), normalized_from.clone(), mode);
        if let Some(cached) = self.resolution_cache.resolve_results.get(&cache_key) {
            return cached.clone();
        }

        let resolved = if let Some(builtin) = normalize_builtin_specifier(specifier) {
            Some(builtin)
        } else if specifier.starts_with('/') {
            self.resolve_path(specifier, mode)
        } else if specifier.starts_with("./")
            || specifier.starts_with("../")
            || specifier == "."
            || specifier == ".."
        {
            self.resolve_path(&join_guest_path(&normalized_from, specifier), mode)
        } else if specifier.starts_with('#') {
            self.resolve_package_imports(specifier, &normalized_from, mode)
        } else {
            self.resolve_node_modules(specifier, &normalized_from, mode)
        };

        self.resolution_cache
            .resolve_results
            .insert(cache_key, resolved.clone());
        resolved
    }

    fn load_file(&mut self, path: &str) -> Option<String> {
        let bare = path.trim_start_matches("node:");
        if is_builtin_specifier(path) {
            return Some(build_builtin_module_wrapper(bare));
        }

        let host_path = self.translator.guest_to_host(path)?;
        fs::read_to_string(host_path).ok()
    }

    fn resolve_package_imports(
        &mut self,
        request: &str,
        from_dir: &str,
        mode: ModuleResolveMode,
    ) -> Option<String> {
        let mut dir = normalize_guest_path(from_dir);
        loop {
            let pkg_json_path = join_guest_path(&dir, "package.json");
            if let Some(pkg_json) = self.read_package_json(&pkg_json_path) {
                if let Some(imports) = &pkg_json.imports {
                    if let Some(target) = resolve_imports_target(imports, request, mode) {
                        let target_path = if target.starts_with('/') {
                            target
                        } else {
                            join_guest_path(&dir, &target)
                        };
                        return self.resolve_path(&target_path, mode);
                    }
                    return None;
                }
            }
            if dir == "/" {
                break;
            }
            dir = dirname_guest_path(&dir);
        }
        None
    }

    fn resolve_node_modules(
        &mut self,
        request: &str,
        from_dir: &str,
        mode: ModuleResolveMode,
    ) -> Option<String> {
        let (package_name, subpath) = split_package_request(request)?;
        let mut dir = normalize_guest_path(from_dir);
        loop {
            for package_dir in node_modules_candidate_dirs(&dir, package_name) {
                if let Some(entry) =
                    self.resolve_package_entry_from_dir(&package_dir, subpath, mode)
                {
                    return Some(entry);
                }
            }
            if dir == "/" {
                break;
            }
            dir = dirname_guest_path(&dir);
        }

        self.resolve_package_entry_from_dir(
            &join_guest_path("/node_modules", package_name),
            subpath,
            mode,
        )
    }

    fn resolve_package_entry_from_dir(
        &mut self,
        package_dir: &str,
        subpath: &str,
        mode: ModuleResolveMode,
    ) -> Option<String> {
        let package_json_path = join_guest_path(package_dir, "package.json");
        let pkg_json = self.read_package_json(&package_json_path);
        if pkg_json.is_none() && !self.cached_exists(package_dir) {
            return None;
        }

        if let Some(pkg_json) = pkg_json.as_ref() {
            if let Some(exports) = &pkg_json.exports {
                let exports_subpath = if subpath.is_empty() {
                    String::from(".")
                } else {
                    format!("./{subpath}")
                };
                let exports_target = resolve_exports_target(exports, &exports_subpath, mode)?;
                let target_path = join_guest_path(package_dir, &exports_target);
                return self.resolve_path(&target_path, mode).or(Some(target_path));
            }
        }

        if !subpath.is_empty() {
            return self.resolve_path(&join_guest_path(package_dir, subpath), mode);
        }

        let entry_field = pkg_json
            .as_ref()
            .and_then(|pkg_json| pkg_json.main.as_deref())
            .unwrap_or("index.js");
        let entry_path = join_guest_path(package_dir, entry_field);
        self.resolve_path(&entry_path, mode)
            .or_else(|| self.resolve_path(&join_guest_path(package_dir, "index"), mode))
    }

    fn resolve_path(&mut self, base_path: &str, mode: ModuleResolveMode) -> Option<String> {
        if self.cached_stat(base_path) == Some(false) {
            return Some(normalize_guest_path(base_path));
        }

        for extension in [".js", ".json", ".mjs", ".cjs"] {
            let candidate = format!("{}{}", normalize_guest_path(base_path), extension);
            if self.cached_exists(&candidate) {
                return Some(candidate);
            }
        }

        if self.cached_stat(base_path) == Some(true) {
            let pkg_json_path = join_guest_path(base_path, "package.json");
            if let Some(pkg_json) = self.read_package_json(&pkg_json_path) {
                if let Some(main) = pkg_json.main.as_deref() {
                    let entry_path = join_guest_path(base_path, main);
                    if entry_path != normalize_guest_path(base_path) {
                        if let Some(entry) = self.resolve_path(&entry_path, mode) {
                            return Some(entry);
                        }
                    }
                }
                if mode == ModuleResolveMode::Import
                    && pkg_json.package_type.as_deref() == Some("module")
                    && self.cached_exists(&join_guest_path(base_path, "index.js"))
                {
                    return Some(join_guest_path(base_path, "index.js"));
                }
            }

            for extension in [".js", ".json", ".mjs", ".cjs"] {
                let index_path = join_guest_path(base_path, &format!("index{extension}"));
                if self.cached_exists(&index_path) {
                    return Some(index_path);
                }
            }
        }

        None
    }

    fn read_package_json(&mut self, guest_path: &str) -> Option<LocalPackageJson> {
        if let Some(cached) = self
            .resolution_cache
            .package_json_results
            .get(guest_path)
            .cloned()
        {
            return cached;
        }

        let parsed = self
            .translator
            .guest_to_host(guest_path)
            .and_then(|host_path| fs::read_to_string(host_path).ok())
            .and_then(|contents| serde_json::from_str::<LocalPackageJson>(&contents).ok());
        self.resolution_cache
            .package_json_results
            .insert(guest_path.to_owned(), parsed.clone());
        parsed
    }

    fn cached_exists(&mut self, guest_path: &str) -> bool {
        if let Some(cached) = self.resolution_cache.exists_results.get(guest_path) {
            return *cached;
        }
        let exists = self
            .translator
            .guest_to_host(guest_path)
            .map(|host_path| host_path.exists())
            .unwrap_or(false);
        self.resolution_cache
            .exists_results
            .insert(guest_path.to_owned(), exists);
        exists
    }

    fn cached_stat(&mut self, guest_path: &str) -> Option<bool> {
        if let Some(cached) = self.resolution_cache.stat_results.get(guest_path) {
            return *cached;
        }
        let result = self
            .translator
            .guest_to_host(guest_path)
            .and_then(|host_path| fs::metadata(host_path).ok())
            .map(|metadata| metadata.is_dir());
        self.resolution_cache
            .stat_results
            .insert(guest_path.to_owned(), result);
        result
    }
}

fn normalize_module_resolve_context(path: &str) -> String {
    let normalized = normalize_guest_path(path);
    if normalized.ends_with(".js")
        || normalized.ends_with(".mjs")
        || normalized.ends_with(".cjs")
        || normalized.ends_with(".json")
        || normalized.ends_with(".ts")
        || normalized.ends_with(".mts")
        || normalized.ends_with(".cts")
    {
        dirname_guest_path(&normalized)
    } else {
        normalized
    }
}

fn dirname_guest_path(path: &str) -> String {
    let normalized = normalize_guest_path(path);
    if normalized == "/" {
        return normalized;
    }
    normalized
        .rsplit_once('/')
        .map(|(parent, _)| {
            if parent.is_empty() {
                String::from("/")
            } else {
                parent.to_owned()
            }
        })
        .unwrap_or_else(|| String::from("/"))
}

fn normalize_builtin_specifier(specifier: &str) -> Option<String> {
    let bare = specifier.trim_start_matches("node:");
    match bare {
        "child_process" | "crypto" | "dgram" | "dns" | "events" | "fs" | "fs/promises" | "http"
        | "http2" | "https" | "module" | "net" | "os" | "path" | "process" | "stream" | "tls"
        | "tty" | "url" | "util" | "zlib" => Some(format!("node:{bare}")),
        _ => None,
    }
}

fn is_builtin_specifier(specifier: &str) -> bool {
    normalize_builtin_specifier(specifier).is_some()
}

fn build_builtin_module_wrapper(module_name: &str) -> String {
    let default_target = format!(
        "globalThis._requireFrom({}, \"/\")",
        serde_json::to_string(&format!("node:{module_name}"))
            .unwrap_or_else(|_| format!("\"node:{module_name}\""))
    );
    let mut exports = builtin_named_exports(module_name)
        .into_iter()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    exports.sort_unstable();

    let mut source = format!("const _m = {default_target};\nexport default _m;\n");
    for name in exports {
        source.push_str(&format!("export const {name} = _m[\"{name}\"];\n"));
    }
    source
}

fn builtin_named_exports(module_name: &str) -> &'static [&'static str] {
    match module_name {
        "events" => &["EventEmitter", "once"],
        "fs" => &["constants", "promises", "readFileSync"],
        "fs/promises" => &["access", "open", "readFile", "writeFile"],
        "http" => &[
            "Agent",
            "METHODS",
            "STATUS_CODES",
            "createServer",
            "request",
        ],
        "http2" => &["connect", "createServer", "createSecureServer"],
        "https" => &["Agent", "createServer", "request"],
        "net" => &[
            "Socket",
            "Server",
            "connect",
            "createConnection",
            "createServer",
        ],
        "os" => &[
            "EOL",
            "availableParallelism",
            "cpus",
            "homedir",
            "hostname",
            "tmpdir",
        ],
        "path" => &["basename", "dirname", "join", "resolve", "sep"],
        "tls" => &[
            "TLSSocket",
            "Server",
            "connect",
            "createSecureContext",
            "createServer",
            "getCiphers",
        ],
        "url" => &["URL", "fileURLToPath", "pathToFileURL"],
        _ => &[],
    }
}

fn split_package_request(request: &str) -> Option<(&str, &str)> {
    if request.starts_with('@') {
        let mut parts = request.splitn(3, '/');
        let scope = parts.next()?;
        let name = parts.next()?;
        let package_name = &request[..scope.len() + 1 + name.len()];
        let subpath = parts.next().unwrap_or("");
        Some((package_name, subpath))
    } else {
        request
            .split_once('/')
            .map(|(package, subpath)| (package, subpath))
            .or(Some((request, "")))
    }
}

fn node_modules_candidate_dirs(dir: &str, package_name: &str) -> Vec<String> {
    let mut candidates = HashSet::new();
    candidates.insert(join_guest_path(
        dir,
        &format!("node_modules/{package_name}"),
    ));
    candidates.insert(join_guest_path(
        dir,
        &format!("node_modules/.pnpm/node_modules/{package_name}"),
    ));
    if dir == "/node_modules" || dir.ends_with("/node_modules") {
        candidates.insert(join_guest_path(dir, package_name));
    }
    if let Some(index) = dir.rfind("/node_modules/") {
        let root = &dir[..index + "/node_modules".len()];
        candidates.insert(join_guest_path(
            root,
            &format!(".pnpm/node_modules/{package_name}"),
        ));
    }
    let mut candidates = candidates.into_iter().collect::<Vec<_>>();
    candidates.sort();
    candidates
}

fn resolve_exports_target(
    exports_field: &Value,
    subpath: &str,
    mode: ModuleResolveMode,
) -> Option<String> {
    match exports_field {
        Value::String(value) => (subpath == ".").then(|| value.clone()),
        Value::Array(values) => values
            .iter()
            .find_map(|value| resolve_exports_target(value, subpath, mode)),
        Value::Object(record) => {
            if subpath == "." && !record.keys().any(|key| key.starts_with("./")) {
                return resolve_conditional_target(record, mode);
            }
            if let Some(value) = record.get(subpath) {
                return resolve_exports_target(value, ".", mode);
            }
            for (key, value) in record {
                if let Some((prefix, suffix)) = key.split_once('*') {
                    if subpath.starts_with(prefix) && subpath.ends_with(suffix) {
                        let wildcard = &subpath[prefix.len()..subpath.len() - suffix.len()];
                        let resolved = resolve_exports_target(value, ".", mode)?;
                        return Some(resolved.replace('*', wildcard));
                    }
                }
            }
            if subpath == "." {
                record
                    .get(".")
                    .and_then(|value| resolve_exports_target(value, ".", mode))
            } else {
                None
            }
        }
        _ => None,
    }
}

fn resolve_conditional_target(
    record: &serde_json::Map<String, Value>,
    mode: ModuleResolveMode,
) -> Option<String> {
    let order: &[&str] = match mode {
        ModuleResolveMode::Import => &["import", "node", "module", "default", "require"],
        ModuleResolveMode::Require => &["require", "node", "default", "import", "module"],
    };
    for key in order {
        if let Some(value) = record.get(*key) {
            if let Some(resolved) = resolve_exports_target(value, ".", mode) {
                return Some(resolved);
            }
        }
    }
    record
        .values()
        .find_map(|value| resolve_exports_target(value, ".", mode))
}

fn resolve_imports_target(
    imports_field: &Value,
    specifier: &str,
    mode: ModuleResolveMode,
) -> Option<String> {
    match imports_field {
        Value::String(value) => Some(value.clone()),
        Value::Array(values) => values
            .iter()
            .find_map(|value| resolve_imports_target(value, specifier, mode)),
        Value::Object(record) => {
            if let Some(value) = record.get(specifier) {
                return resolve_exports_target(value, ".", mode);
            }
            for (key, value) in record {
                if let Some((prefix, suffix)) = key.split_once('*') {
                    if specifier.starts_with(prefix) && specifier.ends_with(suffix) {
                        let wildcard = &specifier[prefix.len()..specifier.len() - suffix.len()];
                        let resolved = resolve_exports_target(value, ".", mode)?;
                        return Some(resolved.replace('*', wildcard));
                    }
                }
            }
            None
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nix::fcntl::OFlag;
    use nix::unistd::pipe2;
    use serde_json::Value;
    use std::io::BufRead;

    #[test]
    fn javascript_sync_rpc_timeout_writes_clear_error_response() {
        let (reader_fd, writer_fd) = pipe2(OFlag::O_CLOEXEC).expect("create pipe");
        let reader = File::from(reader_fd);
        let writer = File::from(writer_fd);
        let response_writer =
            JavascriptSyncRpcResponseWriter::new(writer, Duration::from_millis(50));
        let pending = Arc::new(Mutex::new(Some(PendingSyncRpcState::Pending(7))));

        spawn_javascript_sync_rpc_timeout(
            7,
            Duration::from_millis(20),
            pending.clone(),
            Some(response_writer),
        );

        let mut line = String::new();
        let mut reader = BufReader::new(reader);
        reader.read_line(&mut line).expect("read timeout response");

        let response: Value = serde_json::from_str(line.trim()).expect("parse timeout response");
        assert_eq!(response["id"], Value::from(7));
        assert_eq!(response["ok"], Value::from(false));
        assert_eq!(
            response["error"]["code"],
            Value::String(String::from("ERR_AGENT_OS_NODE_SYNC_RPC_TIMEOUT"))
        );
        assert!(response["error"]["message"]
            .as_str()
            .expect("timeout message")
            .contains("timed out after 20ms"));
        assert_eq!(
            *pending.lock().expect("pending state lock"),
            Some(PendingSyncRpcState::TimedOut(7))
        );
    }

    #[test]
    fn javascript_sync_rpc_response_writer_times_out_when_queue_is_full() {
        let (sender, _receiver) = mpsc::sync_channel(1);
        let writer = JavascriptSyncRpcResponseWriter {
            sender,
            timeout: Duration::from_millis(30),
        };

        writer
            .send(b"first\n".to_vec())
            .expect("queue first response");

        let started = Instant::now();
        let error = writer
            .send(b"second\n".to_vec())
            .expect_err("full queue should time out");
        assert!(
            started.elapsed() >= Duration::from_millis(30),
            "send should wait for the configured timeout"
        );
        assert!(error
            .to_string()
            .contains("timed out after 30ms while queueing JavaScript sync RPC response"));
    }
}
