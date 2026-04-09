use crate::common::{encode_json_string, frozen_time_ms};
use crate::node_import_cache::{
    NodeImportCache, NodeImportCacheCleanup, NODE_IMPORT_CACHE_ASSET_ROOT_ENV,
};
use crate::node_process::{
    apply_guest_env, configure_node_control_channel, create_node_control_channel,
    ensure_host_cwd_exists, harden_node_command, node_binary, spawn_node_control_reader,
    spawn_stream_reader, ExportedChildFds, LinePrefixFilter, NodeControlMessage,
};
use crate::runtime_support::{
    compile_cache_ready, configure_compile_cache, env_flag_enabled, file_fingerprint,
    import_cache_root, resolve_execution_path, sandbox_root, warmup_marker_path,
    NODE_COMPILE_CACHE_ENV, NODE_DISABLE_COMPILE_CACHE_ENV, NODE_FROZEN_TIME_ENV,
    NODE_SANDBOX_ROOT_ENV,
};
use nix::fcntl::OFlag;
use nix::unistd::pipe2;
use serde::Deserialize;
use serde_json::json;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::{
    error::TryRecvError as TokioTryRecvError, unbounded_channel, UnboundedReceiver,
};
use tokio::time;
const NODE_ALLOWED_BUILTINS_ENV: &str = "AGENT_OS_ALLOWED_NODE_BUILTINS";
const NODE_ALLOW_PROCESS_BINDINGS_ENV: &str = "AGENT_OS_ALLOW_PROCESS_BINDINGS";
const NODE_IMPORT_CACHE_PATH_ENV: &str = "AGENT_OS_NODE_IMPORT_CACHE_PATH";
const PYODIDE_INDEX_URL_ENV: &str = "AGENT_OS_PYODIDE_INDEX_URL";
const PYODIDE_PACKAGE_BASE_URL_ENV: &str = "AGENT_OS_PYODIDE_PACKAGE_BASE_URL";
const PYTHON_CODE_ENV: &str = "AGENT_OS_PYTHON_CODE";
const PYTHON_FILE_ENV: &str = "AGENT_OS_PYTHON_FILE";
const PYTHON_PREWARM_ONLY_ENV: &str = "AGENT_OS_PYTHON_PREWARM_ONLY";
const PYTHON_WARMUP_DEBUG_ENV: &str = "AGENT_OS_PYTHON_WARMUP_DEBUG";
const PYTHON_WARMUP_METRICS_PREFIX: &str = "__AGENT_OS_PYTHON_WARMUP_METRICS__:";
const PYTHON_OUTPUT_BUFFER_MAX_BYTES_ENV: &str = "AGENT_OS_PYTHON_OUTPUT_BUFFER_MAX_BYTES";
const PYTHON_EXECUTION_TIMEOUT_MS_ENV: &str = "AGENT_OS_PYTHON_EXECUTION_TIMEOUT_MS";
const PYTHON_MAX_OLD_SPACE_MB_ENV: &str = "AGENT_OS_PYTHON_MAX_OLD_SPACE_MB";
const PYTHON_VFS_RPC_REQUEST_FD_ENV: &str = "AGENT_OS_PYTHON_VFS_RPC_REQUEST_FD";
const PYTHON_VFS_RPC_RESPONSE_FD_ENV: &str = "AGENT_OS_PYTHON_VFS_RPC_RESPONSE_FD";
const PYTHON_VFS_RPC_TIMEOUT_MS_ENV: &str = "AGENT_OS_PYTHON_VFS_RPC_TIMEOUT_MS";
const PYTHON_VFS_RPC_MAX_PENDING_REQUESTS_ENV: &str =
    "AGENT_OS_PYTHON_VFS_RPC_MAX_PENDING_REQUESTS";
const PYTHON_EXIT_CONTROL_PREFIX: &str = "__AGENT_OS_PYTHON_EXIT__:";
const PYTHON_WARMUP_MARKER_VERSION: &str = "1";
const DEFAULT_PYTHON_OUTPUT_BUFFER_MAX_BYTES: usize = 1024 * 1024;
const DEFAULT_PYTHON_EXECUTION_TIMEOUT_MS: u64 = 5 * 60 * 1000;
const DEFAULT_PYTHON_MAX_OLD_SPACE_MB: usize = 1024;
const DEFAULT_PYTHON_VFS_RPC_TIMEOUT_MS: u64 = 30_000;
const DEFAULT_PYTHON_VFS_RPC_MAX_PENDING_REQUESTS: usize = 1000;
const CONTROLLED_STDERR_PREFIXES: &[&str] = &[PYTHON_EXIT_CONTROL_PREFIX];
const RESERVED_PYTHON_ENV_KEYS: &[&str] = &[
    NODE_COMPILE_CACHE_ENV,
    NODE_DISABLE_COMPILE_CACHE_ENV,
    NODE_ALLOWED_BUILTINS_ENV,
    NODE_ALLOW_PROCESS_BINDINGS_ENV,
    NODE_SANDBOX_ROOT_ENV,
    NODE_FROZEN_TIME_ENV,
    NODE_IMPORT_CACHE_ASSET_ROOT_ENV,
    NODE_IMPORT_CACHE_PATH_ENV,
    PYODIDE_INDEX_URL_ENV,
    PYODIDE_PACKAGE_BASE_URL_ENV,
    PYTHON_CODE_ENV,
    PYTHON_EXECUTION_TIMEOUT_MS_ENV,
    PYTHON_FILE_ENV,
    PYTHON_MAX_OLD_SPACE_MB_ENV,
    PYTHON_OUTPUT_BUFFER_MAX_BYTES_ENV,
    PYTHON_PREWARM_ONLY_ENV,
    PYTHON_VFS_RPC_REQUEST_FD_ENV,
    PYTHON_VFS_RPC_RESPONSE_FD_ENV,
    PYTHON_VFS_RPC_MAX_PENDING_REQUESTS_ENV,
    PYTHON_VFS_RPC_TIMEOUT_MS_ENV,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PythonVfsRpcMethod {
    Read,
    Write,
    Stat,
    ReadDir,
    Mkdir,
    HttpRequest,
    DnsLookup,
    SubprocessRun,
}

impl PythonVfsRpcMethod {
    fn from_wire(value: &str) -> Option<Self> {
        match value {
            "fsRead" => Some(Self::Read),
            "fsWrite" => Some(Self::Write),
            "fsStat" => Some(Self::Stat),
            "fsReaddir" => Some(Self::ReadDir),
            "fsMkdir" => Some(Self::Mkdir),
            "httpRequest" => Some(Self::HttpRequest),
            "dnsLookup" => Some(Self::DnsLookup),
            "subprocessRun" => Some(Self::SubprocessRun),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PythonVfsRpcRequest {
    pub id: u64,
    pub method: PythonVfsRpcMethod,
    pub path: String,
    pub content_base64: Option<String>,
    pub recursive: bool,
    pub url: Option<String>,
    pub http_method: Option<String>,
    pub headers: BTreeMap<String, String>,
    pub body_base64: Option<String>,
    pub hostname: Option<String>,
    pub family: Option<u8>,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub env: BTreeMap<String, String>,
    pub shell: bool,
    pub max_buffer: Option<usize>,
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
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PythonVfsRpcRequestWire {
    id: u64,
    method: String,
    #[serde(default)]
    path: String,
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
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    shell: bool,
    #[serde(default, rename = "maxBuffer")]
    max_buffer: Option<usize>,
}

struct PythonVfsRpcChannels {
    parent_request_reader: File,
    parent_response_writer: Arc<Mutex<BufWriter<File>>>,
    child_request_writer: OwnedFd,
    child_response_reader: OwnedFd,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartPythonExecutionRequest {
    pub vm_id: String,
    pub context_id: String,
    pub code: String,
    pub file_path: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
    pub cwd: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PythonExecutionEvent {
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
    VfsRpcRequest(PythonVfsRpcRequest),
    Exited(i32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PythonProcessEvent {
    Stdout(Vec<u8>),
    RawStderr(Vec<u8>),
    VfsRpcRequest(PythonVfsRpcRequest),
    Control(NodeControlMessage),
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
    VmMismatch { expected: String, found: String },
    MissingChildStream(&'static str),
    PrepareRuntime(std::io::Error),
    PrepareWarmPath(std::io::Error),
    WarmupSpawn(std::io::Error),
    WarmupFailed { exit_code: i32, stderr: String },
    Spawn(std::io::Error),
    StdinClosed,
    Stdin(std::io::Error),
    Kill(std::io::Error),
    Wait(std::io::Error),
    TimedOut(Duration),
    PendingVfsRpcRequest(u64),
    RpcChannel(String),
    RpcResponse(String),
    EventChannelClosed,
}

impl fmt::Display for PythonExecutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingContext(context_id) => {
                write!(f, "unknown guest Python context: {context_id}")
            }
            Self::VmMismatch { expected, found } => {
                write!(
                    f,
                    "guest Python context belongs to vm {expected}, not {found}"
                )
            }
            Self::MissingChildStream(name) => write!(f, "node child missing {name} pipe"),
            Self::PrepareRuntime(err) => {
                write!(f, "failed to prepare guest Python runtime assets: {err}")
            }
            Self::PrepareWarmPath(err) => {
                write!(f, "failed to prepare guest Python warm path: {err}")
            }
            Self::WarmupSpawn(err) => {
                write!(f, "failed to start guest Python warmup process: {err}")
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
            Self::Wait(err) => write!(f, "failed to wait for guest Python runtime: {err}"),
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
            Self::RpcChannel(message) => {
                write!(
                    f,
                    "failed to configure guest Python VFS RPC channel: {message}"
                )
            }
            Self::RpcResponse(message) => {
                write!(
                    f,
                    "failed to reply to guest Python VFS RPC request: {message}"
                )
            }
            Self::EventChannelClosed => {
                f.write_str("guest Python event channel closed unexpectedly")
            }
        }
    }
}

impl std::error::Error for PythonExecutionError {}

#[derive(Debug)]
pub struct PythonExecution {
    execution_id: String,
    child_pid: u32,
    child: Arc<Mutex<Option<Child>>>,
    stdin: Option<ChildStdin>,
    events: RefCell<UnboundedReceiver<PythonExecutionEvent>>,
    pending_exit_code: Arc<Mutex<Option<i32>>>,
    pending_vfs_rpc: Arc<Mutex<Option<PendingVfsRpcState>>>,
    pending_vfs_rpc_count: Arc<AtomicUsize>,
    vfs_rpc_responses: Arc<Mutex<BufWriter<File>>>,
    output_buffer_max_bytes: usize,
    execution_timeout: Option<Duration>,
    _import_cache_guard: Arc<NodeImportCacheCleanup>,
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
    pub fn execution_id(&self) -> &str {
        &self.execution_id
    }

    pub fn child_pid(&self) -> u32 {
        self.child_pid
    }

    pub fn write_stdin(&mut self, chunk: &[u8]) -> Result<(), PythonExecutionError> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or(PythonExecutionError::StdinClosed)?;
        stdin
            .write_all(chunk)
            .and_then(|()| stdin.flush())
            .map_err(PythonExecutionError::Stdin)
    }

    pub fn close_stdin(&mut self) -> Result<(), PythonExecutionError> {
        if let Some(stdin) = self.stdin.take() {
            drop(stdin);
        }
        Ok(())
    }

    pub fn cancel(&mut self) -> Result<(), PythonExecutionError> {
        self.kill()
    }

    pub fn kill(&mut self) -> Result<(), PythonExecutionError> {
        self.close_stdin()?;
        if let Some(exit_code) = self.terminate_child()? {
            self.store_pending_exit_code(exit_code)?;
        }
        Ok(())
    }

    pub fn respond_vfs_rpc_success(
        &mut self,
        id: u64,
        payload: PythonVfsRpcResponsePayload,
    ) -> Result<(), PythonExecutionError> {
        match self.clear_pending_vfs_rpc(id)? {
            PendingVfsRpcResolution::Pending => {
                release_python_vfs_rpc_slot(self.pending_vfs_rpc_count.as_ref());
            }
            PendingVfsRpcResolution::TimedOut => {
                return Err(PythonExecutionError::RpcResponse(format!(
                    "VFS RPC request {id} is no longer pending"
                )));
            }
            PendingVfsRpcResolution::Missing => {}
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
        };

        write_python_vfs_rpc_response(
            &self.vfs_rpc_responses,
            json!({
                "id": id,
                "ok": true,
                "result": result,
            }),
        )
    }

    pub fn respond_vfs_rpc_error(
        &mut self,
        id: u64,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<(), PythonExecutionError> {
        match self.clear_pending_vfs_rpc(id)? {
            PendingVfsRpcResolution::Pending => {
                release_python_vfs_rpc_slot(self.pending_vfs_rpc_count.as_ref());
            }
            PendingVfsRpcResolution::TimedOut => {
                return Err(PythonExecutionError::RpcResponse(format!(
                    "VFS RPC request {id} is no longer pending"
                )));
            }
            PendingVfsRpcResolution::Missing => {}
        }

        write_python_vfs_rpc_response(
            &self.vfs_rpc_responses,
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
    ) -> Result<Option<PythonExecutionEvent>, PythonExecutionError> {
        if timeout.is_zero() {
            return match self.events.borrow_mut().try_recv() {
                Ok(event) => Ok(Some(event)),
                Err(TokioTryRecvError::Empty) => {
                    if let Some(exit_code) = self.take_pending_exit_code()? {
                        Ok(Some(PythonExecutionEvent::Exited(exit_code)))
                    } else {
                        Ok(None)
                    }
                }
                Err(TokioTryRecvError::Disconnected) => {
                    if let Some(exit_code) = self.take_pending_exit_code()? {
                        Ok(Some(PythonExecutionEvent::Exited(exit_code)))
                    } else {
                        Err(PythonExecutionError::EventChannelClosed)
                    }
                }
            };
        }

        let mut events = self.events.borrow_mut();
        match time::timeout(timeout, events.recv()).await {
            Ok(Some(event)) => Ok(Some(event)),
            Ok(None) => {
                if let Some(exit_code) = self.take_pending_exit_code()? {
                    Ok(Some(PythonExecutionEvent::Exited(exit_code)))
                } else {
                    Err(PythonExecutionError::EventChannelClosed)
                }
            }
            Err(_) => {
                if let Some(exit_code) = self.take_pending_exit_code()? {
                    Ok(Some(PythonExecutionEvent::Exited(exit_code)))
                } else {
                    Ok(None)
                }
            }
        }
    }

    pub fn poll_event_blocking(
        &self,
        timeout: Duration,
    ) -> Result<Option<PythonExecutionEvent>, PythonExecutionError> {
        let deadline = Instant::now() + timeout;
        loop {
            match self.events.borrow_mut().try_recv() {
                Ok(event) => return Ok(Some(event)),
                Err(TokioTryRecvError::Disconnected) => {
                    if let Some(exit_code) = self.take_pending_exit_code()? {
                        return Ok(Some(PythonExecutionEvent::Exited(exit_code)));
                    }
                    return Err(PythonExecutionError::EventChannelClosed);
                }
                Err(TokioTryRecvError::Empty) => {
                    if let Some(exit_code) = self.take_pending_exit_code()? {
                        return Ok(Some(PythonExecutionEvent::Exited(exit_code)));
                    }
                    if Instant::now() >= deadline {
                        return Ok(None);
                    }
                    thread::sleep(Duration::from_millis(1));
                }
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
            let poll_timeout = timeout
                .map(|limit| {
                    let elapsed = started.elapsed();
                    if elapsed >= limit {
                        Duration::ZERO
                    } else {
                        limit.saturating_sub(elapsed).min(Duration::from_millis(50))
                    }
                })
                .unwrap_or_else(|| Duration::from_millis(50));

            let event = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("python wait runtime")
                .block_on(self.poll_event(poll_timeout))?;

            match event {
                Some(PythonExecutionEvent::Stdout(chunk)) => stdout.extend(&chunk),
                Some(PythonExecutionEvent::Stderr(chunk)) => stderr.extend(&chunk),
                Some(PythonExecutionEvent::VfsRpcRequest(request)) => {
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

    fn terminate_child(&self) -> Result<Option<i32>, PythonExecutionError> {
        let mut child_slot = self
            .child
            .lock()
            .map_err(|_| PythonExecutionError::EventChannelClosed)?;
        let Some(child) = child_slot.as_mut() else {
            return Ok(None);
        };

        let exit_code = match child.try_wait().map_err(PythonExecutionError::Wait)? {
            Some(status) => status.code().unwrap_or(1),
            None => {
                child.kill().map_err(PythonExecutionError::Kill)?;
                child
                    .wait()
                    .map_err(PythonExecutionError::Wait)?
                    .code()
                    .unwrap_or(1)
            }
        };

        *child_slot = None;
        Ok(Some(exit_code))
    }

    fn store_pending_exit_code(&self, exit_code: i32) -> Result<(), PythonExecutionError> {
        let mut pending = self
            .pending_exit_code
            .lock()
            .map_err(|_| PythonExecutionError::EventChannelClosed)?;
        *pending = Some(exit_code);
        Ok(())
    }

    fn take_pending_exit_code(&self) -> Result<Option<i32>, PythonExecutionError> {
        let mut pending = self
            .pending_exit_code
            .lock()
            .map_err(|_| PythonExecutionError::EventChannelClosed)?;
        Ok(pending.take())
    }

    fn clear_pending_vfs_rpc(
        &self,
        id: u64,
    ) -> Result<PendingVfsRpcResolution, PythonExecutionError> {
        let mut pending = self
            .pending_vfs_rpc
            .lock()
            .map_err(|_| PythonExecutionError::EventChannelClosed)?;
        match *pending {
            Some(PendingVfsRpcState::Pending(current)) if current == id => {
                *pending = None;
                Ok(PendingVfsRpcResolution::Pending)
            }
            Some(PendingVfsRpcState::TimedOut(current)) if current == id => {
                Ok(PendingVfsRpcResolution::TimedOut)
            }
            _ => Ok(PendingVfsRpcResolution::Missing),
        }
    }
}

impl Drop for PythonExecution {
    fn drop(&mut self) {
        let _ = self.close_stdin();
        let _ = self.terminate_child();
    }
}

#[derive(Debug, Default)]
pub struct PythonExecutionEngine {
    next_context_id: usize,
    next_execution_id: usize,
    contexts: BTreeMap<String, PythonContext>,
    import_caches: BTreeMap<String, NodeImportCache>,
}

impl PythonExecutionEngine {
    pub fn bundled_pyodide_dist_path_for_vm(
        &mut self,
        vm_id: &str,
    ) -> Result<PathBuf, PythonExecutionError> {
        let import_cache = self.import_caches.entry(vm_id.to_owned()).or_default();
        import_cache
            .ensure_materialized()
            .map_err(PythonExecutionError::PrepareRuntime)?;
        Ok(import_cache.pyodide_dist_path().to_path_buf())
    }

    pub fn create_context(&mut self, request: CreatePythonContextRequest) -> PythonContext {
        self.next_context_id += 1;
        self.import_caches.entry(request.vm_id.clone()).or_default();

        let context = PythonContext {
            context_id: format!("python-ctx-{}", self.next_context_id),
            vm_id: request.vm_id,
            pyodide_dist_path: request.pyodide_dist_path,
        };
        self.contexts
            .insert(context.context_id.clone(), context.clone());
        context
    }

    pub fn start_execution(
        &mut self,
        request: StartPythonExecutionRequest,
    ) -> Result<PythonExecution, PythonExecutionError> {
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
        let warmup_metrics = {
            let import_cache = self.import_caches.entry(context.vm_id.clone()).or_default();
            import_cache
                .ensure_materialized()
                .map_err(PythonExecutionError::PrepareRuntime)?;
            prewarm_python_path(import_cache, &context, &request, frozen_time_ms)?
        };

        self.next_execution_id += 1;
        let execution_id = format!("exec-{}", self.next_execution_id);
        let rpc_channels = create_python_vfs_rpc_channels()?;
        let control_channel = create_node_control_channel().map_err(PythonExecutionError::Spawn)?;
        let import_cache = self
            .import_caches
            .get(&context.vm_id)
            .expect("vm import cache should exist after materialization");
        let import_cache_guard = import_cache.cleanup_guard();
        let pending_vfs_rpc_count = Arc::new(AtomicUsize::new(0));
        let (mut child, rpc_request_reader, rpc_response_writer) = create_node_child(
            import_cache,
            &context,
            &request,
            rpc_channels,
            &control_channel.child_writer,
            frozen_time_ms,
        )?;
        let child_pid = child.id();

        let stdin = child.stdin.take();
        let stdout = child
            .stdout
            .take()
            .ok_or(PythonExecutionError::MissingChildStream("stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or(PythonExecutionError::MissingChildStream("stderr"))?;

        let (sender, receiver) = mpsc::channel();
        if let Some(metrics) = warmup_metrics {
            let _ = sender.send(PythonProcessEvent::RawStderr(metrics));
        }
        let stdout_reader = spawn_stream_reader(stdout, sender.clone(), PythonProcessEvent::Stdout);
        let stderr_reader =
            spawn_stream_reader(stderr, sender.clone(), PythonProcessEvent::RawStderr);
        let _rpc_reader = spawn_python_vfs_rpc_reader(
            rpc_request_reader,
            sender.clone(),
            rpc_response_writer.clone(),
            pending_vfs_rpc_count.clone(),
            python_vfs_rpc_max_pending_requests(&request),
        );
        let _control_reader = spawn_node_control_reader(
            control_channel.parent_reader,
            sender.clone(),
            PythonProcessEvent::Control,
            |message| PythonProcessEvent::RawStderr(message.into_bytes()),
        );
        let child = Arc::new(Mutex::new(Some(child)));
        spawn_python_waiter(
            child.clone(),
            stdout_reader,
            stderr_reader,
            sender,
            PythonProcessEvent::Exited,
            |message| PythonProcessEvent::RawStderr(message.into_bytes()),
        );

        let pending_exit_code = Arc::new(Mutex::new(None));
        let pending_vfs_rpc = Arc::new(Mutex::new(None));
        let stderr_filter = Arc::new(Mutex::new(LinePrefixFilter::default()));
        let vfs_rpc_timeout = python_vfs_rpc_timeout(&request);
        let events = spawn_python_event_bridge(
            receiver,
            pending_vfs_rpc.clone(),
            pending_vfs_rpc_count.clone(),
            rpc_response_writer.clone(),
            stderr_filter,
            vfs_rpc_timeout,
        );

        Ok(PythonExecution {
            execution_id,
            child_pid,
            child,
            stdin,
            events: RefCell::new(events),
            pending_exit_code,
            pending_vfs_rpc,
            pending_vfs_rpc_count,
            vfs_rpc_responses: rpc_response_writer,
            output_buffer_max_bytes: python_output_buffer_max_bytes(&request),
            execution_timeout: python_execution_timeout(&request),
            _import_cache_guard: import_cache_guard,
        })
    }

    pub fn dispose_vm(&mut self, vm_id: &str) {
        self.contexts.retain(|_, context| context.vm_id != vm_id);
        self.import_caches.remove(vm_id);
    }
}

fn spawn_python_event_bridge(
    receiver: Receiver<PythonProcessEvent>,
    pending_vfs_rpc: Arc<Mutex<Option<PendingVfsRpcState>>>,
    pending_vfs_rpc_count: Arc<AtomicUsize>,
    vfs_rpc_responses: Arc<Mutex<BufWriter<File>>>,
    stderr_filter: Arc<Mutex<LinePrefixFilter>>,
    vfs_rpc_timeout: Duration,
) -> UnboundedReceiver<PythonExecutionEvent> {
    let (sender, forwarded) = unbounded_channel();
    thread::spawn(move || {
        while let Ok(event) = receiver.recv() {
            let forwarded_event = match event {
                PythonProcessEvent::Stdout(chunk) => Some(PythonExecutionEvent::Stdout(chunk)),
                PythonProcessEvent::RawStderr(chunk) => {
                    let mut filter = match stderr_filter.lock() {
                        Ok(filter) => filter,
                        Err(_) => break,
                    };
                    let filtered = filter.filter_chunk(&chunk, CONTROLLED_STDERR_PREFIXES);
                    if filtered.is_empty() {
                        None
                    } else {
                        Some(PythonExecutionEvent::Stderr(filtered))
                    }
                }
                PythonProcessEvent::VfsRpcRequest(request) => {
                    if set_pending_vfs_rpc_state(&pending_vfs_rpc, request.id).is_err() {
                        break;
                    }
                    spawn_python_vfs_rpc_timeout(
                        request.id,
                        vfs_rpc_timeout,
                        pending_vfs_rpc.clone(),
                        pending_vfs_rpc_count.clone(),
                        vfs_rpc_responses.clone(),
                    );
                    Some(PythonExecutionEvent::VfsRpcRequest(request))
                }
                PythonProcessEvent::Exited(exit_code) => {
                    Some(PythonExecutionEvent::Exited(exit_code))
                }
                PythonProcessEvent::Control(_) => None,
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

fn set_pending_vfs_rpc_state(
    pending_vfs_rpc: &Arc<Mutex<Option<PendingVfsRpcState>>>,
    id: u64,
) -> Result<(), PythonExecutionError> {
    let mut pending = pending_vfs_rpc
        .lock()
        .map_err(|_| PythonExecutionError::EventChannelClosed)?;
    *pending = Some(PendingVfsRpcState::Pending(id));
    Ok(())
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
        .env
        .get(PYTHON_OUTPUT_BUFFER_MAX_BYTES_ENV)
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(DEFAULT_PYTHON_OUTPUT_BUFFER_MAX_BYTES)
}

fn python_execution_timeout(request: &StartPythonExecutionRequest) -> Option<Duration> {
    match request.env.get(PYTHON_EXECUTION_TIMEOUT_MS_ENV) {
        Some(value) => {
            let trimmed = value.trim();
            if trimmed == "0" {
                None
            } else {
                Some(Duration::from_millis(
                    trimmed
                        .parse::<u64>()
                        .ok()
                        .filter(|value| *value > 0)
                        .unwrap_or(DEFAULT_PYTHON_EXECUTION_TIMEOUT_MS),
                ))
            }
        }
        None => Some(Duration::from_millis(DEFAULT_PYTHON_EXECUTION_TIMEOUT_MS)),
    }
}

fn python_max_old_space_mb(request: &StartPythonExecutionRequest) -> usize {
    request
        .env
        .get(PYTHON_MAX_OLD_SPACE_MB_ENV)
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_PYTHON_MAX_OLD_SPACE_MB)
}

fn python_vfs_rpc_timeout(request: &StartPythonExecutionRequest) -> Duration {
    Duration::from_millis(
        request
            .env
            .get(PYTHON_VFS_RPC_TIMEOUT_MS_ENV)
            .and_then(|value| value.trim().parse::<u64>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(DEFAULT_PYTHON_VFS_RPC_TIMEOUT_MS),
    )
}

fn python_vfs_rpc_max_pending_requests(request: &StartPythonExecutionRequest) -> usize {
    request
        .env
        .get(PYTHON_VFS_RPC_MAX_PENDING_REQUESTS_ENV)
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_PYTHON_VFS_RPC_MAX_PENDING_REQUESTS)
}

fn spawn_python_vfs_rpc_timeout(
    id: u64,
    timeout: Duration,
    pending: Arc<Mutex<Option<PendingVfsRpcState>>>,
    pending_count: Arc<AtomicUsize>,
    responses: Arc<Mutex<BufWriter<File>>>,
) {
    thread::spawn(move || {
        thread::sleep(timeout);
        let should_timeout = match pending.lock() {
            Ok(mut guard) if *guard == Some(PendingVfsRpcState::Pending(id)) => {
                *guard = Some(PendingVfsRpcState::TimedOut(id));
                true
            }
            Ok(_) => false,
            Err(_) => false,
        };

        if !should_timeout {
            return;
        }

        release_python_vfs_rpc_slot(pending_count.as_ref());
        let _ = write_python_vfs_rpc_response(
            &responses,
            json!({
                "id": id,
                "ok": false,
                "error": {
                    "code": "ERR_AGENT_OS_PYTHON_VFS_RPC_TIMEOUT",
                    "message": format!(
                        "guest Python VFS RPC request {id} timed out after {}ms",
                        timeout.as_millis()
                    ),
                },
            }),
        );
    });
}

fn spawn_python_waiter<E, FE, FW>(
    child: Arc<Mutex<Option<Child>>>,
    stdout_reader: JoinHandle<()>,
    stderr_reader: JoinHandle<()>,
    sender: Sender<E>,
    exit_event: FE,
    wait_error_event: FW,
) where
    E: Send + 'static,
    FE: Fn(i32) -> E + Send + 'static,
    FW: Fn(String) -> E + Send + 'static,
{
    thread::spawn(move || loop {
        let outcome = {
            let mut child_slot = match child.lock() {
                Ok(child_slot) => child_slot,
                Err(_) => {
                    let _ = sender.send(wait_error_event(String::from(
                        "agent-os execution wait error: child lock poisoned\n",
                    )));
                    return;
                }
            };
            let Some(child) = child_slot.as_mut() else {
                return;
            };

            match child.try_wait() {
                Ok(Some(status)) => {
                    let exit_code = status.code().unwrap_or(1);
                    *child_slot = None;
                    Some(Ok(exit_code))
                }
                Ok(None) => None,
                Err(err) => {
                    *child_slot = None;
                    Some(Err(err))
                }
            }
        };

        match outcome {
            Some(Ok(exit_code)) => {
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                let _ = sender.send(exit_event(exit_code));
                return;
            }
            Some(Err(err)) => {
                let _ = sender.send(wait_error_event(format!(
                    "agent-os execution wait error: {err}\n"
                )));
                return;
            }
            None => thread::sleep(Duration::from_millis(10)),
        }
    });
}

fn create_node_child(
    import_cache: &NodeImportCache,
    context: &PythonContext,
    request: &StartPythonExecutionRequest,
    rpc_channels: PythonVfsRpcChannels,
    control_fd: &OwnedFd,
    frozen_time_ms: u128,
) -> Result<(std::process::Child, File, Arc<Mutex<BufWriter<File>>>), PythonExecutionError> {
    ensure_host_cwd_exists(&request.cwd).map_err(PythonExecutionError::Spawn)?;
    let mut command = Command::new(node_binary());
    let mut exported_fds = ExportedChildFds::default();
    configure_python_node_sandbox(&mut command, import_cache, context, request);
    command
        .arg(format!(
            "--max-old-space-size={}",
            python_max_old_space_mb(request)
        ))
        .arg("--no-warnings")
        .arg("--import")
        .arg(import_cache.timing_bootstrap_path())
        .arg(import_cache.python_runner_path())
        .current_dir(&request.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env(
            PYODIDE_INDEX_URL_ENV,
            resolved_pyodide_dist_path(&context.pyodide_dist_path, &request.cwd),
        )
        .env(
            PYODIDE_PACKAGE_BASE_URL_ENV,
            request
                .env
                .get(PYODIDE_PACKAGE_BASE_URL_ENV)
                .cloned()
                .unwrap_or_else(|| {
                    resolved_pyodide_dist_path(&context.pyodide_dist_path, &request.cwd)
                        .to_string_lossy()
                        .into_owned()
                }),
        )
        .env(NODE_IMPORT_CACHE_ASSET_ROOT_ENV, import_cache.asset_root())
        .env(NODE_IMPORT_CACHE_PATH_ENV, import_cache.cache_path())
        .env(NODE_ALLOW_PROCESS_BINDINGS_ENV, "1")
        .env(PYTHON_CODE_ENV, &request.code)
        .env(
            PYTHON_VFS_RPC_TIMEOUT_MS_ENV,
            request
                .env
                .get(PYTHON_VFS_RPC_TIMEOUT_MS_ENV)
                .cloned()
                .unwrap_or_else(|| DEFAULT_PYTHON_VFS_RPC_TIMEOUT_MS.to_string()),
        )
        .env(NODE_FROZEN_TIME_ENV, frozen_time_ms.to_string());

    if let Some(file_path) = &request.file_path {
        command.env(PYTHON_FILE_ENV, file_path);
    }

    exported_fds
        .export(
            &mut command,
            PYTHON_VFS_RPC_REQUEST_FD_ENV,
            &rpc_channels.child_request_writer,
        )
        .map_err(|error| PythonExecutionError::RpcChannel(error.to_string()))?;
    exported_fds
        .export(
            &mut command,
            PYTHON_VFS_RPC_RESPONSE_FD_ENV,
            &rpc_channels.child_response_reader,
        )
        .map_err(|error| PythonExecutionError::RpcChannel(error.to_string()))?;
    apply_guest_env(&mut command, &request.env, RESERVED_PYTHON_ENV_KEYS);
    configure_node_control_channel(&mut command, control_fd, &mut exported_fds)
        .map_err(PythonExecutionError::Spawn)?;
    configure_node_command(&mut command, import_cache)?;
    let child = command.spawn().map_err(PythonExecutionError::Spawn)?;
    Ok((
        child,
        rpc_channels.parent_request_reader,
        rpc_channels.parent_response_writer,
    ))
}

fn configure_python_node_sandbox(
    command: &mut Command,
    import_cache: &NodeImportCache,
    context: &PythonContext,
    request: &StartPythonExecutionRequest,
) {
    let sandbox_root = sandbox_root(&request.env, &request.cwd);
    let cache_root = import_cache_root(import_cache, import_cache.asset_root());
    let compile_cache_dir = import_cache.shared_compile_cache_dir();
    let pyodide_dist_path = resolved_pyodide_dist_path(&context.pyodide_dist_path, &request.cwd);
    let read_paths = vec![
        cache_root.clone(),
        compile_cache_dir.clone(),
        pyodide_dist_path,
    ];
    let write_paths = vec![cache_root, compile_cache_dir, sandbox_root.clone()];

    harden_node_command(
        command,
        &sandbox_root,
        &read_paths,
        &write_paths,
        false,
        false,
        true,
        false,
    );
}

fn configure_node_command(
    command: &mut Command,
    import_cache: &NodeImportCache,
) -> Result<(), PythonExecutionError> {
    let compile_cache_dir = import_cache.shared_compile_cache_dir();
    configure_compile_cache(command, &compile_cache_dir)
        .map_err(PythonExecutionError::PrepareWarmPath)?;
    Ok(())
}

fn resolved_pyodide_dist_path(path: &Path, cwd: &Path) -> PathBuf {
    resolve_execution_path(path, cwd)
}

fn prewarm_python_path(
    import_cache: &NodeImportCache,
    context: &PythonContext,
    request: &StartPythonExecutionRequest,
    frozen_time_ms: u128,
) -> Result<Option<Vec<u8>>, PythonExecutionError> {
    let debug_enabled = python_warmup_metrics_enabled(request);
    let marker_contents = warmup_marker_contents(import_cache, context, request);
    let marker_path = warmup_marker_path(
        import_cache.prewarm_marker_dir(),
        "python-runner-prewarm",
        PYTHON_WARMUP_MARKER_VERSION,
        &marker_contents,
    );
    if marker_path.exists() && compile_cache_ready(&import_cache.shared_compile_cache_dir()) {
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

    let warmup_started = Instant::now();
    ensure_host_cwd_exists(&request.cwd).map_err(PythonExecutionError::WarmupSpawn)?;
    let mut command = Command::new(node_binary());
    configure_python_node_sandbox(&mut command, import_cache, context, request);
    command
        .arg(format!(
            "--max-old-space-size={}",
            python_max_old_space_mb(request)
        ))
        .arg("--no-warnings")
        .arg("--import")
        .arg(import_cache.timing_bootstrap_path())
        .arg(import_cache.python_runner_path())
        .current_dir(&request.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .env(
            PYODIDE_INDEX_URL_ENV,
            resolved_pyodide_dist_path(&context.pyodide_dist_path, &request.cwd),
        )
        .env(NODE_IMPORT_CACHE_ASSET_ROOT_ENV, import_cache.asset_root())
        .env(NODE_IMPORT_CACHE_PATH_ENV, import_cache.cache_path())
        .env(NODE_ALLOW_PROCESS_BINDINGS_ENV, "1")
        .env(PYTHON_PREWARM_ONLY_ENV, "1")
        .env(NODE_FROZEN_TIME_ENV, frozen_time_ms.to_string());
    configure_node_command(&mut command, import_cache)?;

    let output = command
        .output()
        .map_err(PythonExecutionError::WarmupSpawn)?;
    let duration_ms = warmup_started.elapsed().as_secs_f64() * 1000.0;
    if !output.status.success() {
        return Err(PythonExecutionError::WarmupFailed {
            exit_code: output.status.code().unwrap_or(1),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
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
        node_binary(),
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
            "{PYTHON_WARMUP_METRICS_PREFIX}{{\"phase\":\"prewarm\",\"executed\":{},\"reason\":{},\"durationMs\":{duration_ms:.3},\"compileCacheDir\":{},\"pyodideDistPath\":{}}}\n",
            if executed { "true" } else { "false" },
            encode_json_string(reason),
            encode_json_string(&compile_cache_dir.display().to_string()),
            encode_json_string(&pyodide_dist_path.display().to_string()),
        )
        .into_bytes(),
    )
}

fn create_python_vfs_rpc_channels() -> Result<PythonVfsRpcChannels, PythonExecutionError> {
    let (parent_request_reader, child_request_writer) = pipe2(OFlag::O_CLOEXEC)
        .map_err(|error| PythonExecutionError::RpcChannel(error.to_string()))?;
    let (child_response_reader, parent_response_writer) = pipe2(OFlag::O_CLOEXEC)
        .map_err(|error| PythonExecutionError::RpcChannel(error.to_string()))?;

    Ok(PythonVfsRpcChannels {
        parent_request_reader: File::from(parent_request_reader),
        parent_response_writer: Arc::new(Mutex::new(BufWriter::new(File::from(
            parent_response_writer,
        )))),
        child_request_writer,
        child_response_reader,
    })
}

fn try_reserve_python_vfs_rpc_slot(
    pending_count: &AtomicUsize,
    max_pending_requests: usize,
) -> bool {
    let mut current = pending_count.load(Ordering::Acquire);

    loop {
        if current >= max_pending_requests {
            return false;
        }

        match pending_count.compare_exchange(
            current,
            current + 1,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => return true,
            Err(observed) => current = observed,
        }
    }
}

fn release_python_vfs_rpc_slot(pending_count: &AtomicUsize) {
    let _ = pending_count.fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
        current.checked_sub(1)
    });
}

fn spawn_python_vfs_rpc_reader(
    reader: File,
    sender: Sender<PythonProcessEvent>,
    responses: Arc<Mutex<BufWriter<File>>>,
    pending_count: Arc<AtomicUsize>,
    max_pending_requests: usize,
) -> JoinHandle<()> {
    thread::spawn(move || {
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

                    match parse_python_vfs_rpc_request(trimmed) {
                        Ok(request) => {
                            if !try_reserve_python_vfs_rpc_slot(
                                pending_count.as_ref(),
                                max_pending_requests,
                            ) {
                                let _ = write_python_vfs_rpc_response(
                                    &responses,
                                    json!({
                                        "id": request.id,
                                        "ok": false,
                                        "error": {
                                            "code": "ERR_AGENT_OS_PYTHON_VFS_RPC_QUEUE_FULL",
                                            "message": format!(
                                                "guest Python VFS RPC queue exceeded configured limit of {max_pending_requests} pending requests"
                                            ),
                                        },
                                    }),
                                );
                                continue;
                            }
                            if sender
                                .send(PythonProcessEvent::VfsRpcRequest(request))
                                .is_err()
                            {
                                release_python_vfs_rpc_slot(pending_count.as_ref());
                                return;
                            }
                        }
                        Err(message) => {
                            if sender
                                .send(PythonProcessEvent::RawStderr(message.into_bytes()))
                                .is_err()
                            {
                                return;
                            }
                        }
                    }
                }
                Err(error) => {
                    let _ = sender.send(PythonProcessEvent::RawStderr(
                        format!("agent-os python vfs rpc read error: {error}\n").into_bytes(),
                    ));
                    return;
                }
            }
        }
    })
}

fn parse_python_vfs_rpc_request(line: &str) -> Result<PythonVfsRpcRequest, String> {
    let wire: PythonVfsRpcRequestWire = serde_json::from_str(line)
        .map_err(|error| format!("invalid agent-os python vfs rpc request: {error}\n"))?;
    let method = PythonVfsRpcMethod::from_wire(&wire.method).ok_or_else(|| {
        let subject = if !wire.path.is_empty() {
            wire.path.clone()
        } else if let Some(url) = wire.url.clone() {
            url
        } else if let Some(hostname) = wire.hostname.clone() {
            hostname
        } else if let Some(command) = wire.command.clone() {
            command
        } else {
            String::from("<unknown>")
        };
        format!(
            "unsupported agent-os python rpc method {} for {}\n",
            wire.method, subject
        )
    })?;

    Ok(PythonVfsRpcRequest {
        id: wire.id,
        method,
        path: wire.path,
        content_base64: wire.content_base64,
        recursive: wire.recursive,
        url: wire.url,
        http_method: wire.http_method,
        headers: wire.headers,
        body_base64: wire.body_base64,
        hostname: wire.hostname,
        family: wire.family,
        command: wire.command,
        args: wire.args,
        cwd: wire.cwd,
        env: wire.env,
        shell: wire.shell,
        max_buffer: wire.max_buffer,
    })
}

fn write_python_vfs_rpc_response(
    writer: &Arc<Mutex<BufWriter<File>>>,
    response: serde_json::Value,
) -> Result<(), PythonExecutionError> {
    let mut writer = writer.lock().map_err(|_| {
        PythonExecutionError::RpcResponse(String::from("VFS RPC writer lock poisoned"))
    })?;
    serde_json::to_writer(&mut *writer, &response)
        .map_err(|error| PythonExecutionError::RpcResponse(error.to_string()))?;
    writer
        .write_all(b"\n")
        .and_then(|()| writer.flush())
        .map_err(|error| PythonExecutionError::RpcResponse(error.to_string()))
}
