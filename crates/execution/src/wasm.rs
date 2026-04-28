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
use crate::v8_host::V8SessionHandle;
use crate::v8_runtime;
use base64::Engine as _;
use serde_json::{Value, json};
use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::fs;
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const WASM_MODULE_PATH_ENV: &str = "AGENT_OS_WASM_MODULE_PATH";
const WASM_GUEST_ARGV_ENV: &str = "AGENT_OS_GUEST_ARGV";
const WASM_GUEST_ENV_ENV: &str = "AGENT_OS_GUEST_ENV";
const WASM_PERMISSION_TIER_ENV: &str = "AGENT_OS_WASM_PERMISSION_TIER";
const WASM_PREWARM_ONLY_ENV: &str = "AGENT_OS_WASM_PREWARM_ONLY";
const WASM_HOST_CWD_ENV: &str = "AGENT_OS_WASM_HOST_CWD";
const WASM_SANDBOX_ROOT_ENV: &str = "AGENT_OS_SANDBOX_ROOT";
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
const DEFAULT_WASM_GUEST_HOME: &str = "/root";
const DEFAULT_WASM_GUEST_USER: &str = "root";
const DEFAULT_WASM_GUEST_SHELL: &str = "/bin/sh";
const DEFAULT_WASM_GUEST_PATH: &str =
    "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
// Warmup is a best-effort compile-cache optimization; fall back to a cold start
// instead of burning minutes on a stalled prewarm session.
const DEFAULT_WASM_PREWARM_TIMEOUT_MS: u64 = 30_000;
const MAX_SYNC_WASM_PREWARM_MODULE_BYTES: u64 = 16 * 1024 * 1024;
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
    pending_events: VecDeque<WasmExecutionEvent>,
    stdout_stream_buffer: Vec<u8>,
    stderr_stream_buffer: Vec<u8>,
}

#[derive(Debug)]
struct WasmInternalSyncRpc {
    module_guest_paths: Vec<String>,
    module_host_path: PathBuf,
    guest_cwd: String,
    host_cwd: PathBuf,
    sandbox_root: Option<PathBuf>,
    guest_path_mappings: Vec<WasmGuestPathMapping>,
    next_fd: u32,
    open_files: BTreeMap<u32, fs::File>,
    pending_events: VecDeque<WasmExecutionEvent>,
}

#[derive(Debug, Clone)]
struct WasmGuestPathMapping {
    guest_path: String,
    host_path: PathBuf,
}

impl WasmExecution {
    pub fn execution_id(&self) -> &str {
        &self.execution_id
    }

    pub fn child_pid(&self) -> u32 {
        self.child_pid
    }

    pub fn v8_session_handle(&self) -> V8SessionHandle {
        self.inner.v8_session_handle()
    }

    pub fn uses_shared_v8_runtime(&self) -> bool {
        self.inner.uses_shared_v8_runtime()
    }

    pub fn write_stdin(&mut self, chunk: &[u8]) -> Result<(), WasmExecutionError> {
        self.inner.write_stdin(chunk).map_err(map_javascript_error)
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
            if let Some(event) = self.pending_events.pop_front() {
                return Ok(Some(event));
            }
            if let Some(event) = self.internal_sync_rpc.pending_events.pop_front() {
                self.enqueue_wasm_event(event)?;
                continue;
            }
            match self
                .inner
                .poll_event(timeout)
                .await
                .map_err(map_javascript_error)?
            {
                Some(event) => {
                    if let JavascriptExecutionEvent::SyncRpcRequest(request) = &event {
                        if self.handle_internal_sync_rpc(request)? {
                            continue;
                        }
                        if let Some(signal_state) = self.handle_signal_state_sync_rpc(request)? {
                            return Ok(Some(signal_state));
                        }
                    }
                    self.enqueue_javascript_event(event)?;
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
            if let Some(event) = self.pending_events.pop_front() {
                return Ok(Some(event));
            }
            if let Some(event) = self.internal_sync_rpc.pending_events.pop_front() {
                self.enqueue_wasm_event(event)?;
                continue;
            }
            match self
                .inner
                .poll_event_blocking(timeout)
                .map_err(map_javascript_error)?
            {
                Some(event) => {
                    if let JavascriptExecutionEvent::SyncRpcRequest(request) = &event {
                        if self.handle_internal_sync_rpc(request)? {
                            continue;
                        }
                        if let Some(signal_state) = self.handle_signal_state_sync_rpc(request)? {
                            return Ok(Some(signal_state));
                        }
                    }
                    self.enqueue_javascript_event(event)?;
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
                Some(WasmExecutionEvent::SyncRpcRequest(request)) => {
                    if self.handle_wait_sync_rpc_request(&request, &mut stdout, &mut stderr)? {
                        continue;
                    }
                    return Err(WasmExecutionError::RpcResponse(format!(
                        "unexpected guest WebAssembly sync RPC request {} while waiting",
                        request.method
                    )));
                }
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

    fn enqueue_javascript_event(
        &mut self,
        event: JavascriptExecutionEvent,
    ) -> Result<(), WasmExecutionError> {
        match event {
            JavascriptExecutionEvent::Stdout(chunk) => {
                self.enqueue_stream_chunk(StreamChannel::Stdout, chunk)?
            }
            JavascriptExecutionEvent::Stderr(chunk) => {
                self.enqueue_stream_chunk(StreamChannel::Stderr, chunk)?
            }
            JavascriptExecutionEvent::SyncRpcRequest(request) => {
                self.pending_events
                    .push_back(WasmExecutionEvent::SyncRpcRequest(request));
            }
            JavascriptExecutionEvent::SignalState {
                signal,
                registration,
            } => {
                self.pending_events
                    .push_back(WasmExecutionEvent::SignalState {
                        signal,
                        registration: registration.into(),
                    });
            }
            JavascriptExecutionEvent::Exited(code) => {
                self.flush_stream_buffers();
                self.pending_events
                    .push_back(WasmExecutionEvent::Exited(code));
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
                self.flush_stream_buffers();
                self.pending_events
                    .push_back(WasmExecutionEvent::Exited(code));
            }
            other => self.pending_events.push_back(other),
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
        buffer.extend_from_slice(&chunk);

        while let Some(newline_index) = buffer.iter().position(|byte| *byte == b'\n') {
            let line = buffer.drain(..=newline_index).collect::<Vec<_>>();
            if let Some(signal_state) = parse_wasm_signal_state_line(&line)? {
                self.pending_events.push_back(signal_state);
                continue;
            }
            self.pending_events.push_back(match channel {
                StreamChannel::Stdout => WasmExecutionEvent::Stdout(line),
                StreamChannel::Stderr => WasmExecutionEvent::Stderr(line),
            });
        }

        Ok(())
    }

    fn flush_stream_buffers(&mut self) {
        if !self.stdout_stream_buffer.is_empty() {
            self.pending_events
                .push_back(WasmExecutionEvent::Stdout(std::mem::take(
                    &mut self.stdout_stream_buffer,
                )));
        }
        if !self.stderr_stream_buffer.is_empty() {
            self.pending_events
                .push_back(WasmExecutionEvent::Stderr(std::mem::take(
                    &mut self.stderr_stream_buffer,
                )));
        }
    }

    fn handle_wait_sync_rpc_request(
        &mut self,
        request: &JavascriptSyncRpcRequest,
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

        if request.method != "__kernel_stdio_write" {
            return Ok(false);
        }

        let Some(descriptor) = request.args.first().and_then(Value::as_u64) else {
            return Err(WasmExecutionError::RpcResponse(String::from(
                "missing __kernel_stdio_write descriptor",
            )));
        };
        let Some(bytes) = decode_wasm_bytes_arg(request.args.get(1)) else {
            return Err(WasmExecutionError::RpcResponse(String::from(
                "missing __kernel_stdio_write payload bytes",
            )));
        };

        match descriptor {
            1 => stdout.extend_from_slice(&bytes),
            2 => stderr.extend_from_slice(&bytes),
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

#[derive(Clone, Copy)]
enum StreamChannel {
    Stdout,
    Stderr,
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
        verify_wasm_module_header(&resolved_module)?;
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
        let guest_path_mappings = wasm_guest_path_mappings(&request);

        Ok(WasmExecution {
            execution_id,
            child_pid,
            inner: javascript_execution,
            execution_timeout,
            pending_events: VecDeque::new(),
            stdout_stream_buffer: Vec::new(),
            stderr_stream_buffer: Vec::new(),
            internal_sync_rpc: WasmInternalSyncRpc {
                module_guest_paths: wasm_guest_module_paths(
                    &resolved_module.specifier,
                    &request.env,
                ),
                module_host_path: resolved_module.resolved_path.clone(),
                guest_cwd: wasm_guest_cwd(&request.env),
                host_cwd: request.cwd.clone(),
                sandbox_root: wasm_sandbox_root(&request.env),
                guest_path_mappings,
                next_fd: 64,
                open_files: BTreeMap::new(),
                pending_events: VecDeque::new(),
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
            return Ok(false);
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
        let target = fs::read_link(&host_path)
            .map(|target| Value::String(target.to_string_lossy().into_owned()));
        return respond_wasm_sync_rpc_value(execution, request, path, target).map(|()| true);
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
        if fd == 1 || fd == 2 {
            internal_sync_rpc.pending_events.push_back(if fd == 1 {
                WasmExecutionEvent::Stdout(bytes.clone())
            } else {
                WasmExecutionEvent::Stderr(bytes.clone())
            });
            execution
                .respond_sync_rpc_success(request.id, json!(bytes.len()))
                .map_err(map_javascript_error)?;
            return Ok(true);
        }
        let position = request.args.get(2).and_then(Value::as_u64);
        let Some(file) = internal_sync_rpc.open_files.get_mut(&(fd as u32)) else {
            return Ok(false);
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
            return Ok(false);
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
    if let Some(host_path) = translate_wasm_host_runtime_path(path, internal_sync_rpc) {
        return Some(host_path);
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
            return Some(join_host_path(&mapping.host_path, &suffix));
        }
    }
    if let Some(suffix) = strip_guest_prefix(&normalized_path, &internal_sync_rpc.guest_cwd) {
        return Some(join_host_path(&internal_sync_rpc.host_cwd, &suffix));
    }
    if normalized_path.starts_with('/') {
        let root_candidate = internal_sync_rpc
            .sandbox_root
            .as_ref()
            .map(|root| join_host_path(root, normalized_path.trim_start_matches('/')));
        if let Some(candidate) = root_candidate.as_ref() {
            if candidate.exists() {
                return Some(candidate.clone());
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
                        return Some(candidate);
                    }
                }
            }
            if let Some(suffix) =
                strip_guest_prefix(&cwd_relative_guest_path, &internal_sync_rpc.guest_cwd)
            {
                let candidate = join_host_path(&internal_sync_rpc.host_cwd, &suffix);
                if candidate.exists() {
                    return Some(candidate);
                }
            }
        }

        return root_candidate;
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

fn respond_wasm_sync_rpc_metadata(
    execution: &mut JavascriptExecution,
    request: &JavascriptSyncRpcRequest,
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
    request: &JavascriptSyncRpcRequest,
    label: &str,
    result: Result<(), std::io::Error>,
) -> Result<(), WasmExecutionError> {
    respond_wasm_sync_rpc_value(execution, request, label, result.map(|()| Value::Null))
}

fn respond_wasm_sync_rpc_value(
    execution: &mut JavascriptExecution,
    request: &JavascriptSyncRpcRequest,
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
    env.extend(
        internal_env
            .iter()
            .filter(|(key, _)| key.as_str() != "AGENT_OS_WASM_MODULE_BASE64")
            .map(|(key, value)| (key.clone(), value.clone())),
    );

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
    let guest_path_mappings = wasm_guest_path_mappings(request);
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
            String::from("AGENT_OS_WASM_MODULE_BASE64"),
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
        WASM_HOST_CWD_ENV.to_string(),
        request.cwd.to_string_lossy().into_owned(),
    );
    internal_env.insert(
        String::from("AGENT_OS_GUEST_PATH_MAPPINGS"),
        encode_wasm_guest_path_mappings(&guest_path_mappings),
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
  const __agentOsCrypto = () => __agentOsRequireBuiltin("node:crypto");
  const __agentOsWasiErrnoSuccess = 0;
  const __agentOsWasiErrnoAcces = 2;
  const __agentOsWasiErrnoBadf = 8;
  const __agentOsWasiErrnoExist = 20;
  const __agentOsWasiErrnoFault = 21;
  const __agentOsWasiErrnoInval = 28;
  const __agentOsWasiErrnoIo = 29;
  const __agentOsWasiErrnoNoent = 44;
  const __agentOsWasiErrnoNosys = 52;
  const __agentOsWasiErrnoNotdir = 54;
  const __agentOsWasiErrnoRofs = 69;
  const __agentOsWasiFiletypeUnknown = 0;
  const __agentOsWasiFiletypeCharacterDevice = 2;
  const __agentOsWasiFiletypeDirectory = 3;
  const __agentOsWasiFiletypeRegularFile = 4;
  const __agentOsWasiFiletypeSymbolicLink = 7;
  const __agentOsWasiLookupSymlinkFollow = 1;
  const __agentOsWasiOpenCreate = 1;
  const __agentOsWasiOpenDirectory = 2;
  const __agentOsWasiOpenExclusive = 4;
  const __agentOsWasiOpenTruncate = 8;
  const __agentOsWasiRightFdWrite = 1n << 6n;
  const __agentOsWasiDefaultRightsBase = 0xffffffffffffffffn;
  const __agentOsWasiDefaultRightsInheriting = 0xffffffffffffffffn;
  const __agentOsWasiWhenceSet = 0;
  const __agentOsWasiWhenceCur = 1;
  const __agentOsWasiWhenceEnd = 2;
  const __agentOsKernelStdioSyncRpcEnabled = () =>
    process?.env?.AGENT_OS_WASI_STDIO_SYNC_RPC === "1";
  const __agentOsWasiDebugEnabled = () => process?.env?.AGENT_OS_WASM_WASI_DEBUG === "1";
  const __agentOsWasiDebug = (message) => {{
    if (!__agentOsWasiDebugEnabled() || typeof process?.stderr?.write !== "function") {{
      return;
    }}
    try {{
      process.stderr.write(`[agent-os-wasi] ${{message}}\n`);
    }} catch {{
      // Ignore debug logging failures.
    }}
  }};

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
        [0, {{ kind: "stdin", fdFlags: 0 }}],
        [1, {{ kind: "stdout", fdFlags: 0 }}],
        [2, {{ kind: "stderr", fdFlags: 0 }}],
      ]);
      for (const [guestPath, spec] of Object.entries(this.preopens)) {{
        const normalized = this._normalizePreopenSpec(spec);
        if (!normalized) {{
          continue;
        }}
        this.fdTable.set(this.nextFd++, {{
          kind: "preopen",
          guestPath: String(guestPath),
          hostPath: normalized.hostPath,
          rightsBase: normalized.rightsBase,
          rightsInheriting: normalized.rightsInheriting,
          fdFlags: 0,
        }});
      }}
      this.wasiImport = {{
        args_get: (...args) => this._argsGet(...args),
        args_sizes_get: (...args) => this._argsSizesGet(...args),
        clock_time_get: (...args) => this._clockTimeGet(...args),
        clock_res_get: (...args) => this._clockResGet(...args),
        environ_get: (...args) => this._environGet(...args),
        environ_sizes_get: (...args) => this._environSizesGet(...args),
        fd_close: (...args) => this._fdClose(...args),
        fd_fdstat_get: (...args) => this._fdFdstatGet(...args),
        fd_fdstat_set_flags: (...args) => this._fdFdstatSetFlags(...args),
        fd_filestat_get: (...args) => this._fdFilestatGet(...args),
        fd_filestat_set_size: (...args) => this._fdFilestatSetSize(...args),
        fd_prestat_dir_name: (...args) => this._fdPrestatDirName(...args),
        fd_prestat_get: (...args) => this._fdPrestatGet(...args),
        fd_pread: (...args) => this._fdPread(...args),
        fd_pwrite: (...args) => this._fdPwrite(...args),
        fd_readdir: (...args) => this._fdReaddir(...args),
        fd_read: (...args) => this._fdRead(...args),
        fd_seek: (...args) => this._fdSeek(...args),
        fd_sync: (...args) => this._fdSync(...args),
        fd_tell: (...args) => this._fdTell(...args),
        fd_write: (...args) => this._fdWrite(...args),
        path_create_directory: (...args) => this._pathCreateDirectory(...args),
        path_filestat_get: (...args) => this._pathFilestatGet(...args),
        path_link: (...args) => this._pathLink(...args),
        path_open: (...args) => this._pathOpen(...args),
        path_readlink: (...args) => this._pathReadlink(...args),
        path_remove_directory: (...args) => this._pathRemoveDirectory(...args),
        path_rename: (...args) => this._pathRename(...args),
        path_symlink: (...args) => this._pathSymlink(...args),
        path_unlink_file: (...args) => this._pathUnlinkFile(...args),
        poll_oneoff: (...args) => this._pollOneoff(...args),
        proc_exit: (...args) => this._procExit(...args),
        random_get: (...args) => this._randomGet(...args),
        sched_yield: (...args) => this._schedYield(...args),
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

    _normalizeRights(value, fallback) {{
      try {{
        return BigInt(value);
      }} catch {{
        return fallback;
      }}
    }}

    _normalizePreopenSpec(value) {{
      if (typeof value === "string") {{
        return {{
          hostPath: String(value),
          rightsBase: __agentOsWasiDefaultRightsBase,
          rightsInheriting: __agentOsWasiDefaultRightsInheriting,
        }};
      }}
      if (!value || typeof value !== "object" || typeof value.hostPath !== "string") {{
        return null;
      }}
      return {{
        hostPath: String(value.hostPath),
        rightsBase: this._normalizeRights(
          value.rightsBase,
          __agentOsWasiDefaultRightsBase,
        ),
        rightsInheriting: this._normalizeRights(
          value.rightsInheriting,
          __agentOsWasiDefaultRightsInheriting,
        ),
      }};
    }}

    _descriptorRightsBase(entry) {{
      return this._normalizeRights(
        entry?.rightsBase,
        __agentOsWasiDefaultRightsBase,
      );
    }}

    _descriptorRightsInheriting(entry) {{
      return this._normalizeRights(
        entry?.rightsInheriting,
        __agentOsWasiDefaultRightsInheriting,
      );
    }}

    _hasWriteRights(rights) {{
      try {{
        return (BigInt(rights) & __agentOsWasiRightFdWrite) !== 0n;
      }} catch {{
        return true;
      }}
    }}

    _writeUint32(ptr, value) {{
      try {{
        this._memoryView().setUint32(Number(ptr) >>> 0, Number(value) >>> 0, true);
        return __agentOsWasiErrnoSuccess;
      }} catch {{
        __agentOsWasiDebug(`writeUint32 failed ptr=${{Number(ptr)}} value=${{Number(value)}}`);
        return __agentOsWasiErrnoFault;
      }}
    }}

    _writeUint64(ptr, value) {{
      try {{
        this._memoryView().setBigUint64(Number(ptr) >>> 0, BigInt(value), true);
        return __agentOsWasiErrnoSuccess;
      }} catch {{
        __agentOsWasiDebug(`writeUint64 failed ptr=${{Number(ptr)}} value=${{String(value)}}`);
        return __agentOsWasiErrnoFault;
      }}
    }}

    _writeBytes(ptr, bytes) {{
      try {{
        this._memoryBytes().set(bytes, Number(ptr) >>> 0);
        return __agentOsWasiErrnoSuccess;
      }} catch {{
        __agentOsWasiDebug(`writeBytes failed ptr=${{Number(ptr)}} len=${{bytes?.length ?? 0}}`);
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

    _decodeSyncRpcBytes(value) {{
      if (value == null) {{
        return null;
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
      return null;
    }}

    _dequeuePipeBytes(pipe, maxBytes) {{
      if (!pipe || !Array.isArray(pipe.chunks) || pipe.chunks.length === 0) {{
        return Buffer.alloc(0);
      }}

      let remaining = Math.max(0, Number(maxBytes) >>> 0);
      if (remaining === 0) {{
        return Buffer.alloc(0);
      }}

      const parts = [];
      while (remaining > 0 && pipe.chunks.length > 0) {{
        const chunk = pipe.chunks[0];
        if (!chunk || chunk.length === 0) {{
          pipe.chunks.shift();
          continue;
        }}

        if (chunk.length <= remaining) {{
          parts.push(chunk);
          pipe.chunks.shift();
          remaining -= chunk.length;
          continue;
        }}

        parts.push(chunk.subarray(0, remaining));
        pipe.chunks[0] = chunk.subarray(remaining);
        remaining = 0;
      }}

      return Buffer.concat(parts);
    }}

    _enqueuePipeBytes(pipe, bytes) {{
      if (!pipe || !Array.isArray(pipe.chunks)) {{
        return;
      }}
      const chunk = Buffer.from(bytes ?? []);
      if (chunk.length === 0) {{
        return;
      }}
      pipe.chunks.push(chunk);
    }}

    _flushPipeConsumers(pipe) {{
      if (
        !pipe ||
        typeof pipe.consumers?.entries !== "function" ||
        !Array.isArray(pipe.chunks) ||
        pipe.chunks.length === 0 ||
        typeof globalThis?.__agentOsSyncRpc?.callSync !== "function"
      ) {{
        return false;
      }}

      let flushed = false;
      while (pipe.chunks.length > 0) {{
        const chunk = pipe.chunks.shift();
        if (!chunk || chunk.length === 0) {{
          continue;
        }}

        for (const [consumerKey, consumer] of Array.from(pipe.consumers.entries())) {{
          if (!consumer || typeof consumer.childId !== "string") {{
            pipe.consumers.delete(consumerKey);
            continue;
          }}
          try {{
            globalThis.__agentOsSyncRpc.callSync("child_process.write_stdin", [
              consumer.childId,
              chunk,
            ]);
            flushed = true;
          }} catch {{
            pipe.consumers.delete(consumerKey);
          }}
        }}
      }}

      return flushed;
    }}

    _closePipeConsumers(pipe) {{
      if (
        !pipe ||
        typeof pipe.consumers?.entries !== "function" ||
        typeof globalThis?.__agentOsSyncRpc?.callSync !== "function"
      ) {{
        return false;
      }}

      let closed = false;
      for (const [consumerKey, consumer] of Array.from(pipe.consumers.entries())) {{
        if (!consumer || typeof consumer.childId !== "string") {{
          pipe.consumers.delete(consumerKey);
          continue;
        }}
        try {{
          globalThis.__agentOsSyncRpc.callSync("child_process.close_stdin", [
            consumer.childId,
          ]);
          closed = true;
        }} catch {{
          // Ignore close errors during teardown.
        }}
        pipe.consumers.delete(consumerKey);
      }}

      return closed;
    }}

    _pumpPipeProducers(pipe, waitMs) {{
      if (
        !pipe ||
        typeof pipe.producers?.entries !== "function" ||
        typeof globalThis?.__agentOsSyncRpc?.callSync !== "function"
      ) {{
        return false;
      }}

      let processed = false;
      for (const [producerKey, producer] of Array.from(pipe.producers.entries())) {{
        if (!producer || typeof producer.childId !== "string") {{
          pipe.producers.delete(producerKey);
          continue;
        }}

        let event = null;
        try {{
          event = globalThis.__agentOsSyncRpc.callSync("child_process.poll", [
            producer.childId,
            Math.max(0, Number(waitMs) >>> 0),
          ]);
        }} catch {{
          pipe.producers.delete(producerKey);
          continue;
        }}

        if (!event) {{
          continue;
        }}

        processed = true;
        const streamType =
          producer.stream === "stderr" ? "stderr" : producer.stream === "stdout" ? "stdout" : null;
        if ((event.type === "stdout" || event.type === "stderr") && event.type === streamType) {{
          const chunk = this._decodeSyncRpcBytes(event.data);
          if (chunk && chunk.length > 0) {{
            pipe.chunks.push(Buffer.from(chunk));
          }}
          continue;
        }}

        if (event.type === "exit") {{
          pipe.producers.delete(producerKey);
          if (pipe.producers.size === 0 && (pipe.writeHandleCount ?? 0) === 0) {{
            this._closePipeConsumers(pipe);
          }}
          continue;
        }}
      }}

      return processed;
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

    _stringTable(values) {{
      return values.map((value) => Buffer.from(`${{String(value)}}\0`, "utf8"));
    }}

    _writeStringTable(values, offsetsPtr, bufferPtr) {{
      try {{
        const view = this._memoryView();
        const memory = this._memoryBytes();
        let cursor = Number(bufferPtr) >>> 0;
        for (let index = 0; index < values.length; index += 1) {{
          const bytes = values[index];
          view.setUint32((Number(offsetsPtr) >>> 0) + index * 4, cursor, true);
          memory.set(bytes, cursor);
          cursor += bytes.length;
        }}
        return __agentOsWasiErrnoSuccess;
      }} catch {{
        __agentOsWasiDebug(
          `writeStringTable failed offsetsPtr=${{Number(offsetsPtr)}} bufferPtr=${{Number(bufferPtr)}} count=${{values.length}}`,
        );
        return __agentOsWasiErrnoFault;
      }}
    }}

    _filetypeForStats(stats) {{
      if (!stats) {{
        return __agentOsWasiFiletypeUnknown;
      }}
      if (typeof stats.isDirectory === "function" && stats.isDirectory()) {{
        return __agentOsWasiFiletypeDirectory;
      }}
      if (typeof stats.isFile === "function" && stats.isFile()) {{
        return __agentOsWasiFiletypeRegularFile;
      }}
      if (typeof stats.isSymbolicLink === "function" && stats.isSymbolicLink()) {{
        return __agentOsWasiFiletypeSymbolicLink;
      }}
      if (typeof stats.isCharacterDevice === "function" && stats.isCharacterDevice()) {{
        return __agentOsWasiFiletypeCharacterDevice;
      }}
      return __agentOsWasiFiletypeUnknown;
    }}

    _fdFiletype(entry) {{
      if (!entry) {{
        return __agentOsWasiFiletypeUnknown;
      }}
      if (
        entry.kind === "stdin" ||
        entry.kind === "stdout" ||
        entry.kind === "stderr"
      ) {{
        return __agentOsWasiFiletypeCharacterDevice;
      }}
      if (entry.kind === "preopen" || entry.kind === "directory") {{
        return __agentOsWasiFiletypeDirectory;
      }}
      if (entry.kind === "symlink") {{
        return __agentOsWasiFiletypeSymbolicLink;
      }}
      return __agentOsWasiFiletypeRegularFile;
    }}

    _mapFsError(error) {{
      switch (error?.code) {{
        case "EACCES":
        case "EPERM":
          return __agentOsWasiErrnoAcces;
        case "ENOENT":
          return __agentOsWasiErrnoNoent;
        case "ENOTDIR":
          return __agentOsWasiErrnoNotdir;
        case "EEXIST":
          return __agentOsWasiErrnoExist;
        case "EINVAL":
          return __agentOsWasiErrnoInval;
        case "EROFS":
          return __agentOsWasiErrnoRofs;
        default:
          return __agentOsWasiErrnoIo;
      }}
    }}

    _descriptorEntry(fd) {{
      return this.fdTable.get(Number(fd) >>> 0) ?? null;
    }}

    _localFdHandle(fd) {{
      const entry = this._descriptorEntry(fd);
      if (!entry || typeof entry.realFd !== "number") {{
        return null;
      }}
      return {{
        kind: "host-passthrough",
        targetFd: entry.realFd,
        displayFd: Number(fd) >>> 0,
        refCount: 1,
        open: true,
      }};
    }}

    _externalFdHandle(fd) {{
      const descriptor = Number(fd) >>> 0;
      const localHandle = this._localFdHandle(descriptor);
      if (localHandle) {{
        return localHandle;
      }}
      try {{
        if (typeof lookupFdHandle === "function") {{
          return lookupFdHandle(descriptor) ?? null;
        }}
      }} catch {{
        // Fall through to other lookup paths.
      }}
      try {{
        if (typeof globalThis.lookupFdHandle === "function") {{
          return globalThis.lookupFdHandle(descriptor) ?? null;
        }}
      }} catch {{
        // Ignore missing global bridge helpers.
      }}
      return null;
    }}

    _descriptorHostPath(entry) {{
      if (!entry) {{
        return null;
      }}
      if (typeof entry.hostPath === "string") {{
        return entry.hostPath;
      }}
      if (typeof entry.realFd === "number") {{
        return __agentOsFs().readlinkSync(`/proc/self/fd/${{entry.realFd}}`);
      }}
      return null;
    }}

    _descriptorFsPath(entry) {{
      if (!entry) {{
        return null;
      }}
      if (typeof entry.hostPath === "string" && entry.hostPath.length > 0) {{
        return entry.hostPath;
      }}
      if (typeof entry.guestPath === "string" && entry.guestPath.length > 0) {{
        return entry.guestPath;
      }}
      return null;
    }}

    _descriptorGuestPath(entry) {{
      if (!entry) {{
        return null;
      }}
      const guestPath = typeof entry.guestPath === "string" ? entry.guestPath : null;
      if (guestPath === ".") {{
        const pwd =
          typeof this.env?.PWD === "string" && this.env.PWD.startsWith("/")
            ? this.env.PWD
            : typeof this.env?.HOME === "string" && this.env.HOME.startsWith("/")
              ? this.env.HOME
              : "/";
        return __agentOsPath().posix.normalize(pwd);
      }}
      if (typeof guestPath === "string" && guestPath.length > 0) {{
        return __agentOsPath().posix.normalize(guestPath);
      }}
      return null;
    }}

    _resolveHostPathForGuestPath(guestPath) {{
      const normalized = __agentOsPath().posix.normalize(guestPath);
      const mappings = [];
      for (const entry of this.fdTable.values()) {{
        if (entry?.kind !== "preopen" || typeof entry.hostPath !== "string") {{
          continue;
        }}
        const guestRoot = this._descriptorGuestPath(entry);
        if (typeof guestRoot !== "string") {{
          continue;
        }}
        mappings.push({{ guestRoot, hostPath: entry.hostPath }});
      }}
      mappings.sort((left, right) => right.guestRoot.length - left.guestRoot.length);

      for (const mapping of mappings) {{
        const matchesRoot = mapping.guestRoot === "/" && normalized.startsWith("/");
        const matchesNested =
          normalized === mapping.guestRoot ||
          normalized.startsWith(`${{mapping.guestRoot}}/`);
        if (!matchesRoot && !matchesNested) {{
          continue;
        }}
        const suffix =
          normalized === mapping.guestRoot
            ? ""
            : mapping.guestRoot === "/"
              ? normalized.slice(1)
              : normalized.slice(mapping.guestRoot.length + 1);
        return suffix
          ? __agentOsPath().join(mapping.hostPath, ...suffix.split("/"))
          : mapping.hostPath;
      }}

      return null;
    }}

    _resolveDescriptorPath(fd, pathPtr, pathLen) {{
      const entry = this._descriptorEntry(fd);
      if (!entry) {{
        return {{ error: __agentOsWasiErrnoBadf }};
      }}
      const baseGuestPath = this._descriptorGuestPath(entry);
      if (typeof baseGuestPath !== "string") {{
        return {{ error: __agentOsWasiErrnoBadf }};
      }}
      const target = this._readString(pathPtr, pathLen);
      const guestPath = target.startsWith("/")
        ? __agentOsPath().posix.normalize(target)
        : __agentOsPath().posix.resolve(baseGuestPath, target);
      const hostPath = this._resolveHostPathForGuestPath(guestPath);
      if (typeof hostPath !== "string") {{
        return {{ error: __agentOsWasiErrnoNoent }};
      }}
      return {{
        error: __agentOsWasiErrnoSuccess,
        guestPath,
        hostPath,
      }};
    }}

    _writeFilestat(statPtr, stats, fallbackType) {{
      try {{
        const view = this._memoryView();
        const offset = Number(statPtr) >>> 0;
        const filetype = stats ? this._filetypeForStats(stats) : fallbackType;
        view.setBigUint64(offset, 0n, true);
        view.setBigUint64(offset + 8, BigInt(stats?.ino ?? 0), true);
        view.setUint8(offset + 16, filetype);
        view.setBigUint64(offset + 24, BigInt(stats?.nlink ?? 1), true);
        view.setBigUint64(offset + 32, BigInt(stats?.size ?? 0), true);
        view.setBigUint64(offset + 40, BigInt(Math.trunc((stats?.atimeMs ?? 0) * 1000000)), true);
        view.setBigUint64(offset + 48, BigInt(Math.trunc((stats?.mtimeMs ?? 0) * 1000000)), true);
        view.setBigUint64(offset + 56, BigInt(Math.trunc((stats?.ctimeMs ?? 0) * 1000000)), true);
        return __agentOsWasiErrnoSuccess;
      }} catch {{
        return __agentOsWasiErrnoFault;
      }}
    }}

    _argsSizesGet(argcPtr, argvBufSizePtr) {{
      const values = this._stringTable(this.args);
      const total = values.reduce((sum, value) => sum + value.length, 0);
      const argcStatus = this._writeUint32(argcPtr, values.length);
      if (argcStatus !== __agentOsWasiErrnoSuccess) {{
        return argcStatus;
      }}
      return this._writeUint32(argvBufSizePtr, total);
    }}

    _argsGet(argvPtr, argvBufPtr) {{
      return this._writeStringTable(this._stringTable(this.args), argvPtr, argvBufPtr);
    }}

    _environEntries() {{
      return Object.entries(this.env).map(([key, value]) => `${{key}}=${{value}}`);
    }}

    _environSizesGet(countPtr, bufSizePtr) {{
      const values = this._stringTable(this._environEntries());
      const total = values.reduce((sum, value) => sum + value.length, 0);
      const countStatus = this._writeUint32(countPtr, values.length);
      if (countStatus !== __agentOsWasiErrnoSuccess) {{
        return countStatus;
      }}
      return this._writeUint32(bufSizePtr, total);
    }}

    _environGet(environPtr, environBufPtr) {{
      return this._writeStringTable(
        this._stringTable(this._environEntries()),
        environPtr,
        environBufPtr,
      );
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
        const handle = this._externalFdHandle(descriptor);
        if (handle?.kind === "pipe-write" && handle.pipe) {{
          this._enqueuePipeBytes(handle.pipe, bytes);
          this._flushPipeConsumers(handle.pipe);
          return this._writeUint32(nwrittenPtr, bytes.length);
        }}
        if (
          (handle?.kind === "passthrough" || handle?.kind === "host-passthrough") &&
          typeof handle.targetFd === "number"
        ) {{
          if (descriptor === 1 || descriptor === 2) {{
            const sidecarManagedProcess =
              typeof process?.env?.AGENT_OS_SANDBOX_ROOT === "string" &&
              process.env.AGENT_OS_SANDBOX_ROOT.length > 0;
            const useKernelStdioSyncRpc =
              sidecarManagedProcess || __agentOsKernelStdioSyncRpcEnabled();
            if (useKernelStdioSyncRpc) {{
              const written = Number(
                globalThis.__agentOsSyncRpc.callSync("__kernel_stdio_write", [descriptor, bytes]),
              ) >>> 0;
              return this._writeUint32(nwrittenPtr, written);
            }}
          }}
          const written = __agentOsFs().writeSync(
            handle.targetFd,
            bytes,
            0,
            bytes.length,
            null,
          );
          return this._writeUint32(nwrittenPtr, written);
        }}
        const entry = this.fdTable.get(descriptor);
        if (!entry) {{
          return __agentOsWasiErrnoBadf;
        }}
        if (entry.kind === "stdout") {{
          const sidecarManagedProcess =
            typeof process?.env?.AGENT_OS_SANDBOX_ROOT === "string" &&
            process.env.AGENT_OS_SANDBOX_ROOT.length > 0;
          const useKernelStdioSyncRpc =
            sidecarManagedProcess || __agentOsKernelStdioSyncRpcEnabled();
          const written = useKernelStdioSyncRpc
            ? Number(globalThis.__agentOsSyncRpc.callSync("__kernel_stdio_write", [1, bytes])) >>> 0
            : (process.stdout.write(bytes), bytes.length);
          return this._writeUint32(nwrittenPtr, written);
        }}
        if (entry.kind === "stderr") {{
          const sidecarManagedProcess =
            typeof process?.env?.AGENT_OS_SANDBOX_ROOT === "string" &&
            process.env.AGENT_OS_SANDBOX_ROOT.length > 0;
          const useKernelStdioSyncRpc =
            sidecarManagedProcess || __agentOsKernelStdioSyncRpcEnabled();
          const written = useKernelStdioSyncRpc
            ? Number(globalThis.__agentOsSyncRpc.callSync("__kernel_stdio_write", [2, bytes])) >>> 0
            : (process.stderr.write(bytes), bytes.length);
          return this._writeUint32(nwrittenPtr, written);
        }}
        if (entry.kind === "file") {{
          const position = typeof entry.offset === "number" ? entry.offset : null;
          const written = __agentOsFs().writeSync(
            entry.realFd,
            bytes,
            0,
            bytes.length,
            position,
          );
          if (typeof entry.offset === "number") {{
            entry.offset += written;
          }}
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
        const handle = this._externalFdHandle(descriptor);
        if (
          (handle?.kind === "passthrough" || handle?.kind === "host-passthrough") &&
          typeof handle.targetFd === "number"
        ) {{
          const written = __agentOsFs().writeSync(
            handle.targetFd,
            bytes,
            0,
            bytes.length,
            Number(offset) >>> 0,
          );
          return this._writeUint32(nwrittenPtr, written);
        }}
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

    _fdPread(fd, iovs, iovsLen, offset, nreadPtr) {{
      try {{
        const descriptor = Number(fd) >>> 0;
        const explicitOffset = Number(offset) >>> 0;
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
        const handle = this._externalFdHandle(descriptor);
        if (
          (handle?.kind === "passthrough" || handle?.kind === "host-passthrough") &&
          typeof handle.targetFd === "number"
        ) {{
          const bytesRead = __agentOsFs().readSync(
            handle.targetFd,
            buffer,
            0,
            totalLength,
            explicitOffset,
          );
          const written = this._writeToIovs(iovs, iovsLen, buffer.subarray(0, bytesRead));
          return this._writeUint32(nreadPtr, written);
        }}
        const entry = this.fdTable.get(descriptor);
        if (!entry || entry.kind !== "file") {{
          return __agentOsWasiErrnoBadf;
        }}
        const bytesRead = __agentOsFs().readSync(
          entry.realFd,
          buffer,
          0,
          totalLength,
          explicitOffset,
        );
        const written = this._writeToIovs(iovs, iovsLen, buffer.subarray(0, bytesRead));
        return this._writeUint32(nreadPtr, written);
      }} catch {{
        return __agentOsWasiErrnoFault;
      }}
    }}

    _fdRead(fd, iovs, iovsLen, nreadPtr) {{
      try {{
        const descriptor = Number(fd) >>> 0;
        const handle = this._externalFdHandle(descriptor);
        if (handle?.kind === "pipe-read" && handle.pipe) {{
          const totalLength = (() => {{
            const view = this._memoryView();
            let length = 0;
            for (let index = 0; index < (Number(iovsLen) >>> 0); index += 1) {{
              const entryOffset = (Number(iovs) >>> 0) + index * 8;
              length += view.getUint32(entryOffset + 4, true);
            }}
            return length >>> 0;
          }})();
          while (handle.pipe.chunks.length === 0) {{
            if (handle.pipe.writeHandleCount === 0 && handle.pipe.producers.size === 0) {{
              return this._writeUint32(nreadPtr, 0);
            }}
            this._pumpPipeProducers(handle.pipe, 10);
          }}
          const chunk = this._dequeuePipeBytes(handle.pipe, totalLength);
          const written = this._writeToIovs(iovs, iovsLen, chunk);
          return this._writeUint32(nreadPtr, written);
        }}
        const entry = this.fdTable.get(descriptor);
        if (!entry) {{
          return __agentOsWasiErrnoBadf;
        }}
        if (entry.kind === "stdin") {{
          const totalLength = (() => {{
            const view = this._memoryView();
            let length = 0;
            for (let index = 0; index < (Number(iovsLen) >>> 0); index += 1) {{
              const entryOffset = (Number(iovs) >>> 0) + index * 8;
              length += view.getUint32(entryOffset + 4, true);
            }}
            return length >>> 0;
          }})();
          const syncRpc =
            typeof globalThis?.__agentOsSyncRpc?.callSync === "function"
              ? globalThis.__agentOsSyncRpc
              : null;
          const sidecarManagedProcess =
            typeof process?.env?.AGENT_OS_SANDBOX_ROOT === "string" &&
            process.env.AGENT_OS_SANDBOX_ROOT.length > 0;
          if (syncRpc && (sidecarManagedProcess || __agentOsKernelStdioSyncRpcEnabled())) {{
            try {{
              let chunk = null;
              while (true) {{
                const response = syncRpc.callSync("__kernel_stdin_read", [totalLength, 10]);
                if (
                  response &&
                  typeof response === "object" &&
                  typeof response.dataBase64 === "string"
                ) {{
                  chunk = Buffer.from(response.dataBase64, "base64");
                  break;
                }}
                if (response && typeof response === "object" && response.done === true) {{
                  chunk = Buffer.alloc(0);
                  break;
                }}
                if (
                  typeof Atomics?.wait === "function" &&
                  typeof syntheticWaitArray !== "undefined"
                ) {{
                  Atomics.wait(syntheticWaitArray, 0, 0, 10);
                }}
              }}
              if (!chunk || chunk.length === 0) {{
                return this._writeUint32(nreadPtr, 0);
              }}
              const written = this._writeToIovs(iovs, iovsLen, chunk);
              return this._writeUint32(nreadPtr, written);
            }} catch {{
              // Fall back to direct stdin reads when the sync bridge is unavailable
              // in the standalone runner bootstrap.
            }}
          }}
          const buffer = Buffer.alloc(totalLength);
          const directStdinFd =
            (handle?.kind === "passthrough" || handle?.kind === "host-passthrough") &&
            typeof handle.targetFd === "number"
              ? handle.targetFd
              : typeof process?.stdin?.fd === "number"
                ? process.stdin.fd
                : 0;
          const bytesRead = __agentOsFs().readSync(
            directStdinFd,
            buffer,
            0,
            totalLength,
            null,
          );
          const written = this._writeToIovs(iovs, iovsLen, buffer.subarray(0, bytesRead));
          return this._writeUint32(nreadPtr, written);
        }}
        if (
          (handle?.kind === "passthrough" || handle?.kind === "host-passthrough") &&
          typeof handle.targetFd === "number"
        ) {{
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
          const bytesRead = __agentOsFs().readSync(
            handle.targetFd,
            buffer,
            0,
            totalLength,
            null,
          );
          const written = this._writeToIovs(iovs, iovsLen, buffer.subarray(0, bytesRead));
          return this._writeUint32(nreadPtr, written);
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
        const position = typeof entry.offset === "number" ? entry.offset : null;
        const bytesRead = __agentOsFs().readSync(
          entry.realFd,
          buffer,
          0,
          totalLength,
          position,
        );
        if (typeof entry.offset === "number") {{
          entry.offset += bytesRead;
        }}
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
        const retainedDelegateRefs = (() => {{
          try {{
            if (typeof globalThis.__agentOsWasiDelegateFdRefCount === "function") {{
              return Number(globalThis.__agentOsWasiDelegateFdRefCount(descriptor)) || 0;
            }}
          }} catch {{
            // Fall through to the default close path.
          }}
          return 0;
        }})();
        if (entry.kind === "file" && retainedDelegateRefs <= 0) {{
          __agentOsFs().closeSync(entry.realFd);
        }}
        if (descriptor > 2 && retainedDelegateRefs <= 0) {{
          this.fdTable.delete(descriptor);
        }}
        return __agentOsWasiErrnoSuccess;
      }} catch {{
        return __agentOsWasiErrnoFault;
      }}
    }}

    _fdSync(fd) {{
      try {{
        const descriptor = Number(fd) >>> 0;
        const handle = this._externalFdHandle(descriptor);
        if (
          (handle?.kind === "passthrough" || handle?.kind === "host-passthrough") &&
          typeof handle.targetFd === "number"
        ) {{
          __agentOsFs().fsyncSync(handle.targetFd);
          return __agentOsWasiErrnoSuccess;
        }}
        const entry = this.fdTable.get(descriptor);
        if (!entry || entry.kind !== "file" || typeof entry.realFd !== "number") {{
          return __agentOsWasiErrnoBadf;
        }}
        __agentOsFs().fsyncSync(entry.realFd);
        return __agentOsWasiErrnoSuccess;
      }} catch {{
        return __agentOsWasiErrnoFault;
      }}
    }}

    _fdFdstatGet(fd, statPtr) {{
      try {{
        const entry = this._descriptorEntry(fd);
        if (!entry) {{
          return __agentOsWasiErrnoBadf;
        }}
        const view = this._memoryView();
        const offset = Number(statPtr) >>> 0;
        view.setUint8(offset, this._fdFiletype(entry));
        view.setUint16(offset + 2, (Number(entry.fdFlags) >>> 0) & 0xffff, true);
        view.setBigUint64(offset + 8, this._descriptorRightsBase(entry), true);
        view.setBigUint64(offset + 16, this._descriptorRightsInheriting(entry), true);
        return __agentOsWasiErrnoSuccess;
      }} catch {{
        return __agentOsWasiErrnoFault;
      }}
    }}

    _fdFdstatSetFlags(fd, flags) {{
      try {{
        const entry = this._descriptorEntry(fd);
        if (!entry) {{
          return __agentOsWasiErrnoBadf;
        }}
        entry.fdFlags = (Number(flags) >>> 0) & 0xffff;
        return __agentOsWasiErrnoSuccess;
      }} catch {{
        return __agentOsWasiErrnoFault;
      }}
    }}

    _fdFilestatGet(fd, statPtr) {{
      try {{
        const entry = this._descriptorEntry(fd);
        if (!entry) {{
          return __agentOsWasiErrnoBadf;
        }}
        if (
          entry.kind === "stdin" ||
          entry.kind === "stdout" ||
          entry.kind === "stderr"
        ) {{
          return this._writeFilestat(statPtr, null, __agentOsWasiFiletypeCharacterDevice);
        }}
        if (entry.kind === "preopen") {{
          const stats = __agentOsFs().statSync(entry.guestPath);
          return this._writeFilestat(statPtr, stats, __agentOsWasiFiletypeDirectory);
        }}
        const stats =
          typeof entry.realFd === "number"
            ? __agentOsFs().fstatSync(entry.realFd)
            : __agentOsFs().statSync(this._descriptorFsPath(entry));
        return this._writeFilestat(statPtr, stats, this._fdFiletype(entry));
      }} catch (error) {{
        return this._mapFsError(error);
      }}
    }}

    _fdFilestatSetSize(fd, size) {{
      try {{
        const entry = this._descriptorEntry(fd);
        if (!entry || entry.kind !== "file" || typeof entry.realFd !== "number") {{
          return __agentOsWasiErrnoBadf;
        }}
        __agentOsFs().ftruncateSync(entry.realFd, Number(size));
        return __agentOsWasiErrnoSuccess;
      }} catch (error) {{
        return this._mapFsError(error);
      }}
    }}

    _fdSeek(fd, offset, whence, newOffsetPtr) {{
      try {{
        const entry = this._descriptorEntry(fd);
        if (!entry || entry.kind !== "file" || typeof entry.realFd !== "number") {{
          return __agentOsWasiErrnoBadf;
        }}
        const delta = Number(offset);
        if (!Number.isFinite(delta)) {{
          return __agentOsWasiErrnoInval;
        }}
        const currentOffset = typeof entry.offset === "number" ? entry.offset : 0;
        let nextOffset = 0;
        switch (Number(whence) >>> 0) {{
          case __agentOsWasiWhenceSet:
            nextOffset = delta;
            break;
          case __agentOsWasiWhenceCur:
            nextOffset = currentOffset + delta;
            break;
          case __agentOsWasiWhenceEnd: {{
            const stats = __agentOsFs().fstatSync(entry.realFd);
            nextOffset = Number(stats?.size ?? 0) + delta;
            break;
          }}
          default:
            return __agentOsWasiErrnoInval;
        }}
        if (!Number.isFinite(nextOffset) || nextOffset < 0) {{
          return __agentOsWasiErrnoInval;
        }}
        entry.offset = nextOffset;
        return this._writeUint64(newOffsetPtr, BigInt(nextOffset));
      }} catch (error) {{
        return this._mapFsError(error);
      }}
    }}

    _fdTell(fd, offsetPtr) {{
      try {{
        const entry = this._descriptorEntry(fd);
        if (!entry || entry.kind !== "file") {{
          return __agentOsWasiErrnoBadf;
        }}
        const offset = typeof entry.offset === "number" ? entry.offset : 0;
        return this._writeUint64(offsetPtr, BigInt(offset));
      }} catch (error) {{
        return this._mapFsError(error);
      }}
    }}

    _fdPrestatGet(fd, prestatPtr) {{
      try {{
        const entry = this._descriptorEntry(fd);
        if (!entry || entry.kind !== "preopen") {{
          return __agentOsWasiErrnoBadf;
        }}
        const guestPath = this._descriptorGuestPath(entry);
        if (typeof guestPath !== "string") {{
          return __agentOsWasiErrnoBadf;
        }}
        const view = this._memoryView();
        const offset = Number(prestatPtr) >>> 0;
        view.setUint8(offset, 0);
        view.setUint32(offset + 4, Buffer.byteLength(guestPath), true);
        return __agentOsWasiErrnoSuccess;
      }} catch {{
        return __agentOsWasiErrnoFault;
      }}
    }}

    _fdPrestatDirName(fd, pathPtr, pathLen) {{
      try {{
        const entry = this._descriptorEntry(fd);
        if (!entry || entry.kind !== "preopen") {{
          return __agentOsWasiErrnoBadf;
        }}
        const guestPath = this._descriptorGuestPath(entry);
        if (typeof guestPath !== "string") {{
          return __agentOsWasiErrnoBadf;
        }}
        const bytes = Buffer.from(guestPath, "utf8");
        if ((Number(pathLen) >>> 0) < bytes.length) {{
          return __agentOsWasiErrnoFault;
        }}
        return this._writeBytes(pathPtr, bytes);
      }} catch {{
        return __agentOsWasiErrnoFault;
      }}
    }}

    _fdReaddir(fd, bufPtr, bufLen, cookie, bufUsedPtr) {{
      try {{
        const entry = this._descriptorEntry(fd);
        const fsPath = this._descriptorFsPath(entry);
        if (
          !entry ||
          (entry.kind !== "preopen" && entry.kind !== "directory") ||
          typeof fsPath !== "string"
        ) {{
          return __agentOsWasiErrnoBadf;
        }}
        const dirents = __agentOsFs()
          .readdirSync(fsPath, {{ withFileTypes: true }})
          .sort((left, right) => left.name.localeCompare(right.name));
        const view = this._memoryView();
        const memory = this._memoryBytes();
        let offset = Number(bufPtr) >>> 0;
        const limit = offset + (Number(bufLen) >>> 0);
        let used = 0;
        for (let index = Number(cookie) >>> 0; index < dirents.length; index += 1) {{
          const dirent = dirents[index];
          const nameBytes = Buffer.from(dirent.name, "utf8");
          const recordLen = 24 + nameBytes.length;
          if (offset + recordLen > limit) {{
            break;
          }}
          view.setBigUint64(offset, BigInt(index + 1), true);
          view.setBigUint64(offset + 8, BigInt(index + 1), true);
          view.setUint32(offset + 16, nameBytes.length, true);
          view.setUint8(
            offset + 20,
            dirent.isDirectory()
              ? __agentOsWasiFiletypeDirectory
              : dirent.isSymbolicLink()
                ? __agentOsWasiFiletypeSymbolicLink
                : __agentOsWasiFiletypeRegularFile,
          );
          memory.set(nameBytes, offset + 24);
          offset += recordLen;
          used += recordLen;
        }}
        return this._writeUint32(bufUsedPtr, used);
      }} catch (error) {{
        return this._mapFsError(error);
      }}
    }}

    _pathCreateDirectory(fd, pathPtr, pathLen) {{
      try {{
        const resolved = this._resolveDescriptorPath(fd, pathPtr, pathLen);
        if (resolved.error !== __agentOsWasiErrnoSuccess) {{
          return resolved.error;
        }}
        __agentOsFs().mkdirSync(resolved.hostPath);
        return __agentOsWasiErrnoSuccess;
      }} catch (error) {{
        return this._mapFsError(error);
      }}
    }}

    _pathLink(oldFd, _oldFlags, oldPathPtr, oldPathLen, newFd, newPathPtr, newPathLen) {{
      try {{
        const source = this._resolveDescriptorPath(oldFd, oldPathPtr, oldPathLen);
        if (source.error !== __agentOsWasiErrnoSuccess) {{
          return source.error;
        }}
        const destination = this._resolveDescriptorPath(newFd, newPathPtr, newPathLen);
        if (destination.error !== __agentOsWasiErrnoSuccess) {{
          return destination.error;
        }}
        __agentOsFs().linkSync(source.hostPath, destination.hostPath);
        return __agentOsWasiErrnoSuccess;
      }} catch (error) {{
        return this._mapFsError(error);
      }}
    }}

    _pathOpen(fd, _dirflags, pathPtr, pathLen, oflags, rightsBase, rightsInheriting, _fdflags, openedFdPtr) {{
      try {{
        const entry = this._descriptorEntry(fd);
        const baseGuestPath = this._descriptorGuestPath(entry);
        if (
          !entry ||
          (entry.kind !== "preopen" && entry.kind !== "directory") ||
          typeof entry.hostPath !== "string" ||
          typeof baseGuestPath !== "string"
        ) {{
          return __agentOsWasiErrnoBadf;
        }}
        const target = this._readString(pathPtr, pathLen);
        const guestPath = target.startsWith("/")
          ? __agentOsPath().posix.normalize(target)
          : __agentOsPath().posix.resolve(baseGuestPath, target);
        const baseHostPath = __agentOsPath().resolve(entry.hostPath);
        const hostPath = __agentOsPath().resolve(baseHostPath, target);
        const hostSuffix = __agentOsPath().relative(baseHostPath, hostPath);
        if (
          hostPath !== baseHostPath &&
          (hostSuffix === ".." ||
            hostSuffix.startsWith(`..${{__agentOsPath().sep}}`) ||
            __agentOsPath().isAbsolute(hostSuffix))
        ) {{
          return __agentOsWasiErrnoNoent;
        }}
        const requestedFlags = Number(oflags) >>> 0;
        const openDirectory = (requestedFlags & __agentOsWasiOpenDirectory) !== 0;
        const createOrTruncate =
          (requestedFlags & __agentOsWasiOpenCreate) !== 0 ||
          (requestedFlags & __agentOsWasiOpenTruncate) !== 0;
        const allowedRightsBase = this._descriptorRightsBase(entry);
        const allowedRightsInheriting = this._descriptorRightsInheriting(entry);
        const requestedRightsBase = this._normalizeRights(rightsBase, allowedRightsInheriting);
        const requestedRightsInheriting = this._normalizeRights(
          rightsInheriting,
          allowedRightsInheriting,
        );
        if (
          (requestedRightsBase & ~allowedRightsInheriting) !== 0n ||
          (requestedRightsInheriting & ~allowedRightsInheriting) !== 0n
        ) {{
          return __agentOsWasiErrnoAcces;
        }}
        const requestedWriteAccess =
          !openDirectory &&
          (createOrTruncate || this._hasWriteRights(requestedRightsBase));
        if (
          requestedWriteAccess &&
          !this._hasWriteRights(allowedRightsBase)
        ) {{
          return __agentOsWasiErrnoAcces;
        }}
        const fsConstants = __agentOsFs().constants ?? {{}};
        let openFlags = requestedWriteAccess
          ? fsConstants.O_RDWR ?? 2
          : fsConstants.O_RDONLY ?? 0;
        if ((requestedFlags & __agentOsWasiOpenCreate) !== 0) {{
          openFlags |= fsConstants.O_CREAT ?? 64;
        }}
        if ((requestedFlags & __agentOsWasiOpenExclusive) !== 0) {{
          openFlags |= fsConstants.O_EXCL ?? 128;
        }}
        if ((requestedFlags & __agentOsWasiOpenTruncate) !== 0) {{
          openFlags |= fsConstants.O_TRUNC ?? 512;
        }}
        if (openDirectory) {{
          openFlags |= fsConstants.O_DIRECTORY ?? 0;
        }}
        if (createOrTruncate && !openDirectory) {{
          __agentOsFs().statSync(__agentOsPath().dirname(hostPath));
        }} else {{
          __agentOsFs().statSync(hostPath);
        }}
        const realFd = __agentOsFs().openSync(hostPath, openFlags);
        const stats =
          createOrTruncate && !openDirectory
            ? __agentOsFs().fstatSync(realFd)
            : __agentOsFs().statSync(hostPath);
        const openedFd = this.nextFd++;
        this.fdTable.set(openedFd, {{
          kind: stats.isDirectory() ? "directory" : "file",
          guestPath,
          hostPath,
          realFd,
          offset: 0,
          rightsBase: requestedRightsBase & allowedRightsInheriting,
          rightsInheriting: requestedRightsInheriting & allowedRightsInheriting,
          fdFlags: (Number(_fdflags) >>> 0) & 0xffff,
        }});
        return this._writeUint32(openedFdPtr, openedFd);
      }} catch (error) {{
        return this._mapFsError(error);
      }}
    }}

    _pathSymlink(targetPtr, targetLen, fd, pathPtr, pathLen) {{
      try {{
        const resolved = this._resolveDescriptorPath(fd, pathPtr, pathLen);
        if (resolved.error !== __agentOsWasiErrnoSuccess) {{
          return resolved.error;
        }}
        const target = this._readString(targetPtr, targetLen);
        __agentOsFs().symlinkSync(target, resolved.hostPath);
        return __agentOsWasiErrnoSuccess;
      }} catch (error) {{
        return this._mapFsError(error);
      }}
    }}

    _pathRemoveDirectory(fd, pathPtr, pathLen) {{
      try {{
        const resolved = this._resolveDescriptorPath(fd, pathPtr, pathLen);
        if (resolved.error !== __agentOsWasiErrnoSuccess) {{
          return resolved.error;
        }}
        __agentOsFs().rmdirSync(resolved.hostPath);
        return __agentOsWasiErrnoSuccess;
      }} catch (error) {{
        return this._mapFsError(error);
      }}
    }}

    _pathRename(oldFd, oldPathPtr, oldPathLen, newFd, newPathPtr, newPathLen) {{
      try {{
        const source = this._resolveDescriptorPath(oldFd, oldPathPtr, oldPathLen);
        if (source.error !== __agentOsWasiErrnoSuccess) {{
          return source.error;
        }}
        const destination = this._resolveDescriptorPath(newFd, newPathPtr, newPathLen);
        if (destination.error !== __agentOsWasiErrnoSuccess) {{
          return destination.error;
        }}
        __agentOsFs().renameSync(source.hostPath, destination.hostPath);
        return __agentOsWasiErrnoSuccess;
      }} catch (error) {{
        return this._mapFsError(error);
      }}
    }}

    _pathUnlinkFile(fd, pathPtr, pathLen) {{
      try {{
        const resolved = this._resolveDescriptorPath(fd, pathPtr, pathLen);
        if (resolved.error !== __agentOsWasiErrnoSuccess) {{
          return resolved.error;
        }}
        __agentOsFs().unlinkSync(resolved.hostPath);
        return __agentOsWasiErrnoSuccess;
      }} catch (error) {{
        return this._mapFsError(error);
      }}
    }}

    _pathFilestatGet(fd, flags, pathPtr, pathLen, statPtr) {{
      try {{
        const resolved = this._resolveDescriptorPath(fd, pathPtr, pathLen);
        if (resolved.error !== __agentOsWasiErrnoSuccess) {{
          return resolved.error;
        }}
        const follow = (Number(flags) & __agentOsWasiLookupSymlinkFollow) !== 0;
        const stats = follow
          ? __agentOsFs().statSync(resolved.hostPath)
          : __agentOsFs().lstatSync(resolved.hostPath);
        return this._writeFilestat(statPtr, stats, this._filetypeForStats(stats));
      }} catch (error) {{
        return this._mapFsError(error);
      }}
    }}

    _pathReadlink(fd, pathPtr, pathLen, bufPtr, bufLen, bufUsedPtr) {{
      try {{
        const resolved = this._resolveDescriptorPath(fd, pathPtr, pathLen);
        if (resolved.error !== __agentOsWasiErrnoSuccess) {{
          return resolved.error;
        }}
        const bytes = Buffer.from(__agentOsFs().readlinkSync(resolved.guestPath), "utf8");
        const length = Math.min(bytes.length, Number(bufLen) >>> 0);
        const writeStatus = this._writeBytes(bufPtr, bytes.subarray(0, length));
        if (writeStatus !== __agentOsWasiErrnoSuccess) {{
          return writeStatus;
        }}
        return this._writeUint32(bufUsedPtr, length);
      }} catch (error) {{
        return this._mapFsError(error);
      }}
    }}

    _pollOneoff(inPtr, outPtr, nsubscriptions, neventsPtr) {{
      try {{
        const subscriptionCount = Number(nsubscriptions) >>> 0;
        if (subscriptionCount === 0) {{
          return this._writeUint32(neventsPtr, 0);
        }}

        const subscriptionSize = 48;
        const eventSize = 32;
        const kernelPollIn = 0x0001;
        const kernelPollOut = 0x0004;
        const kernelPollErr = 0x0008;
        const kernelPollHup = 0x0010;
        const view = this._memoryView();
        const memory = this._memoryBytes();
        const syncRpc =
          typeof globalThis?.__agentOsSyncRpc?.callSync === "function"
            ? globalThis.__agentOsSyncRpc
            : null;
        const subscriptions = [];
        let timeoutMs = null;

        for (let index = 0; index < subscriptionCount; index += 1) {{
          const base = (Number(inPtr) >>> 0) + index * subscriptionSize;
          const tag = view.getUint8(base + 8);
          const userdata = memory.slice(base, base + 8);
          if (tag === 0) {{
            const timeoutNs = view.getBigUint64(base + 24, true);
            const relativeTimeoutMs = Number(timeoutNs / 1000000n);
            timeoutMs =
              timeoutMs == null ? relativeTimeoutMs : Math.min(timeoutMs, relativeTimeoutMs);
            subscriptions.push({{ kind: "clock", userdata }});
            continue;
          }}

          if (tag !== 1 && tag !== 2) {{
            subscriptions.push({{ kind: "unsupported", userdata }});
            continue;
          }}

          const fd = view.getUint32(base + 16, true);
          const descriptor = Number(fd) >>> 0;
          const handle = this._externalFdHandle(descriptor);
          const entry = this._descriptorEntry(descriptor);
          let targetFd = null;
          if (
            (handle?.kind === "passthrough" || handle?.kind === "host-passthrough") &&
            typeof handle.targetFd === "number"
          ) {{
            targetFd = Number(handle.targetFd) >>> 0;
          }} else if (
            entry?.kind === "stdin" ||
            entry?.kind === "stdout" ||
            entry?.kind === "stderr"
          ) {{
            targetFd = descriptor;
          }}

          subscriptions.push({{
            kind: tag === 1 ? "fd_read" : "fd_write",
            fd: descriptor,
            handle,
            targetFd,
            userdata,
          }});
        }}

        const deadline = timeoutMs == null ? null : Date.now() + Math.max(0, timeoutMs);
        const readyEvents = [];

        while (readyEvents.length === 0) {{
          for (const subscription of subscriptions) {{
            if (subscription.kind === "fd_read" && subscription.handle?.kind === "pipe-read") {{
              const pipe = subscription.handle.pipe;
              if (
                pipe &&
                (pipe.chunks.length > 0 ||
                  (pipe.writeHandleCount === 0 && pipe.producers.size === 0))
              ) {{
                readyEvents.push({{
                  userdata: subscription.userdata,
                  error: __agentOsWasiErrnoSuccess,
                  type: 1,
                  nbytes: pipe.chunks[0]?.length ?? 0,
                  flags: 0,
                }});
              }}
              continue;
            }}

            if (subscription.kind === "fd_write" && subscription.handle?.kind === "pipe-write") {{
              readyEvents.push({{
                userdata: subscription.userdata,
                error: __agentOsWasiErrnoSuccess,
                type: 2,
                nbytes: 65536,
                flags: 0,
              }});
            }}
          }}

          if (readyEvents.length > 0) {{
            break;
          }}

          const pollTargets = subscriptions
            .filter(
              (subscription) =>
                (subscription.kind === "fd_read" || subscription.kind === "fd_write") &&
                typeof subscription.targetFd === "number",
            )
            .map((subscription) => ({{
              fd: subscription.targetFd,
              events: subscription.kind === "fd_read" ? kernelPollIn : kernelPollOut,
            }}));
          const waitMs =
            deadline == null ? 10 : Math.max(0, Math.min(10, deadline - Date.now()));

          if (syncRpc && pollTargets.length > 0) {{
            let response = null;
            try {{
              response = syncRpc.callSync("__kernel_poll", [pollTargets, waitMs]);
            }} catch (error) {{
              __agentOsWasiDebug(
                `poll_oneoff __kernel_poll failed: ${{
                  error instanceof Error ? error.message : String(error)
                }}`,
              );
            }}

            const responseEntries = Array.isArray(response?.fds) ? response.fds : [];
            for (const subscription of subscriptions) {{
              if (
                (subscription.kind !== "fd_read" && subscription.kind !== "fd_write") ||
                typeof subscription.targetFd !== "number"
              ) {{
                continue;
              }}

              const responseEntry = responseEntries.find(
                (entry) => (Number(entry?.fd) >>> 0) === subscription.targetFd,
              );
              const revents = Number(responseEntry?.revents) >>> 0;
              const interested =
                subscription.kind === "fd_read"
                  ? kernelPollIn | kernelPollErr | kernelPollHup
                  : kernelPollOut | kernelPollErr | kernelPollHup;
              if ((revents & interested) === 0) {{
                continue;
              }}

              readyEvents.push({{
                userdata: subscription.userdata,
                error: __agentOsWasiErrnoSuccess,
                type: subscription.kind === "fd_read" ? 1 : 2,
                nbytes: subscription.kind === "fd_read" ? 1 : 65536,
                flags: 0,
              }});
            }}
          }}

          if (readyEvents.length > 0) {{
            break;
          }}

          let pumped = false;
          for (const subscription of subscriptions) {{
            if (subscription.kind === "fd_read" && subscription.handle?.kind === "pipe-read") {{
              pumped = this._pumpPipeProducers(subscription.handle.pipe, 10) || pumped;
            }}
          }}

          if (pumped) {{
            continue;
          }}

          if (deadline != null && Date.now() >= deadline) {{
            break;
          }}

          if (
            pollTargets.length === 0 &&
            typeof Atomics?.wait !== "function" &&
            deadline == null
          ) {{
            break;
          }}

          if (
            typeof Atomics?.wait === "function" &&
            typeof syntheticWaitArray !== "undefined"
          ) {{
            Atomics.wait(syntheticWaitArray, 0, 0, waitMs);
          }} else if (!syncRpc && pollTargets.length === 0) {{
            break;
          }}
        }}

        if (
          readyEvents.length === 0 &&
          subscriptions.some((subscription) => subscription.kind === "clock")
        ) {{
          const clockSubscription = subscriptions.find(
            (subscription) => subscription.kind === "clock",
          );
          readyEvents.push({{
            userdata: clockSubscription.userdata,
            error: __agentOsWasiErrnoSuccess,
            type: 0,
            nbytes: 0,
            flags: 0,
          }});
        }}

        for (let index = 0; index < readyEvents.length; index += 1) {{
          const base = (Number(outPtr) >>> 0) + index * eventSize;
          const event = readyEvents[index];
          memory.set(event.userdata, base);
          view.setUint16(base + 8, event.error, true);
          view.setUint8(base + 10, event.type);
          view.setBigUint64(base + 16, BigInt(event.nbytes), true);
          view.setUint16(base + 24, event.flags, true);
        }}

        return this._writeUint32(neventsPtr, readyEvents.length);
      }} catch (error) {{
        __agentOsWasiDebug(
          `poll_oneoff failed: ${{error instanceof Error ? error.message : String(error)}}`,
        );
        return __agentOsWasiErrnoFault;
      }}
    }}

    _randomGet(bufPtr, bufLen) {{
      try {{
        const length = Number(bufLen) >>> 0;
        const bytes = Buffer.allocUnsafe(length);
        __agentOsCrypto().randomFillSync(bytes);
        return this._writeBytes(bufPtr, bytes);
      }} catch {{
        return __agentOsWasiErrnoFault;
      }}
    }}

    _schedYield() {{
      return __agentOsWasiErrnoSuccess;
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
        case "__kernel_stdio_write":
          if (typeof _kernelStdioWriteRaw === "undefined") {{
            throw new Error("Agent OS WASM kernel stdio bridge is unavailable");
          }}
          return _kernelStdioWriteRaw.applySync(void 0, args);
        case "__kernel_stdin_read":
          if (typeof _kernelStdinReadRaw === "undefined") {{
            throw new Error("Agent OS WASM kernel stdin bridge is unavailable");
          }}
          return _kernelStdinReadRaw.applySync(void 0, args);
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
        sandbox_root: wasm_sandbox_root(&request.env),
        guest_path_mappings: wasm_guest_path_mappings(request),
        next_fd: 64,
        open_files: BTreeMap::new(),
        pending_events: VecDeque::new(),
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
        .get("AGENT_OS_GUEST_PATH_MAPPINGS")
        .and_then(|value| serde_json::from_str::<Vec<Value>>(value).ok())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|mapping| {
            Some(WasmGuestPathMapping {
                guest_path: mapping.get("guestPath")?.as_str()?.to_owned(),
                host_path: PathBuf::from(mapping.get("hostPath")?.as_str()?),
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
    mappings.sort_by(|left, right| right.guest_path.len().cmp(&left.guest_path.len()));
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
                })
            })
            .collect::<Vec<_>>(),
    )
    .unwrap_or_else(|_| String::from("[]"))
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

/// Sniff the first bytes of a resolved WebAssembly module and refuse to hand
/// non-`\0asm` content (such as `#!/bin/sh` shell shims) to `WebAssembly.compile`.
///
/// Without this guard, resolving a `node_modules/.bin/<cmd>` shell shim against
/// the WASM path produces an opaque `CompileError: WebAssembly.Module(): expected
/// magic word 00 61 73 6d, found 23 21 2f 62 @+0` during prewarm. That error
/// cascades through hundreds of downstream tests as `ERR_AGENT_OS_NODE_SYNC_RPC:
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
    wasm_limit_u64(&request.env, WASM_MAX_MEMORY_BYTES_ENV)
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
        Some(maximum_bytes) if maximum_bytes > memory_limit => {
            Err(WasmExecutionError::InvalidModule(format!(
                "WebAssembly memory maximum of {maximum_bytes} bytes exceeds the configured limit of {memory_limit} bytes"
            )))
        }
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
        StartWasmExecutionRequest, WASM_MAX_FUEL_ENV, WASM_MAX_MEMORY_BYTES_ENV, WASM_PAGE_BYTES,
        WASM_PREWARM_TIMEOUT_MS_ENV, WASM_SANDBOX_ROOT_ENV, WasmInternalSyncRpc,
        WasmPermissionTier, build_wasm_runner_bootstrap, resolve_wasm_execution_timeout,
        resolve_wasm_prewarm_timeout, resolved_module_path, translate_wasm_guest_path,
        wasm_guest_module_paths, wasm_memory_limit_pages, wasm_sandbox_root,
    };
    use std::collections::{BTreeMap, VecDeque};
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::{Path, PathBuf};
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
    fn translate_wasm_guest_path_uses_sandbox_root_for_absolute_paths() {
        let temp = tempdir().expect("create temp dir");
        let sandbox_root = temp.path().join("shadow-root");
        let cwd = sandbox_root.join("home/user");
        fs::create_dir_all(cwd.join("project")).expect("create host cwd");

        let internal_sync_rpc = WasmInternalSyncRpc {
            module_guest_paths: Vec::new(),
            module_host_path: sandbox_root.join("module.wasm"),
            guest_cwd: String::from("/home/user"),
            host_cwd: cwd.clone(),
            sandbox_root: Some(sandbox_root.clone()),
            guest_path_mappings: Vec::new(),
            next_fd: 64,
            open_files: Default::default(),
            pending_events: VecDeque::new(),
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
            }],
            next_fd: 64,
            open_files: Default::default(),
            pending_events: VecDeque::new(),
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
                },
                super::WasmGuestPathMapping {
                    guest_path: String::from("/__agentos/commands/0"),
                    host_path: mapped_root.clone(),
                },
            ],
            next_fd: 64,
            open_files: Default::default(),
            pending_events: VecDeque::new(),
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
            }],
            next_fd: 64,
            open_files: Default::default(),
            pending_events: VecDeque::new(),
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
            String::from("/tmp/agent-os-shadow"),
        )]));
        assert_eq!(sandbox_root, Some(PathBuf::from("/tmp/agent-os-shadow")));

        let relative = wasm_sandbox_root(&BTreeMap::from([(
            String::from(WASM_SANDBOX_ROOT_ENV),
            String::from("relative/shadow"),
        )]));
        assert_eq!(relative, None);
    }

    #[test]
    fn wasm_guest_path_mappings_mount_root_to_sandbox_root() {
        let temp = tempdir().expect("create temp dir");
        let sandbox_root = temp.path().join("shadow-root");
        let host_cwd = sandbox_root.join("home/user");
        fs::create_dir_all(&host_cwd).expect("create host cwd");

        let mappings = super::wasm_guest_path_mappings(&request_with_env(
            &host_cwd,
            BTreeMap::from([
                (String::from("PWD"), String::from("/home/user")),
                (
                    String::from(WASM_SANDBOX_ROOT_ENV),
                    sandbox_root.to_string_lossy().into_owned(),
                ),
            ]),
        ));

        assert!(
            mappings
                .iter()
                .any(|mapping| { mapping.guest_path == "/" && mapping.host_path == sandbox_root })
        );
        assert!(mappings.iter().any(|mapping| {
            mapping.guest_path == "/home/user" && mapping.host_path == host_cwd
        }));
    }

    #[test]
    fn wasm_runner_bootstrap_keeps_root_preopens_rooted() {
        let bootstrap = build_wasm_runner_bootstrap(&BTreeMap::new(), None);

        assert!(bootstrap.contains("if (guestPath === \".\") {"));
        assert!(!bootstrap.contains("if (guestPath === \".\" || guestPath === \"/\") {"));
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
