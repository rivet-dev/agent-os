use crate::common::{encode_json_string, frozen_time_ms};
use crate::node_import_cache::NodeImportCache;
use crate::node_process::{
    apply_guest_env, configure_node_control_channel, create_node_control_channel,
    encode_json_string_array, encode_json_string_map, env_builtin_enabled, harden_node_command,
    node_binary, node_resolution_read_paths, resolve_path_like_specifier,
    spawn_node_control_reader, spawn_stream_reader, LinePrefixFilter, NodeControlMessage,
    NodeSignalDispositionAction, NodeSignalHandlerRegistration,
};
use crate::runtime_support::{
    configure_compile_cache, env_flag_enabled, file_fingerprint, import_cache_root, sandbox_root,
    warmup_marker_path, NODE_COMPILE_CACHE_ENV, NODE_DISABLE_COMPILE_CACHE_ENV,
    NODE_FROZEN_TIME_ENV, NODE_SANDBOX_ROOT_ENV,
};
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{
    mpsc::{self, Receiver, RecvTimeoutError},
    Arc, Mutex,
};
use std::thread::JoinHandle;
use std::time::Duration;

const WASM_MODULE_PATH_ENV: &str = "AGENT_OS_WASM_MODULE_PATH";
const WASM_GUEST_ARGV_ENV: &str = "AGENT_OS_GUEST_ARGV";
const WASM_GUEST_ENV_ENV: &str = "AGENT_OS_GUEST_ENV";
const WASM_PERMISSION_TIER_ENV: &str = "AGENT_OS_WASM_PERMISSION_TIER";
const WASM_PREWARM_ONLY_ENV: &str = "AGENT_OS_WASM_PREWARM_ONLY";
const WASM_WARMUP_DEBUG_ENV: &str = "AGENT_OS_WASM_WARMUP_DEBUG";
pub const WASM_MAX_FUEL_ENV: &str = "AGENT_OS_WASM_MAX_FUEL";
pub const WASM_MAX_MEMORY_BYTES_ENV: &str = "AGENT_OS_WASM_MAX_MEMORY_BYTES";
pub const WASM_MAX_STACK_BYTES_ENV: &str = "AGENT_OS_WASM_MAX_STACK_BYTES";
const WASM_WARMUP_METRICS_PREFIX: &str = "__AGENT_OS_WASM_WARMUP_METRICS__:";
const WASM_WARMUP_MARKER_VERSION: &str = "1";
const SIGNAL_STATE_CONTROL_PREFIX: &str = "__AGENT_OS_SIGNAL_STATE__:";
const CONTROLLED_STDERR_PREFIXES: &[&str] = &[SIGNAL_STATE_CONTROL_PREFIX];
const RESERVED_WASM_ENV_KEYS: &[&str] = &[
    NODE_COMPILE_CACHE_ENV,
    NODE_DISABLE_COMPILE_CACHE_ENV,
    NODE_FROZEN_TIME_ENV,
    NODE_SANDBOX_ROOT_ENV,
    WASM_PERMISSION_TIER_ENV,
    WASM_GUEST_ARGV_ENV,
    WASM_GUEST_ENV_ENV,
    WASM_MODULE_PATH_ENV,
    WASM_MAX_FUEL_ENV,
    WASM_MAX_MEMORY_BYTES_ENV,
    WASM_MAX_STACK_BYTES_ENV,
    WASM_PREWARM_ONLY_ENV,
];
const WASM_PAGE_BYTES: u64 = 65_536;
const WASM_TIMEOUT_EXIT_CODE: i32 = 124;

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
    SignalState {
        signal: u32,
        registration: WasmSignalHandlerRegistration,
    },
    Exited(i32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum WasmProcessEvent {
    Stdout(Vec<u8>),
    RawStderr(Vec<u8>),
    Control(NodeControlMessage),
    Exited(i32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WasmExecutionResult {
    pub execution_id: String,
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[derive(Debug)]
pub enum WasmExecutionError {
    MissingContext(String),
    VmMismatch { expected: String, found: String },
    MissingModulePath,
    InvalidLimit(String),
    InvalidModule(String),
    MissingChildStream(&'static str),
    PrepareWarmPath(std::io::Error),
    WarmupSpawn(std::io::Error),
    WarmupTimeout(Duration),
    WarmupFailed { exit_code: i32, stderr: String },
    Spawn(std::io::Error),
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
            Self::MissingChildStream(name) => write!(f, "node child missing {name} pipe"),
            Self::PrepareWarmPath(err) => {
                write!(f, "failed to prepare shared WebAssembly warm path: {err}")
            }
            Self::WarmupSpawn(err) => {
                write!(f, "failed to start WebAssembly warmup process: {err}")
            }
            Self::WarmupTimeout(timeout) => {
                write!(
                    f,
                    "WebAssembly warmup exceeded the configured fuel budget after {} ms",
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
    stdin: Option<ChildStdin>,
    events: Receiver<WasmProcessEvent>,
    stderr_filter: Arc<Mutex<LinePrefixFilter>>,
}

impl WasmExecution {
    pub fn execution_id(&self) -> &str {
        &self.execution_id
    }

    pub fn child_pid(&self) -> u32 {
        self.child_pid
    }

    pub fn write_stdin(&mut self, chunk: &[u8]) -> Result<(), WasmExecutionError> {
        let stdin = self.stdin.as_mut().ok_or(WasmExecutionError::StdinClosed)?;
        stdin
            .write_all(chunk)
            .and_then(|()| stdin.flush())
            .map_err(WasmExecutionError::Stdin)
    }

    pub fn close_stdin(&mut self) -> Result<(), WasmExecutionError> {
        if let Some(stdin) = self.stdin.take() {
            drop(stdin);
        }
        Ok(())
    }

    pub fn poll_event(
        &self,
        timeout: Duration,
    ) -> Result<Option<WasmExecutionEvent>, WasmExecutionError> {
        match self.events.recv_timeout(timeout) {
            Ok(WasmProcessEvent::Stdout(chunk)) => Ok(Some(WasmExecutionEvent::Stdout(chunk))),
            Ok(WasmProcessEvent::RawStderr(chunk)) => {
                let mut filter = self
                    .stderr_filter
                    .lock()
                    .map_err(|_| WasmExecutionError::EventChannelClosed)?;
                let filtered = filter.filter_chunk(&chunk, CONTROLLED_STDERR_PREFIXES);
                if filtered.is_empty() {
                    return Ok(None);
                }
                Ok(Some(WasmExecutionEvent::Stderr(filtered)))
            }
            Ok(WasmProcessEvent::Control(NodeControlMessage::SignalState {
                signal,
                registration,
            })) => Ok(Some(WasmExecutionEvent::SignalState {
                signal,
                registration: registration.into(),
            })),
            Ok(WasmProcessEvent::Control(_)) => Ok(None),
            Ok(WasmProcessEvent::Exited(code)) => Ok(Some(WasmExecutionEvent::Exited(code))),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Err(WasmExecutionError::EventChannelClosed),
        }
    }

    pub fn wait(mut self) -> Result<WasmExecutionResult, WasmExecutionError> {
        self.close_stdin()?;

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        loop {
            match self.events.recv() {
                Ok(WasmProcessEvent::Stdout(chunk)) => stdout.extend(chunk),
                Ok(WasmProcessEvent::RawStderr(chunk)) => {
                    let mut filter = self
                        .stderr_filter
                        .lock()
                        .map_err(|_| WasmExecutionError::EventChannelClosed)?;
                    stderr.extend(filter.filter_chunk(&chunk, CONTROLLED_STDERR_PREFIXES));
                }
                Ok(WasmProcessEvent::Control(_)) => {}
                Ok(WasmProcessEvent::Exited(exit_code)) => {
                    return Ok(WasmExecutionResult {
                        execution_id: self.execution_id,
                        exit_code,
                        stdout,
                        stderr,
                    });
                }
                Err(_) => return Err(WasmExecutionError::EventChannelClosed),
            }
        }
    }
}

#[derive(Debug, Default)]
pub struct WasmExecutionEngine {
    next_context_id: usize,
    next_execution_id: usize,
    contexts: BTreeMap<String, WasmContext>,
    import_caches: BTreeMap<String, NodeImportCache>,
}

impl WasmExecutionEngine {
    pub fn create_context(&mut self, request: CreateWasmContextRequest) -> WasmContext {
        self.next_context_id += 1;
        self.import_caches.entry(request.vm_id.clone()).or_default();

        let context = WasmContext {
            context_id: format!("wasm-ctx-{}", self.next_context_id),
            vm_id: request.vm_id,
            module_path: request.module_path,
        };
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

        {
            let import_cache = self.import_caches.entry(context.vm_id.clone()).or_default();
            import_cache
                .ensure_materialized()
                .map_err(WasmExecutionError::PrepareWarmPath)?;
        }
        let frozen_time_ms = frozen_time_ms();
        validate_module_limits(&context, &request)?;
        let execution_timeout = resolve_wasm_execution_timeout(&request)?;
        let import_cache = self
            .import_caches
            .get(&context.vm_id)
            .expect("vm import cache should exist after materialization");
        let warmup_metrics = prewarm_wasm_path(
            import_cache,
            &context,
            &request,
            frozen_time_ms,
            execution_timeout,
        )?;

        self.next_execution_id += 1;
        let execution_id = format!("exec-{}", self.next_execution_id);
        let guest_argv = guest_argv(&context, &request)?;
        let control_channel = create_node_control_channel().map_err(WasmExecutionError::Spawn)?;
        let mut child = create_node_child(
            import_cache,
            &context,
            &request,
            &guest_argv,
            frozen_time_ms,
            &control_channel.child_writer,
        )?;
        let child_pid = child.id();

        let stdin = child.stdin.take();
        let stdout = child
            .stdout
            .take()
            .ok_or(WasmExecutionError::MissingChildStream("stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or(WasmExecutionError::MissingChildStream("stderr"))?;

        let (sender, receiver) = mpsc::channel();
        if let Some(metrics) = warmup_metrics {
            let _ = sender.send(WasmProcessEvent::RawStderr(metrics));
        }

        let stdout_reader = spawn_stream_reader(stdout, sender.clone(), WasmProcessEvent::Stdout);
        let stderr_reader =
            spawn_stream_reader(stderr, sender.clone(), WasmProcessEvent::RawStderr);
        let _control_reader = spawn_node_control_reader(
            control_channel.parent_reader,
            sender.clone(),
            WasmProcessEvent::Control,
            |message| WasmProcessEvent::RawStderr(message.into_bytes()),
        );
        spawn_wasm_waiter(
            child,
            stdout_reader,
            stderr_reader,
            execution_timeout,
            sender,
        );

        Ok(WasmExecution {
            execution_id,
            child_pid,
            stdin,
            events: receiver,
            stderr_filter: Arc::new(Mutex::new(LinePrefixFilter::default())),
        })
    }

    pub fn dispose_vm(&mut self, vm_id: &str) {
        self.contexts.retain(|_, context| context.vm_id != vm_id);
        self.import_caches.remove(vm_id);
    }
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

fn create_node_child(
    import_cache: &NodeImportCache,
    context: &WasmContext,
    request: &StartWasmExecutionRequest,
    guest_argv: &[String],
    frozen_time_ms: u128,
    control_fd: &std::os::fd::OwnedFd,
) -> Result<std::process::Child, WasmExecutionError> {
    let mut command = Command::new(node_binary());
    configure_wasm_node_sandbox(&mut command, import_cache, context, request)?;
    command
        .arg("--no-warnings")
        .arg("--import")
        .arg(import_cache.timing_bootstrap_path())
        .arg(import_cache.wasm_runner_path())
        .current_dir(&request.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env(WASM_MODULE_PATH_ENV, module_path(context, request)?);

    apply_guest_env(&mut command, &request.env, RESERVED_WASM_ENV_KEYS);
    command
        .env(WASM_GUEST_ARGV_ENV, encode_json_string_array(guest_argv))
        .env(WASM_GUEST_ENV_ENV, encode_json_string_map(&request.env))
        .env(
            WASM_PERMISSION_TIER_ENV,
            request.permission_tier.as_env_value(),
        );

    configure_node_control_channel(&mut command, control_fd);
    configure_node_command(&mut command, import_cache, frozen_time_ms, request)?;

    command.spawn().map_err(WasmExecutionError::Spawn)
}

fn prewarm_wasm_path(
    import_cache: &NodeImportCache,
    context: &WasmContext,
    request: &StartWasmExecutionRequest,
    frozen_time_ms: u128,
    execution_timeout: Option<Duration>,
) -> Result<Option<Vec<u8>>, WasmExecutionError> {
    let debug_enabled = env_flag_enabled(&request.env, WASM_WARMUP_DEBUG_ENV);
    let marker_contents = warmup_marker_contents(context, request);
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
            context,
            request,
        ));
    }

    let guest_argv = guest_argv(context, request)?;
    let mut command = Command::new(node_binary());
    configure_wasm_node_sandbox(&mut command, import_cache, context, request)?;
    command
        .arg("--no-warnings")
        .arg("--import")
        .arg(import_cache.timing_bootstrap_path())
        .arg(import_cache.wasm_runner_path())
        .current_dir(&request.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .env(WASM_PREWARM_ONLY_ENV, "1")
        .env(WASM_MODULE_PATH_ENV, module_path(context, request)?)
        .env(WASM_GUEST_ARGV_ENV, encode_json_string_array(&guest_argv))
        .env(WASM_GUEST_ENV_ENV, encode_json_string_map(&request.env))
        .env(
            WASM_PERMISSION_TIER_ENV,
            request.permission_tier.as_env_value(),
        );

    configure_node_command(&mut command, import_cache, frozen_time_ms, request)?;

    let output = run_warmup_command(command, execution_timeout)?;
    if !output.status.success() {
        return Err(WasmExecutionError::WarmupFailed {
            exit_code: output.status.code().unwrap_or(1),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }

    fs::write(&marker_path, marker_contents).map_err(WasmExecutionError::PrepareWarmPath)?;

    Ok(warmup_metrics_line(
        debug_enabled,
        true,
        "executed",
        import_cache,
        context,
        request,
    ))
}

fn configure_wasm_node_sandbox(
    command: &mut Command,
    import_cache: &NodeImportCache,
    context: &WasmContext,
    request: &StartWasmExecutionRequest,
) -> Result<(), WasmExecutionError> {
    let sandbox_root = sandbox_root(&request.env, &request.cwd);
    let cache_root = import_cache_root(import_cache, import_cache.prewarm_marker_dir());
    let compile_cache_dir = import_cache.shared_compile_cache_dir();
    let mut read_paths = vec![cache_root.clone(), compile_cache_dir.clone()];
    let mut write_paths = vec![cache_root, compile_cache_dir];

    if request.permission_tier.workspace_write_enabled() {
        write_paths.push(sandbox_root.clone());
    }

    if let Some(module_path) =
        resolve_path_like_specifier(&request.cwd, &module_path(context, request)?)
    {
        read_paths.push(module_path.clone());
        if let Some(parent) = module_path.parent() {
            read_paths.push(parent.to_path_buf());
        }
    }

    read_paths.extend(node_resolution_read_paths(
        std::iter::once(request.cwd.clone()).chain(
            resolve_path_like_specifier(&request.cwd, &module_path(context, request)?)
                .and_then(|path| path.parent().map(Path::to_path_buf)),
        ),
    ));

    harden_node_command(
        command,
        &sandbox_root,
        &read_paths,
        &write_paths,
        true,
        true,
        env_builtin_enabled(&request.env, "worker_threads"),
        false,
    );
    Ok(())
}

fn configure_node_command(
    command: &mut Command,
    import_cache: &NodeImportCache,
    frozen_time_ms: u128,
    request: &StartWasmExecutionRequest,
) -> Result<(), WasmExecutionError> {
    let compile_cache_dir = import_cache.shared_compile_cache_dir();
    configure_compile_cache(command, &compile_cache_dir)
        .map_err(WasmExecutionError::PrepareWarmPath)?;

    if let Some(stack_bytes) = wasm_stack_limit_bytes(request)? {
        let stack_kib = (stack_bytes.saturating_add(1023) / 1024).max(64);
        command.arg(format!("--stack-size={stack_kib}"));
    }

    command.env(NODE_FROZEN_TIME_ENV, frozen_time_ms.to_string());
    Ok(())
}

fn warmup_marker_contents(context: &WasmContext, request: &StartWasmExecutionRequest) -> String {
    let module_specifier = module_path(context, request).unwrap_or_default();
    let resolved_path = resolved_module_path(&module_specifier, &request.cwd);
    let module_fingerprint = file_fingerprint(&resolved_path);

    [
        env!("CARGO_PKG_NAME").to_string(),
        env!("CARGO_PKG_VERSION").to_string(),
        WASM_WARMUP_MARKER_VERSION.to_string(),
        module_specifier,
        resolved_path.display().to_string(),
        module_fingerprint,
    ]
    .join("\n")
}

fn warmup_metrics_line(
    debug_enabled: bool,
    executed: bool,
    reason: &str,
    import_cache: &NodeImportCache,
    context: &WasmContext,
    request: &StartWasmExecutionRequest,
) -> Option<Vec<u8>> {
    if !debug_enabled {
        return None;
    }

    let module_specifier = module_path(context, request).ok()?;
    Some(
        format!(
            "{WASM_WARMUP_METRICS_PREFIX}{{\"executed\":{},\"reason\":{},\"modulePath\":{},\"compileCacheDir\":{}}}\n",
            if executed { "true" } else { "false" },
            encode_json_string(reason),
            encode_json_string(&module_specifier),
            encode_json_string(&import_cache.shared_compile_cache_dir().display().to_string()),
        )
        .into_bytes(),
    )
}

fn resolved_module_path(specifier: &str, cwd: &Path) -> PathBuf {
    if specifier.starts_with("file:") {
        return PathBuf::from(specifier);
    }
    if is_path_like(specifier) {
        return cwd.join(specifier);
    }
    PathBuf::from(specifier)
}

fn is_path_like(specifier: &str) -> bool {
    specifier.starts_with('.') || specifier.starts_with('/') || specifier.starts_with("file:")
}

#[derive(Debug)]
struct WarmupOutput {
    status: std::process::ExitStatus,
    stderr: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChildWaitError {
    TimedOut,
    WaitFailed,
}

fn run_warmup_command(
    mut command: Command,
    timeout: Option<Duration>,
) -> Result<WarmupOutput, WasmExecutionError> {
    let mut child = command.spawn().map_err(WasmExecutionError::WarmupSpawn)?;
    let Some(mut stderr) = child.stderr.take() else {
        return Err(WasmExecutionError::MissingChildStream("stderr"));
    };

    let status =
        wait_for_child_with_optional_timeout(&mut child, timeout).map_err(|timed_out| {
            if timed_out == ChildWaitError::TimedOut {
                WasmExecutionError::WarmupTimeout(timeout.expect("timeout should be present"))
            } else {
                WasmExecutionError::WarmupSpawn(std::io::Error::other(
                    "failed to wait for WebAssembly warmup child",
                ))
            }
        })?;

    let mut stderr_bytes = Vec::new();
    let _ = stderr.read_to_end(&mut stderr_bytes);
    Ok(WarmupOutput {
        status,
        stderr: stderr_bytes,
    })
}

fn spawn_wasm_waiter(
    mut child: Child,
    stdout_reader: JoinHandle<()>,
    stderr_reader: JoinHandle<()>,
    timeout: Option<Duration>,
    sender: mpsc::Sender<WasmProcessEvent>,
) {
    std::thread::spawn(move || {
        let wait_result = wait_for_child_with_optional_timeout(&mut child, timeout);
        match wait_result {
            Ok(status) => {
                let exit_code = status.code().unwrap_or(1);
                let _ = sender.send(WasmProcessEvent::Exited(exit_code));
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return;
            }
            Err(ChildWaitError::TimedOut) => {
                let _ = sender.send(WasmProcessEvent::RawStderr(
                    b"WebAssembly fuel budget exhausted\n".to_vec(),
                ));
                let _ = sender.send(WasmProcessEvent::Exited(WASM_TIMEOUT_EXIT_CODE));
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return;
            }
            Err(ChildWaitError::WaitFailed) => {
                let _ = sender.send(WasmProcessEvent::RawStderr(
                    b"agent-os execution wait error: failed to wait for WebAssembly child\n"
                        .to_vec(),
                ));
                let _ = sender.send(WasmProcessEvent::Exited(1));
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return;
            }
        }
    });
}

fn wait_for_child_with_optional_timeout(
    child: &mut Child,
    timeout: Option<Duration>,
) -> Result<std::process::ExitStatus, ChildWaitError> {
    if timeout.is_none() {
        return child.wait().map_err(|_| ChildWaitError::WaitFailed);
    }

    let timeout = timeout.expect("timeout should be present");
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(ChildWaitError::TimedOut);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return Err(ChildWaitError::WaitFailed),
        }
    }
}

fn resolve_wasm_execution_timeout(
    request: &StartWasmExecutionRequest,
) -> Result<Option<Duration>, WasmExecutionError> {
    Ok(wasm_limit_u64(&request.env, WASM_MAX_FUEL_ENV)?.map(Duration::from_millis))
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
    context: &WasmContext,
    request: &StartWasmExecutionRequest,
) -> Result<(), WasmExecutionError> {
    let Some(memory_limit) = wasm_memory_limit_bytes(request)? else {
        return Ok(());
    };

    let resolved_path = resolved_module_path(&module_path(context, request)?, &request.cwd);
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
        None if module_limits.initial_memory_bytes.is_some() => Err(WasmExecutionError::InvalidModule(
            String::from(
                "configured WebAssembly memory limit requires the module to declare a memory maximum",
            ),
        )),
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
        let section_size = read_varuint(bytes, &mut offset)? as usize;
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
                let import_count = read_varuint(bytes, &mut cursor)? as usize;
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
                let memory_count = read_varuint(bytes, &mut cursor)? as usize;
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
    let length = read_varuint(bytes, offset)? as usize;
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

    loop {
        let byte = read_byte(bytes, offset)?;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
        shift = shift.saturating_add(7);
        if shift >= 64 {
            return Err(WasmExecutionError::InvalidModule(String::from(
                "varuint is too large",
            )));
        }
    }
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
