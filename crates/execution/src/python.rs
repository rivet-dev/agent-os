use crate::common::{encode_json_string, frozen_time_ms, stable_hash64};
use crate::node_import_cache::{NodeImportCache, NODE_IMPORT_CACHE_ASSET_ROOT_ENV};
use crate::node_process::{
    apply_guest_env, configure_node_control_channel, create_node_control_channel,
    harden_node_command, node_binary, spawn_node_control_reader, spawn_stream_reader,
    LinePrefixFilter, NodeControlMessage,
};
use nix::fcntl::{fcntl, FcntlArg, FdFlag, OFlag};
use nix::unistd::pipe2;
use serde::Deserialize;
use serde_json::json;
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::os::fd::{AsRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, UNIX_EPOCH};

const NODE_COMPILE_CACHE_ENV: &str = "NODE_COMPILE_CACHE";
const NODE_DISABLE_COMPILE_CACHE_ENV: &str = "NODE_DISABLE_COMPILE_CACHE";
const NODE_FROZEN_TIME_ENV: &str = "AGENT_OS_FROZEN_TIME_MS";
const NODE_SANDBOX_ROOT_ENV: &str = "AGENT_OS_SANDBOX_ROOT";
const NODE_ALLOWED_BUILTINS_ENV: &str = "AGENT_OS_ALLOWED_NODE_BUILTINS";
const NODE_IMPORT_CACHE_PATH_ENV: &str = "AGENT_OS_NODE_IMPORT_CACHE_PATH";
const PYODIDE_INDEX_URL_ENV: &str = "AGENT_OS_PYODIDE_INDEX_URL";
const PYTHON_CODE_ENV: &str = "AGENT_OS_PYTHON_CODE";
const PYTHON_FILE_ENV: &str = "AGENT_OS_PYTHON_FILE";
const PYTHON_PREWARM_ONLY_ENV: &str = "AGENT_OS_PYTHON_PREWARM_ONLY";
const PYTHON_WARMUP_DEBUG_ENV: &str = "AGENT_OS_PYTHON_WARMUP_DEBUG";
const PYTHON_WARMUP_METRICS_PREFIX: &str = "__AGENT_OS_PYTHON_WARMUP_METRICS__:";
const PYTHON_VFS_RPC_REQUEST_FD_ENV: &str = "AGENT_OS_PYTHON_VFS_RPC_REQUEST_FD";
const PYTHON_VFS_RPC_RESPONSE_FD_ENV: &str = "AGENT_OS_PYTHON_VFS_RPC_RESPONSE_FD";
const PYTHON_EXIT_CONTROL_PREFIX: &str = "__AGENT_OS_PYTHON_EXIT__:";
const PYTHON_WARMUP_MARKER_VERSION: &str = "1";
const CONTROLLED_STDERR_PREFIXES: &[&str] = &[PYTHON_EXIT_CONTROL_PREFIX];
const RESERVED_PYTHON_ENV_KEYS: &[&str] = &[
    NODE_COMPILE_CACHE_ENV,
    NODE_DISABLE_COMPILE_CACHE_ENV,
    NODE_ALLOWED_BUILTINS_ENV,
    NODE_SANDBOX_ROOT_ENV,
    NODE_FROZEN_TIME_ENV,
    NODE_IMPORT_CACHE_ASSET_ROOT_ENV,
    NODE_IMPORT_CACHE_PATH_ENV,
    PYODIDE_INDEX_URL_ENV,
    PYTHON_CODE_ENV,
    PYTHON_FILE_ENV,
    PYTHON_PREWARM_ONLY_ENV,
    PYTHON_VFS_RPC_REQUEST_FD_ENV,
    PYTHON_VFS_RPC_RESPONSE_FD_ENV,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PythonVfsRpcMethod {
    Read,
    Write,
    Stat,
    ReadDir,
    Mkdir,
}

impl PythonVfsRpcMethod {
    fn from_wire(value: &str) -> Option<Self> {
        match value {
            "fsRead" => Some(Self::Read),
            "fsWrite" => Some(Self::Write),
            "fsStat" => Some(Self::Stat),
            "fsReaddir" => Some(Self::ReadDir),
            "fsMkdir" => Some(Self::Mkdir),
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
    Read { content_base64: String },
    Stat { stat: PythonVfsRpcStat },
    ReadDir { entries: Vec<String> },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PythonVfsRpcRequestWire {
    id: u64,
    method: String,
    path: String,
    #[serde(default)]
    content_base64: Option<String>,
    #[serde(default)]
    recursive: bool,
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
    events: Receiver<PythonProcessEvent>,
    pending_exit_code: Arc<Mutex<Option<i32>>>,
    vfs_rpc_responses: Arc<Mutex<BufWriter<File>>>,
    stderr_filter: Arc<Mutex<LinePrefixFilter>>,
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

    pub fn poll_event(
        &self,
        timeout: Duration,
    ) -> Result<Option<PythonExecutionEvent>, PythonExecutionError> {
        match self.events.recv_timeout(timeout) {
            Ok(PythonProcessEvent::Stdout(chunk)) => Ok(Some(PythonExecutionEvent::Stdout(chunk))),
            Ok(PythonProcessEvent::RawStderr(chunk)) => {
                let mut filter = self
                    .stderr_filter
                    .lock()
                    .map_err(|_| PythonExecutionError::EventChannelClosed)?;
                let filtered = filter.filter_chunk(&chunk, CONTROLLED_STDERR_PREFIXES);
                if filtered.is_empty() {
                    return Ok(None);
                }
                Ok(Some(PythonExecutionEvent::Stderr(filtered)))
            }
            Ok(PythonProcessEvent::VfsRpcRequest(request)) => {
                Ok(Some(PythonExecutionEvent::VfsRpcRequest(request)))
            }
            Ok(PythonProcessEvent::Control(NodeControlMessage::PythonExit { exit_code })) => {
                self.store_pending_exit_code(exit_code)?;
                self.finalize_child_exit(exit_code)?;
                Ok(Some(PythonExecutionEvent::Exited(exit_code)))
            }
            Ok(PythonProcessEvent::Control(_)) => Ok(None),
            Err(RecvTimeoutError::Timeout) => {
                if let Some(exit_code) = self.take_pending_exit_code()? {
                    self.finalize_child_exit(exit_code)?;
                    return Ok(Some(PythonExecutionEvent::Exited(exit_code)));
                }
                self.poll_child_exit()
            }
            Err(RecvTimeoutError::Disconnected) => {
                if let Some(exit_code) = self.take_pending_exit_code()? {
                    self.finalize_child_exit(exit_code)?;
                    return Ok(Some(PythonExecutionEvent::Exited(exit_code)));
                }
                if let Some(event) = self.poll_child_exit()? {
                    return Ok(Some(event));
                }
                Err(PythonExecutionError::EventChannelClosed)
            }
        }
    }

    pub fn wait(
        mut self,
        timeout: Option<Duration>,
    ) -> Result<PythonExecutionResult, PythonExecutionError> {
        self.close_stdin()?;

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let started = Instant::now();

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

            match self.poll_event(poll_timeout)? {
                Some(PythonExecutionEvent::Stdout(chunk)) => stdout.extend(chunk),
                Some(PythonExecutionEvent::Stderr(chunk)) => stderr.extend(chunk),
                Some(PythonExecutionEvent::VfsRpcRequest(request)) => {
                    return Err(PythonExecutionError::PendingVfsRpcRequest(request.id));
                }
                Some(PythonExecutionEvent::Exited(exit_code)) => {
                    return Ok(PythonExecutionResult {
                        execution_id: self.execution_id.clone(),
                        exit_code,
                        stdout,
                        stderr,
                    });
                }
                None => {}
            }

            if let Some(limit) = timeout {
                if started.elapsed() >= limit {
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

    fn poll_child_exit(&self) -> Result<Option<PythonExecutionEvent>, PythonExecutionError> {
        let mut child_slot = self
            .child
            .lock()
            .map_err(|_| PythonExecutionError::EventChannelClosed)?;
        let Some(child) = child_slot.as_mut() else {
            return Ok(None);
        };

        match child.try_wait().map_err(PythonExecutionError::Wait)? {
            Some(status) => {
                *child_slot = None;
                Ok(Some(PythonExecutionEvent::Exited(
                    status.code().unwrap_or(1),
                )))
            }
            None => Ok(None),
        }
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

    fn finalize_child_exit(&self, _exit_code: i32) -> Result<(), PythonExecutionError> {
        let _ = self.terminate_child()?;
        Ok(())
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
    import_cache: NodeImportCache,
}

impl PythonExecutionEngine {
    pub fn bundled_pyodide_dist_path(&self) -> &Path {
        self.import_cache.pyodide_dist_path()
    }

    pub fn create_context(&mut self, request: CreatePythonContextRequest) -> PythonContext {
        self.next_context_id += 1;

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

        self.import_cache
            .ensure_materialized()
            .map_err(PythonExecutionError::PrepareRuntime)?;
        let frozen_time_ms = frozen_time_ms();
        let warmup_metrics =
            prewarm_python_path(&self.import_cache, &context, &request, frozen_time_ms)?;

        self.next_execution_id += 1;
        let execution_id = format!("exec-{}", self.next_execution_id);
        let rpc_channels = create_python_vfs_rpc_channels()?;
        let control_channel = create_node_control_channel().map_err(PythonExecutionError::Spawn)?;
        let (mut child, rpc_request_reader, rpc_response_writer) = create_node_child(
            &self.import_cache,
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
        let _rpc_reader = spawn_python_vfs_rpc_reader(rpc_request_reader, sender.clone());
        let _control_reader = spawn_node_control_reader(
            control_channel.parent_reader,
            sender.clone(),
            PythonProcessEvent::Control,
            |message| PythonProcessEvent::RawStderr(message.into_bytes()),
        );
        let _stdout_reader = stdout_reader;
        let _stderr_reader = stderr_reader;
        let _sender = sender;
        let child = Arc::new(Mutex::new(Some(child)));

        Ok(PythonExecution {
            execution_id,
            child_pid,
            child,
            stdin,
            events: receiver,
            pending_exit_code: Arc::new(Mutex::new(None)),
            vfs_rpc_responses: rpc_response_writer,
            stderr_filter: Arc::new(Mutex::new(LinePrefixFilter::default())),
        })
    }
}

fn create_node_child(
    import_cache: &NodeImportCache,
    context: &PythonContext,
    request: &StartPythonExecutionRequest,
    rpc_channels: PythonVfsRpcChannels,
    control_fd: &OwnedFd,
    frozen_time_ms: u128,
) -> Result<(std::process::Child, File, Arc<Mutex<BufWriter<File>>>), PythonExecutionError> {
    let mut command = Command::new(node_binary());
    configure_python_node_sandbox(&mut command, import_cache, context, request);
    command
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
        .env(NODE_IMPORT_CACHE_ASSET_ROOT_ENV, import_cache.asset_root())
        .env(NODE_IMPORT_CACHE_PATH_ENV, import_cache.cache_path())
        .env(PYTHON_CODE_ENV, &request.code)
        .env(
            PYTHON_VFS_RPC_REQUEST_FD_ENV,
            rpc_channels.child_request_writer.as_raw_fd().to_string(),
        )
        .env(
            PYTHON_VFS_RPC_RESPONSE_FD_ENV,
            rpc_channels.child_response_reader.as_raw_fd().to_string(),
        )
        .env(NODE_FROZEN_TIME_ENV, frozen_time_ms.to_string());

    if let Some(file_path) = &request.file_path {
        command.env(PYTHON_FILE_ENV, file_path);
    }

    apply_guest_env(&mut command, &request.env, RESERVED_PYTHON_ENV_KEYS);
    configure_node_control_channel(&mut command, control_fd);
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
    let sandbox_root = request
        .env
        .get(NODE_SANDBOX_ROOT_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| request.cwd.clone());
    let cache_root = import_cache
        .cache_path()
        .parent()
        .unwrap_or(import_cache.asset_root())
        .to_path_buf();
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
        true,
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
    fs::create_dir_all(&compile_cache_dir).map_err(PythonExecutionError::PrepareWarmPath)?;

    command
        .env_remove(NODE_DISABLE_COMPILE_CACHE_ENV)
        .env(NODE_COMPILE_CACHE_ENV, compile_cache_dir);
    Ok(())
}

fn resolved_pyodide_dist_path(path: &Path, cwd: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

fn prewarm_python_path(
    import_cache: &NodeImportCache,
    context: &PythonContext,
    request: &StartPythonExecutionRequest,
    frozen_time_ms: u128,
) -> Result<Option<Vec<u8>>, PythonExecutionError> {
    let debug_enabled = python_warmup_metrics_enabled(request);
    let marker_path = warmup_marker_path(import_cache, context, request);
    if marker_path.exists() && compile_cache_ready(import_cache) {
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
    let mut command = Command::new(node_binary());
    configure_python_node_sandbox(&mut command, import_cache, context, request);
    command
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

    fs::write(
        &marker_path,
        warmup_marker_contents(import_cache, context, request),
    )
    .map_err(PythonExecutionError::PrepareWarmPath)?;
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

fn warmup_marker_path(
    import_cache: &NodeImportCache,
    context: &PythonContext,
    request: &StartPythonExecutionRequest,
) -> PathBuf {
    import_cache.prewarm_marker_dir().join(format!(
        "python-runner-prewarm-v{PYTHON_WARMUP_MARKER_VERSION}-{:016x}.stamp",
        stable_hash64(warmup_marker_contents(import_cache, context, request).as_bytes()),
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

fn compile_cache_ready(import_cache: &NodeImportCache) -> bool {
    let compile_cache_dir = import_cache.shared_compile_cache_dir();
    fs::read_dir(compile_cache_dir)
        .ok()
        .and_then(|mut entries| entries.next())
        .is_some()
}

fn python_warmup_metrics_enabled(request: &StartPythonExecutionRequest) -> bool {
    request
        .env
        .get(PYTHON_WARMUP_DEBUG_ENV)
        .is_some_and(|value| value == "1")
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

fn file_fingerprint(path: &Path) -> String {
    match fs::metadata(path) {
        Ok(metadata) => format!(
            "{}:{}",
            metadata.len(),
            metadata
                .modified()
                .ok()
                .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
                .map(|duration| duration.as_millis().to_string())
                .unwrap_or_else(|| String::from("unknown"))
        ),
        Err(_) => String::from("missing"),
    }
}

fn create_python_vfs_rpc_channels() -> Result<PythonVfsRpcChannels, PythonExecutionError> {
    let (parent_request_reader, child_request_writer) = pipe2(OFlag::O_CLOEXEC)
        .map_err(|error| PythonExecutionError::RpcChannel(error.to_string()))?;
    let (child_response_reader, parent_response_writer) = pipe2(OFlag::O_CLOEXEC)
        .map_err(|error| PythonExecutionError::RpcChannel(error.to_string()))?;

    clear_cloexec(&child_request_writer)?;
    clear_cloexec(&child_response_reader)?;

    Ok(PythonVfsRpcChannels {
        parent_request_reader: File::from(parent_request_reader),
        parent_response_writer: Arc::new(Mutex::new(BufWriter::new(File::from(
            parent_response_writer,
        )))),
        child_request_writer,
        child_response_reader,
    })
}

fn clear_cloexec(fd: &OwnedFd) -> Result<(), PythonExecutionError> {
    fcntl(fd.as_raw_fd(), FcntlArg::F_SETFD(FdFlag::empty()))
        .map_err(|error| PythonExecutionError::RpcChannel(error.to_string()))?;
    Ok(())
}

fn spawn_python_vfs_rpc_reader(reader: File, sender: Sender<PythonProcessEvent>) -> JoinHandle<()> {
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
                            if sender
                                .send(PythonProcessEvent::VfsRpcRequest(request))
                                .is_err()
                            {
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
        format!(
            "unsupported agent-os python vfs rpc method {} for path {}\n",
            wire.method, wire.path
        )
    })?;

    Ok(PythonVfsRpcRequest {
        id: wire.id,
        method,
        path: wire.path,
        content_base64: wire.content_base64,
        recursive: wire.recursive,
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
