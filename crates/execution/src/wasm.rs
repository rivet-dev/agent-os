use crate::common::{
    encode_json_string, encode_json_string_array, encode_json_string_map, frozen_time_ms,
};
use crate::javascript::{
    CreateJavascriptContextRequest, JavascriptExecution, JavascriptExecutionEngine,
    JavascriptExecutionError, JavascriptExecutionEvent, JavascriptSyncRpcRequest,
    StartJavascriptExecutionRequest,
};
use crate::node_import_cache::NodeImportCache;
use crate::runtime_support::{env_flag_enabled, file_fingerprint, warmup_marker_path};
use crate::signal::{NodeSignalDispositionAction, NodeSignalHandlerRegistration};
use crate::v8_runtime;
use base64::Engine as _;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const WASM_MODULE_PATH_ENV: &str = "AGENT_OS_WASM_MODULE_PATH";
const WASM_GUEST_ARGV_ENV: &str = "AGENT_OS_GUEST_ARGV";
const WASM_GUEST_ENV_ENV: &str = "AGENT_OS_GUEST_ENV";
const WASM_PERMISSION_TIER_ENV: &str = "AGENT_OS_WASM_PERMISSION_TIER";
const WASM_PREWARM_ONLY_ENV: &str = "AGENT_OS_WASM_PREWARM_ONLY";
const WASM_MODULE_BASE64_ENV: &str = "AGENT_OS_WASM_MODULE_BASE64";
const WASM_WARMUP_DEBUG_ENV: &str = "AGENT_OS_WASM_WARMUP_DEBUG";
pub const WASM_PREWARM_TIMEOUT_MS_ENV: &str = "AGENT_OS_WASM_PREWARM_TIMEOUT_MS";
pub const WASM_MAX_FUEL_ENV: &str = "AGENT_OS_WASM_MAX_FUEL";
pub const WASM_MAX_MEMORY_BYTES_ENV: &str = "AGENT_OS_WASM_MAX_MEMORY_BYTES";
pub const WASM_MAX_STACK_BYTES_ENV: &str = "AGENT_OS_WASM_MAX_STACK_BYTES";
const WASM_WARMUP_METRICS_PREFIX: &str = "__AGENT_OS_WASM_WARMUP_METRICS__:";
const WASM_SIGNAL_STATE_PREFIX: &str = "__AGENT_OS_WASM_SIGNAL_STATE__:";
const WASM_WARMUP_MARKER_VERSION: &str = "1";
const WASM_PAGE_BYTES: u64 = 65_536;
const WASM_TIMEOUT_EXIT_CODE: i32 = 124;
const MAX_WASM_MODULE_FILE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_WASM_IMPORT_SECTION_ENTRIES: usize = 16_384;
const MAX_WASM_MEMORY_SECTION_ENTRIES: usize = 1_024;
const MAX_WASM_VARUINT_BYTES: usize = 10;
// Warmup is a best-effort compile-cache optimization; fall back to a cold start
// instead of burning minutes on a stalled prewarm session.
const DEFAULT_WASM_PREWARM_TIMEOUT_MS: u64 = 30_000;
const WASM_MAX_MEM_PAGES_FLAG: &str = "--wasm-max-mem-pages=";
const WASM_INLINE_RUNNER_ENTRYPOINT: &str = "./__agent_os_wasm_runner__.mjs";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WasmSignalDispositionAction {
    Default,
    Ignore,
    User,
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
    fn as_env_value(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::ReadWrite => "read-write",
            Self::ReadOnly => "read-only",
            Self::Isolated => "isolated",
        }
    }

    fn workspace_write_enabled(self) -> bool {
        matches!(self, Self::Full | Self::ReadWrite)
    }

    fn wasi_enabled(self) -> bool {
        !matches!(self, Self::Isolated)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WasmSignalHandlerRegistration {
    pub action: WasmSignalDispositionAction,
    pub mask: Vec<u32>,
    pub flags: u32,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartWasmExecutionRequest {
    pub vm_id: String,
    pub context_id: String,
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: PathBuf,
    pub permission_tier: WasmPermissionTier,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WasmExecutionEvent {
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
    SyncRpcRequest(JavascriptSyncRpcRequest),
    SignalState {
        signal: u32,
        registration: WasmSignalHandlerRegistration,
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedWasmModule {
    specifier: String,
    resolved_path: PathBuf,
}

#[derive(Debug)]
pub enum WasmExecutionError {
    MissingContext(String),
    VmMismatch { expected: String, found: String },
    MissingModulePath,
    InvalidLimit(String),
    InvalidModule(String),
    PrepareWarmPath(std::io::Error),
    WarmupSpawn(std::io::Error),
    WarmupTimeout(Duration),
    WarmupFailed { exit_code: i32, stderr: String },
    Spawn(std::io::Error),
    RpcResponse(String),
    StdinClosed,
    Stdin(std::io::Error),
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
            Self::InvalidModule(message) => write!(f, "invalid WebAssembly module: {message}"),
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
            Self::RpcResponse(message) => {
                write!(
                    f,
                    "failed to write guest WebAssembly sync RPC response: {message}"
                )
            }
            Self::StdinClosed => f.write_str("guest WebAssembly stdin is already closed"),
            Self::Stdin(err) => write!(f, "failed to write guest stdin: {err}"),
            Self::EventChannelClosed => {
                f.write_str("guest WebAssembly event channel closed unexpectedly")
            }
        }
    }
}

impl std::error::Error for WasmExecutionError {}

#[derive(Debug)]
pub struct WasmExecution {
    execution_id: String,
    child_pid: u32,
    inner: JavascriptExecution,
    execution_timeout: Option<Duration>,
    internal_sync_rpc: WasmInternalSyncRpc,
}

#[derive(Debug)]
struct WasmInternalSyncRpc {
    module_guest_paths: Vec<String>,
    module_host_path: PathBuf,
    guest_cwd: String,
    host_cwd: PathBuf,
    next_fd: u32,
    open_files: BTreeMap<u32, fs::File>,
}

impl WasmExecution {
    pub fn execution_id(&self) -> &str {
        &self.execution_id
    }

    pub fn child_pid(&self) -> u32 {
        self.child_pid
    }

    pub fn write_stdin(&mut self, chunk: &[u8]) -> Result<(), WasmExecutionError> {
        self.inner.write_stdin(chunk).map_err(map_javascript_error)
    }

    pub fn close_stdin(&mut self) -> Result<(), WasmExecutionError> {
        self.inner.close_stdin().map_err(map_javascript_error)
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

    pub async fn poll_event(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<WasmExecutionEvent>, WasmExecutionError> {
        loop {
            match self
                .inner
                .poll_event(timeout)
                .await
                .map_err(map_javascript_error)?
            {
                Some(event) => {
                    if let Some(signal_state) = translate_wasm_signal_state_stream_event(&event)? {
                        return Ok(Some(signal_state));
                    }
                    if let JavascriptExecutionEvent::SyncRpcRequest(request) = &event {
                        if self.handle_internal_sync_rpc(request)? {
                            continue;
                        }
                        if let Some(signal_state) = self.handle_signal_state_sync_rpc(request)? {
                            return Ok(Some(signal_state));
                        }
                    }
                    if let Some(event) = translate_javascript_event(event) {
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
    ) -> Result<Option<WasmExecutionEvent>, WasmExecutionError> {
        loop {
            match self
                .inner
                .poll_event_blocking(timeout)
                .map_err(map_javascript_error)?
            {
                Some(event) => {
                    if let Some(signal_state) = translate_wasm_signal_state_stream_event(&event)? {
                        return Ok(Some(signal_state));
                    }
                    if let JavascriptExecutionEvent::SyncRpcRequest(request) = &event {
                        if self.handle_internal_sync_rpc(request)? {
                            continue;
                        }
                        if let Some(signal_state) = self.handle_signal_state_sync_rpc(request)? {
                            return Ok(Some(signal_state));
                        }
                    }
                    if let Some(event) = translate_javascript_event(event) {
                        return Ok(Some(event));
                    }
                }
                None => return Ok(None),
            }
        }
    }

    pub fn wait(mut self) -> Result<WasmExecutionResult, WasmExecutionError> {
        self.close_stdin()?;
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let started = Instant::now();

        loop {
            let poll_timeout = self
                .execution_timeout
                .map(|limit| {
                    let elapsed = started.elapsed();
                    if elapsed >= limit {
                        Duration::ZERO
                    } else {
                        limit.saturating_sub(elapsed).min(Duration::from_millis(50))
                    }
                })
                .unwrap_or_else(|| Duration::from_millis(50));

            match self.poll_event_blocking(poll_timeout)? {
                Some(WasmExecutionEvent::Stdout(chunk)) => stdout.extend(chunk),
                Some(WasmExecutionEvent::Stderr(chunk)) => stderr.extend(chunk),
                Some(WasmExecutionEvent::SyncRpcRequest(_)) => {}
                Some(WasmExecutionEvent::SignalState { .. }) => {}
                Some(WasmExecutionEvent::Exited(exit_code)) => {
                    return Ok(WasmExecutionResult {
                        execution_id: self.execution_id,
                        exit_code,
                        stdout,
                        stderr,
                    });
                }
                None => {}
            }

            if let Some(limit) = self.execution_timeout {
                if started.elapsed() >= limit {
                    let _ = self.inner.terminate();
                    stderr.extend_from_slice(b"WebAssembly fuel budget exhausted\n");
                    return Ok(WasmExecutionResult {
                        execution_id: self.execution_id,
                        exit_code: WASM_TIMEOUT_EXIT_CODE,
                        stdout,
                        stderr,
                    });
                }
            }
        }
    }

    fn handle_internal_sync_rpc(
        &mut self,
        request: &JavascriptSyncRpcRequest,
    ) -> Result<bool, WasmExecutionError> {
        handle_internal_wasm_sync_rpc_request(&mut self.inner, &mut self.internal_sync_rpc, request)
    }

    fn handle_signal_state_sync_rpc(
        &mut self,
        request: &JavascriptSyncRpcRequest,
    ) -> Result<Option<WasmExecutionEvent>, WasmExecutionError> {
        translate_wasm_signal_state_sync_rpc_request(&mut self.inner, request)
    }
}

#[derive(Debug, Default)]
pub struct WasmExecutionEngine {
    next_context_id: usize,
    next_execution_id: usize,
    contexts: BTreeMap<String, WasmContext>,
    import_caches: BTreeMap<String, NodeImportCache>,
    javascript_context_ids: BTreeMap<String, String>,
    javascript_engine: JavascriptExecutionEngine,
}

impl WasmExecutionEngine {
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

    pub fn start_execution(
        &mut self,
        request: StartWasmExecutionRequest,
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

        let resolved_module = resolve_wasm_module(&context, &request)?;
        let prewarm_timeout = resolve_wasm_prewarm_timeout(&request)?;
        let javascript_context_id = self
            .javascript_context_ids
            .get(&context.context_id)
            .cloned()
            .ok_or_else(|| WasmExecutionError::MissingContext(context.context_id.clone()))?;
        {
            let import_cache = self.import_caches.entry(context.vm_id.clone()).or_default();
            import_cache
                .ensure_materialized_with_timeout(prewarm_timeout)
                .map_err(WasmExecutionError::PrepareWarmPath)?;
        }
        let frozen_time_ms = frozen_time_ms();
        validate_module_limits(&resolved_module, &request)?;
        let execution_timeout = resolve_wasm_execution_timeout(&request)?;
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
            frozen_time_ms,
            prewarm_timeout,
        ) {
            Ok(metrics) => metrics,
            Err(WasmExecutionError::WarmupTimeout(_)) => None,
            Err(error) => return Err(error),
        };

        self.next_execution_id += 1;
        let execution_id = format!("exec-{}", self.next_execution_id);
        let javascript_execution = start_wasm_javascript_execution(
            &mut self.javascript_engine,
            import_cache,
            &javascript_context_id,
            &resolved_module,
            &request,
            frozen_time_ms,
            false,
            warmup_metrics.as_deref(),
        )?;
        let child_pid = javascript_execution.child_pid();

        Ok(WasmExecution {
            execution_id,
            child_pid,
            inner: javascript_execution,
            execution_timeout,
            internal_sync_rpc: WasmInternalSyncRpc {
                module_guest_paths: wasm_guest_module_paths(
                    &resolved_module.specifier,
                    &request.env,
                ),
                module_host_path: resolved_module.resolved_path.clone(),
                guest_cwd: wasm_guest_cwd(&request.env),
                host_cwd: request.cwd.clone(),
                next_fd: 64,
                open_files: BTreeMap::new(),
            },
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

fn map_javascript_error(error: JavascriptExecutionError) -> WasmExecutionError {
    match error {
        JavascriptExecutionError::EmptyArgv => WasmExecutionError::Spawn(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "guest WebAssembly bootstrap requires a JavaScript entrypoint",
        )),
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
        JavascriptExecutionError::ExpiredSyncRpcRequest(id) => WasmExecutionError::RpcResponse(
            format!("guest WebAssembly sync RPC request {id} is no longer pending"),
        ),
        JavascriptExecutionError::RpcResponse(message) => WasmExecutionError::RpcResponse(message),
        JavascriptExecutionError::Terminate(error) => WasmExecutionError::Spawn(error),
        JavascriptExecutionError::StdinClosed => WasmExecutionError::StdinClosed,
        JavascriptExecutionError::Stdin(error) => WasmExecutionError::Stdin(error),
        JavascriptExecutionError::EventChannelClosed => WasmExecutionError::EventChannelClosed,
    }
}

fn translate_javascript_event(event: JavascriptExecutionEvent) -> Option<WasmExecutionEvent> {
    match event {
        JavascriptExecutionEvent::Stdout(chunk) => Some(WasmExecutionEvent::Stdout(chunk)),
        JavascriptExecutionEvent::Stderr(chunk) => Some(WasmExecutionEvent::Stderr(chunk)),
        JavascriptExecutionEvent::SyncRpcRequest(request) => {
            Some(WasmExecutionEvent::SyncRpcRequest(request))
        }
        JavascriptExecutionEvent::SignalState {
            signal,
            registration,
        } => Some(WasmExecutionEvent::SignalState {
            signal,
            registration: registration.into(),
        }),
        JavascriptExecutionEvent::Exited(code) => Some(WasmExecutionEvent::Exited(code)),
    }
}

fn handle_internal_wasm_sync_rpc_request(
    execution: &mut JavascriptExecution,
    internal_sync_rpc: &mut WasmInternalSyncRpc,
    request: &JavascriptSyncRpcRequest,
) -> Result<bool, WasmExecutionError> {
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

    if request.method == "fs.openSync" {
        let Some(path) = request.args.first().and_then(Value::as_str) else {
            return Err(WasmExecutionError::RpcResponse(String::from(
                "missing fs.openSync path",
            )));
        };
        let Some(host_path) = translate_wasm_guest_path(path, internal_sync_rpc) else {
            return Err(WasmExecutionError::RpcResponse(format!(
                "unmapped guest path for fs.openSync: {path}"
            )));
        };
        let flags = request.args.get(1).unwrap_or(&Value::Null);
        let file = open_wasm_guest_file(&host_path, flags)?;
        let fd = internal_sync_rpc.next_fd;
        internal_sync_rpc.next_fd += 1;
        internal_sync_rpc.open_files.insert(fd, file);
        execution
            .respond_sync_rpc_success(request.id, json!(fd))
            .map_err(map_javascript_error)?;
        return Ok(true);
    }

    if request.method == "fs.closeSync" {
        let Some(fd) = request.args.first().and_then(Value::as_u64) else {
            return Err(WasmExecutionError::RpcResponse(String::from(
                "missing fs.closeSync fd",
            )));
        };
        internal_sync_rpc.open_files.remove(&(fd as u32));
        execution
            .respond_sync_rpc_success(request.id, Value::Null)
            .map_err(map_javascript_error)?;
        return Ok(true);
    }

    if request.method == "fs.writeSync" {
        let Some(fd) = request.args.first().and_then(Value::as_u64) else {
            return Err(WasmExecutionError::RpcResponse(String::from(
                "missing fs.writeSync fd",
            )));
        };
        let bytes = decode_wasm_bytes_arg(request.args.get(1)).ok_or_else(|| {
            WasmExecutionError::RpcResponse(String::from("missing fs.writeSync bytes"))
        })?;
        let position = request.args.get(2).and_then(Value::as_u64);
        let Some(file) = internal_sync_rpc.open_files.get_mut(&(fd as u32)) else {
            return Err(WasmExecutionError::RpcResponse(format!(
                "unknown fs.writeSync fd: {fd}"
            )));
        };
        if let Some(position) = position {
            file.seek(SeekFrom::Start(position))
                .map_err(WasmExecutionError::Spawn)?;
        }
        let written = file.write(&bytes).map_err(WasmExecutionError::Spawn)?;
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
        let length = request.args.get(1).and_then(Value::as_u64).unwrap_or(0) as usize;
        let position = request.args.get(2).and_then(Value::as_u64);
        let Some(file) = internal_sync_rpc.open_files.get_mut(&(fd as u32)) else {
            return Err(WasmExecutionError::RpcResponse(format!(
                "unknown fs.readSync fd: {fd}"
            )));
        };
        if let Some(position) = position {
            file.seek(SeekFrom::Start(position))
                .map_err(WasmExecutionError::Spawn)?;
        }
        let mut buffer = vec![0u8; length];
        let bytes_read = file.read(&mut buffer).map_err(WasmExecutionError::Spawn)?;
        buffer.truncate(bytes_read);
        execution
            .respond_sync_rpc_success(
                request.id,
                json!({
                    "__agentOsType": "bytes",
                    "base64": v8_runtime::base64_encode_pub(&buffer),
                }),
            )
            .map_err(map_javascript_error)?;
        return Ok(true);
    }

    Ok(false)
}

fn translate_wasm_guest_path(
    path: &str,
    internal_sync_rpc: &WasmInternalSyncRpc,
) -> Option<PathBuf> {
    if path == internal_sync_rpc.module_host_path.to_string_lossy() {
        return Some(internal_sync_rpc.module_host_path.clone());
    }
    if internal_sync_rpc
        .module_guest_paths
        .iter()
        .any(|candidate| candidate == path)
    {
        return Some(internal_sync_rpc.module_host_path.clone());
    }
    strip_guest_prefix(path, &internal_sync_rpc.guest_cwd)
        .map(|suffix| join_host_path(&internal_sync_rpc.host_cwd, &suffix))
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

fn decode_wasm_bytes_arg(value: Option<&Value>) -> Option<Vec<u8>> {
    let value = value?;
    let base64 = value.as_object()?.get("base64")?.as_str()?;
    base64::engine::general_purpose::STANDARD
        .decode(base64)
        .ok()
}

fn open_wasm_guest_file(path: &Path, flags: &Value) -> Result<fs::File, WasmExecutionError> {
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
                WasmExecutionError::RpcResponse(format!(
                    "unsupported fs.openSync flags: {flags_label}"
                ))
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
        WasmExecutionError::Spawn(std::io::Error::new(
            error.kind(),
            format!(
                "failed to open guest file {} with flags {}: {error}",
                path.display(),
                flags_label
            ),
        ))
    })
}

fn translate_wasm_signal_state_sync_rpc_request(
    execution: &mut JavascriptExecution,
    request: &JavascriptSyncRpcRequest,
) -> Result<Option<WasmExecutionEvent>, WasmExecutionError> {
    if request.method != "process.signal_state" {
        return Ok(None);
    }

    let signal = request
        .args
        .first()
        .and_then(Value::as_u64)
        .ok_or_else(|| WasmExecutionError::RpcResponse(String::from("missing signal number")))?;
    let action = match request
        .args
        .get(1)
        .and_then(Value::as_str)
        .unwrap_or("default")
    {
        "ignore" => WasmSignalDispositionAction::Ignore,
        "user" => WasmSignalDispositionAction::User,
        _ => WasmSignalDispositionAction::Default,
    };
    let mask = request
        .args
        .get(2)
        .and_then(Value::as_str)
        .map(|value| serde_json::from_str::<Vec<u32>>(value))
        .transpose()
        .map_err(|error| WasmExecutionError::RpcResponse(error.to_string()))?
        .unwrap_or_default();
    let flags = request
        .args
        .get(3)
        .and_then(Value::as_u64)
        .unwrap_or_default() as u32;

    execution
        .respond_sync_rpc_success(request.id, Value::Null)
        .map_err(map_javascript_error)?;

    Ok(Some(WasmExecutionEvent::SignalState {
        signal: signal as u32,
        registration: WasmSignalHandlerRegistration {
            action,
            mask,
            flags,
        },
    }))
}

fn translate_wasm_signal_state_stream_event(
    event: &JavascriptExecutionEvent,
) -> Result<Option<WasmExecutionEvent>, WasmExecutionError> {
    let chunk = match event {
        JavascriptExecutionEvent::Stdout(chunk) | JavascriptExecutionEvent::Stderr(chunk) => chunk,
        _ => return Ok(None),
    };
    let text = std::str::from_utf8(chunk)
        .map_err(|error| WasmExecutionError::RpcResponse(error.to_string()))?;
    let payload = match text.trim().strip_prefix(WASM_SIGNAL_STATE_PREFIX) {
        Some(payload) => payload,
        None => return Ok(None),
    };
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
        "ignore" => WasmSignalDispositionAction::Ignore,
        "user" => WasmSignalDispositionAction::User,
        _ => WasmSignalDispositionAction::Default,
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
        registration: WasmSignalHandlerRegistration {
            action,
            mask,
            flags,
        },
    }))
}

fn start_wasm_javascript_execution(
    javascript_engine: &mut JavascriptExecutionEngine,
    import_cache: &NodeImportCache,
    javascript_context_id: &str,
    resolved_module: &ResolvedWasmModule,
    request: &StartWasmExecutionRequest,
    frozen_time_ms: u128,
    prewarm_only: bool,
    warmup_metrics: Option<&[u8]>,
) -> Result<JavascriptExecution, WasmExecutionError> {
    let internal_env =
        build_wasm_internal_env(resolved_module, request, frozen_time_ms, prewarm_only);
    let inline_code = build_wasm_runner_module_source(import_cache, &internal_env, warmup_metrics)?;
    let mut env = request.env.clone();
    env.extend(internal_env);

    javascript_engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: request.vm_id.clone(),
            context_id: javascript_context_id.to_owned(),
            argv: vec![String::from(WASM_INLINE_RUNNER_ENTRYPOINT)],
            env,
            cwd: request.cwd.clone(),
            inline_code: Some(inline_code),
        })
        .map_err(map_javascript_error)
}

fn build_wasm_internal_env(
    resolved_module: &ResolvedWasmModule,
    request: &StartWasmExecutionRequest,
    frozen_time_ms: u128,
    prewarm_only: bool,
) -> BTreeMap<String, String> {
    let mut internal_env = request
        .env
        .iter()
        .filter(|(key, _)| key.starts_with("AGENT_OS_"))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<BTreeMap<_, _>>();

    internal_env.insert(
        WASM_MODULE_PATH_ENV.to_string(),
        resolved_module.specifier.clone(),
    );
    if let Ok(module_bytes) = fs::read(&resolved_module.resolved_path) {
        internal_env.insert(
            WASM_MODULE_BASE64_ENV.to_string(),
            v8_runtime::base64_encode_pub(&module_bytes),
        );
    }
    internal_env.insert(
        WASM_GUEST_ARGV_ENV.to_string(),
        encode_json_string_array(&warmup_guest_argv(resolved_module, request)),
    );
    internal_env.insert(
        WASM_GUEST_ENV_ENV.to_string(),
        encode_json_string_map(&guest_visible_wasm_env(&request.env)),
    );
    internal_env.insert(
        WASM_PERMISSION_TIER_ENV.to_string(),
        request.permission_tier.as_env_value().to_string(),
    );
    internal_env.insert(
        String::from("AGENT_OS_FROZEN_TIME_MS"),
        frozen_time_ms.to_string(),
    );

    if prewarm_only {
        internal_env.insert(WASM_PREWARM_ONLY_ENV.to_string(), String::from("1"));
    } else {
        internal_env.remove(WASM_PREWARM_ONLY_ENV);
    }
    internal_env.remove("AGENT_OS_KEEP_STDIN_OPEN");

    internal_env
}

fn build_wasm_runner_module_source(
    import_cache: &NodeImportCache,
    internal_env: &BTreeMap<String, String>,
    warmup_metrics: Option<&[u8]>,
) -> Result<String, WasmExecutionError> {
    let runner_source = fs::read_to_string(import_cache.wasm_runner_path())
        .map_err(WasmExecutionError::PrepareWarmPath)?;
    let runner_source = runner_source.replace(
        "import { WASI } from 'node:wasi';\n",
        "const { WASI } = globalThis.__agentOsWasiModule;\n",
    );
    let bootstrap = build_wasm_runner_bootstrap(internal_env, warmup_metrics);
    Ok(insert_wasm_runner_bootstrap(&runner_source, &bootstrap))
}

fn build_wasm_runner_bootstrap(
    internal_env: &BTreeMap<String, String>,
    warmup_metrics: Option<&[u8]>,
) -> String {
    let internal_env_json =
        serde_json::to_string(internal_env).unwrap_or_else(|_| String::from("{}"));
    let warmup_metrics_json = warmup_metrics.map(|bytes| {
        serde_json::to_string(&String::from_utf8_lossy(bytes).to_string())
            .unwrap_or_else(|_| String::from("\"\""))
    });
    let warmup_emit = warmup_metrics_json
        .map(|metrics| {
            format!(
                "if (typeof process?.stderr?.write === \"function\") {{\n  process.stderr.write({metrics});\n}}\n"
            )
        })
        .unwrap_or_default();

    format!(
        r#"const __agentOsWasmInternalEnv = {internal_env_json};
const __agentOsRequireBuiltin = (specifier) => {{
  if (typeof globalThis.require === "function") {{
    return globalThis.require(specifier);
  }}
  if (typeof process?.getBuiltinModule === "function") {{
    return process.getBuiltinModule(specifier);
  }}
  throw new Error(`Agent OS WASM bootstrap cannot load ${{specifier}}`);
}};
if (typeof globalThis !== "undefined" && typeof globalThis.__agentOsWasiModule === "undefined") {{
  const __agentOsFs = () => __agentOsRequireBuiltin("node:fs");
  const __agentOsPath = () => __agentOsRequireBuiltin("node:path");
  const __agentOsWasiErrnoSuccess = 0;
  const __agentOsWasiErrnoBadf = 8;
  const __agentOsWasiErrnoFault = 21;
  const __agentOsWasiErrnoNosys = 52;

  class WASI {{
    constructor(options = {{}}) {{
      this.args = Array.isArray(options.args) ? options.args.map((value) => String(value)) : [];
      this.env =
        options.env && typeof options.env === "object"
          ? Object.fromEntries(
              Object.entries(options.env).map(([key, value]) => [String(key), String(value)]),
            )
          : {{}};
      this.preopens = options.preopens && typeof options.preopens === "object" ? options.preopens : {{}};
      this.returnOnExit = options.returnOnExit === true;
      this.instance = null;
      this.nextFd = 3;
      this.fdTable = new Map([
        [0, {{ kind: "stdin" }}],
        [1, {{ kind: "stdout" }}],
        [2, {{ kind: "stderr" }}],
      ]);
      for (const [guestPath, hostPath] of Object.entries(this.preopens)) {{
        this.fdTable.set(this.nextFd++, {{
          kind: "preopen",
          guestPath: String(guestPath),
          hostPath: String(hostPath),
        }});
      }}
      this.wasiImport = {{
        clock_time_get: (...args) => this._clockTimeGet(...args),
        clock_res_get: (...args) => this._clockResGet(...args),
        fd_close: (...args) => this._fdClose(...args),
        fd_pwrite: (...args) => this._fdPwrite(...args),
        fd_read: (...args) => this._fdRead(...args),
        fd_write: (...args) => this._fdWrite(...args),
        path_open: (...args) => this._pathOpen(...args),
        poll_oneoff: (...args) => this._pollOneoff(...args),
        proc_exit: (...args) => this._procExit(...args),
      }};
    }}

    start(instance) {{
      this.instance = instance;
      try {{
        if (typeof instance?.exports?._start === "function") {{
          instance.exports._start();
        }}
        return 0;
      }} catch (error) {{
        if (error && error.__agentOsWasiExit === true) {{
          return Number(error.code) >>> 0;
        }}
        throw error;
      }}
    }}

    _memoryView() {{
      const memory = this.instance?.exports?.memory;
      if (!(memory instanceof WebAssembly.Memory)) {{
        throw new Error("WASI memory export is unavailable");
      }}
      return new DataView(memory.buffer);
    }}

    _memoryBytes() {{
      const memory = this.instance?.exports?.memory;
      if (!(memory instanceof WebAssembly.Memory)) {{
        throw new Error("WASI memory export is unavailable");
      }}
      return new Uint8Array(memory.buffer);
    }}

    _writeUint32(ptr, value) {{
      try {{
        this._memoryView().setUint32(Number(ptr) >>> 0, Number(value) >>> 0, true);
        return __agentOsWasiErrnoSuccess;
      }} catch {{
        return __agentOsWasiErrnoFault;
      }}
    }}

    _writeUint64(ptr, value) {{
      try {{
        this._memoryView().setBigUint64(Number(ptr) >>> 0, BigInt(value), true);
        return __agentOsWasiErrnoSuccess;
      }} catch {{
        return __agentOsWasiErrnoFault;
      }}
    }}

    _readBytes(ptr, len) {{
      const start = Number(ptr) >>> 0;
      const end = start + (Number(len) >>> 0);
      return Buffer.from(this._memoryBytes().slice(start, end));
    }}

    _readString(ptr, len) {{
      return this._readBytes(ptr, len).toString("utf8");
    }}

    _collectIovs(iovs, iovsLen) {{
      const view = this._memoryView();
      const chunks = [];
      for (let index = 0; index < (Number(iovsLen) >>> 0); index += 1) {{
        const entryOffset = (Number(iovs) >>> 0) + index * 8;
        const ptr = view.getUint32(entryOffset, true);
        const len = view.getUint32(entryOffset + 4, true);
        chunks.push(this._readBytes(ptr, len));
      }}
      return Buffer.concat(chunks);
    }}

    _writeToIovs(iovs, iovsLen, bytes) {{
      const view = this._memoryView();
      const memory = this._memoryBytes();
      let sourceOffset = 0;
      for (let index = 0; index < (Number(iovsLen) >>> 0) && sourceOffset < bytes.length; index += 1) {{
        const entryOffset = (Number(iovs) >>> 0) + index * 8;
        const ptr = view.getUint32(entryOffset, true);
        const len = view.getUint32(entryOffset + 4, true);
        const chunk = bytes.subarray(sourceOffset, sourceOffset + len);
        memory.set(chunk, Number(ptr) >>> 0);
        sourceOffset += chunk.length;
      }}
      return sourceOffset;
    }}

    _clockTimeGet(_clockId, _precision, resultPtr) {{
      return this._writeUint64(resultPtr, BigInt(Date.now()) * 1000000n);
    }}

    _clockResGet(_clockId, resultPtr) {{
      return this._writeUint64(resultPtr, 1000000n);
    }}

    _fdWrite(fd, iovs, iovsLen, nwrittenPtr) {{
      try {{
        const bytes = this._collectIovs(iovs, iovsLen);
        const descriptor = Number(fd) >>> 0;
        const entry = this.fdTable.get(descriptor);
        if (!entry) {{
          return __agentOsWasiErrnoBadf;
        }}
        if (entry.kind === "stdout") {{
          process.stdout.write(bytes);
          return this._writeUint32(nwrittenPtr, bytes.length);
        }}
        if (entry.kind === "stderr") {{
          process.stderr.write(bytes);
          return this._writeUint32(nwrittenPtr, bytes.length);
        }}
        if (entry.kind === "file") {{
          const written = __agentOsFs().writeSync(entry.realFd, bytes, 0, bytes.length);
          return this._writeUint32(nwrittenPtr, written);
        }}
        return __agentOsWasiErrnoBadf;
      }} catch {{
        return __agentOsWasiErrnoFault;
      }}
    }}

    _fdPwrite(fd, iovs, iovsLen, offset, nwrittenPtr) {{
      try {{
        const bytes = this._collectIovs(iovs, iovsLen);
        const descriptor = Number(fd) >>> 0;
        const entry = this.fdTable.get(descriptor);
        if (!entry || entry.kind !== "file") {{
          return __agentOsWasiErrnoBadf;
        }}
        const written = __agentOsFs().writeSync(
          entry.realFd,
          bytes,
          0,
          bytes.length,
          Number(offset) >>> 0,
        );
        return this._writeUint32(nwrittenPtr, written);
      }} catch {{
        return __agentOsWasiErrnoFault;
      }}
    }}

    _fdRead(fd, iovs, iovsLen, nreadPtr) {{
      try {{
        const descriptor = Number(fd) >>> 0;
        const entry = this.fdTable.get(descriptor);
        if (!entry) {{
          return __agentOsWasiErrnoBadf;
        }}
        if (entry.kind === "stdin") {{
          return this._writeUint32(nreadPtr, 0);
        }}
        if (entry.kind !== "file") {{
          return __agentOsWasiErrnoBadf;
        }}
        const totalLength = (() => {{
          const view = this._memoryView();
          let length = 0;
          for (let index = 0; index < (Number(iovsLen) >>> 0); index += 1) {{
            const entryOffset = (Number(iovs) >>> 0) + index * 8;
            length += view.getUint32(entryOffset + 4, true);
          }}
          return length >>> 0;
        }})();
        const buffer = Buffer.alloc(totalLength);
        const bytesRead = __agentOsFs().readSync(entry.realFd, buffer, 0, totalLength, null);
        const written = this._writeToIovs(iovs, iovsLen, buffer.subarray(0, bytesRead));
        return this._writeUint32(nreadPtr, written);
      }} catch {{
        return __agentOsWasiErrnoFault;
      }}
    }}

    _fdClose(fd) {{
      try {{
        const descriptor = Number(fd) >>> 0;
        const entry = this.fdTable.get(descriptor);
        if (!entry) {{
          return __agentOsWasiErrnoBadf;
        }}
        if (entry.kind === "file") {{
          __agentOsFs().closeSync(entry.realFd);
        }}
        if (descriptor > 2) {{
          this.fdTable.delete(descriptor);
        }}
        return __agentOsWasiErrnoSuccess;
      }} catch {{
        return __agentOsWasiErrnoFault;
      }}
    }}

    _pathOpen(fd, _dirflags, pathPtr, pathLen, oflags, _rightsBase, _rightsInheriting, _fdflags, openedFdPtr) {{
      try {{
        const descriptor = Number(fd) >>> 0;
        const entry = this.fdTable.get(descriptor);
        if (!entry || entry.kind !== "preopen") {{
          return __agentOsWasiErrnoBadf;
        }}
        const target = this._readString(pathPtr, pathLen);
        const hostPath = __agentOsPath().resolve(entry.hostPath, target);
        const mode = (Number(oflags) & 0x1) !== 0 || (Number(oflags) & 0x8) !== 0 ? "w+" : "r";
        const realFd = __agentOsFs().openSync(hostPath, mode);
        const openedFd = this.nextFd++;
        this.fdTable.set(openedFd, {{ kind: "file", realFd }});
        return this._writeUint32(openedFdPtr, openedFd);
      }} catch {{
        return __agentOsWasiErrnoFault;
      }}
    }}

    _pollOneoff(_inPtr, _outPtr, _nsubscriptions, neventsPtr) {{
      return this._writeUint32(neventsPtr, 0);
    }}

    _procExit(code) {{
      if (this.returnOnExit) {{
        const error = new Error(`wasi exit(${{Number(code) >>> 0}})`);
        error.__agentOsWasiExit = true;
        error.code = Number(code) >>> 0;
        throw error;
      }}
      process.exit(Number(code) >>> 0);
    }}
  }}

  Object.defineProperty(globalThis, "__agentOsWasiModule", {{
    configurable: true,
    enumerable: false,
    value: {{ WASI }},
    writable: true,
  }});
}}
if (typeof process !== "undefined") {{
  process.env = {{ ...(process.env || {{}}), ...__agentOsWasmInternalEnv }};
}}
if (typeof globalThis !== "undefined" && typeof globalThis.__agentOsSyncRpc === "undefined") {{
  const __agentOsNormalizeBytes = (value) => {{
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
      value.__agentOsType === "bytes" &&
      typeof value.base64 === "string"
    ) {{
      return Buffer.from(value.base64, "base64");
    }}
    return value;
  }};
  const __agentOsWasmSyncRpc = {{
    callSync(method, args = []) {{
      switch (method) {{
        case "fs.fstatSync":
          return __agentOsRequireBuiltin("node:fs").fstatSync(...args);
        case "fs.lstatSync":
          return __agentOsRequireBuiltin("node:fs").lstatSync(...args);
        case "fs.statSync":
          return __agentOsRequireBuiltin("node:fs").statSync(...args);
        case "fs.chmodSync":
          return __agentOsRequireBuiltin("node:fs").chmodSync(...args);
        case "__kernel_poll":
          if (typeof _kernelPollRaw === "undefined") {{
            throw new Error("Agent OS WASM kernel poll bridge is unavailable");
          }}
          return _kernelPollRaw.applySync(void 0, args);
        case "child_process.spawn": {{
          if (typeof _childProcessSpawnStart === "undefined") {{
            throw new Error("Agent OS WASM child_process bridge is unavailable");
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
            throw new Error("Agent OS WASM child_process poll bridge is unavailable");
          }}
          return _childProcessPoll.applySync(void 0, args);
        case "child_process.kill":
          if (typeof _childProcessKill === "undefined") {{
            throw new Error("Agent OS WASM child_process kill bridge is unavailable");
          }}
          return _childProcessKill.applySync(void 0, args);
        case "child_process.write_stdin": {{
          if (typeof _childProcessStdinWrite === "undefined") {{
            throw new Error("Agent OS WASM child_process stdin bridge is unavailable");
          }}
          const [childId, chunk] = args;
          return _childProcessStdinWrite.applySync(void 0, [
            childId,
            __agentOsNormalizeBytes(chunk),
          ]);
        }}
        case "child_process.close_stdin":
          if (typeof _childProcessStdinClose === "undefined") {{
            throw new Error("Agent OS WASM child_process stdin-close bridge is unavailable");
          }}
          return _childProcessStdinClose.applySync(void 0, args);
        case "net.connect":
          if (typeof _netSocketConnectRaw === "undefined") {{
            throw new Error("Agent OS WASM net.connect bridge is unavailable");
          }}
          return _netSocketConnectRaw.applySync(void 0, args);
        case "net.poll":
          if (typeof _netSocketPollRaw === "undefined") {{
            throw new Error("Agent OS WASM net.poll bridge is unavailable");
          }}
          return _netSocketPollRaw.applySync(void 0, args);
        case "net.write":
          if (typeof _netSocketWriteRaw === "undefined") {{
            throw new Error("Agent OS WASM net.write bridge is unavailable");
          }}
          return _netSocketWriteRaw.applySync(void 0, args);
        case "net.destroy":
          if (typeof _netSocketDestroyRaw === "undefined") {{
            throw new Error("Agent OS WASM net.destroy bridge is unavailable");
          }}
          return _netSocketDestroyRaw.applySync(void 0, args);
        case "net.socket_upgrade_tls":
          if (typeof _netSocketUpgradeTlsRaw === "undefined") {{
            throw new Error("Agent OS WASM TLS-upgrade bridge is unavailable");
          }}
          return _netSocketUpgradeTlsRaw.applySync(void 0, args);
        case "process.signal_state": {{
          if (typeof _processSignalState === "undefined") {{
            throw new Error("Agent OS WASM signal-state bridge is unavailable");
          }}
          const [signal, action = "default", maskJson = "[]", flags = 0] = args;
          return _processSignalState.applySyncPromise(void 0, [
            signal,
            action,
            maskJson,
            flags,
          ]);
        }}
        default:
          throw new Error(`Agent OS WASM sync RPC method not implemented in V8 runtime: ${{method}}`);
      }}
    }},
    async call(method, args = []) {{
      return this.callSync(method, args);
    }},
  }};
  Object.defineProperty(globalThis, "__agentOsSyncRpc", {{
    configurable: true,
    enumerable: false,
    value: __agentOsWasmSyncRpc,
    writable: true,
  }});
}}
{warmup_emit}"#
    )
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

fn prewarm_wasm_path(
    import_cache: &NodeImportCache,
    javascript_engine: &mut JavascriptExecutionEngine,
    javascript_context_id: &str,
    resolved_module: &ResolvedWasmModule,
    request: &StartWasmExecutionRequest,
    frozen_time_ms: u128,
    prewarm_timeout: Duration,
) -> Result<Option<Vec<u8>>, WasmExecutionError> {
    let debug_enabled = env_flag_enabled(&request.env, WASM_WARMUP_DEBUG_ENV);
    let marker_contents = warmup_marker_contents(resolved_module);
    let marker_path = warmup_marker_path(
        import_cache.prewarm_marker_dir(),
        "wasm-runner-prewarm",
        WASM_WARMUP_MARKER_VERSION,
        &marker_contents,
    );

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
        import_cache,
        javascript_context_id,
        resolved_module,
        request,
        frozen_time_ms,
        true,
        None,
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
        next_fd: 64,
        open_files: BTreeMap::new(),
    };
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let started = Instant::now();

    loop {
        let poll_timeout = prewarm_timeout.saturating_sub(started.elapsed());
        if poll_timeout.is_zero() {
            let _ = prewarm_execution.terminate();
            return Err(WasmExecutionError::WarmupTimeout(prewarm_timeout));
        }

        match prewarm_execution
            .poll_event_blocking(poll_timeout)
            .map_err(map_javascript_error)?
        {
            Some(JavascriptExecutionEvent::Stdout(chunk)) => stdout.extend(chunk),
            Some(JavascriptExecutionEvent::Stderr(chunk)) => stderr.extend(chunk),
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
                let _ = prewarm_execution.terminate();
                return Err(WasmExecutionError::WarmupTimeout(prewarm_timeout));
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

fn guest_argv(
    context: &WasmContext,
    request: &StartWasmExecutionRequest,
) -> Result<Vec<String>, WasmExecutionError> {
    if !request.argv.is_empty() {
        return Ok(request.argv.clone());
    }

    match &context.module_path {
        Some(module_path) => Ok(vec![module_path.clone()]),
        None => Err(WasmExecutionError::MissingModulePath),
    }
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
        .unwrap_or_else(|| String::from("/root"))
}

fn mapped_guest_paths_for_host_path(
    host_path: &Path,
    env: &BTreeMap<String, String>,
) -> Vec<String> {
    if !host_path.is_absolute() {
        return Vec::new();
    }

    let mappings = env
        .get("AGENT_OS_GUEST_PATH_MAPPINGS")
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

fn guest_visible_wasm_env(env: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    env.iter()
        .filter(|(key, _)| !is_internal_wasm_guest_env_key(key))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn is_internal_wasm_guest_env_key(key: &str) -> bool {
    key.starts_with("AGENT_OS_") || key.starts_with("NODE_SYNC_RPC_")
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

fn resolve_wasm_execution_timeout(
    request: &StartWasmExecutionRequest,
) -> Result<Option<Duration>, WasmExecutionError> {
    // Node's WASI runtime does not expose per-instruction fuel metering, so the
    // configured "fuel" budget is currently enforced as a tight wall-clock
    // timeout while still being passed through to the child process for
    // observability and future in-runtime enforcement.
    Ok(wasm_limit_u64(&request.env, WASM_MAX_FUEL_ENV)?.map(Duration::from_millis))
}

fn resolve_wasm_prewarm_timeout(
    request: &StartWasmExecutionRequest,
) -> Result<Duration, WasmExecutionError> {
    Ok(Duration::from_millis(
        wasm_limit_u64(&request.env, WASM_PREWARM_TIMEOUT_MS_ENV)?
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

fn warmup_guest_argv(
    resolved_module: &ResolvedWasmModule,
    request: &StartWasmExecutionRequest,
) -> Vec<String> {
    if !request.argv.is_empty() {
        return request.argv.clone();
    }

    vec![resolved_module.specifier.clone()]
}

fn wasm_stack_limit_bytes(
    request: &StartWasmExecutionRequest,
) -> Result<Option<usize>, WasmExecutionError> {
    wasm_limit_usize(&request.env, WASM_MAX_STACK_BYTES_ENV)
}

fn wasm_memory_limit_bytes(
    request: &StartWasmExecutionRequest,
) -> Result<Option<u64>, WasmExecutionError> {
    wasm_limit_u64(&request.env, WASM_MAX_MEMORY_BYTES_ENV)
}

fn wasm_memory_limit_pages(memory_limit_bytes: u64) -> Result<u32, WasmExecutionError> {
    let pages = memory_limit_bytes / WASM_PAGE_BYTES;
    u32::try_from(pages).map_err(|_| {
        WasmExecutionError::InvalidLimit(format!(
            "{WASM_MAX_MEMORY_BYTES_ENV}={memory_limit_bytes}: exceeds V8's wasm page limit range"
        ))
    })
}

fn wasm_limit_u64(
    env: &BTreeMap<String, String>,
    key: &str,
) -> Result<Option<u64>, WasmExecutionError> {
    let Some(value) = env.get(key) else {
        return Ok(None);
    };
    value
        .parse::<u64>()
        .map(Some)
        .map_err(|error| WasmExecutionError::InvalidLimit(format!("{key}={value}: {error}")))
}

fn wasm_limit_usize(
    env: &BTreeMap<String, String>,
    key: &str,
) -> Result<Option<usize>, WasmExecutionError> {
    let Some(value) = env.get(key) else {
        return Ok(None);
    };
    value
        .parse::<usize>()
        .map(Some)
        .map_err(|error| WasmExecutionError::InvalidLimit(format!("{key}={value}: {error}")))
}

fn validate_module_limits(
    resolved_module: &ResolvedWasmModule,
    request: &StartWasmExecutionRequest,
) -> Result<(), WasmExecutionError> {
    let Some(memory_limit) = wasm_memory_limit_bytes(request)? else {
        return Ok(());
    };

    let resolved_path = &resolved_module.resolved_path;
    let metadata = fs::metadata(&resolved_path).map_err(|error| {
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
    let bytes = fs::read(&resolved_path).map_err(|error| {
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
            return Err(WasmExecutionError::InvalidModule(format!(
                "initial WebAssembly memory of {initial_bytes} bytes exceeds the configured limit of {memory_limit} bytes"
            )));
        }
    }

    match module_limits.maximum_memory_bytes {
        Some(maximum_bytes) if maximum_bytes > memory_limit => Err(WasmExecutionError::InvalidModule(
            format!(
                "WebAssembly memory maximum of {maximum_bytes} bytes exceeds the configured limit of {memory_limit} bytes"
            ),
        )),
        Some(_) => Ok(()),
        None => Ok(()),
    }
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

impl From<NodeSignalDispositionAction> for WasmSignalDispositionAction {
    fn from(value: NodeSignalDispositionAction) -> Self {
        match value {
            NodeSignalDispositionAction::Default => Self::Default,
            NodeSignalDispositionAction::Ignore => Self::Ignore,
            NodeSignalDispositionAction::User => Self::User,
        }
    }
}

impl From<NodeSignalHandlerRegistration> for WasmSignalHandlerRegistration {
    fn from(value: NodeSignalHandlerRegistration) -> Self {
        Self {
            action: value.action.into(),
            mask: value.mask,
            flags: value.flags,
        }
    }
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
        resolve_wasm_execution_timeout, resolve_wasm_prewarm_timeout, resolved_module_path,
        wasm_guest_module_paths, wasm_memory_limit_pages, StartWasmExecutionRequest,
        WasmPermissionTier, WASM_MAX_FUEL_ENV, WASM_MAX_MEMORY_BYTES_ENV, WASM_PAGE_BYTES,
        WASM_PREWARM_TIMEOUT_MS_ENV,
    };
    use std::collections::BTreeMap;
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::Path;
    use std::time::Duration;
    use tempfile::tempdir;

    fn request_with_env(cwd: &Path, env: BTreeMap<String, String>) -> StartWasmExecutionRequest {
        StartWasmExecutionRequest {
            vm_id: String::from("vm-wasm"),
            context_id: String::from("ctx-wasm"),
            argv: Vec::new(),
            env,
            cwd: cwd.to_path_buf(),
            permission_tier: WasmPermissionTier::Full,
        }
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
    fn wasm_prewarm_timeout_is_separate_from_execution_timeout() {
        let temp = tempdir().expect("create temp dir");
        let request = request_with_env(
            temp.path(),
            BTreeMap::from([
                (String::from(WASM_MAX_FUEL_ENV), String::from("25")),
                (
                    String::from(WASM_PREWARM_TIMEOUT_MS_ENV),
                    String::from("750"),
                ),
            ]),
        );

        assert_eq!(
            resolve_wasm_execution_timeout(&request).expect("execution timeout"),
            Some(Duration::from_millis(25))
        );
        assert_eq!(
            resolve_wasm_prewarm_timeout(&request).expect("prewarm timeout"),
            Duration::from_millis(750)
        );
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
                String::from("AGENT_OS_GUEST_PATH_MAPPINGS"),
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
