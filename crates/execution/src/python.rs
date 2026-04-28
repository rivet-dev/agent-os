use crate::common::{encode_json_string, frozen_time_ms};
use crate::javascript::{
    CreateJavascriptContextRequest, JavascriptExecution, JavascriptExecutionEngine,
    JavascriptExecutionError, JavascriptExecutionEvent, JavascriptSyncRpcRequest,
    StartJavascriptExecutionRequest,
};
use crate::node_import_cache::{NodeImportCache, NODE_IMPORT_CACHE_ASSET_ROOT_ENV};
use crate::runtime_support::{
    env_flag_enabled, file_fingerprint, resolve_execution_path, warmup_marker_path,
    NODE_DISABLE_COMPILE_CACHE_ENV, NODE_FROZEN_TIME_ENV,
};
use crate::v8_runtime;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
const NODE_ALLOW_PROCESS_BINDINGS_ENV: &str = "AGENT_OS_ALLOW_PROCESS_BINDINGS";
const NODE_GUEST_PATH_MAPPINGS_ENV: &str = "AGENT_OS_GUEST_PATH_MAPPINGS";
const NODE_SYNC_RPC_DATA_BYTES_ENV: &str = "AGENT_OS_NODE_SYNC_RPC_DATA_BYTES";
const NODE_SYNC_RPC_WAIT_TIMEOUT_MS_ENV: &str = "AGENT_OS_NODE_SYNC_RPC_WAIT_TIMEOUT_MS";
const V8_HEAP_LIMIT_MB_ENV: &str = "AGENT_OS_V8_HEAP_LIMIT_MB";
const PYODIDE_INDEX_URL_ENV: &str = "AGENT_OS_PYODIDE_INDEX_URL";
const PYODIDE_PACKAGE_BASE_URL_ENV: &str = "AGENT_OS_PYODIDE_PACKAGE_BASE_URL";
const PYODIDE_GUEST_ROOT: &str = "/__agent_os_pyodide";
const PYODIDE_CACHE_GUEST_ROOT: &str = "/__agent_os_pyodide_cache";
const PYTHON_CODE_ENV: &str = "AGENT_OS_PYTHON_CODE";
const PYTHON_FILE_ENV: &str = "AGENT_OS_PYTHON_FILE";
const PYTHON_PREWARM_ONLY_ENV: &str = "AGENT_OS_PYTHON_PREWARM_ONLY";
const PYTHON_WARMUP_DEBUG_ENV: &str = "AGENT_OS_PYTHON_WARMUP_DEBUG";
const PYTHON_WARMUP_METRICS_PREFIX: &str = "__AGENT_OS_PYTHON_WARMUP_METRICS__:";
const PYTHON_OUTPUT_BUFFER_MAX_BYTES_ENV: &str = "AGENT_OS_PYTHON_OUTPUT_BUFFER_MAX_BYTES";
const PYTHON_EXECUTION_TIMEOUT_MS_ENV: &str = "AGENT_OS_PYTHON_EXECUTION_TIMEOUT_MS";
const PYTHON_MAX_OLD_SPACE_MB_ENV: &str = "AGENT_OS_PYTHON_MAX_OLD_SPACE_MB";
const PYTHON_VFS_RPC_TIMEOUT_MS_ENV: &str = "AGENT_OS_PYTHON_VFS_RPC_TIMEOUT_MS";
const PYTHON_WARMUP_MARKER_VERSION: &str = "2";
const DEFAULT_PYTHON_OUTPUT_BUFFER_MAX_BYTES: usize = 1024 * 1024;
const DEFAULT_PYTHON_EXECUTION_TIMEOUT_MS: u64 = 5 * 60 * 1000;
const DEFAULT_PYTHON_MAX_OLD_SPACE_MB: usize = 0;
const DEFAULT_PYTHON_VFS_RPC_TIMEOUT_MS: u64 = 30_000;
const PYTHON_SYNC_RPC_DATA_BYTES: usize = 20 * 1024 * 1024;
const PYTHON_SYNC_RPC_WAIT_TIMEOUT_MS: u64 = 120_000;
const PYTHON_PREWARM_TIMEOUT: Duration = Duration::from_secs(120);

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
struct PythonVfsBridgeRequestWire {
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
    JavascriptSyncRpcRequest(JavascriptSyncRpcRequest),
    VfsRpcRequest(PythonVfsRpcRequest),
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
    PrepareRuntime(std::io::Error),
    PrepareWarmPath(std::io::Error),
    WarmupFailed { exit_code: i32, stderr: String },
    Spawn(std::io::Error),
    StdinClosed,
    Stdin(std::io::Error),
    Kill(std::io::Error),
    TimedOut(Duration),
    PendingVfsRpcRequest(u64),
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
    inner: JavascriptExecution,
    pyodide_dist_path: PathBuf,
    pending_vfs_rpc: Arc<Mutex<Option<PendingVfsRpcState>>>,
    v8_session: crate::v8_host::V8SessionHandle,
    output_buffer_max_bytes: usize,
    execution_timeout: Option<Duration>,
    vfs_rpc_timeout: Duration,
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

    pub fn uses_shared_v8_runtime(&self) -> bool {
        self.inner.uses_shared_v8_runtime()
    }

    pub fn write_stdin(&mut self, chunk: &[u8]) -> Result<(), PythonExecutionError> {
        self.inner.write_kernel_stdin_only(chunk);
        Ok(())
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

    pub fn respond_vfs_rpc_success(
        &mut self,
        id: u64,
        payload: PythonVfsRpcResponsePayload,
    ) -> Result<(), PythonExecutionError> {
        match self.clear_pending_vfs_rpc(id)? {
            PendingVfsRpcResolution::Pending => {}
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

        self.inner
            .respond_sync_rpc_success(id, result)
            .map_err(map_javascript_error)
    }

    pub fn respond_vfs_rpc_error(
        &mut self,
        id: u64,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<(), PythonExecutionError> {
        match self.clear_pending_vfs_rpc(id)? {
            PendingVfsRpcResolution::Pending => {}
            PendingVfsRpcResolution::TimedOut => {
                return Err(PythonExecutionError::RpcResponse(format!(
                    "VFS RPC request {id} is no longer pending"
                )));
            }
            PendingVfsRpcResolution::Missing => {}
        }

        self.inner
            .respond_sync_rpc_error(id, code, message)
            .map_err(map_javascript_error)
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

    pub async fn poll_event(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<PythonExecutionEvent>, PythonExecutionError> {
        let started = Instant::now();
        loop {
            let remaining = if timeout.is_zero() {
                Duration::ZERO
            } else {
                timeout.saturating_sub(started.elapsed())
            };
            match self
                .inner
                .poll_event(remaining)
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

    pub fn poll_event_blocking(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<PythonExecutionEvent>, PythonExecutionError> {
        let deadline = Instant::now() + timeout;
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
                Some(PythonExecutionEvent::JavascriptSyncRpcRequest(request)) => {
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
                    set_pending_vfs_rpc_state(&self.pending_vfs_rpc, request.id)?;
                    spawn_python_vfs_rpc_timeout(
                        request.id,
                        self.vfs_rpc_timeout,
                        self.pending_vfs_rpc.clone(),
                        self.v8_session.clone(),
                    );
                    Ok(Some(PythonExecutionEvent::VfsRpcRequest(request)))
                } else {
                    if let Some(action) =
                        python_javascript_sync_rpc_action(&self.pyodide_dist_path, &request)?
                    {
                        respond_python_javascript_sync_rpc_action(
                            &mut self.inner,
                            request.id,
                            action,
                        )?;
                        Ok(None)
                    } else {
                        Ok(Some(PythonExecutionEvent::JavascriptSyncRpcRequest(
                            request,
                        )))
                    }
                }
            }
        }
    }
}

impl Drop for PythonExecution {
    fn drop(&mut self) {
        let _ = self.close_stdin();
        let _ = self.inner.terminate();
    }
}

#[derive(Debug, Default)]
pub struct PythonExecutionEngine {
    next_context_id: usize,
    next_execution_id: usize,
    contexts: BTreeMap<String, PythonContext>,
    import_caches: BTreeMap<String, NodeImportCache>,
    javascript_context_ids: BTreeMap<String, String>,
    javascript_engine: JavascriptExecutionEngine,
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
                .ensure_materialized()
                .map_err(PythonExecutionError::PrepareRuntime)?;
            prewarm_python_path(
                import_cache,
                &mut self.javascript_engine,
                &javascript_context_id,
                &context,
                &request,
                frozen_time_ms,
            )?
        };

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
            import_cache,
            &javascript_context_id,
            &context,
            &request,
            frozen_time_ms,
            false,
            warmup_metrics.as_deref(),
        )?;
        let pending_vfs_rpc = Arc::new(Mutex::new(None));
        let vfs_rpc_timeout = python_vfs_rpc_timeout(&request);

        Ok(PythonExecution {
            execution_id,
            child_pid: javascript_execution.child_pid(),
            v8_session: javascript_execution.v8_session_handle(),
            inner: javascript_execution,
            pyodide_dist_path,
            pending_vfs_rpc,
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
    pending_vfs_rpc: &Arc<Mutex<Option<PendingVfsRpcState>>>,
    id: u64,
) -> Result<(), PythonExecutionError> {
    let mut pending = pending_vfs_rpc
        .lock()
        .map_err(|_| PythonExecutionError::EventChannelClosed)?;
    *pending = Some(PendingVfsRpcState::Pending(id));
    Ok(())
}

fn map_javascript_error(error: JavascriptExecutionError) -> PythonExecutionError {
    match error {
        JavascriptExecutionError::EmptyArgv => PythonExecutionError::Spawn(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "guest Python bootstrap requires a JavaScript entrypoint",
        )),
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
        JavascriptExecutionError::ExpiredSyncRpcRequest(id) => {
            PythonExecutionError::RpcResponse(format!("VFS RPC request {id} is no longer pending"))
        }
        JavascriptExecutionError::RpcResponse(message) => {
            PythonExecutionError::RpcResponse(message)
        }
        JavascriptExecutionError::Terminate(error) => PythonExecutionError::Kill(error),
        JavascriptExecutionError::StdinClosed => PythonExecutionError::StdinClosed,
        JavascriptExecutionError::Stdin(error) => PythonExecutionError::Stdin(error),
        JavascriptExecutionError::EventChannelClosed => PythonExecutionError::EventChannelClosed,
    }
}

fn start_python_javascript_execution(
    javascript_engine: &mut JavascriptExecutionEngine,
    import_cache: &NodeImportCache,
    javascript_context_id: &str,
    context: &PythonContext,
    request: &StartPythonExecutionRequest,
    frozen_time_ms: u128,
    prewarm_only: bool,
    warmup_metrics: Option<&[u8]>,
) -> Result<JavascriptExecution, PythonExecutionError> {
    let internal_env =
        build_python_internal_env(import_cache, context, request, frozen_time_ms, prewarm_only);
    let inline_code =
        build_python_runner_module_source(import_cache, &internal_env, warmup_metrics)?;
    let mut env = request.env.clone();
    env.extend(internal_env);

    javascript_engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: request.vm_id.clone(),
            context_id: javascript_context_id.to_owned(),
            argv: vec![import_cache.python_runner_path().display().to_string()],
            env,
            cwd: request.cwd.clone(),
            inline_code: Some(inline_code),
        })
        .map_err(map_javascript_error)
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
        .filter(|(key, _)| key.starts_with("AGENT_OS_"))
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
        NODE_SYNC_RPC_WAIT_TIMEOUT_MS_ENV.to_string(),
        PYTHON_SYNC_RPC_WAIT_TIMEOUT_MS.to_string(),
    );
    internal_env.insert(
        NODE_DISABLE_COMPILE_CACHE_ENV.to_string(),
        String::from("1"),
    );
    internal_env.insert(
        V8_HEAP_LIMIT_MB_ENV.to_string(),
        python_max_old_space_mb(request).to_string(),
    );
    internal_env.insert(PYTHON_CODE_ENV.to_string(), request.code.clone());
    internal_env.insert(NODE_FROZEN_TIME_ENV.to_string(), frozen_time_ms.to_string());
    if prewarm_only {
        internal_env.insert(PYTHON_PREWARM_ONLY_ENV.to_string(), String::from("1"));
    } else {
        internal_env.insert(String::from("AGENT_OS_KEEP_STDIN_OPEN"), String::from("1"));
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
    pyodide_dist_path
        .parent()
        .and_then(Path::parent)
        .unwrap_or(pyodide_dist_path)
        .join("pyodide-package-cache")
}

fn build_python_runner_module_source(
    import_cache: &NodeImportCache,
    internal_env: &BTreeMap<String, String>,
    warmup_metrics: Option<&[u8]>,
) -> Result<String, PythonExecutionError> {
    let runner_source = fs::read_to_string(import_cache.python_runner_path())
        .map_err(PythonExecutionError::PrepareRuntime)?;
    let runner_source =
        format!("import * as __agentOsConstantsBinding from 'node:constants';\n{runner_source}");
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
            "globalThis.__agentOsPythonInternalEnv = {internal_env_json};\n\
if (typeof process !== 'undefined') {{\n  process.env = {{ ...(process.env || {{}}), ...globalThis.__agentOsPythonInternalEnv }};\n}}\n\
if (typeof process?.stderr?.write === 'function') {{\n  process.stderr.write({warmup_metrics_json});\n}}\n"
        ),
        None => format!(
            "globalThis.__agentOsPythonInternalEnv = {internal_env_json};\n\
if (typeof process !== 'undefined') {{\n  process.env = {{ ...(process.env || {{}}), ...globalThis.__agentOsPythonInternalEnv }};\n}}\n"
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
    request: &JavascriptSyncRpcRequest,
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
            "unsupported agent-os python rpc method {} for {}",
            wire.method, request.id
        ))
    })?;

    Ok(PythonVfsRpcRequest {
        id: request.id,
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

fn spawn_python_vfs_rpc_timeout(
    id: u64,
    timeout: Duration,
    pending: Arc<Mutex<Option<PendingVfsRpcState>>>,
    v8_session: crate::v8_host::V8SessionHandle,
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

        let _ = v8_session.send_bridge_response(
            id,
            1,
            format!(
                "ERR_AGENT_OS_PYTHON_VFS_RPC_TIMEOUT: guest Python VFS RPC request {id} timed out after {}ms",
                timeout.as_millis()
            )
            .into_bytes(),
        );
    });
}

fn resolved_pyodide_dist_path(path: &Path, cwd: &Path) -> PathBuf {
    resolve_execution_path(path, cwd)
}

fn prewarm_python_path(
    import_cache: &NodeImportCache,
    javascript_engine: &mut JavascriptExecutionEngine,
    javascript_context_id: &str,
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
    let marker_exists = marker_path.exists();

    let started = Instant::now();
    let mut prewarm_execution = start_python_javascript_execution(
        javascript_engine,
        import_cache,
        javascript_context_id,
        context,
        request,
        frozen_time_ms,
        true,
        None,
    )?;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut sync_rpc_log = Vec::new();
    let result = loop {
        match prewarm_execution
            .poll_event_blocking(PYTHON_PREWARM_TIMEOUT)
            .map_err(map_javascript_error)?
        {
            Some(JavascriptExecutionEvent::Stdout(chunk)) => stdout.extend(chunk),
            Some(JavascriptExecutionEvent::Stderr(chunk)) => stderr.extend(chunk),
            Some(JavascriptExecutionEvent::Exited(exit_code)) => {
                break PythonExecutionResult {
                    execution_id: String::from("python-prewarm"),
                    exit_code,
                    stdout,
                    stderr,
                };
            }
            Some(JavascriptExecutionEvent::SignalState { .. }) => {}
            Some(JavascriptExecutionEvent::SyncRpcRequest(sync_request)) => {
                sync_rpc_log.push(format!(
                    "{} {} {:?}",
                    sync_request.id, sync_request.method, sync_request.args
                ));
                let pyodide_dist_path =
                    resolved_pyodide_dist_path(&context.pyodide_dist_path, &request.cwd);
                if let Some(action) =
                    python_javascript_sync_rpc_action(&pyodide_dist_path, &sync_request)?
                {
                    respond_python_javascript_sync_rpc_action(
                        &mut prewarm_execution,
                        sync_request.id,
                        action,
                    )?;
                    sync_rpc_log.push(format!("responded {}", sync_request.id));
                    continue;
                }
                if let Some((code, message)) = python_javascript_sync_rpc_error(&sync_request) {
                    prewarm_execution
                        .respond_sync_rpc_error(sync_request.id, code, message)
                        .map_err(map_javascript_error)?;
                    sync_rpc_log.push(format!("errored {}", sync_request.id));
                    continue;
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
                        String::from_utf8_lossy(&stdout),
                        String::from_utf8_lossy(&stderr),
                        sync_rpc_log.join("\n"),
                    ),
                });
            }
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

enum PythonJavascriptSyncRpcAction {
    Success(Value),
    Error { code: &'static str, message: String },
}

fn python_javascript_sync_rpc_action(
    pyodide_dist_path: &Path,
    request: &JavascriptSyncRpcRequest,
) -> Result<Option<PythonJavascriptSyncRpcAction>, PythonExecutionError> {
    let Some(path) = request.args.first().and_then(Value::as_str) else {
        return Ok(None);
    };
    let path_kind = python_managed_path_kind(pyodide_dist_path, path);
    let Some(host_path) = path_kind.host_path() else {
        return Ok(None);
    };

    Ok(Some(match request.method.as_str() {
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
                    "__agentOsType": "bytes",
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
    if let Some(normalized) = path.strip_prefix(PYODIDE_GUEST_ROOT) {
        let relative = normalized.trim_start_matches('/');
        return PythonManagedResolvedPath {
            kind: PythonManagedPathKind::GuestPyodide,
            host_path: Some(if relative.is_empty() {
                pyodide_dist_path.to_path_buf()
            } else {
                pyodide_dist_path.join(relative)
            }),
        };
    }

    let cache_path = pyodide_cache_path(pyodide_dist_path);
    if let Some(normalized) = path.strip_prefix(PYODIDE_CACHE_GUEST_ROOT) {
        let relative = normalized.trim_start_matches('/');
        return PythonManagedResolvedPath {
            kind: PythonManagedPathKind::GuestCache,
            host_path: Some(if relative.is_empty() {
                cache_path
            } else {
                cache_path.join(relative)
            }),
        };
    }

    let candidate = PathBuf::from(path);
    if candidate.is_absolute()
        && (candidate == pyodide_dist_path
            || path_is_within_root(&candidate, pyodide_dist_path)
            || candidate == cache_path
            || path_is_within_root(&candidate, &cache_path))
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

fn path_is_within_root(path: &Path, root: &Path) -> bool {
    path == root || path.starts_with(root)
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
        .get("__agentOsType")
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

fn python_javascript_sync_rpc_error(
    request: &JavascriptSyncRpcRequest,
) -> Option<(&'static str, String)> {
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
        String::from("agent-os-v8"),
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
