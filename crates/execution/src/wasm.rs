use crate::common::{encode_json_string, frozen_time_ms, stable_hash64};
use crate::node_import_cache::NodeImportCache;
use crate::node_process::{
    apply_guest_env, configure_node_control_channel, create_node_control_channel,
    encode_json_string_array, encode_json_string_map, harden_node_command, node_binary,
    node_resolution_read_paths, resolve_path_like_specifier, spawn_node_control_reader,
    spawn_stream_reader, spawn_waiter, LinePrefixFilter, NodeControlMessage,
    NodeSignalDispositionAction, NodeSignalHandlerRegistration,
};
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::{
    mpsc::{self, Receiver, RecvTimeoutError},
    Arc, Mutex,
};
use std::time::{Duration, UNIX_EPOCH};

const WASM_MODULE_PATH_ENV: &str = "AGENT_OS_WASM_MODULE_PATH";
const WASM_GUEST_ARGV_ENV: &str = "AGENT_OS_GUEST_ARGV";
const WASM_GUEST_ENV_ENV: &str = "AGENT_OS_GUEST_ENV";
const WASM_PREWARM_ONLY_ENV: &str = "AGENT_OS_WASM_PREWARM_ONLY";
const WASM_WARMUP_DEBUG_ENV: &str = "AGENT_OS_WASM_WARMUP_DEBUG";
const WASM_WARMUP_METRICS_PREFIX: &str = "__AGENT_OS_WASM_WARMUP_METRICS__:";
const NODE_COMPILE_CACHE_ENV: &str = "NODE_COMPILE_CACHE";
const NODE_DISABLE_COMPILE_CACHE_ENV: &str = "NODE_DISABLE_COMPILE_CACHE";
const NODE_FROZEN_TIME_ENV: &str = "AGENT_OS_FROZEN_TIME_MS";
const WASM_WARMUP_MARKER_VERSION: &str = "1";
const SIGNAL_STATE_CONTROL_PREFIX: &str = "__AGENT_OS_SIGNAL_STATE__:";
const CONTROLLED_STDERR_PREFIXES: &[&str] = &[SIGNAL_STATE_CONTROL_PREFIX];
const RESERVED_WASM_ENV_KEYS: &[&str] = &[
    NODE_COMPILE_CACHE_ENV,
    NODE_DISABLE_COMPILE_CACHE_ENV,
    NODE_FROZEN_TIME_ENV,
    WASM_GUEST_ARGV_ENV,
    WASM_GUEST_ENV_ENV,
    WASM_MODULE_PATH_ENV,
    WASM_PREWARM_ONLY_ENV,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WasmSignalDispositionAction {
    Default,
    Ignore,
    User,
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
    MissingChildStream(&'static str),
    PrepareWarmPath(std::io::Error),
    WarmupSpawn(std::io::Error),
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
            Self::MissingChildStream(name) => write!(f, "node child missing {name} pipe"),
            Self::PrepareWarmPath(err) => {
                write!(f, "failed to prepare shared WebAssembly warm path: {err}")
            }
            Self::WarmupSpawn(err) => {
                write!(f, "failed to start WebAssembly warmup process: {err}")
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
    import_cache: NodeImportCache,
}

impl WasmExecutionEngine {
    pub fn create_context(&mut self, request: CreateWasmContextRequest) -> WasmContext {
        self.next_context_id += 1;

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

        self.import_cache
            .ensure_materialized()
            .map_err(WasmExecutionError::PrepareWarmPath)?;
        let frozen_time_ms = frozen_time_ms();
        let warmup_metrics =
            prewarm_wasm_path(&self.import_cache, &context, &request, frozen_time_ms)?;

        self.next_execution_id += 1;
        let execution_id = format!("exec-{}", self.next_execution_id);
        let guest_argv = guest_argv(&context, &request)?;
        let control_channel = create_node_control_channel().map_err(WasmExecutionError::Spawn)?;
        let mut child = create_node_child(
            &self.import_cache,
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
        spawn_waiter(
            child,
            stdout_reader,
            stderr_reader,
            true,
            sender,
            WasmProcessEvent::Exited,
            |message| WasmProcessEvent::RawStderr(message.into_bytes()),
        );

        Ok(WasmExecution {
            execution_id,
            child_pid,
            stdin,
            events: receiver,
            stderr_filter: Arc::new(Mutex::new(LinePrefixFilter::default())),
        })
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
        .env(WASM_GUEST_ENV_ENV, encode_json_string_map(&request.env));

    configure_node_control_channel(&mut command, control_fd);
    configure_node_command(&mut command, import_cache, frozen_time_ms)?;

    command.spawn().map_err(WasmExecutionError::Spawn)
}

fn prewarm_wasm_path(
    import_cache: &NodeImportCache,
    context: &WasmContext,
    request: &StartWasmExecutionRequest,
    frozen_time_ms: u128,
) -> Result<Option<Vec<u8>>, WasmExecutionError> {
    let debug_enabled = request
        .env
        .get(WASM_WARMUP_DEBUG_ENV)
        .is_some_and(|value| value == "1");
    let marker_path = warmup_marker_path(import_cache, context, request);

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
        .env(WASM_GUEST_ENV_ENV, encode_json_string_map(&request.env));

    configure_node_command(&mut command, import_cache, frozen_time_ms)?;

    let output = command.output().map_err(WasmExecutionError::WarmupSpawn)?;
    if !output.status.success() {
        return Err(WasmExecutionError::WarmupFailed {
            exit_code: output.status.code().unwrap_or(1),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }

    fs::write(&marker_path, warmup_marker_contents(context, request))
        .map_err(WasmExecutionError::PrepareWarmPath)?;

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
    let cache_root = import_cache
        .cache_path()
        .parent()
        .unwrap_or(import_cache.prewarm_marker_dir())
        .to_path_buf();
    let compile_cache_dir = import_cache.shared_compile_cache_dir();
    let mut read_paths = vec![cache_root.clone(), compile_cache_dir.clone()];
    let write_paths = vec![cache_root, compile_cache_dir, request.cwd.clone()];

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
        &request.cwd,
        &read_paths,
        &write_paths,
        true,
        true,
        false,
    );
    Ok(())
}

fn configure_node_command(
    command: &mut Command,
    import_cache: &NodeImportCache,
    frozen_time_ms: u128,
) -> Result<(), WasmExecutionError> {
    let compile_cache_dir = import_cache.shared_compile_cache_dir();
    fs::create_dir_all(&compile_cache_dir).map_err(WasmExecutionError::PrepareWarmPath)?;

    command
        .env_remove(NODE_DISABLE_COMPILE_CACHE_ENV)
        .env(NODE_COMPILE_CACHE_ENV, &compile_cache_dir)
        .env(NODE_FROZEN_TIME_ENV, frozen_time_ms.to_string());
    Ok(())
}

fn warmup_marker_path(
    import_cache: &NodeImportCache,
    context: &WasmContext,
    request: &StartWasmExecutionRequest,
) -> PathBuf {
    import_cache.prewarm_marker_dir().join(format!(
        "wasm-runner-prewarm-v{WASM_WARMUP_MARKER_VERSION}-{:016x}.stamp",
        stable_hash64(warmup_marker_contents(context, request).as_bytes()),
    ))
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
