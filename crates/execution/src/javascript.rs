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
use nix::fcntl::OFlag;
use nix::unistd::pipe2;
use serde::Deserialize;
use serde_json::{from_str, json, Value};
use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::process::{ChildStdin, Command, Stdio};
use std::sync::{
    mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError},
    Arc, Mutex,
};
use std::thread;
use std::time::{Duration, Instant};

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
const NODE_WARMUP_SPECIFIERS: &[&str] = &["node:path", "node:url", "node:fs/promises"];
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
    events: Receiver<JavascriptProcessEvent>,
    stderr_filter: Arc<Mutex<LinePrefixFilter>>,
    pending_sync_rpc: Arc<Mutex<Option<PendingSyncRpcState>>>,
    sync_rpc_responses: Option<JavascriptSyncRpcResponseWriter>,
    sync_rpc_timeout: Duration,
    _import_cache_guard: Arc<NodeImportCacheCleanup>,
}

impl JavascriptExecution {
    pub fn execution_id(&self) -> &str {
        &self.execution_id
    }

    pub fn child_pid(&self) -> u32 {
        self.child_pid
    }

    pub fn write_stdin(&mut self, chunk: &[u8]) -> Result<(), JavascriptExecutionError> {
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
        if let Some(stdin) = self.stdin.take() {
            drop(stdin);
        }
        Ok(())
    }

    pub fn respond_sync_rpc_success(
        &mut self,
        id: u64,
        result: Value,
    ) -> Result<(), JavascriptExecutionError> {
        let Some(writer) = &self.sync_rpc_responses else {
            return Err(JavascriptExecutionError::RpcResponse(String::from(
                "no sync RPC channel is active for this JavaScript execution",
            )));
        };

        match self.clear_pending_sync_rpc(id)? {
            PendingSyncRpcResolution::Pending => {}
            PendingSyncRpcResolution::TimedOut => {
                return Err(JavascriptExecutionError::ExpiredSyncRpcRequest(id));
            }
            PendingSyncRpcResolution::Missing => {}
        }

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
        let Some(writer) = &self.sync_rpc_responses else {
            return Err(JavascriptExecutionError::RpcResponse(String::from(
                "no sync RPC channel is active for this JavaScript execution",
            )));
        };

        match self.clear_pending_sync_rpc(id)? {
            PendingSyncRpcResolution::Pending => {}
            PendingSyncRpcResolution::TimedOut => {
                return Err(JavascriptExecutionError::ExpiredSyncRpcRequest(id));
            }
            PendingSyncRpcResolution::Missing => {}
        }

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

    pub fn poll_event(
        &self,
        timeout: Duration,
    ) -> Result<Option<JavascriptExecutionEvent>, JavascriptExecutionError> {
        match self.events.recv_timeout(timeout) {
            Ok(JavascriptProcessEvent::Stdout(chunk)) => {
                Ok(Some(JavascriptExecutionEvent::Stdout(chunk)))
            }
            Ok(JavascriptProcessEvent::RawStderr(chunk)) => {
                let mut filter = self
                    .stderr_filter
                    .lock()
                    .map_err(|_| JavascriptExecutionError::EventChannelClosed)?;
                let filtered = filter.filter_chunk(&chunk, CONTROLLED_STDERR_PREFIXES);
                if filtered.is_empty() {
                    return Ok(None);
                }
                Ok(Some(JavascriptExecutionEvent::Stderr(filtered)))
            }
            Ok(JavascriptProcessEvent::SyncRpcRequest(request)) => {
                self.set_pending_sync_rpc(request.id)?;
                spawn_javascript_sync_rpc_timeout(
                    request.id,
                    self.sync_rpc_timeout,
                    self.pending_sync_rpc.clone(),
                    self.sync_rpc_responses.clone(),
                );
                Ok(Some(JavascriptExecutionEvent::SyncRpcRequest(request)))
            }
            Ok(JavascriptProcessEvent::Control(NodeControlMessage::NodeImportCacheMetrics {
                metrics,
            })) => Ok(Some(JavascriptExecutionEvent::Stderr(
                format!(
                    "{}{}\n",
                    crate::node_import_cache::NODE_IMPORT_CACHE_METRICS_PREFIX,
                    serde_json::to_string(&metrics).unwrap_or_else(|_| String::from("{}"))
                )
                .into_bytes(),
            ))),
            Ok(JavascriptProcessEvent::Control(NodeControlMessage::SignalState {
                signal,
                registration,
            })) => Ok(Some(JavascriptExecutionEvent::SignalState {
                signal,
                registration,
            })),
            Ok(JavascriptProcessEvent::Control(_)) => Ok(None),
            Ok(JavascriptProcessEvent::Exited(code)) => {
                Ok(Some(JavascriptExecutionEvent::Exited(code)))
            }
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => {
                Err(JavascriptExecutionError::EventChannelClosed)
            }
        }
    }

    pub fn wait(mut self) -> Result<JavascriptExecutionResult, JavascriptExecutionError> {
        self.close_stdin()?;

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        loop {
            match self.events.recv() {
                Ok(JavascriptProcessEvent::Stdout(chunk)) => stdout.extend(chunk),
                Ok(JavascriptProcessEvent::RawStderr(chunk)) => {
                    let mut filter = self
                        .stderr_filter
                        .lock()
                        .map_err(|_| JavascriptExecutionError::EventChannelClosed)?;
                    stderr.extend(filter.filter_chunk(&chunk, CONTROLLED_STDERR_PREFIXES));
                }
                Ok(JavascriptProcessEvent::SyncRpcRequest(request)) => {
                    return Err(JavascriptExecutionError::PendingSyncRpcRequest(request.id));
                }
                Ok(JavascriptProcessEvent::Control(
                    NodeControlMessage::NodeImportCacheMetrics { metrics },
                )) => stderr.extend(
                    format!(
                        "{}{}\n",
                        crate::node_import_cache::NODE_IMPORT_CACHE_METRICS_PREFIX,
                        serde_json::to_string(&metrics).unwrap_or_else(|_| String::from("{}"))
                    )
                    .into_bytes(),
                ),
                Ok(JavascriptProcessEvent::Control(NodeControlMessage::SignalState { .. })) => {}
                Ok(JavascriptProcessEvent::Control(_)) => {}
                Ok(JavascriptProcessEvent::Exited(exit_code)) => {
                    return Ok(JavascriptExecutionResult {
                        execution_id: self.execution_id,
                        exit_code,
                        stdout,
                        stderr,
                    });
                }
                Err(_) => return Err(JavascriptExecutionError::EventChannelClosed),
            }
        }
    }

    fn set_pending_sync_rpc(&self, id: u64) -> Result<(), JavascriptExecutionError> {
        let mut pending = self.pending_sync_rpc.lock().map_err(|_| {
            JavascriptExecutionError::RpcResponse(String::from(
                "sync RPC pending-request state lock poisoned",
            ))
        })?;
        *pending = Some(PendingSyncRpcState::Pending(id));
        Ok(())
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

#[derive(Debug, Default)]
pub struct JavascriptExecutionEngine {
    next_context_id: usize,
    next_execution_id: usize,
    contexts: BTreeMap<String, JavascriptContext>,
    import_caches: BTreeMap<String, NodeImportCache>,
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

        let frozen_time_ms = frozen_time_ms();
        let warmup_metrics = {
            let import_cache = self.import_caches.entry(context.vm_id.clone()).or_default();
            import_cache
                .ensure_materialized()
                .map_err(JavascriptExecutionError::PrepareImportCache)?;
            prewarm_node_import_path(import_cache, &context, &request, frozen_time_ms)?
        };

        self.next_execution_id += 1;
        let execution_id = format!("exec-{}", self.next_execution_id);
        let control_channel =
            create_node_control_channel().map_err(JavascriptExecutionError::Spawn)?;
        let sync_rpc_channels = Some(create_javascript_sync_rpc_channels()?);
        let import_cache = self
            .import_caches
            .get(&context.vm_id)
            .expect("vm import cache should exist after materialization");
        let import_cache_guard = import_cache.cleanup_guard();
        let sync_rpc_timeout = javascript_sync_rpc_timeout(&request);
        let (mut child, sync_rpc_request_reader, sync_rpc_response_writer) = create_node_child(
            import_cache,
            &context,
            &request,
            frozen_time_ms,
            &control_channel.child_writer,
            sync_rpc_channels,
        )?;
        let child_pid = child.id();

        let stdin = child.stdin.take();
        let stdout = child
            .stdout
            .take()
            .ok_or(JavascriptExecutionError::MissingChildStream("stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or(JavascriptExecutionError::MissingChildStream("stderr"))?;

        let (sender, receiver) = mpsc::channel();
        if let Some(metrics) = warmup_metrics {
            let _ = sender.send(JavascriptProcessEvent::RawStderr(metrics));
        }

        let stdout_reader =
            spawn_stream_reader(stdout, sender.clone(), JavascriptProcessEvent::Stdout);
        let stderr_reader =
            spawn_stream_reader(stderr, sender.clone(), JavascriptProcessEvent::RawStderr);
        if let Some(reader) = sync_rpc_request_reader {
            let _sync_rpc_reader = spawn_javascript_sync_rpc_reader(reader, sender.clone());
        }
        let _control_reader = spawn_node_control_reader(
            control_channel.parent_reader,
            sender.clone(),
            JavascriptProcessEvent::Control,
            |message| JavascriptProcessEvent::RawStderr(message.into_bytes()),
        );
        spawn_waiter(
            child,
            stdout_reader,
            stderr_reader,
            true,
            sender,
            JavascriptProcessEvent::Exited,
            |message| JavascriptProcessEvent::RawStderr(message.into_bytes()),
        );

        Ok(JavascriptExecution {
            execution_id,
            child_pid,
            stdin,
            events: receiver,
            stderr_filter: Arc::new(Mutex::new(LinePrefixFilter::default())),
            pending_sync_rpc: Arc::new(Mutex::new(None)),
            sync_rpc_responses: sync_rpc_response_writer,
            sync_rpc_timeout,
            _import_cache_guard: import_cache_guard,
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

fn inherited_node_permission_enabled(env: &BTreeMap<String, String>, key: &str) -> Option<bool> {
    env.get(key).and_then(|value| match value.as_str() {
        "1" | "true" => Some(true),
        "0" | "false" => Some(false),
        _ => None,
    })
}

fn parse_env_path_list(env: &BTreeMap<String, String>, key: &str) -> Vec<PathBuf> {
    env.get(key)
        .and_then(|value| from_str::<Vec<String>>(value).ok())
        .into_iter()
        .flatten()
        .map(PathBuf::from)
        .collect()
}

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
