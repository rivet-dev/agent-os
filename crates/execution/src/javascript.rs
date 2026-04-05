use crate::common::{encode_json_string, frozen_time_ms, stable_hash64};
use crate::node_import_cache::{NodeImportCache, NODE_IMPORT_CACHE_ASSET_ROOT_ENV};
use crate::node_process::{
    apply_guest_env, configure_node_control_channel, create_node_control_channel,
    encode_json_string_array, harden_node_command, node_binary, node_resolution_read_paths,
    resolve_path_like_specifier, spawn_node_control_reader, spawn_stream_reader, spawn_waiter,
    LinePrefixFilter, NodeControlMessage,
};
use serde_json::from_str;
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{ChildStdin, Command, Stdio};
use std::sync::{
    mpsc::{self, Receiver, RecvTimeoutError},
    Arc, Mutex,
};
use std::time::Duration;

const NODE_ENTRYPOINT_ENV: &str = "AGENT_OS_ENTRYPOINT";
const NODE_BOOTSTRAP_ENV: &str = "AGENT_OS_BOOTSTRAP_MODULE";
const NODE_GUEST_ARGV_ENV: &str = "AGENT_OS_GUEST_ARGV";
const NODE_PREWARM_IMPORTS_ENV: &str = "AGENT_OS_NODE_PREWARM_IMPORTS";
const NODE_WARMUP_DEBUG_ENV: &str = "AGENT_OS_NODE_WARMUP_DEBUG";
const NODE_WARMUP_METRICS_PREFIX: &str = "__AGENT_OS_NODE_WARMUP_METRICS__:";
const NODE_COMPILE_CACHE_ENV: &str = "NODE_COMPILE_CACHE";
const NODE_DISABLE_COMPILE_CACHE_ENV: &str = "NODE_DISABLE_COMPILE_CACHE";
const NODE_IMPORT_COMPILE_CACHE_NAMESPACE_VERSION: &str = "3";
const NODE_IMPORT_CACHE_LOADER_PATH_ENV: &str = "AGENT_OS_NODE_IMPORT_CACHE_LOADER_PATH";
const NODE_IMPORT_CACHE_PATH_ENV: &str = "AGENT_OS_NODE_IMPORT_CACHE_PATH";
const NODE_FROZEN_TIME_ENV: &str = "AGENT_OS_FROZEN_TIME_MS";
const NODE_KEEP_STDIN_OPEN_ENV: &str = "AGENT_OS_KEEP_STDIN_OPEN";
const NODE_GUEST_ENTRYPOINT_ENV: &str = "AGENT_OS_GUEST_ENTRYPOINT";
const NODE_GUEST_PATH_MAPPINGS_ENV: &str = "AGENT_OS_GUEST_PATH_MAPPINGS";
const NODE_VIRTUAL_PROCESS_EXEC_PATH_ENV: &str = "AGENT_OS_VIRTUAL_PROCESS_EXEC_PATH";
const NODE_VIRTUAL_PROCESS_PID_ENV: &str = "AGENT_OS_VIRTUAL_PROCESS_PID";
const NODE_VIRTUAL_PROCESS_PPID_ENV: &str = "AGENT_OS_VIRTUAL_PROCESS_PPID";
const NODE_VIRTUAL_PROCESS_UID_ENV: &str = "AGENT_OS_VIRTUAL_PROCESS_UID";
const NODE_VIRTUAL_PROCESS_GID_ENV: &str = "AGENT_OS_VIRTUAL_PROCESS_GID";
const NODE_EXTRA_FS_READ_PATHS_ENV: &str = "AGENT_OS_EXTRA_FS_READ_PATHS";
const NODE_EXTRA_FS_WRITE_PATHS_ENV: &str = "AGENT_OS_EXTRA_FS_WRITE_PATHS";
const NODE_ALLOWED_BUILTINS_ENV: &str = "AGENT_OS_ALLOWED_NODE_BUILTINS";
const NODE_LOOPBACK_EXEMPT_PORTS_ENV: &str = "AGENT_OS_LOOPBACK_EXEMPT_PORTS";
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
    NODE_FROZEN_TIME_ENV,
    NODE_GUEST_ENTRYPOINT_ENV,
    NODE_GUEST_ARGV_ENV,
    NODE_GUEST_PATH_MAPPINGS_ENV,
    NODE_VIRTUAL_PROCESS_EXEC_PATH_ENV,
    NODE_VIRTUAL_PROCESS_PID_ENV,
    NODE_VIRTUAL_PROCESS_PPID_ENV,
    NODE_VIRTUAL_PROCESS_UID_ENV,
    NODE_VIRTUAL_PROCESS_GID_ENV,
    NODE_IMPORT_CACHE_ASSET_ROOT_ENV,
    NODE_IMPORT_CACHE_LOADER_PATH_ENV,
    NODE_IMPORT_CACHE_PATH_ENV,
    NODE_KEEP_STDIN_OPEN_ENV,
    NODE_ALLOWED_BUILTINS_ENV,
    NODE_LOOPBACK_EXEMPT_PORTS_ENV,
];

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
    Exited(i32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum JavascriptProcessEvent {
    Stdout(Vec<u8>),
    RawStderr(Vec<u8>),
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
}

#[derive(Debug, Default)]
pub struct JavascriptExecutionEngine {
    next_context_id: usize,
    next_execution_id: usize,
    contexts: BTreeMap<String, JavascriptContext>,
    import_cache: NodeImportCache,
}

impl JavascriptExecutionEngine {
    pub fn create_context(&mut self, request: CreateJavascriptContextRequest) -> JavascriptContext {
        self.next_context_id += 1;

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

        self.import_cache
            .ensure_materialized()
            .map_err(JavascriptExecutionError::PrepareImportCache)?;
        let frozen_time_ms = frozen_time_ms();
        let warmup_metrics =
            prewarm_node_import_path(&self.import_cache, &context, &request, frozen_time_ms)?;

        self.next_execution_id += 1;
        let execution_id = format!("exec-{}", self.next_execution_id);
        let control_channel =
            create_node_control_channel().map_err(JavascriptExecutionError::Spawn)?;
        let mut child = create_node_child(
            &self.import_cache,
            &context,
            &request,
            frozen_time_ms,
            &control_channel.child_writer,
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
        })
    }
}

fn prewarm_node_import_path(
    import_cache: &NodeImportCache,
    context: &JavascriptContext,
    request: &StartJavascriptExecutionRequest,
    frozen_time_ms: u128,
) -> Result<Option<Vec<u8>>, JavascriptExecutionError> {
    let debug_enabled = request
        .env
        .get(NODE_WARMUP_DEBUG_ENV)
        .is_some_and(|value| value == "1");

    let Some(_compile_cache_dir) = &context.compile_cache_dir else {
        return Ok(warmup_metrics_line(
            debug_enabled,
            false,
            "compile-cache-disabled",
            import_cache,
        ));
    };

    let marker_path = warmup_marker_path(import_cache);
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
) -> Result<std::process::Child, JavascriptExecutionError> {
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

    if let Some(bootstrap_module) = &context.bootstrap_module {
        command.env(NODE_BOOTSTRAP_ENV, bootstrap_module);
    }

    configure_node_control_channel(&mut command, control_fd);
    configure_node_command(&mut command, import_cache, context, frozen_time_ms)?;

    command.spawn().map_err(JavascriptExecutionError::Spawn)
}

fn configure_node_sandbox(
    command: &mut Command,
    import_cache: &NodeImportCache,
    context: &JavascriptContext,
    request: &StartJavascriptExecutionRequest,
) -> Result<(), JavascriptExecutionError> {
    let cache_root = import_cache
        .cache_path()
        .parent()
        .unwrap_or(import_cache.asset_root())
        .to_path_buf();
    let mut read_paths = vec![cache_root.clone()];
    let mut write_paths = vec![cache_root, request.cwd.clone()];

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
        &request.cwd,
        &read_paths,
        &write_paths,
        true,
        false,
        env_builtin_enabled(&request.env, "child_process"),
    );
    Ok(())
}

fn parse_env_path_list(env: &BTreeMap<String, String>, key: &str) -> Vec<PathBuf> {
    env.get(key)
        .and_then(|value| from_str::<Vec<String>>(value).ok())
        .into_iter()
        .flatten()
        .map(PathBuf::from)
        .collect()
}

fn env_builtin_enabled(env: &BTreeMap<String, String>, builtin: &str) -> bool {
    env.get(NODE_ALLOWED_BUILTINS_ENV)
        .and_then(|value| from_str::<Vec<String>>(value).ok())
        .is_some_and(|builtins| builtins.iter().any(|entry| entry == builtin))
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
        fs::create_dir_all(compile_cache_dir)
            .map_err(JavascriptExecutionError::PrepareImportCache)?;
        command.env_remove(NODE_DISABLE_COMPILE_CACHE_ENV);
        command.env(NODE_COMPILE_CACHE_ENV, compile_cache_dir);
    }

    Ok(())
}

fn warmup_marker_path(import_cache: &NodeImportCache) -> PathBuf {
    import_cache.prewarm_marker_dir().join(format!(
        "node-import-prewarm-v{NODE_WARMUP_MARKER_VERSION}-{:016x}.stamp",
        stable_hash64(warmup_marker_contents().as_bytes()),
    ))
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
