use crate::common::stable_hash64;
use crate::node_import_cache::{
    NodeImportCache, NodeImportCacheCleanup, NODE_IMPORT_CACHE_ASSET_ROOT_ENV,
};
use crate::node_process::{LinePrefixFilter, NodeControlMessage, NodeSignalHandlerRegistration};
use crate::runtime_support::{
    NODE_COMPILE_CACHE_ENV, NODE_DISABLE_COMPILE_CACHE_ENV, NODE_FROZEN_TIME_ENV,
    NODE_SANDBOX_ROOT_ENV,
};
use crate::v8_host::{V8RuntimeHost, V8SessionHandle};
use crate::v8_ipc::BinaryFrame;
use crate::v8_runtime;
use getrandom::getrandom;
use serde::Deserialize;
use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fmt;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::{
    mpsc::{self, Receiver, SyncSender, TrySendError},
    Arc, Condvar, Mutex, OnceLock,
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
const V8_HEAP_LIMIT_MB_ENV: &str = "AGENT_OS_V8_HEAP_LIMIT_MB";
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
    kernel_stdin: Arc<LocalKernelStdinBridge>,
    v8_session: Option<V8SessionHandle>,
}

#[derive(Debug, Default)]
struct LocalKernelStdinBridge {
    state: Mutex<LocalKernelStdinState>,
    ready: Condvar,
}

#[derive(Debug, Default)]
struct LocalKernelStdinState {
    bytes: VecDeque<u8>,
    closed: bool,
}

#[derive(Debug, Clone, Default)]
struct GuestPathTranslator {
    implicit_guest_cwd: String,
    implicit_host_cwd: PathBuf,
    sandbox_root: Option<PathBuf>,
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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum PolyfillSourceKind {
    NodeStdlibBrowser,
    CustomBridge,
    Denied,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PolyfillRegistryGroup {
    source: PolyfillSourceKind,
    #[serde(default)]
    error_code: Option<String>,
    names: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PolyfillRegistry {
    version: u32,
    groups: Vec<PolyfillRegistryGroup>,
}

static POLYFILL_REGISTRY: OnceLock<PolyfillRegistry> = OnceLock::new();

fn polyfill_registry() -> &'static PolyfillRegistry {
    POLYFILL_REGISTRY.get_or_init(|| {
        serde_json::from_str(include_str!("../assets/polyfill-registry.json"))
            .expect("polyfill-registry.json must be valid")
    })
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
    fn is_known_host_path(&self, host_path: &Path) -> bool {
        if host_path.starts_with(&self.implicit_host_cwd) {
            return true;
        }

        if let Some(sandbox_root) = &self.sandbox_root {
            if host_path.starts_with(sandbox_root) {
                return true;
            }
        }

        self.mappings.iter().any(|mapping| {
            host_path.starts_with(&mapping.host_path)
                || fs::canonicalize(&mapping.host_path)
                    .map(|real_path| host_path.starts_with(real_path))
                    .unwrap_or(false)
        })
    }

    fn from_request(request: &StartJavascriptExecutionRequest) -> Self {
        let implicit_guest_cwd = request
            .env
            .get("PWD")
            .filter(|value| value.starts_with('/'))
            .cloned()
            .or_else(|| {
                request
                    .env
                    .get("HOME")
                    .filter(|value| value.starts_with('/'))
                    .cloned()
            })
            .unwrap_or_else(|| String::from("/root"));
        let mut mappings = parse_guest_path_mappings(request)
            .into_iter()
            .filter(|mapping| mapping.guest_path.starts_with('/'))
            .collect::<Vec<_>>();

        if !mappings
            .iter()
            .any(|mapping| mapping.guest_path == implicit_guest_cwd)
        {
            mappings.push(GuestPathMapping {
                guest_path: implicit_guest_cwd.clone(),
                host_path: request.cwd.clone(),
            });
        }

        sort_guest_path_mappings(&mut mappings);

        Self {
            implicit_guest_cwd,
            implicit_host_cwd: request.cwd.clone(),
            sandbox_root: request
                .env
                .get(NODE_SANDBOX_ROOT_ENV)
                .filter(|value| Path::new(value.as_str()).is_absolute())
                .map(PathBuf::from),
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
            if self.is_known_host_path(path) {
                return path.to_path_buf();
            }
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
            if let Ok(real_mapping_path) = fs::canonicalize(&mapping.host_path) {
                if let Ok(stripped) = host_path.strip_prefix(&real_mapping_path) {
                    return join_guest_path(
                        &mapping.guest_path,
                        &stripped.to_string_lossy().replace('\\', "/"),
                    );
                }
            }
        }

        if let Ok(stripped) = host_path.strip_prefix(&self.implicit_host_cwd) {
            return join_guest_path(
                &self.implicit_guest_cwd,
                &stripped.to_string_lossy().replace('\\', "/"),
            );
        }

        if let Some(sandbox_root) = &self.sandbox_root {
            if let Ok(stripped) = host_path.strip_prefix(sandbox_root) {
                return join_guest_path("/", &stripped.to_string_lossy().replace('\\', "/"));
            }
        }

        let basename = host_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("unknown");
        join_guest_path("/unknown", basename)
    }

    fn guest_to_host(&self, guest_path: &str) -> Option<PathBuf> {
        let normalized = normalize_guest_path(guest_path);
        let mut fallback_candidate = None;

        for mapping in &self.mappings {
            if let Some(suffix) = strip_guest_prefix(&normalized, &mapping.guest_path) {
                let candidate = join_host_path(&mapping.host_path, suffix);
                if candidate.exists() {
                    return Some(candidate);
                }
                if let Ok(real_mapping_path) = fs::canonicalize(&mapping.host_path) {
                    let real_candidate = join_host_path(&real_mapping_path, suffix);
                    if real_candidate.exists() {
                        return Some(real_candidate);
                    }
                    if let Some(sibling_candidate) =
                        resolve_pnpm_sibling_host_path(&real_mapping_path, suffix)
                    {
                        return Some(sibling_candidate);
                    }
                }
                fallback_candidate.get_or_insert(candidate);
            }
        }
        if fallback_candidate.is_some() {
            return fallback_candidate;
        }

        if let Some(suffix) = strip_guest_prefix(&normalized, &self.implicit_guest_cwd) {
            return Some(join_host_path(&self.implicit_host_cwd, suffix));
        }

        if let Some(sandbox_root) = &self.sandbox_root {
            return Some(join_host_path(
                sandbox_root,
                normalized.trim_start_matches('/'),
            ));
        }

        let path = PathBuf::from(&normalized);
        if path.is_absolute() {
            Some(path)
        } else {
            None
        }
    }

    fn canonical_guest_path(&self, guest_path: &str) -> Option<String> {
        let host_path = self.guest_to_host(guest_path)?;
        let canonical = fs::canonicalize(host_path).ok()?;
        for mapping in &self.mappings {
            if strip_guest_prefix(guest_path, &mapping.guest_path).is_none() {
                continue;
            }
            if let Ok(stripped) = canonical.strip_prefix(&mapping.host_path) {
                return Some(join_guest_path(
                    &mapping.guest_path,
                    &stripped.to_string_lossy().replace('\\', "/"),
                ));
            }
            if let Ok(real_mapping_path) = fs::canonicalize(&mapping.host_path) {
                if let Ok(stripped) = canonical.strip_prefix(&real_mapping_path) {
                    return Some(join_guest_path(
                        &mapping.guest_path,
                        &stripped.to_string_lossy().replace('\\', "/"),
                    ));
                }
            }
        }
        if let Some(node_modules_root) = self
            .mappings
            .iter()
            .find(|mapping| mapping.guest_path == "/root/node_modules")
        {
            if let Ok(stripped) = canonical.strip_prefix(&node_modules_root.host_path) {
                return Some(join_guest_path(
                    &node_modules_root.guest_path,
                    &stripped.to_string_lossy().replace('\\', "/"),
                ));
            }
            if let Ok(real_root) = fs::canonicalize(&node_modules_root.host_path) {
                if let Ok(stripped) = canonical.strip_prefix(&real_root) {
                    return Some(join_guest_path(
                        &node_modules_root.guest_path,
                        &stripped.to_string_lossy().replace('\\', "/"),
                    ));
                }
            }
        }
        let guest = self.host_to_guest_string(&canonical);
        (!guest.starts_with("/unknown/")).then_some(normalize_guest_path(&guest))
    }
}

fn sort_guest_path_mappings(mappings: &mut [GuestPathMapping]) {
    mappings.sort_by(|left, right| {
        right
            .guest_path
            .len()
            .cmp(&left.guest_path.len())
            .then_with(|| {
                right
                    .host_path
                    .components()
                    .count()
                    .cmp(&left.host_path.components().count())
            })
    });
}

#[doc(hidden)]
pub struct ModuleResolutionTestHarness {
    local_bridge: LocalBridgeState,
}

impl ModuleResolutionTestHarness {
    pub fn new(host_root: impl Into<PathBuf>) -> Self {
        let host_root = host_root.into();
        let mut mappings = vec![
            GuestPathMapping {
                guest_path: String::from("/root/node_modules"),
                host_path: host_root.join("node_modules"),
            },
            GuestPathMapping {
                guest_path: String::from("/root"),
                host_path: host_root.clone(),
            },
        ];
        sort_guest_path_mappings(&mut mappings);

        Self {
            local_bridge: LocalBridgeState {
                translator: GuestPathTranslator {
                    implicit_guest_cwd: String::from("/root"),
                    implicit_host_cwd: host_root,
                    sandbox_root: None,
                    mappings,
                },
                ..LocalBridgeState::default()
            },
        }
    }

    pub fn resolve_import(&mut self, specifier: &str, from_path: &str) -> Option<String> {
        self.local_bridge
            .resolve_module(specifier, from_path, ModuleResolveMode::Import)
    }

    pub fn resolve_require(&mut self, specifier: &str, from_path: &str) -> Option<String> {
        self.local_bridge
            .resolve_module(specifier, from_path, ModuleResolveMode::Require)
    }
}

fn resolve_pnpm_sibling_host_path(real_mapping_path: &Path, suffix: &str) -> Option<PathBuf> {
    let trimmed = suffix.strip_prefix("node_modules/")?;
    let mut current = Some(real_mapping_path);
    while let Some(path) = current {
        if path.file_name().and_then(|name| name.to_str()) == Some("node_modules") {
            let candidate = join_host_path(path, trimmed);
            if candidate.exists() {
                return Some(candidate);
            }
            break;
        }
        current = path.parent();
    }
    None
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
                "base64": v8_runtime::base64_encode_pub(data.as_bytes()),
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
    PrepareImportCache(std::io::Error),
    Spawn(std::io::Error),
    PendingSyncRpcRequest(u64),
    ExpiredSyncRpcRequest(u64),
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
            Self::PrepareImportCache(err) => {
                write!(
                    f,
                    "failed to prepare sidecar-scoped Node import cache: {err}"
                )
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
    events: RefCell<UnboundedReceiver<JavascriptExecutionEvent>>,
    pending_sync_rpc: Arc<Mutex<Option<PendingSyncRpcState>>>,
    kernel_stdin: Arc<LocalKernelStdinBridge>,
    _import_cache_guard: Arc<NodeImportCacheCleanup>,
    v8_session: V8SessionHandle,
}

impl JavascriptExecution {
    pub fn execution_id(&self) -> &str {
        &self.execution_id
    }

    pub fn child_pid(&self) -> u32 {
        self.child_pid
    }

    pub(crate) fn v8_session_handle(&self) -> V8SessionHandle {
        self.v8_session.clone()
    }

    pub fn uses_shared_v8_runtime(&self) -> bool {
        true
    }

    pub fn write_stdin(&mut self, chunk: &[u8]) -> Result<(), JavascriptExecutionError> {
        self.kernel_stdin.write(chunk);
        let payload =
            v8_runtime::json_to_cbor_payload(&Value::String(String::from_utf8_lossy(chunk).into()))
                .map_err(JavascriptExecutionError::Stdin)?;
        self.v8_session
            .send_stream_event("stdin", payload)
            .map_err(JavascriptExecutionError::Stdin)
    }

    pub fn close_stdin(&mut self) -> Result<(), JavascriptExecutionError> {
        self.kernel_stdin.close();
        let _ = self.v8_session.send_stream_event("stdin_end", vec![]);
        Ok(())
    }

    pub fn terminate(&self) -> Result<(), JavascriptExecutionError> {
        self.v8_session
            .terminate()
            .map_err(JavascriptExecutionError::Terminate)
    }

    pub fn send_stream_event(
        &self,
        event_type: &str,
        payload: Value,
    ) -> Result<(), JavascriptExecutionError> {
        let payload = v8_runtime::json_to_cbor_payload(&payload)
            .map_err(|error| JavascriptExecutionError::RpcResponse(error.to_string()))?;
        self.v8_session
            .send_stream_event(event_type, payload)
            .map_err(|error| JavascriptExecutionError::RpcResponse(error.to_string()))
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

        let payload = translate_legacy_bridge_value_to_v8(&result);
        let payload = v8_runtime::json_to_cbor_payload(&payload)
            .map_err(|e| JavascriptExecutionError::RpcResponse(e.to_string()))?;
        self.v8_session
            .send_bridge_response(id, 0, payload)
            .map_err(|e| JavascriptExecutionError::RpcResponse(e.to_string()))
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

        let error_msg = format!("{}: {}", code.into(), message.into());
        self.v8_session
            .send_bridge_response(id, 1, error_msg.into_bytes())
            .map_err(|e| JavascriptExecutionError::RpcResponse(e.to_string()))
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
                    return Err(JavascriptExecutionError::EventChannelClosed);
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

        // Lazily spawn the V8 runtime host, or replace a dead shared host.
        let should_spawn_v8_host = match self.v8_host.as_mut() {
            Some(v8_host) => !v8_host
                .is_alive()
                .map_err(JavascriptExecutionError::Spawn)?,
            None => true,
        };
        if should_spawn_v8_host {
            self.v8_host = Some(V8RuntimeHost::spawn().map_err(JavascriptExecutionError::Spawn)?);
        }
        let v8_host = self.v8_host.as_ref().unwrap();

        // Create a V8 session
        let session_id = format!("v8-{execution_id}");
        let frame_receiver = v8_host.register_session(&session_id);

        v8_host
            .send_frame(&BinaryFrame::CreateSession {
                session_id: session_id.clone(),
                heap_limit_mb: javascript_heap_limit_mb(&request),
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
        let process_argv = if matches!(guest_entrypoint.as_str(), "-e" | "--eval") {
            std::iter::once(String::from("node"))
                .chain(request.argv.iter().skip(1).cloned())
                .collect::<Vec<_>>()
        } else {
            std::iter::once(String::from("node"))
                .chain(std::iter::once(guest_entrypoint.clone()))
                .chain(request.argv.iter().skip(1).cloned())
                .collect::<Vec<_>>()
        };
        let use_module_mode = host_entrypoint_uses_module_mode(&host_entrypoint);
        let user_code = if let Some(inline_code) = request.inline_code.clone() {
            strip_javascript_hashbang(&inline_code)
        } else if use_module_mode {
            strip_javascript_hashbang(&fs::read_to_string(&host_entrypoint).map_err(|error| {
                JavascriptExecutionError::PrepareImportCache(std::io::Error::new(
                    error.kind(),
                    format!(
                        "failed to read JavaScript entrypoint {}: {error}",
                        host_entrypoint.display()
                    ),
                ))
            })?)
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
        let kernel_stdin = Arc::new(LocalKernelStdinBridge::default());
        let events = spawn_v8_event_bridge(
            frame_receiver,
            pending_sync_rpc.clone(),
            sync_rpc_timeout,
            v8_session.clone(),
            LocalBridgeState {
                translator,
                kernel_stdin: kernel_stdin.clone(),
                v8_session: Some(v8_session.clone()),
                ..Default::default()
            },
        );

        Ok(JavascriptExecution {
            execution_id,
            child_pid: v8_host.child_pid(),
            events: RefCell::new(events),
            pending_sync_rpc,
            kernel_stdin,
            _import_cache_guard: import_cache_guard,
            v8_session,
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

fn javascript_sync_rpc_timeout(request: &StartJavascriptExecutionRequest) -> Duration {
    let timeout_ms = request
        .env
        .get(NODE_SYNC_RPC_WAIT_TIMEOUT_MS_ENV)
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(NODE_SYNC_RPC_DEFAULT_WAIT_TIMEOUT_MS);
    Duration::from_millis(timeout_ms)
}

fn javascript_heap_limit_mb(request: &StartJavascriptExecutionRequest) -> u32 {
    request
        .env
        .get(V8_HEAP_LIMIT_MB_ENV)
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(0)
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

fn host_entrypoint_uses_module_mode(entrypoint: &Path) -> bool {
    match entrypoint.extension().and_then(|ext| ext.to_str()) {
        Some("mjs" | "mts") => true,
        Some("js") => nearest_package_json_type(entrypoint).as_deref() == Some("module"),
        _ => false,
    }
}

fn nearest_package_json_type(entrypoint: &Path) -> Option<String> {
    let mut current = entrypoint.parent();
    while let Some(dir) = current {
        let package_json = dir.join("package.json");
        if let Ok(contents) = fs::read_to_string(&package_json) {
            if let Ok(pkg) = serde_json::from_str::<LocalPackageJson>(&contents) {
                return pkg.package_type;
            }
        }
        current = dir.parent();
    }
    None
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
    const configuredHeapLimitMb = Number.parseInt(
      nextEnv.AGENT_OS_V8_HEAP_LIMIT_MB ?? "",
      10,
    );
    if (Number.isFinite(configuredHeapLimitMb) && configuredHeapLimitMb > 0) {{
      Object.defineProperty(globalThis, "__agentOsV8HeapLimitBytes", {{
        configurable: true,
        enumerable: false,
        value: configuredHeapLimitMb * 1024 * 1024,
        writable: true,
      }});
    }}
    if (nextEnv.AGENT_OS_ALLOW_PROCESS_BINDINGS === "1" && typeof process.binding === "function") {{
      const originalProcessBinding = process.binding.bind(process);
      process.binding = (name) => {{
        const bindingName = String(name);
        if (
          bindingName === "constants" &&
          typeof __agentOsConstantsBinding !== "undefined"
        ) {{
          const constantsBinding =
            __agentOsConstantsBinding.default ?? __agentOsConstantsBinding;
          return {{
            fs: constantsBinding,
            crypto: constantsBinding,
            zlib: constantsBinding,
            trace: constantsBinding,
            internal: constantsBinding,
            os: {{
              UV_UDP_REUSEADDR: constantsBinding.UV_UDP_REUSEADDR,
              dlopen: constantsBinding.dlopen,
              errno: constantsBinding.errno,
              signals: constantsBinding.signals,
              priority: constantsBinding.priority,
            }},
          }};
        }}
        try {{
          return originalProcessBinding(name);
        }} catch (error) {{
          const originalMessage =
            error && typeof error === "object" && typeof error.message === "string"
              ? error.message
              : String(error);
          throw new Error(
            `process.binding(${{bindingName}}) failed: ${{originalMessage}}`
          );
        }}
      }};
    }}
    const nextPid = Number(nextEnv.AGENT_OS_VIRTUAL_PROCESS_PID);
    if (Number.isFinite(nextPid) && nextPid > 0) {{
      process.pid = nextPid;
    }}
    const nextPpid = Number(nextEnv.AGENT_OS_VIRTUAL_PROCESS_PPID);
    if (Number.isFinite(nextPpid) && nextPpid >= 0) {{
      process.ppid = nextPpid;
    }}
    const nextUid = Number(nextEnv.AGENT_OS_VIRTUAL_PROCESS_UID);
    if (Number.isFinite(nextUid) && nextUid >= 0) {{
      process.uid = nextUid;
      process.euid = nextUid;
    }}
    const nextGid = Number(nextEnv.AGENT_OS_VIRTUAL_PROCESS_GID);
    if (Number.isFinite(nextGid) && nextGid >= 0) {{
      process.gid = nextGid;
      process.egid = nextGid;
      process.groups = [nextGid];
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
        let mut emitted_exit = false;
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
                        // Respond to the bridge call
                        let _ = v8_session.send_bridge_response(
                            call_id,
                            0,
                            v8_runtime::json_to_cbor_payload(&Value::Null).unwrap_or_default(),
                        );
                        if method == "_log" {
                            let _ = sender.send(JavascriptExecutionEvent::Stdout(msg.into_bytes()));
                        } else {
                            let _ = sender.send(JavascriptExecutionEvent::Stderr(msg.into_bytes()));
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
                    let resolved_exit_code = error
                        .as_ref()
                        .and_then(|err| {
                            if err.error_type == "ProcessExitError"
                                || err.message.starts_with("process.exit(")
                            {
                                parse_process_exit_code_message(&err.message)
                            } else {
                                None
                            }
                        })
                        .unwrap_or(exit_code);
                    let should_emit_error = if let Some(err) = &error {
                        !(resolved_exit_code == 0
                            && (err.error_type == "ProcessExitError"
                                || err.message.starts_with("process.exit(")))
                    } else {
                        false
                    };
                    if should_emit_error {
                        let err = error.as_ref().expect("checked above");
                        let error_msg = if err.stack.is_empty() {
                            format!("{}: {}\n", err.error_type, err.message)
                        } else {
                            format!("{}\n", err.stack)
                        };
                        let _ =
                            sender.send(JavascriptExecutionEvent::Stderr(error_msg.into_bytes()));
                    }
                    emitted_exit = true;
                    Some(JavascriptExecutionEvent::Exited(resolved_exit_code))
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

        if !emitted_exit {
            let _ = sender.send(JavascriptExecutionEvent::Exited(1));
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
                if getrandom(&mut bytes).is_err() {
                    return Some(LocalBridgeCallResult::Immediate(Value::Null));
                }
                Some(LocalBridgeCallResult::Immediate(Value::String(
                    v8_runtime::base64_encode_pub(&bytes),
                )))
            }
            "_cryptoRandomUUID" => {
                let mut bytes = [0u8; 16];
                if getrandom(&mut bytes).is_err() {
                    return Some(LocalBridgeCallResult::Immediate(Value::Null));
                }
                bytes[6] = (bytes[6] & 0x0f) | 0x40;
                bytes[8] = (bytes[8] & 0x3f) | 0x80;

                Some(LocalBridgeCallResult::Immediate(Value::String(format!(
                    "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
                    bytes[0],
                    bytes[1],
                    bytes[2],
                    bytes[3],
                    bytes[4],
                    bytes[5],
                    bytes[6],
                    bytes[7],
                    bytes[8],
                    bytes[9],
                    bytes[10],
                    bytes[11],
                    bytes[12],
                    bytes[13],
                    bytes[14],
                    bytes[15],
                ))))
            }
            "_kernelStdinRead" => Some(LocalBridgeCallResult::Immediate(
                self.kernel_stdin.read(args),
            )),
            "_scheduleTimer" => {
                self.schedule_bridge_timer_response(call_id, timer_delay_ms(args.first()));
                Some(LocalBridgeCallResult::Deferred)
            }
            _ => None,
        }
    }

    fn handle_polyfill_dispatch(&mut self, args: &[Value]) -> Value {
        let Some(dispatch) = args.first().and_then(Value::as_str) else {
            return Value::Null;
        };
        if !dispatch.starts_with("__bd:") {
            return polyfill_expression(dispatch)
                .map(Value::String)
                .unwrap_or(Value::Null);
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
        let normalized_from = self
            .translator
            .canonical_guest_path(from_dir)
            .map(|path| normalize_module_resolve_context(&path))
            .unwrap_or_else(|| normalize_module_resolve_context(from_dir));
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
        let source = fs::read_to_string(host_path).ok()?;
        Some(
            if matches!(
                Path::new(path).extension().and_then(|ext| ext.to_str()),
                Some("js" | "mjs" | "cjs")
            ) {
                strip_javascript_hashbang(&source)
            } else {
                source
            },
        )
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
        let mut scanned_pnpm_roots = HashSet::new();
        loop {
            let candidate_dirs = node_modules_candidate_dirs(&dir, package_name);
            for package_dir in candidate_dirs {
                if let Some(entry) =
                    self.resolve_package_entry_from_dir(&package_dir, subpath, mode)
                {
                    return Some(entry);
                }
            }
            for node_modules_root in node_modules_search_roots(&dir) {
                if scanned_pnpm_roots.insert(node_modules_root.clone()) {
                    if let Some(entry) = self.resolve_pnpm_virtual_store_entry(
                        &node_modules_root,
                        package_name,
                        subpath,
                        mode,
                    ) {
                        return Some(entry);
                    }
                }
            }
            if dir == "/" {
                break;
            }
            dir = dirname_guest_path(&dir);
        }

        ["/root/node_modules", "/node_modules"]
            .into_iter()
            .find_map(|root| {
                self.resolve_package_entry_from_dir(
                    &join_guest_path(root, package_name),
                    subpath,
                    mode,
                )
            })
            .or_else(|| {
                ["/root/node_modules", "/node_modules"]
                    .into_iter()
                    .find_map(|root| {
                        self.resolve_pnpm_virtual_store_entry(root, package_name, subpath, mode)
                    })
            })
    }

    fn resolve_pnpm_virtual_store_entry(
        &mut self,
        node_modules_root: &str,
        package_name: &str,
        subpath: &str,
        mode: ModuleResolveMode,
    ) -> Option<String> {
        let store_root = join_guest_path(node_modules_root, ".pnpm");
        let host_store_root = self.translator.guest_to_host(&store_root)?;
        let mut entries = fs::read_dir(host_store_root)
            .ok()?
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let file_type = entry.file_type().ok()?;
                file_type
                    .is_dir()
                    .then(|| entry.file_name().to_string_lossy().into_owned())
            })
            .collect::<Vec<_>>();
        entries.sort();
        for entry in entries {
            let package_dir =
                join_guest_path(&store_root, &format!("{entry}/node_modules/{package_name}"));
            if let Some(resolved) = self.resolve_package_entry_from_dir(&package_dir, subpath, mode)
            {
                return Some(resolved);
            }
        }
        None
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

impl LocalKernelStdinBridge {
    fn write(&self, chunk: &[u8]) {
        let mut state = self.state.lock().expect("kernel stdin state poisoned");
        state.bytes.extend(chunk.iter().copied());
        self.ready.notify_all();
    }

    fn close(&self) {
        let mut state = self.state.lock().expect("kernel stdin state poisoned");
        state.closed = true;
        self.ready.notify_all();
    }

    fn read(&self, args: &[Value]) -> Value {
        let max_bytes = args
            .first()
            .and_then(Value::as_u64)
            .map(|value| value.clamp(1, 64 * 1024) as usize)
            .unwrap_or(64 * 1024);
        let timeout = Duration::from_millis(args.get(1).and_then(Value::as_u64).unwrap_or(100));
        let deadline = Instant::now() + timeout;
        let mut state = self.state.lock().expect("kernel stdin state poisoned");

        while state.bytes.is_empty() && !state.closed {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Value::Null;
            }
            let (next_state, wait_result) = self
                .ready
                .wait_timeout(state, remaining)
                .expect("kernel stdin wait poisoned");
            state = next_state;
            if wait_result.timed_out() && state.bytes.is_empty() && !state.closed {
                return Value::Null;
            }
        }

        if !state.bytes.is_empty() {
            let read_len = state.bytes.len().min(max_bytes);
            let bytes = state.bytes.drain(..read_len).collect::<Vec<_>>();
            return json!({
                "dataBase64": v8_runtime::base64_encode_pub(&bytes),
            });
        }

        json!({
            "done": true,
        })
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

fn strip_javascript_hashbang(source: &str) -> String {
    if let Some(stripped) = source.strip_prefix("#!") {
        match stripped.find('\n') {
            Some(index) => format!("\n{}", &stripped[index + 1..]),
            None => String::new(),
        }
    } else {
        source.to_owned()
    }
}

fn parse_process_exit_code_message(message: &str) -> Option<i32> {
    let code = message.strip_prefix("process.exit(")?.strip_suffix(')')?;
    code.parse::<i32>().ok()
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
        "assert"
        | "async_hooks"
        | "buffer"
        | "child_process"
        | "cluster"
        | "console"
        | "constants"
        | "crypto"
        | "dgram"
        | "diagnostics_channel"
        | "dns"
        | "events"
        | "fs"
        | "fs/promises"
        | "http"
        | "http2"
        | "https"
        | "inspector"
        | "module"
        | "net"
        | "os"
        | "path"
        | "path/posix"
        | "path/win32"
        | "perf_hooks"
        | "process"
        | "punycode"
        | "querystring"
        | "readline"
        | "repl"
        | "sqlite"
        | "stream"
        | "stream/consumers"
        | "stream/promises"
        | "stream/web"
        | "string_decoder"
        | "sys"
        | "timers"
        | "tls"
        | "timers/promises"
        | "trace_events"
        | "tty"
        | "url"
        | "util"
        | "util/types"
        | "domain"
        | "vm"
        | "v8"
        | "wasi"
        | "worker_threads"
        | "zlib" => Some(format!("node:{bare}")),
        _ => None,
    }
}

fn is_builtin_specifier(specifier: &str) -> bool {
    normalize_builtin_specifier(specifier).is_some()
}

fn polyfill_expression(request: &str) -> Option<String> {
    let normalized = request.trim_start_matches("node:");
    let entry = polyfill_registry()
        .groups
        .iter()
        .find(|group| group.names.iter().any(|name| name == normalized))?;

    Some(match entry.source {
        PolyfillSourceKind::NodeStdlibBrowser | PolyfillSourceKind::CustomBridge => format!(
            "globalThis._requireFrom({}, \"/\")",
            serde_json::to_string(&format!("node:{normalized}"))
                .unwrap_or_else(|_| format!("\"node:{normalized}\""))
        ),
        PolyfillSourceKind::Denied => {
            let error_code = entry.error_code.as_deref().unwrap_or("ERR_ACCESS_DENIED");
            format!(
                "(() => {{ const error = new Error({message}); error.code = {code}; throw error; }})()",
                message = serde_json::to_string(&format!(
                    "node:{normalized} is not available in the Agent OS guest runtime"
                ))
                .unwrap_or_else(|_| format!("\"node:{normalized} is not available in the Agent OS guest runtime\"")),
                code = serde_json::to_string(error_code).unwrap_or_else(|_| "\"ERR_ACCESS_DENIED\"".to_owned())
            )
        }
    })
}

fn build_builtin_module_wrapper(module_name: &str) -> String {
    if module_name == "assert" {
        return String::from(
            r#"class AssertionError extends Error {
  constructor(message = "Assertion failed") {
    super(message);
    this.name = "AssertionError";
  }
}

function fail(message) {
  throw new AssertionError(message);
}

function ok(value, message) {
  if (!value) fail(message);
}

function equal(actual, expected, message) {
  if (actual != expected) fail(message ?? `Expected ${actual} == ${expected}`);
}

function notEqual(actual, expected, message) {
  if (actual == expected) fail(message ?? `Expected ${actual} != ${expected}`);
}

function strictEqual(actual, expected, message) {
  if (actual !== expected) fail(message ?? `Expected ${actual} === ${expected}`);
}

function notStrictEqual(actual, expected, message) {
  if (actual === expected) fail(message ?? `Expected ${actual} !== ${expected}`);
}

function serialize(value) {
  return JSON.stringify(value);
}

function deepEqual(actual, expected, message) {
  if (serialize(actual) !== serialize(expected)) {
    fail(message ?? "Expected values to be deeply equal");
  }
}

function deepStrictEqual(actual, expected, message) {
  return deepEqual(actual, expected, message);
}

function match(actual, expected, message) {
  if (!(expected instanceof RegExp) || !expected.test(String(actual))) {
    fail(message ?? `Expected ${actual} to match ${expected}`);
  }
}

function matchesExpectedError(error, expected) {
  if (expected == null) return true;
  if (expected instanceof RegExp) {
    return expected.test(String(error?.message ?? error));
  }
  if (typeof expected === "function") {
    if (error instanceof expected) return true;
    return expected(error) === true;
  }
  if (typeof expected === "object") {
    return Object.entries(expected).every(([key, value]) => serialize(error?.[key]) === serialize(value));
  }
  return false;
}

function throws(fn, expected, message) {
  if (typeof fn !== "function") {
    fail(message ?? "assert.throws requires a function");
  }

  try {
    fn();
  } catch (error) {
    if (!matchesExpectedError(error, expected)) {
      throw error;
    }
    return error;
  }

  fail(message ?? "Missing expected exception");
}

async function rejects(promiseOrFn, expected, message) {
  let promise;
  if (typeof promiseOrFn === "function") {
    promise = promiseOrFn();
  } else {
    promise = promiseOrFn;
  }

  try {
    await promise;
  } catch (error) {
    if (!matchesExpectedError(error, expected)) {
      throw error;
    }
    return error;
  }

  fail(message ?? "Missing expected rejection");
}

function ifError(error) {
  if (error != null) {
    throw error;
  }
}

function assert(value, message) {
  ok(value, message);
}

Object.assign(assert, {
  AssertionError,
  deepEqual,
  deepStrictEqual,
  equal,
  fail,
  ifError,
  match,
  notEqual,
  notStrictEqual,
  ok,
  rejects,
  strict: assert,
  strictEqual,
  throws,
});

export {
  AssertionError,
  assert as default,
  deepEqual,
  deepStrictEqual,
  equal,
  fail,
  ifError,
  match,
  notEqual,
  notStrictEqual,
  ok,
  rejects,
  assert as strict,
  strictEqual,
  throws,
};
"#,
        );
    }

    if module_name == "path" || module_name == "path/posix" || module_name == "path/win32" {
        return String::from(
            r#"const sep = "/";
const delimiter = ":";

function normalizeSegments(parts) {
  const output = [];
  for (const part of parts) {
    if (!part || part === ".") continue;
    if (part === "..") {
      if (output.length > 0) output.pop();
      continue;
    }
    output.push(part);
  }
  return output;
}

function isAbsolute(path) {
  return String(path || "").startsWith(sep);
}

function join(...parts) {
  const absolute = parts.some((part, index) => index === 0 && isAbsolute(part));
  const normalized = normalizeSegments(parts.flatMap((part) => String(part || "").split(sep)));
  const joined = normalized.join(sep);
  if (!joined) return absolute ? sep : ".";
  return absolute ? `${sep}${joined}` : joined;
}

function dirname(path) {
  const normalized = String(path || "");
  if (!normalized || normalized === sep) return sep;
  const parts = normalizeSegments(normalized.split(sep));
  if (parts.length <= 1) return isAbsolute(normalized) ? sep : ".";
  const dir = parts.slice(0, -1).join(sep);
  return isAbsolute(normalized) ? `${sep}${dir}` : dir;
}

function basename(path) {
  const normalized = normalizeSegments(String(path || "").split(sep));
  return normalized.length === 0 ? "" : normalized[normalized.length - 1];
}

function extname(path) {
  const base = basename(path);
  const index = base.lastIndexOf(".");
  if (index <= 0) return "";
  return base.slice(index);
}

function resolve(...parts) {
  const absoluteParts = [];
  for (let index = parts.length - 1; index >= 0; index -= 1) {
    const part = String(parts[index] || "");
    if (!part) continue;
    absoluteParts.unshift(part);
    if (isAbsolute(part)) break;
  }
  if (absoluteParts.length === 0 || !isAbsolute(absoluteParts[0])) {
    absoluteParts.unshift(typeof process?.cwd === "function" ? process.cwd() : sep);
  }
  return join(...absoluteParts);
}

function relative(from, to) {
  const fromResolved = resolve(from);
  const toResolved = resolve(to);
  if (fromResolved === toResolved) return "";

  const fromParts = normalizeSegments(fromResolved.split(sep));
  const toParts = normalizeSegments(toResolved.split(sep));
  let shared = 0;
  while (
    shared < fromParts.length &&
    shared < toParts.length &&
    fromParts[shared] === toParts[shared]
  ) {
    shared += 1;
  }

  const up = new Array(fromParts.length - shared).fill("..");
  const down = toParts.slice(shared);
  const result = [...up, ...down].join(sep);
  return result || ".";
}

function parse(path) {
  const root = isAbsolute(path) ? sep : "";
  const dir = dirname(path);
  const base = basename(path);
  const ext = extname(path);
  const name = ext ? base.slice(0, -ext.length) : base;
  return { root, dir, base, ext, name };
}

function format(pathObject = {}) {
  const dir = pathObject.dir || pathObject.root || "";
  const base =
    pathObject.base ||
    `${pathObject.name || ""}${pathObject.ext || ""}`;
  if (!dir) return base;
  if (!base) return dir;
  return dir.endsWith(sep) ? `${dir}${base}` : `${dir}${sep}${base}`;
}

function normalize(path) {
  return join(String(path || ""));
}

const pathModule = {
  basename,
  delimiter,
  dirname,
  extname,
  format,
  isAbsolute,
  join,
  normalize,
  parse,
  relative,
  resolve,
  sep,
};
const posix = pathModule;
const win32 = pathModule;

export { basename, delimiter, dirname, extname, format, isAbsolute, join, normalize, parse, posix, relative, resolve, sep, win32 };
export default pathModule;
"#,
        );
    }

    if module_name == "url" {
        return String::from(
            r#"const NativeURL = globalThis.URL;

function normalizeFilePath(value) {
  const path = String(value ?? "");
  if (path.length === 0) {
    return "/";
  }
  return path.startsWith("/") ? path : `/${path}`;
}

function encodeFilePath(path) {
  return path
    .split("/")
    .map((segment, index) =>
      index === 0
        ? ""
        : encodeURIComponent(segment).replace(/[!'()*]/g, (char) =>
            `%${char.charCodeAt(0).toString(16).toUpperCase()}`
          )
    )
    .join("/");
}

function buildFileUrlRecord(href, pathname) {
  const searchParams = new URLSearchParams();
  return {
    href,
    origin: "null",
    protocol: "file:",
    username: "",
    password: "",
    host: "",
    hostname: "",
    port: "",
    pathname,
    search: "",
    searchParams,
    hash: "",
    toString() {
      return href;
    },
    toJSON() {
      return href;
    },
    valueOf() {
      return href;
    },
    [Symbol.toPrimitive]() {
      return href;
    },
  };
}

function fileURLToPath(value) {
  const raw =
    typeof value === "string"
      ? value
      : value && typeof value.href === "string"
        ? value.href
        : String(value ?? "");
  if (raw.startsWith("/")) {
    return raw;
  }
  if (raw.startsWith("file:")) {
    let pathname = raw.startsWith("file://")
      ? raw.slice("file://".length)
      : raw.slice("file:".length);
    const terminatorIndex = pathname.search(/[?#]/);
    if (terminatorIndex >= 0) {
      pathname = pathname.slice(0, terminatorIndex);
    }
    if (!pathname.startsWith("/")) {
      const slashIndex = pathname.indexOf("/");
      if (slashIndex === -1) {
        return "/";
      }
      const host = pathname.slice(0, slashIndex);
      if (host && host !== "localhost") {
        throw new Error(`Expected file URL with an empty host, received ${host}`);
      }
      pathname = pathname.slice(slashIndex);
    }
    return decodeURIComponent(pathname || "/");
  }
  const url = value instanceof NativeURL ? value : new NativeURL(raw);
  if (url.protocol !== "file:") {
    throw new Error(`Expected file URL, received ${url.protocol}`);
  }
  return decodeURIComponent(url.pathname);
}

function pathToFileURL(path) {
  const absolute = normalizeFilePath(path);
  const pathname = encodeFilePath(absolute);
  const href = `file://${pathname}`;

  try {
    const url = new NativeURL("file:///");
    url.pathname = pathname;
    if (typeof url.href === "string" && url.href.startsWith("file:///")) {
      return url;
    }
  } catch {}

  return buildFileUrlRecord(href, pathname);
}

function parse(input, parseQueryString = false) {
  const parsed = new NativeURL(String(input ?? ""));
  const queryString = parsed.search.length > 0 ? parsed.search.slice(1) : null;
  const auth =
    parsed.username || parsed.password
      ? `${decodeURIComponent(parsed.username)}${parsed.password ? `:${decodeURIComponent(parsed.password)}` : ""}`
      : null;
  return {
    href: parsed.href,
    protocol: parsed.protocol,
    slashes: true,
    auth,
    host: parsed.host,
    port: parsed.port || null,
    hostname: parsed.hostname,
    hash: parsed.hash || null,
    search: parsed.search || null,
    query: parseQueryString ? Object.fromEntries(parsed.searchParams.entries()) : queryString,
    pathname: parsed.pathname,
    path: `${parsed.pathname}${parsed.search}`,
  };
}

function format(value) {
  if (value == null) return "";
  if (typeof value === "string") return value;
  if (typeof value.href === "string") return value.href;

  const protocol = typeof value.protocol === "string" ? value.protocol : "http:";
  const slashes = value.slashes === false ? "" : "//";
  const auth =
    typeof value.auth === "string" && value.auth.length > 0 ? `${value.auth}@` : "";
  const host =
    typeof value.host === "string" && value.host.length > 0
      ? value.host
      : `${value.hostname || ""}${value.port ? `:${value.port}` : ""}`;
  const pathname =
    typeof value.pathname === "string"
      ? value.pathname
      : typeof value.path === "string"
        ? value.path
        : "";

  let search = "";
  if (typeof value.search === "string") {
    search = value.search;
  } else if (typeof value.query === "string" && value.query.length > 0) {
    search = value.query.startsWith("?") ? value.query : `?${value.query}`;
  } else if (value.query && typeof value.query === "object") {
    const params = new URLSearchParams();
    for (const [key, entry] of Object.entries(value.query)) {
      if (Array.isArray(entry)) {
        for (const item of entry) {
          params.append(key, String(item));
        }
      } else if (entry != null) {
        params.append(key, String(entry));
      }
    }
    const encoded = params.toString();
    search = encoded ? `?${encoded}` : "";
  }

  const hash = typeof value.hash === "string" ? value.hash : "";
  return `${protocol}${slashes}${auth}${host}${pathname}${search}${hash}`;
}

export { NativeURL as URL, fileURLToPath, format, parse, pathToFileURL };
export default { URL: NativeURL, fileURLToPath, format, parse, pathToFileURL };
"#,
        );
    }

    if module_name == "readline" {
        return String::from(
            r#"class MiniEmitter {
  constructor() {
    this.listeners = new Map();
  }

  on(event, listener) {
    const listeners = this.listeners.get(event) ?? [];
    listeners.push(listener);
    this.listeners.set(event, listeners);
    return this;
  }

  addListener(event, listener) {
    return this.on(event, listener);
  }

  once(event, listener) {
    const wrapped = (...args) => {
      this.off(event, wrapped);
      listener(...args);
    };
    return this.on(event, wrapped);
  }

  off(event, listener) {
    const listeners = this.listeners.get(event) ?? [];
    this.listeners.set(
      event,
      listeners.filter((candidate) => candidate !== listener),
    );
    return this;
  }

  removeListener(event, listener) {
    return this.off(event, listener);
  }

  emit(event, ...args) {
    const listeners = this.listeners.get(event) ?? [];
    for (const listener of listeners) {
      listener(...args);
    }
    return listeners.length > 0;
  }
}

export function createInterface(options = {}) {
  const input = options.input ?? null;
  const output = options.output ?? null;
  const emitter = new MiniEmitter();
  let buffer = "";
  let closed = false;
  let ended = false;
  const queuedLines = [];
  let pendingResolve = null;

  const enqueueLine = (line) => {
    if (pendingResolve) {
      const resolve = pendingResolve;
      pendingResolve = null;
      resolve({ done: false, value: line });
      return;
    }
    queuedLines.push(line);
  };

  const flush = () => {
    if (buffer.length > 0) {
      emitter.emit("line", buffer);
      enqueueLine(buffer);
      buffer = "";
    }
  };

  const onData = (chunk) => {
    buffer += typeof chunk === "string" ? chunk : Buffer.from(chunk).toString("utf8");
    while (true) {
      const index = buffer.indexOf("\n");
      if (index < 0) break;
      const line = buffer.slice(0, index).replace(/\r$/, "");
      buffer = buffer.slice(index + 1);
      emitter.emit("line", line);
      enqueueLine(line);
    }
  };

  const onEnd = () => {
    if (ended) return;
    ended = true;
    flush();
    emitter.emit("close");
    if (pendingResolve) {
      const resolve = pendingResolve;
      pendingResolve = null;
      resolve({ done: true, value: void 0 });
    }
  };

  if (input && typeof input.on === "function") {
    input.on("data", onData);
    input.on("end", onEnd);
    input.on("close", onEnd);
  }

  emitter.close = () => {
    if (closed) return;
    closed = true;
    if (input && typeof input.off === "function") {
      input.off("data", onData);
      input.off("end", onEnd);
      input.off("close", onEnd);
    }
    flush();
    emitter.emit("close");
    if (pendingResolve) {
      const resolve = pendingResolve;
      pendingResolve = null;
      resolve({ done: true, value: void 0 });
    }
  };

  emitter.question = (prompt, callback) => {
    if (output && typeof output.write === "function" && prompt) {
      output.write(String(prompt));
    }
    if (typeof callback === "function") {
      callback("");
    }
  };

  emitter[Symbol.asyncIterator] = () => ({
    next() {
      if (queuedLines.length > 0) {
        return Promise.resolve({ done: false, value: queuedLines.shift() });
      }
      if (closed || ended) {
        return Promise.resolve({ done: true, value: void 0 });
      }
      return new Promise((resolve) => {
        pendingResolve = resolve;
      });
    },
    return() {
      emitter.close();
      return Promise.resolve({ done: true, value: void 0 });
    },
    [Symbol.asyncIterator]() {
      return this;
    },
  });

  return emitter;
}

export default { createInterface };
"#,
        );
    }

    if module_name == "stream" {
        return String::from(
            r#"class MiniEmitter {
  constructor() {
    this._listeners = new Map();
    this._onceListeners = new Map();
  }

  on(event, listener) {
    const listeners = this._listeners.get(event) ?? [];
    listeners.push(listener);
    this._listeners.set(event, listeners);
    return this;
  }

  once(event, listener) {
    const listeners = this._onceListeners.get(event) ?? [];
    listeners.push(listener);
    this._onceListeners.set(event, listeners);
    return this;
  }

  off(event, listener) {
    for (const map of [this._listeners, this._onceListeners]) {
      const listeners = map.get(event) ?? [];
      map.set(
        event,
        listeners.filter((candidate) => candidate !== listener),
      );
    }
    return this;
  }

  removeListener(event, listener) {
    return this.off(event, listener);
  }

  emit(event, ...args) {
    const persistent = [...(this._listeners.get(event) ?? [])];
    const once = [...(this._onceListeners.get(event) ?? [])];
    this._onceListeners.delete(event);
    for (const listener of persistent) {
      listener(...args);
    }
    for (const listener of once) {
      listener(...args);
    }
    return persistent.length + once.length > 0;
  }
}

function getCallback(encodingOrCallback, callback) {
  if (typeof encodingOrCallback === "function") return encodingOrCallback;
  if (typeof callback === "function") return callback;
  return null;
}

function queueResult(callback, error = null) {
  if (typeof callback !== "function") return;
  queueMicrotask(() => callback(error));
}

function createReadableAsyncIterator(stream) {
  const queuedChunks = [];
  let pendingResolve = null;
  let pendingReject = null;
  let done = stream?.readableEnded === true;
  let error = stream?.errored ?? null;

  const cleanup = () => {
    stream?.off?.("data", onData);
    stream?.off?.("end", onEnd);
    stream?.off?.("close", onEnd);
    stream?.off?.("error", onError);
  };

  const settlePending = (result) => {
    if (pendingResolve) {
      const resolve = pendingResolve;
      pendingResolve = null;
      pendingReject = null;
      resolve(result);
    }
  };

  const rejectPending = (reason) => {
    if (pendingReject) {
      const reject = pendingReject;
      pendingResolve = null;
      pendingReject = null;
      reject(reason);
    }
  };

  const onData = (chunk) => {
    if (pendingResolve) {
      settlePending({ done: false, value: chunk });
      return;
    }
    queuedChunks.push(chunk);
  };

  const onEnd = () => {
    if (done) return;
    done = true;
    cleanup();
    settlePending({ done: true, value: void 0 });
  };

  const onError = (reason) => {
    error = reason;
    done = true;
    cleanup();
    rejectPending(reason);
  };

  const pull = () => {
    if (done || typeof stream?._read !== "function") {
      return;
    }
    try {
      stream._read();
    } catch (reason) {
      stream.errored = reason;
      onError(reason);
    }
  };

  stream?.on?.("data", onData);
  stream?.on?.("end", onEnd);
  stream?.on?.("close", onEnd);
  stream?.on?.("error", onError);

  return {
    next() {
      if (error) {
        return Promise.reject(error);
      }
      if (queuedChunks.length > 0) {
        return Promise.resolve({ done: false, value: queuedChunks.shift() });
      }
      if (done) {
        return Promise.resolve({ done: true, value: void 0 });
      }
      pull();
      if (queuedChunks.length > 0) {
        return Promise.resolve({ done: false, value: queuedChunks.shift() });
      }
      if (done) {
        return Promise.resolve({ done: true, value: void 0 });
      }
      return new Promise((resolve, reject) => {
        pendingResolve = resolve;
        pendingReject = reject;
      });
    },
    return() {
      done = true;
      cleanup();
      stream?.destroy?.();
      return Promise.resolve({ done: true, value: void 0 });
    },
    [Symbol.asyncIterator]() {
      return this;
    },
  };
}

class Stream extends MiniEmitter {
  pipe(destination) {
    this.on("data", (chunk) => destination.write(chunk));
    this.once("end", () => destination.end());
    return destination;
  }

  destroy(error) {
    if (this.destroyed) return this;
    this.destroyed = true;
    if (error) {
      this.errored = error;
      queueMicrotask(() => this.emit("error", error));
    }
    queueMicrotask(() => this.emit("close"));
    return this;
  }
}

class Readable extends Stream {
  constructor() {
    super();
    this.readable = true;
    this.readableEnded = false;
    this.destroyed = false;
  }

  push(chunk) {
    if (chunk === null) {
      if (!this.readableEnded) {
        this.readableEnded = true;
        queueMicrotask(() => {
          this.emit("end");
          this.emit("close");
        });
      }
      return false;
    }
    this.emit("data", Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk ?? []));
    return true;
  }

  static fromWeb(stream) {
    if (!stream || typeof stream.getReader !== "function") {
      throw new TypeError("Readable.fromWeb expects a WHATWG ReadableStream");
    }
    return {
      async *[Symbol.asyncIterator]() {
        const reader = stream.getReader();
        try {
          while (true) {
            const { value, done } = await reader.read();
            if (done) break;
            yield Buffer.from(value ?? []);
          }
        } finally {
          reader.releaseLock?.();
        }
      },
    };
  }

  [Symbol.asyncIterator]() {
    return createReadableAsyncIterator(this);
  }
}

class Writable extends Stream {
  constructor() {
    super();
    this.writable = true;
    this.writableEnded = false;
    this.destroyed = false;
  }

  write(chunk, encodingOrCallback, callback) {
    if (this.writableEnded) {
      const error = new Error("write after end");
      queueResult(getCallback(encodingOrCallback, callback), error);
      this.emit("error", error);
      return false;
    }
    const done = getCallback(encodingOrCallback, callback);
    this._write(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk ?? []), done);
    return true;
  }

  _write(_chunk, callback) {
    queueResult(callback);
  }

  end(chunk, encodingOrCallback, callback) {
    if (chunk !== undefined && chunk !== null) {
      this.write(chunk, encodingOrCallback);
    }
    if (this.writableEnded) {
      queueResult(getCallback(encodingOrCallback, callback));
      return this;
    }
    this.writableEnded = true;
    const done = getCallback(encodingOrCallback, callback);
    queueMicrotask(() => {
      queueResult(done);
      this.emit("finish");
      this.emit("close");
    });
    return this;
  }
}

class Duplex extends Readable {
  constructor() {
    super();
    this.writable = true;
    this.writableEnded = false;
  }

  write(chunk, encodingOrCallback, callback) {
    return Writable.prototype.write.call(this, chunk, encodingOrCallback, callback);
  }

  _write(chunk, callback) {
    queueResult(callback);
  }

  end(chunk, encodingOrCallback, callback) {
    return Writable.prototype.end.call(this, chunk, encodingOrCallback, callback);
  }
}

class Transform extends Duplex {
  _write(chunk, callback) {
    try {
      this._transform(chunk, "buffer", (error, output) => {
        if (!error && output !== undefined && output !== null) {
          this.push(output);
        }
        queueResult(callback, error ?? null);
      });
    } catch (error) {
      queueResult(callback, error);
      this.emit("error", error);
    }
  }

  _transform(chunk, _encoding, callback) {
    callback(null, chunk);
  }

  end(chunk, encodingOrCallback, callback) {
    Writable.prototype.end.call(this, chunk, encodingOrCallback, callback);
    this.push(null);
    return this;
  }
}

class PassThrough extends Transform {}

function finished(stream, callback) {
  const done = (error = null) => {
    cleanup();
    if (typeof callback === "function") callback(error);
  };
  const onFinish = () => done();
  const onEnd = () => done();
  const onClose = () => done();
  const onError = (error) => done(error);
  const cleanup = () => {
    stream?.off?.("finish", onFinish);
    stream?.off?.("end", onEnd);
    stream?.off?.("close", onClose);
    stream?.off?.("error", onError);
  };
  stream?.once?.("finish", onFinish);
  stream?.once?.("end", onEnd);
  stream?.once?.("close", onClose);
  stream?.once?.("error", onError);
  return cleanup;
}

function pipeline(...streams) {
  const callback =
    streams.length > 0 && typeof streams[streams.length - 1] === "function"
      ? streams.pop()
      : null;
  if (streams.length < 2) {
    const error = new TypeError("pipeline requires at least two streams");
    callback?.(error);
    throw error;
  }
  for (let index = 0; index < streams.length - 1; index += 1) {
    streams[index].pipe(streams[index + 1]);
  }
  if (callback) {
    finished(streams[streams.length - 1], callback);
  }
  return streams[streams.length - 1];
}

function compose(...streams) {
  return pipeline(...streams);
}

function addAbortSignal(signal, stream) {
  if (signal?.aborted) {
    stream?.destroy?.(signal.reason);
    return stream;
  }
  signal?.addEventListener?.("abort", () => stream?.destroy?.(signal.reason), {
    once: true,
  });
  return stream;
}

function isReadable(stream) {
  return Boolean(stream && stream.readable && !stream.destroyed);
}

function isWritable(stream) {
  return Boolean(stream && stream.writable && !stream.destroyed);
}

function isErrored(stream) {
  return Boolean(stream && stream.errored);
}

function isDisturbed(stream) {
  return Boolean(
    stream && (stream.disturbed === true || stream.locked || stream.readableDidRead === true),
  );
}

const streamModule = {
  Duplex,
  PassThrough,
  Readable,
  Stream,
  Transform,
  Writable,
  addAbortSignal,
  compose,
  finished,
  isDisturbed,
  isErrored,
  isReadable,
  isWritable,
  pipeline,
};

export {
  Duplex,
  PassThrough,
  Readable,
  Stream,
  Transform,
  Writable,
  addAbortSignal,
  compose,
  finished,
  isDisturbed,
  isErrored,
  isReadable,
  isWritable,
  pipeline,
};
export default streamModule;
"#,
        );
    }

    if module_name == "stream/promises" {
        return String::from(
            r#"const _m = globalThis._requireFrom("node:stream/promises", "/");

export default _m;
export const finished = _m.finished;
export const pipeline = _m.pipeline;
"#,
        );
    }

    if module_name == "zlib" {
        return String::from(
            r#"const _m = globalThis._requireFrom("node:zlib", "/");
const zlibConstants =
  typeof _m.constants === "object" && _m.constants !== null
    ? _m.constants
    : Object.fromEntries(
        Object.entries(_m).filter(
          ([key, value]) => /^[A-Z0-9_]+$/.test(key) && typeof value === "number",
        ),
      );

if (typeof _m.constants === "undefined") {
  Object.defineProperty(_m, "constants", {
    configurable: true,
    enumerable: true,
    value: zlibConstants,
    writable: true,
  });
}

export default _m;
export const constants = _m.constants;
export const createDeflate = _m.createDeflate;
export const createInflate = _m.createInflate;
export const deflateSync = _m.deflateSync;
export const inflateSync = _m.inflateSync;
"#,
        );
    }

    if module_name == "stream/web" {
        return String::from(
            r#"export const ReadableStream = globalThis.ReadableStream;
export const WritableStream = globalThis.WritableStream;
export const TransformStream = globalThis.TransformStream;
export const TextEncoderStream = globalThis.TextEncoderStream;
export const TextDecoderStream = globalThis.TextDecoderStream;
export const CompressionStream = globalThis.CompressionStream;
export const DecompressionStream = globalThis.DecompressionStream;
export default {
  ReadableStream,
  WritableStream,
  TransformStream,
  TextEncoderStream,
  TextDecoderStream,
  CompressionStream,
  DecompressionStream,
};
"#,
        );
    }

    if module_name == "fs/promises" {
        return String::from(
            r#"const fsModule = globalThis._requireFrom("node:fs", "/");
const _m = fsModule.promises;

export default _m;
export const constants = fsModule.constants;
export const FileHandle = _m.FileHandle;
export const access = _m.access;
export const appendFile = _m.appendFile;
export const chmod = _m.chmod;
export const chown = _m.chown;
export const copyFile = _m.copyFile;
export const cp = _m.cp;
export const lchmod = _m.lchmod;
export const lchown = _m.lchown;
export const link = _m.link;
export const lstat = _m.lstat;
export const lutimes = _m.lutimes;
export const mkdir = _m.mkdir;
export const mkdtemp = _m.mkdtemp;
export const open = _m.open;
export const opendir = _m.opendir;
export const readFile = _m.readFile;
export const readdir = _m.readdir;
export const readlink = _m.readlink;
export const realpath = _m.realpath;
export const rename = _m.rename;
export const rm = _m.rm;
export const rmdir = _m.rmdir;
export const stat = _m.stat;
export const statfs = _m.statfs;
export const symlink = _m.symlink;
export const truncate = _m.truncate;
export const unlink = _m.unlink;
export const utimes = _m.utimes;
export const watch = _m.watch;
export const writeFile = _m.writeFile;
"#,
        );
    }

    if module_name == "readline" {
        return String::from(
            r#"const _m = globalThis._requireFrom("node:readline", "/");

function createInterface(...args) {
  const interfaceValue = _m.createInterface(...args);
  if (
    interfaceValue &&
    typeof interfaceValue === "object" &&
    typeof interfaceValue[Symbol.asyncIterator] !== "function"
  ) {
    const originalOn = typeof interfaceValue.on === "function"
      ? interfaceValue.on.bind(interfaceValue)
      : null;
    const originalOff = typeof interfaceValue.off === "function"
      ? interfaceValue.off.bind(interfaceValue)
      : typeof interfaceValue.removeListener === "function"
        ? interfaceValue.removeListener.bind(interfaceValue)
        : null;
    const originalClose = typeof interfaceValue.close === "function"
      ? interfaceValue.close.bind(interfaceValue)
      : null;
    const queued = [];
    let pendingResolve = null;
    let done = false;
    const enqueue = (line) => {
      if (pendingResolve) {
        const resolve = pendingResolve;
        pendingResolve = null;
        resolve({ done: false, value: line });
        return;
      }
      queued.push(line);
    };
    const finish = () => {
      if (done) {
        return;
      }
      done = true;
      if (pendingResolve) {
        const resolve = pendingResolve;
        pendingResolve = null;
        resolve({ done: true, value: void 0 });
      }
    };
    originalOn?.("line", enqueue);
    originalOn?.("close", finish);
    interfaceValue[Symbol.asyncIterator] = () => ({
      next() {
        if (queued.length > 0) {
          return Promise.resolve({ done: false, value: queued.shift() });
        }
        if (done) {
          return Promise.resolve({ done: true, value: void 0 });
        }
        return new Promise((resolve) => {
          pendingResolve = resolve;
        });
      },
      return() {
        originalOff?.("line", enqueue);
        originalOff?.("close", finish);
        originalClose?.();
        finish();
        return Promise.resolve({ done: true, value: void 0 });
      },
      [Symbol.asyncIterator]() {
        return this;
      },
    });
  }
  return interfaceValue;
}

export default _m;
export { createInterface };
"#,
        );
    }

    if module_name == "string_decoder" {
        return String::from(
            r#"class StringDecoder {
  constructor(encoding = "utf8") {
    this.encoding = encoding;
    this.decoder = new TextDecoder(encoding, { fatal: false });
  }

  write(input) {
    const buffer =
      typeof input === "string"
        ? Buffer.from(input, this.encoding)
        : Buffer.isBuffer(input)
          ? input
          : Buffer.from(input ?? []);
    return this.decoder.decode(buffer, { stream: true });
  }

  end(input) {
    let output = "";
    if (input !== undefined) {
      output += this.write(input);
    }
    output += this.decoder.decode();
    return output;
  }
}

export { StringDecoder };
export default { StringDecoder };
"#,
        );
    }

    if module_name == "v8" {
        return String::from(
            r#"function serialize(value) {
  return Buffer.from(JSON.stringify(value ?? null), "utf8");
}

function deserialize(value) {
  const buffer = Buffer.isBuffer(value) ? value : Buffer.from(value ?? []);
  return JSON.parse(buffer.toString("utf8"));
}

class Serializer {
  constructor() {
    this._value = null;
  }

  writeHeader() {}

  writeValue(value) {
    this._value = value;
  }

  releaseBuffer() {
    return serialize(this._value);
  }

  transferArrayBuffer() {}
}

class Deserializer {
  constructor(buffer) {
    this._buffer = buffer;
  }

  readHeader() {}

  readValue() {
    return deserialize(this._buffer);
  }

  transferArrayBuffer() {}
}

function cachedDataVersionTag() {
  return 0;
}

function getCppHeapStatistics() {
  return {
    committed_size_bytes: 0,
    resident_size_bytes: 0,
    used_size_bytes: 0,
    space_statistics: [],
  };
}

function getHeapCodeStatistics() {
  return {
    code_and_metadata_size: 0,
    bytecode_and_metadata_size: 0,
    external_script_source_size: 0,
    cpu_profiler_metadata_size: 0,
  };
}

function configuredHeapLimitBytes() {
  const configured = Number(globalThis.__agentOsV8HeapLimitBytes);
  if (!Number.isFinite(configured) || configured <= 0) {
    return 0;
  }
  return configured;
}

function getHeapStatistics() {
  const heapLimit = configuredHeapLimitBytes();
  return {
    total_heap_size: 0,
    total_heap_size_executable: 0,
    total_physical_size: 0,
    total_available_size: 0,
    used_heap_size: 0,
    heap_size_limit: heapLimit,
    malloced_memory: 0,
    peak_malloced_memory: 0,
    does_zap_garbage: 0,
    number_of_native_contexts: 0,
    number_of_detached_contexts: 0,
    total_global_handles_size: 0,
    used_global_handles_size: 0,
    external_memory: 0,
  };
}

function getHeapSpaceStatistics() {
  return [];
}

function getHeapSnapshot() {
  return Readable.fromWeb(
    new ReadableStream({
      start(controller) {
        controller.enqueue(Buffer.from("{}"));
        controller.close();
      },
    }),
  );
}

function isStringOneByteRepresentation(value) {
  return typeof value === "string" && !/[^\x00-\xff]/.test(value);
}

function queryObjects() {
  return [];
}

function setFlagsFromString() {}

function setHeapSnapshotNearHeapLimit() {
  return [];
}

function startCpuProfile() {
  return {
    stop() {
      return {};
    },
  };
}

function stopCoverage() {
  return [];
}

function takeCoverage() {
  return [];
}

function writeHeapSnapshot() {
  return "";
}

class GCProfiler {
  start() {}

  stop() {
    return [];
  }
}

const promiseHooks = {};
const startupSnapshot = {};

export {
  GCProfiler,
  cachedDataVersionTag,
  Deserializer,
  deserialize,
  getCppHeapStatistics,
  getHeapCodeStatistics,
  getHeapSnapshot,
  getHeapSpaceStatistics,
  getHeapStatistics,
  isStringOneByteRepresentation,
  promiseHooks,
  queryObjects,
  serialize,
  Serializer,
  setFlagsFromString,
  setHeapSnapshotNearHeapLimit,
  startCpuProfile,
  startupSnapshot,
  stopCoverage,
  takeCoverage,
  writeHeapSnapshot,
};
export {
  Deserializer as DefaultDeserializer,
  Serializer as DefaultSerializer,
};
export default {
  GCProfiler,
  cachedDataVersionTag,
  DefaultDeserializer: Deserializer,
  DefaultSerializer: Serializer,
  Deserializer,
  deserialize,
  getCppHeapStatistics,
  getHeapCodeStatistics,
  getHeapSnapshot,
  getHeapSpaceStatistics,
  getHeapStatistics,
  isStringOneByteRepresentation,
  promiseHooks,
  queryObjects,
  serialize,
  Serializer,
  setFlagsFromString,
  setHeapSnapshotNearHeapLimit,
  startCpuProfile,
  startupSnapshot,
  stopCoverage,
  takeCoverage,
  writeHeapSnapshot,
};
"#,
        );
    }

    if module_name == "vm" {
        return String::from(
            r#"function runInThisContext(code) {
  return (0, eval)(String(code));
}

function createContext(context = {}) {
  return context;
}

function isContext(context) {
  return Boolean(context) && typeof context === "object";
}

function runInNewContext(code, context = {}) {
  const params = Object.keys(context);
  const values = Object.values(context);
  return Function(...params, `return (${String(code)});`)(...values);
}

class Script {
  constructor(code) {
    this.code = String(code);
  }

  runInThisContext() {
    return runInThisContext(this.code);
  }

  runInNewContext(context = {}) {
    return runInNewContext(this.code, context);
  }
}

export { Script, createContext, isContext, runInNewContext, runInThisContext };
export default { Script, createContext, isContext, runInNewContext, runInThisContext };
"#,
        );
    }

    if module_name == "worker_threads" {
        return String::from(
            r#"function createNotImplementedError(feature) {
  const error = new Error(`node:worker_threads ${feature} is not available in the Agent OS guest runtime`);
  error.code = "ERR_NOT_IMPLEMENTED";
  return error;
}

class MessagePort {
  postMessage() {}
  start() {}
  close() {}
  unref() {
    return this;
  }
  ref() {
    return this;
  }
}

class MessageChannel {
  constructor() {
    this.port1 = new MessagePort();
    this.port2 = new MessagePort();
  }
}

class Worker {
  constructor() {
    throw createNotImplementedError("Worker");
  }
}

function getEnvironmentData() {
  return undefined;
}

function markAsUncloneable() {}

function markAsUntransferable() {}

function moveMessagePortToContext() {
  throw createNotImplementedError("moveMessagePortToContext");
}

function postMessageToThread() {
  throw createNotImplementedError("postMessageToThread");
}

function receiveMessageOnPort() {
  return undefined;
}

function setEnvironmentData() {}

export const BroadcastChannel = globalThis.BroadcastChannel;
export { MessageChannel, MessagePort, Worker, getEnvironmentData, markAsUncloneable, markAsUntransferable, moveMessagePortToContext, postMessageToThread, receiveMessageOnPort, setEnvironmentData };
export const SHARE_ENV = Symbol.for("agent-os.worker_threads.SHARE_ENV");
export const isMainThread = true;
export const parentPort = null;
export const resourceLimits = {};
export const threadId = 0;
export const workerData = null;
export default {
  BroadcastChannel: globalThis.BroadcastChannel,
  MessageChannel,
  MessagePort,
  SHARE_ENV,
  Worker,
  getEnvironmentData,
  isMainThread,
  markAsUncloneable,
  markAsUntransferable,
  moveMessagePortToContext,
  parentPort,
  postMessageToThread,
  receiveMessageOnPort,
  resourceLimits,
  setEnvironmentData,
  threadId,
  workerData,
};
"#,
        );
    }

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
        "async_hooks" => &[
            "AsyncLocalStorage",
            "AsyncResource",
            "createHook",
            "executionAsyncId",
            "triggerAsyncId",
        ],
        "buffer" => &[
            "Blob",
            "Buffer",
            "File",
            "INSPECT_MAX_BYTES",
            "SlowBuffer",
            "isUtf8",
        ],
        "child_process" => &[
            "ChildProcess",
            "exec",
            "execFile",
            "execFileSync",
            "execSync",
            "fork",
            "spawn",
            "spawnSync",
        ],
        "console" => &[
            "Console",
            "assert",
            "clear",
            "context",
            "count",
            "countReset",
            "createTask",
            "debug",
            "dir",
            "dirxml",
            "error",
            "group",
            "groupCollapsed",
            "groupEnd",
            "info",
            "log",
            "profile",
            "profileEnd",
            "table",
            "time",
            "timeEnd",
            "timeLog",
            "timeStamp",
            "trace",
            "warn",
        ],
        "crypto" => &[
            "createHash",
            "createPrivateKey",
            "getHashes",
            "getRandomValues",
            "randomBytes",
            "randomUUID",
            "subtle",
        ],
        "diagnostics_channel" => &["channel", "hasSubscribers", "subscribe", "unsubscribe"],
        "events" => &[
            "EventEmitter",
            "addAbortListener",
            "defaultMaxListeners",
            "errorMonitor",
            "getEventListeners",
            "getMaxListeners",
            "on",
            "once",
            "setMaxListeners",
        ],
        "dns" => &["lookup", "promises", "resolve", "resolve4", "resolve6"],
        "fs" => &[
            "access",
            "accessSync",
            "appendFile",
            "appendFileSync",
            "chmod",
            "chmodSync",
            "closeSync",
            "constants",
            "createReadStream",
            "createWriteStream",
            "existsSync",
            "fstat",
            "fstatSync",
            "fsyncSync",
            "lstat",
            "lstatSync",
            "mkdir",
            "mkdirSync",
            "openSync",
            "readFile",
            "promises",
            "readFileSync",
            "readdir",
            "readSync",
            "readdirSync",
            "readlink",
            "realpathSync",
            "rename",
            "readlinkSync",
            "renameSync",
            "rm",
            "rmSync",
            "stat",
            "statSync",
            "unlink",
            "unlinkSync",
            "watch",
            "watchFile",
            "unwatchFile",
            "writeFile",
            "writeFileSync",
            "writeSync",
        ],
        "fs/promises" => &[
            "access",
            "appendFile",
            "chmod",
            "chown",
            "constants",
            "copyFile",
            "cp",
            "glob",
            "lchown",
            "link",
            "lstat",
            "mkdir",
            "mkdtemp",
            "open",
            "opendir",
            "readFile",
            "readdir",
            "readlink",
            "realpath",
            "rename",
            "rm",
            "rmdir",
            "stat",
            "statfs",
            "symlink",
            "truncate",
            "unlink",
            "utimes",
            "writeFile",
        ],
        "http" => &[
            "Agent",
            "METHODS",
            "STATUS_CODES",
            "createServer",
            "request",
        ],
        "http2" => &["connect", "createServer", "createSecureServer"],
        "https" => &["Agent", "createServer", "request"],
        "module" => &[
            "Module",
            "_cache",
            "_extensions",
            "_resolveFilename",
            "builtinModules",
            "createRequire",
            "findSourceMap",
            "isBuiltin",
            "syncBuiltinESMExports",
            "wrap",
        ],
        "net" => &[
            "BlockList",
            "Socket",
            "SocketAddress",
            "Server",
            "Stream",
            "connect",
            "createConnection",
            "createServer",
            "getDefaultAutoSelectFamily",
            "getDefaultAutoSelectFamilyAttemptTimeout",
            "isIP",
            "isIPv4",
            "isIPv6",
            "setDefaultAutoSelectFamily",
            "setDefaultAutoSelectFamilyAttemptTimeout",
        ],
        "os" => &[
            "EOL",
            "arch",
            "availableParallelism",
            "constants",
            "cpus",
            "endianness",
            "freemem",
            "homedir",
            "hostname",
            "networkInterfaces",
            "platform",
            "release",
            "totalmem",
            "tmpdir",
            "type",
            "userInfo",
            "version",
        ],
        "path" | "path/posix" | "path/win32" => &[
            "basename",
            "delimiter",
            "dirname",
            "extname",
            "format",
            "isAbsolute",
            "join",
            "normalize",
            "parse",
            "posix",
            "relative",
            "resolve",
            "sep",
            "win32",
        ],
        "process" => &[
            "arch", "argv", "argv0", "cwd", "env", "execPath", "exit", "pid", "platform", "ppid",
            "stderr", "stdin", "stdout", "umask", "version", "versions",
        ],
        "perf_hooks" => &[
            "PerformanceObserver",
            "constants",
            "createHistogram",
            "performance",
        ],
        "readline" => &["createInterface"],
        "sqlite" => &["DatabaseSync", "StatementSync", "constants"],
        "stream" => &[
            "Duplex",
            "PassThrough",
            "Readable",
            "Stream",
            "Transform",
            "Writable",
            "addAbortSignal",
            "compose",
            "finished",
            "isDisturbed",
            "isErrored",
            "isReadable",
            "pipeline",
        ],
        "stream/consumers" => &["arrayBuffer", "blob", "buffer", "json", "text"],
        "sys" => &[
            "MIMEType",
            "MIMEParams",
            "TextDecoder",
            "TextEncoder",
            "callbackify",
            "debug",
            "debuglog",
            "deprecate",
            "format",
            "inherits",
            "inspect",
            "parseArgs",
            "promisify",
            "stripVTControlCharacters",
            "types",
        ],
        "timers" => &[
            "clearImmediate",
            "clearInterval",
            "clearTimeout",
            "setImmediate",
            "setInterval",
            "setTimeout",
        ],
        "tty" => &["ReadStream", "WriteStream", "isatty"],
        "tls" => &[
            "TLSSocket",
            "Server",
            "connect",
            "createSecureContext",
            "createServer",
            "getCiphers",
        ],
        "stream/promises" => &["finished", "pipeline"],
        "timers/promises" => &["scheduler", "setImmediate", "setInterval", "setTimeout"],
        "url" => &["URL", "fileURLToPath", "format", "parse", "pathToFileURL"],
        "util" => &[
            "MIMEType",
            "MIMEParams",
            "TextDecoder",
            "TextEncoder",
            "callbackify",
            "debug",
            "debuglog",
            "deprecate",
            "format",
            "inherits",
            "inspect",
            "isDeepStrictEqual",
            "parseArgs",
            "promisify",
            "stripVTControlCharacters",
            "types",
        ],
        "util/types" => &[
            "isAnyArrayBuffer",
            "isArgumentsObject",
            "isArrayBuffer",
            "isArrayBufferView",
            "isAsyncFunction",
            "isBigInt64Array",
            "isBigIntObject",
            "isBigUint64Array",
            "isBooleanObject",
            "isBoxedPrimitive",
            "isCryptoKey",
            "isDataView",
            "isDate",
            "isExternal",
            "isFloat16Array",
            "isFloat32Array",
            "isFloat64Array",
            "isGeneratorFunction",
            "isGeneratorObject",
            "isInt16Array",
            "isInt32Array",
            "isInt8Array",
            "isKeyObject",
            "isMap",
            "isMapIterator",
            "isModuleNamespaceObject",
            "isNativeError",
            "isNumberObject",
            "isPromise",
            "isProxy",
            "isRegExp",
            "isSet",
            "isSetIterator",
            "isSharedArrayBuffer",
            "isStringObject",
            "isSymbolObject",
            "isTypedArray",
            "isUint16Array",
            "isUint32Array",
            "isUint8Array",
            "isUint8ClampedArray",
            "isWeakMap",
            "isWeakSet",
        ],
        "vm" => &[
            "Script",
            "createContext",
            "isContext",
            "runInNewContext",
            "runInThisContext",
        ],
        "v8" => &[
            "cachedDataVersionTag",
            "DefaultDeserializer",
            "DefaultSerializer",
            "Deserializer",
            "GCProfiler",
            "Serializer",
            "deserialize",
            "getCppHeapStatistics",
            "getHeapCodeStatistics",
            "getHeapSnapshot",
            "getHeapSpaceStatistics",
            "getHeapStatistics",
            "isStringOneByteRepresentation",
            "promiseHooks",
            "queryObjects",
            "serialize",
            "setFlagsFromString",
            "setHeapSnapshotNearHeapLimit",
            "startCpuProfile",
            "startupSnapshot",
            "stopCoverage",
            "takeCoverage",
            "writeHeapSnapshot",
        ],
        "worker_threads" => &[
            "MessageChannel",
            "MessagePort",
            "Worker",
            "isMainThread",
            "parentPort",
            "workerData",
        ],
        "zlib" => &[
            "constants",
            "createDeflate",
            "createInflate",
            "deflateSync",
            "inflateSync",
        ],
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

fn node_modules_search_roots(dir: &str) -> Vec<String> {
    let mut roots = HashSet::new();
    roots.insert(join_guest_path(dir, "node_modules"));
    if dir == "/node_modules" || dir.ends_with("/node_modules") {
        roots.insert(normalize_guest_path(dir));
    }
    if let Some(index) = dir.rfind("/node_modules/") {
        roots.insert(dir[..index + "/node_modules".len()].to_owned());
    }
    let mut roots = roots.into_iter().collect::<Vec<_>>();
    roots.sort();
    roots
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
            if subpath == "."
                && !record.contains_key(".")
                && !record.keys().any(|key| key.starts_with("./"))
            {
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
    None
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
