use crate::backend::{
    DirectHostReplyTarget, ExecutionBackend, ExecutionBackendKind, ExecutionExit,
    ExecutionWakeHandle, ExecutionWakeIdentity, HostCallReply, HostServiceError, ShutdownOutcome,
    ShutdownReason, SignalCheckpointOutcome,
};
use crate::common::stable_hash64;
use crate::node_import_cache::{
    NodeImportCache, NodeImportCacheCleanup, NODE_IMPORT_CACHE_ASSET_ROOT_ENV,
};
use crate::runtime_support::{
    NODE_COMPILE_CACHE_ENV, NODE_DISABLE_COMPILE_CACHE_ENV, NODE_FROZEN_TIME_ENV,
    NODE_SANDBOX_ROOT_ENV,
};
use crate::signal::ExecutionSignalHandlerRegistration;
use crate::v8_host::{V8RuntimeHost, V8SessionFrameReceiver, V8SessionHandle};
use crate::v8_ipc::BinaryFrame;
use crate::v8_runtime;
use agentos_bridge::queue_tracker::{register_queue, QueueGauge, TrackedLimit};
use agentos_runtime::accounting::ResourceClass;
use agentos_runtime::RuntimeContext;
use agentos_v8_runtime::runtime_protocol::{RuntimeCommand, WarmSessionHint};
use flume::{Receiver as EventReceiver, Sender as EventSender};
use getrandom::getrandom;
use serde::Serialize;
use serde::{Deserialize, Deserializer};
use serde_json::{json, Value};
use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap, HashMap, HashSet, VecDeque};
use std::fmt;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::os::fd::OwnedFd;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc::{self, Receiver, SyncSender, TrySendError},
    Arc, Condvar, Mutex, OnceLock,
};
use std::thread;
use std::time::{Duration, Instant};
use tokio::sync::Notify;
use tokio::time;

const NODE_ENTRYPOINT_ENV: &str = "AGENTOS_ENTRYPOINT";
const NODE_BOOTSTRAP_ENV: &str = "AGENTOS_BOOTSTRAP_MODULE";
const NODE_GUEST_ARGV_ENV: &str = "AGENTOS_GUEST_ARGV";
const NODE_PREWARM_IMPORTS_ENV: &str = "AGENTOS_NODE_PREWARM_IMPORTS";
const NODE_IMPORT_COMPILE_CACHE_NAMESPACE_VERSION: &str = "3";
const NODE_IMPORT_CACHE_LOADER_PATH_ENV: &str = "AGENTOS_NODE_IMPORT_CACHE_LOADER_PATH";
const NODE_IMPORT_CACHE_PATH_ENV: &str = "AGENTOS_NODE_IMPORT_CACHE_PATH";
const NODE_KEEP_STDIN_OPEN_ENV: &str = "AGENTOS_KEEP_STDIN_OPEN";
const NODE_GUEST_ENTRYPOINT_ENV: &str = "AGENTOS_GUEST_ENTRYPOINT";
const NODE_GUEST_ENTRYPOINT_MODULE_MODE_ENV: &str = "AGENTOS_GUEST_ENTRYPOINT_MODULE_MODE";
const NODE_RETAIN_CONTEXT_ENV: &str = "AGENTOS_RETAIN_LANGUAGE_CONTEXT";
const NODE_INLINE_FILE_PATH_ENV: &str = "AGENTOS_INLINE_FILE_PATH";
const NODE_USE_BUNDLED_TYPESCRIPT_ENV: &str = "AGENTOS_USE_BUNDLED_TYPESCRIPT";
const NODE_TYPESCRIPT_COMPILER_PATH_ENV: &str = "AGENTOS_TYPESCRIPT_COMPILER_PATH";
const NODE_GUEST_PATH_MAPPINGS_ENV: &str = "AGENTOS_GUEST_PATH_MAPPINGS";
const NODE_VIRTUAL_PROCESS_EXEC_PATH_ENV: &str = "AGENTOS_VIRTUAL_PROCESS_EXEC_PATH";
const NODE_VIRTUAL_PROCESS_PID_ENV: &str = "AGENTOS_VIRTUAL_PROCESS_PID";
const NODE_VIRTUAL_PROCESS_PPID_ENV: &str = "AGENTOS_VIRTUAL_PROCESS_PPID";
const NODE_VIRTUAL_PROCESS_UID_ENV: &str = "AGENTOS_VIRTUAL_PROCESS_UID";
const NODE_VIRTUAL_PROCESS_GID_ENV: &str = "AGENTOS_VIRTUAL_PROCESS_GID";
const NODE_PARENT_ALLOW_CHILD_PROCESS_ENV: &str = "AGENTOS_PARENT_NODE_ALLOW_CHILD_PROCESS";
const NODE_PARENT_ALLOW_WORKER_ENV: &str = "AGENTOS_PARENT_NODE_ALLOW_WORKER";
const NODE_EXTRA_FS_READ_PATHS_ENV: &str = "AGENTOS_EXTRA_FS_READ_PATHS";
const NODE_EXTRA_FS_WRITE_PATHS_ENV: &str = "AGENTOS_EXTRA_FS_WRITE_PATHS";
const NODE_ALLOWED_BUILTINS_ENV: &str = "AGENTOS_ALLOWED_NODE_BUILTINS";
const NODE_LOOPBACK_EXEMPT_PORTS_ENV: &str = "AGENTOS_LOOPBACK_EXEMPT_PORTS";
const NODE_SYNC_RPC_ENABLE_ENV: &str = "AGENTOS_NODE_SYNC_RPC_ENABLE";
const NODE_SYNC_RPC_REQUEST_FD_ENV: &str = "AGENTOS_NODE_SYNC_RPC_REQUEST_FD";
const NODE_SYNC_RPC_RESPONSE_FD_ENV: &str = "AGENTOS_NODE_SYNC_RPC_RESPONSE_FD";
const NODE_SYNC_RPC_DATA_BYTES_ENV: &str = "AGENTOS_NODE_SYNC_RPC_DATA_BYTES";
const NODE_SYNC_RPC_WAIT_TIMEOUT_MS_ENV: &str = "AGENTOS_NODE_SYNC_RPC_WAIT_TIMEOUT_MS";
static NEXT_V8_SESSION_ID: AtomicU64 = AtomicU64::new(1);
static JAVASCRIPT_TIMER_WHEEL: OnceLock<Arc<TimerWheel>> = OnceLock::new();
static JAVASCRIPT_TIMER_WHEEL_INIT: Mutex<()> = Mutex::new(());

#[derive(Default)]
struct JsStartPhaseStats {
    calls: u64,
    total_ns: u128,
    max_ns: u128,
}

static JS_START_PHASES: OnceLock<Mutex<BTreeMap<String, JsStartPhaseStats>>> = OnceLock::new();
static JS_EVENT_PHASES: OnceLock<Mutex<BTreeMap<String, JsStartPhaseStats>>> = OnceLock::new();

fn js_start_phases_enabled() -> bool {
    std::env::var("AGENTOS_JS_START_PHASES").as_deref() == Ok("1")
}

fn js_event_phases_enabled() -> bool {
    std::env::var("AGENTOS_JS_EVENT_PHASES").as_deref() == Ok("1")
}

fn record_js_start_phase(stage: &str, elapsed: Duration) {
    if !js_start_phases_enabled() {
        return;
    }
    record_js_phase_stats(
        &JS_START_PHASES,
        "AGENTOS_JS_START_PHASES_FILE",
        stage,
        elapsed,
    );
}

fn record_js_event_phase(stage: &str, elapsed: Duration) {
    if !js_event_phases_enabled() {
        return;
    }
    record_js_phase_stats(
        &JS_EVENT_PHASES,
        "AGENTOS_JS_EVENT_PHASES_FILE",
        stage,
        elapsed,
    );
}

fn record_js_phase_stats(
    phases: &OnceLock<Mutex<BTreeMap<String, JsStartPhaseStats>>>,
    path_env: &str,
    stage: &str,
    elapsed: Duration,
) {
    let phases = phases.get_or_init(|| Mutex::new(BTreeMap::new()));
    let Ok(mut phases) = phases.lock() else {
        eprintln!("ERR_AGENTOS_DIAGNOSTIC_STATE: JavaScript phase statistics lock is poisoned");
        return;
    };
    let stats = phases.entry(stage.to_string()).or_default();
    stats.calls += 1;
    let elapsed_ns = elapsed.as_nanos();
    stats.total_ns += elapsed_ns;
    stats.max_ns = stats.max_ns.max(elapsed_ns);

    let Some(path) = std::env::var_os(path_env) else {
        return;
    };
    let mut output = String::new();
    for (stage, stats) in phases.iter() {
        let total_us = stats.total_ns / 1_000;
        let avg_us = if stats.calls == 0 {
            0
        } else {
            total_us / u128::from(stats.calls)
        };
        let max_us = stats.max_ns / 1_000;
        output.push_str(&format!(
            "stage={stage} calls={} total_us={total_us} avg_us={avg_us} max_us={max_us}\n",
            stats.calls
        ));
    }
    if let Err(error) = fs::write(&path, output) {
        eprintln!(
            "ERR_AGENTOS_DIAGNOSTIC_WRITE: failed to write JavaScript phase statistics to {}: {error}",
            path.to_string_lossy()
        );
    }
}

const DEFAULT_V8_CPU_TIME_LIMIT_MS: u32 = 30_000;
const DEFAULT_V8_WALL_CLOCK_LIMIT_MS: u32 = 0;
const DEFAULT_NODE_IMPORT_CACHE_MATERIALIZE_TIMEOUT_MS: u64 = 30_000;
const NODE_SYNC_RPC_DEFAULT_DATA_BYTES: usize = 4 * 1024 * 1024;
const NODE_SYNC_RPC_DEFAULT_WAIT_TIMEOUT_MS: u64 = 30_000;
const NODE_SYNC_RPC_RESPONSE_QUEUE_CAPACITY: usize = 1;
const FORWARD_KERNEL_STDIN_RPC_ENV: &str = "AGENTOS_FORWARD_KERNEL_STDIN_RPC";
// Defense-in-depth headroom: a transient burst of guest events (e.g. a chatty
// tool/skill turn) should be absorbed by the buffer, so the producer only ever
// hits backpressure under a genuinely stuck consumer rather than on every spike.
const JAVASCRIPT_EVENT_CHANNEL_CAPACITY: usize = 512;
const JAVASCRIPT_EVENT_PAYLOAD_LIMIT_BYTES: usize = 1024 * 1024;
const JAVASCRIPT_CAPTURED_OUTPUT_LIMIT_BYTES: usize = 16 * 1024 * 1024;
const KERNEL_STDIN_BUFFER_LIMIT_BYTES: usize = 16 * 1024 * 1024;
const NODE_WARMUP_MARKER_VERSION: &str = "1";
const NODE_WARMUP_SPECIFIERS: &[&str] = &[
    "agentos:builtin/path",
    "agentos:builtin/url",
    "agentos:builtin/fs-promises",
    "agentos:polyfill/path",
];

#[derive(Debug, Default, Clone)]
struct SyncBridgePhaseStats {
    calls: u64,
    total_us: u64,
    max_us: u64,
}

static SYNC_BRIDGE_PHASES: OnceLock<Mutex<BTreeMap<String, SyncBridgePhaseStats>>> =
    OnceLock::new();
static SYNC_BRIDGE_REQUEST_ENQUEUED: OnceLock<Mutex<HashMap<u64, (String, Instant)>>> =
    OnceLock::new();

fn sync_bridge_phases_enabled() -> bool {
    std::env::var("AGENTOS_SYNC_BRIDGE_PHASES").as_deref() == Ok("1")
}

fn record_sync_bridge_phase(method: &str, stage: &str, elapsed: Duration) {
    if !sync_bridge_phases_enabled() {
        return;
    }
    let stats = SYNC_BRIDGE_PHASES.get_or_init(|| Mutex::new(BTreeMap::new()));
    let Ok(mut stats) = stats.lock() else {
        eprintln!(
            "ERR_AGENTOS_DIAGNOSTIC_STATE: JavaScript sync-bridge statistics lock is poisoned"
        );
        return;
    };
    let elapsed_us = elapsed.as_micros() as u64;
    let key = format!("{method}:{stage}");
    let entry = stats.entry(key).or_default();
    entry.calls += 1;
    entry.total_us = entry.total_us.wrapping_add(elapsed_us);
    entry.max_us = entry.max_us.max(elapsed_us);

    if let Ok(path) = std::env::var("AGENTOS_SYNC_BRIDGE_PHASES_FILE") {
        let mut lines = String::new();
        for (key, value) in stats.iter() {
            let Some((method, stage)) = key.split_once(':') else {
                continue;
            };
            let avg_us = value.total_us.checked_div(value.calls).unwrap_or(0);
            lines.push_str(&format!(
                "method={method} stage={stage} calls={} total_us={} avg_us={} max_us={}\n",
                value.calls, value.total_us, avg_us, value.max_us
            ));
        }
        if let Err(error) = fs::write(&path, lines) {
            eprintln!(
                "ERR_AGENTOS_DIAGNOSTIC_WRITE: failed to write JavaScript sync-bridge statistics to {path}: {error}"
            );
        }
    }
}

pub fn record_sync_bridge_request_enqueued(call_id: u64, method: &str) {
    if !sync_bridge_phases_enabled() {
        return;
    }
    let requests = SYNC_BRIDGE_REQUEST_ENQUEUED.get_or_init(|| Mutex::new(HashMap::new()));
    let Ok(mut requests) = requests.lock() else {
        eprintln!("ERR_AGENTOS_DIAGNOSTIC_STATE: sync-bridge request timing lock is poisoned");
        return;
    };
    if requests.len() > 4096 {
        requests.clear();
    }
    requests.insert(call_id, (method.to_owned(), Instant::now()));
}

pub fn record_sync_bridge_request_observed(call_id: u64, fallback_method: &str) {
    if !sync_bridge_phases_enabled() {
        return;
    }
    let Some(requests) = SYNC_BRIDGE_REQUEST_ENQUEUED.get() else {
        return;
    };
    let Ok(mut requests) = requests.lock() else {
        eprintln!("ERR_AGENTOS_DIAGNOSTIC_STATE: sync-bridge request timing lock is poisoned");
        return;
    };
    let Some((method, started)) = requests.remove(&call_id) else {
        return;
    };
    let method = if method.is_empty() {
        fallback_method
    } else {
        method.as_str()
    };
    record_sync_bridge_phase(method, "request_service_observed", started.elapsed());
}
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
    NODE_GUEST_ENTRYPOINT_MODULE_MODE_ENV,
    NODE_INLINE_FILE_PATH_ENV,
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
    NODE_RETAIN_CONTEXT_ENV,
    NODE_USE_BUNDLED_TYPESCRIPT_ENV,
    NODE_TYPESCRIPT_COMPILER_PATH_ENV,
    NODE_ALLOWED_BUILTINS_ENV,
    NODE_LOOPBACK_EXEMPT_PORTS_ENV,
    NODE_SYNC_RPC_ENABLE_ENV,
    NODE_SYNC_RPC_REQUEST_FD_ENV,
    NODE_SYNC_RPC_RESPONSE_FD_ENV,
    NODE_SYNC_RPC_DATA_BYTES_ENV,
    NODE_SYNC_RPC_WAIT_TIMEOUT_MS_ENV,
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum NodeControlMessage {
    NodeImportCacheMetrics {
        metrics: serde_json::Value,
    },
    PythonExit {
        #[serde(rename = "exitCode")]
        exit_code: i32,
    },
    SignalState {
        signal: u32,
        registration: ExecutionSignalHandlerRegistration,
    },
}

#[derive(Debug, Default)]
struct LinePrefixFilter {
    pending: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostRpcRequest {
    pub id: u64,
    pub method: String,
    pub args: Vec<Value>,
    pub raw_bytes_args: HashMap<usize, Vec<u8>>,
}

#[derive(Debug, Deserialize)]
struct JavascriptSyncRpcRequestWire {
    id: u64,
    method: String,
    #[serde(default)]
    args: Vec<Value>,
}

impl LinePrefixFilter {
    fn filter_chunk(&mut self, chunk: &[u8], prefixes: &[&str]) -> Vec<u8> {
        self.pending.extend_from_slice(chunk);
        let mut filtered = Vec::new();

        while let Some(newline_index) = self.pending.iter().position(|byte| *byte == b'\n') {
            let line = self.pending.drain(..=newline_index).collect::<Vec<_>>();
            if !has_control_prefix(&line, prefixes) {
                filtered.extend_from_slice(&line);
            }
        }

        filtered
    }
}

fn has_control_prefix(line: &[u8], prefixes: &[&str]) -> bool {
    let text = String::from_utf8_lossy(line);
    let trimmed = text.trim_end_matches(['\r', '\n']);
    prefixes.iter().any(|prefix| trimmed.starts_with(prefix))
}

#[cfg(test)]
#[derive(Debug)]
struct JavascriptSyncRpcResponseWriter {
    sender: SyncSender<Vec<u8>>,
    timeout: Duration,
}

#[cfg(test)]
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

#[cfg(test)]
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

#[derive(Debug)]
struct PendingSyncRpcRegistry {
    states: BTreeMap<u64, PendingSyncRpcState>,
    maximum: usize,
    gauge: Arc<QueueGauge>,
}

impl PendingSyncRpcRegistry {
    fn new(maximum: usize) -> Self {
        let maximum = maximum.max(1);
        Self {
            states: BTreeMap::new(),
            maximum,
            gauge: register_queue(TrackedLimit::PendingSyncRpcCalls, maximum),
        }
    }

    fn observe_depth(&self) {
        self.gauge.observe_depth(self.states.len());
    }
}

/// Direct response lane for JavaScript, Python's embedded JavaScript bridge,
/// and compatibility-WASM host calls.
///
/// This deliberately retains only the pending-call token and the thread-safe
/// V8 session lane. It does not retain or make `JavascriptExecution`/the V8
/// isolate `Send`.
#[derive(Debug, Clone)]
pub struct JavascriptSyncRpcResponder {
    pending_sync_rpc: Arc<Mutex<PendingSyncRpcRegistry>>,
    v8_session: V8SessionHandle,
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

/// Per-execution JavaScript runtime limits, carried as typed fields on the
/// execution request rather than `AGENTOS_*` env vars. The sidecar populates
/// these from the per-VM `VmLimits` (which originate from `CreateVmConfig` on
/// the BARE wire); `None` selects the engine default. See the env-vs-wire rule
/// in `crates/sidecar/CLAUDE.md`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JavascriptExecutionLimits {
    /// V8 heap cap in MB. `None`/`Some(0)` keeps the engine default heap.
    pub v8_heap_limit_mb: Option<u32>,
    /// Sync-RPC blocking-wait ceiling in ms. `None` keeps the engine default.
    pub sync_rpc_wait_timeout_ms: Option<u64>,
    /// Active JavaScript CPU-time budget in ms. `None` keeps the engine default;
    /// `Some(0)` disables the CPU watchdog.
    pub cpu_time_limit_ms: Option<u32>,
    /// JavaScript wall-clock backstop in ms. `None` keeps the engine default;
    /// `Some(0)` disables the wall-clock watchdog.
    pub wall_clock_limit_ms: Option<u32>,
    /// Timeout for materializing the per-VM Node import cache.
    pub import_cache_materialize_timeout_ms: Option<u64>,
    /// Maximum live JavaScript timers in this execution. `None` keeps the
    /// engine default. The sidecar supplies the VM-scoped configured value.
    pub max_timers: Option<usize>,
    /// Maximum readiness identities delivered in one V8 turn. Sidecar VM
    /// execution must supply `limits.reactor.workQuantum` here.
    pub reactor_work_quantum: Option<usize>,
    /// Per-call host bridge deadline. Sidecar VMs supply
    /// `limits.reactor.operationDeadlineMs`; zero is invalid.
    pub bridge_call_timeout_ms: Option<u64>,
}

/// Per-execution guest-runtime config carried as typed fields rather than
/// `AGENTOS_*` env vars. The sidecar populates these from kernel state
/// (`user_profile()`, `resource_limits()`) and `CreateVmConfig`; the runtime
/// shim interpolates them into a `_processConfig` object the guest reads, so the
/// guest's virtual identity no longer rides the ambient env channel. `None`
/// keeps the guest-runtime default. See the env-vs-wire rule in
/// `crates/sidecar/CLAUDE.md`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestRuntimeConfig {
    /// Virtual `process.pid`.
    pub virtual_pid: Option<u64>,
    /// Virtual `process.ppid`.
    pub virtual_ppid: Option<u64>,
    /// Virtual `process.uid` / `process.euid`.
    pub virtual_uid: Option<u64>,
    /// Virtual `process.gid` / `process.egid` / `process.groups`.
    pub virtual_gid: Option<u64>,
    /// Virtual `process.execPath`.
    pub virtual_exec_path: Option<String>,
    /// `os.cpus().length`.
    pub os_cpu_count: Option<u64>,
    /// `os.totalmem()` in bytes.
    pub os_totalmem: Option<u64>,
    /// `os.freemem()` in bytes.
    pub os_freemem: Option<u64>,
    /// `os.homedir()`.
    pub os_homedir: Option<String>,
    /// `os.hostname()`.
    pub os_hostname: Option<String>,
    /// `os.tmpdir()`.
    pub os_tmpdir: Option<String>,
    /// `os.type()`.
    pub os_type: Option<String>,
    /// `os.release()`.
    pub os_release: Option<String>,
    /// `os.version()`.
    pub os_version: Option<String>,
    /// `os.machine()`.
    pub os_machine: Option<String>,
    /// Default login shell.
    pub os_shell: Option<String>,
    /// `os.userInfo().username`.
    pub os_user: Option<String>,
    /// Opt-in high-resolution monotonic guest clock. Default false preserves
    /// the security-oriented coarse clock.
    pub high_resolution_time: bool,
    /// Optional agent-SDK bundle (esbuild IIFE) to evaluate into the per-sidecar
    /// V8 snapshot alongside the bridge, so the SDK is loaded once per sidecar and
    /// reused across sessions instead of re-imported on every execution. `None`
    /// keeps the bridge-only snapshot (unchanged behavior). The runtime caches the
    /// snapshot process-wide keyed by sha256(bridge_code + this bundle).
    pub snapshot_userland_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartJavascriptExecutionRequest {
    pub vm_id: String,
    pub context_id: String,
    pub argv: Vec<String>,
    /// Explicit process argv[0]. `Some("")` is distinct from `None` and must be
    /// preserved for Node child_process compatibility.
    pub argv0: Option<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: PathBuf,
    /// Per-execution runtime limits (see [`JavascriptExecutionLimits`]).
    pub limits: JavascriptExecutionLimits,
    /// Per-execution guest-runtime config (see [`GuestRuntimeConfig`]).
    pub guest_runtime: GuestRuntimeConfig,
    /// Optional inline JavaScript code supplied by the sidecar.
    /// Eval entrypoints always execute this source directly. Module-mode file
    /// entrypoints may also use it so the isolate can evaluate the original
    /// source without re-reading through the host. CommonJS file entrypoints
    /// still go through the normal require() wrapper so Node-style globals such
    /// as __filename and __dirname are initialized correctly.
    pub inline_code: Option<String>,
    /// Optional raw WASM module bytes to expose to the runner isolate for this
    /// execution.
    pub wasm_module_bytes: Option<Arc<Vec<u8>>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JavascriptExecutionEvent {
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
    SyncRpcRequest(HostRpcRequest),
    SignalState {
        signal: u32,
        registration: ExecutionSignalHandlerRegistration,
    },
    Exited(i32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum JavascriptProcessEvent {
    Stdout(Vec<u8>),
    RawStderr(Vec<u8>),
    SyncRpcRequest(HostRpcRequest),
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
pub enum ModuleResolveMode {
    Require,
    Import,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalResolvedModuleFormat {
    Module,
    Commonjs,
    Json,
}

impl LocalResolvedModuleFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Module => "module",
            Self::Commonjs => "commonjs",
            Self::Json => "json",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct LocalModuleResolutionCache {
    resolve_results: HashMap<(String, String, ModuleResolveMode), Option<String>>,
    module_format_results: HashMap<String, Option<LocalResolvedModuleFormat>>,
    package_json_results: HashMap<String, Option<LocalPackageJson>>,
    exists_results: HashMap<String, bool>,
    stat_results: HashMap<String, Option<bool>>,
}

/// Read-only filesystem primitives the module resolver needs. The resolution
/// algorithm itself is pure path algebra over these four operations; pointing
/// it at a different backing store (host files vs. the kernel VFS) is purely a
/// matter of supplying a different `ModuleFsReader`.
///
/// All paths are guest paths (e.g. `/root/node_modules/foo/index.js`). Symlink
/// following is the reader's responsibility: `canonical_guest_path` must return
/// the fully-resolved guest path (realpath), and `path_is_dir`/`path_exists`
/// must follow symlinks the way real Node's `fs.stat`/`fs.existsSync` do.
pub trait ModuleFsReader {
    /// Realpath of `guest_path`, expressed as a guest path. `None` if the path
    /// does not resolve (does not exist / escapes the addressable tree).
    fn canonical_guest_path(
        &mut self,
        guest_path: &str,
    ) -> Result<Option<String>, HostServiceError>;

    /// Read the file at `guest_path` as a UTF-8 string, following symlinks.
    fn read_to_string(&mut self, guest_path: &str) -> Result<Option<String>, HostServiceError>;

    /// `Some(true)` if `guest_path` is a directory, `Some(false)` if it exists
    /// but is not a directory, `None` if it does not exist. Follows symlinks.
    fn path_is_dir(&mut self, guest_path: &str) -> Result<Option<bool>, HostServiceError>;

    /// Whether `guest_path` exists, following symlinks.
    fn path_exists(&mut self, guest_path: &str) -> Result<bool, HostServiceError>;
}

/// Guest JavaScript module-resolution mode (the `moduleResolution` axis of
/// `jsRuntime`). Defaults to full Node.js resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum GuestModuleResolution {
    /// node_modules ancestor-walk + exports/conditions + realpath.
    #[default]
    Node,
    /// Relative/absolute ESM only; bare specifiers do not resolve.
    Relative,
    /// No resolution at all; every specifier (relative included) is denied.
    None,
}

impl GuestModuleResolution {
    fn from_env(env: &BTreeMap<String, String>) -> Self {
        match env.get("AGENTOS_JS_MODULE_RESOLUTION").map(String::as_str) {
            Some("relative") => Self::Relative,
            Some("none") => Self::None,
            _ => Self::Node,
        }
    }
}

struct LocalBridgeState {
    runtime: Option<RuntimeContext>,
    timer_resources: Option<Arc<agentos_runtime::accounting::ResourceLedger>>,
    max_timers: usize,
    translator: GuestPathTranslator,
    resolution_cache: LocalModuleResolutionCache,
    /// jsRuntime module-resolution mode for this execution.
    module_resolution: GuestModuleResolution,
    handle_descriptions: HashMap<String, String>,
    next_timer_id: u64,
    timers: Arc<Mutex<HashMap<u64, LocalTimerEntry>>>,
    kernel_stdin: Arc<LocalKernelStdinBridge>,
    forward_kernel_stdin_rpc: bool,
    v8_session: Option<V8SessionHandle>,
    /// Optional read-only reader over the mounted `node_modules` VFS, supplied by
    /// the sidecar. When present, the bridge thread resolves module-resolution
    /// RPCs (`_resolveModule` / `_loadFile` / `_moduleFormat` /
    /// `_batchResolveModules`) inline against this reader, concurrently with the
    /// service loop — so a large cold-start module graph does not serialize
    /// behind / starve the ACP bootstrap on the single service-loop thread.
    /// `None` means "route module resolution to the service loop" (the kernel-VFS
    /// fallback for callers that supply no reader).
    module_reader: Option<Box<dyn ModuleFsReader + Send>>,
    /// The installed reader only exposes trusted host mappings; every other
    /// path is owned by the live kernel VFS. Package-origin resolution must
    /// bypass that partial reader before its `/root/node_modules` compatibility
    /// mapping can shadow the package's dependency closure under `/opt/agentos`.
    module_reader_forwards_to_kernel: bool,
}

impl Default for LocalBridgeState {
    fn default() -> Self {
        let runtime = default_test_runtime_context();
        let timer_resources = runtime
            .as_ref()
            .map(|runtime| Arc::clone(runtime.resources()));
        Self {
            runtime,
            timer_resources,
            max_timers: MAX_TIMERS_PER_EXECUTION,
            translator: GuestPathTranslator::default(),
            resolution_cache: LocalModuleResolutionCache::default(),
            module_resolution: GuestModuleResolution::default(),
            handle_descriptions: HashMap::new(),
            next_timer_id: 0,
            timers: Arc::new(Mutex::new(HashMap::new())),
            kernel_stdin: Arc::new(LocalKernelStdinBridge::default()),
            forward_kernel_stdin_rpc: false,
            v8_session: None,
            module_reader: None,
            module_reader_forwards_to_kernel: false,
        }
    }
}

#[cfg(test)]
fn default_test_runtime_context() -> Option<RuntimeContext> {
    agentos_runtime::SidecarRuntime::process(&agentos_runtime::RuntimeConfig::default())
        .ok()
        .map(agentos_runtime::SidecarRuntime::context)
}

#[cfg(not(test))]
fn default_test_runtime_context() -> Option<RuntimeContext> {
    None
}

impl Drop for LocalBridgeState {
    /// Tear down all tracked timers when the bridge state is dropped (which
    /// happens when the event-bridge service loop exits on session termination —
    /// success, error, or shutdown). Clearing the shared `timers` map cancels both
    /// kernel and bridge timers: any in-flight wheel action that wakes afterwards
    /// finds its entry gone and suppresses its callback via `timer_should_fire`,
    /// so a destroyed session's timers do not fire after the fact.
    fn drop(&mut self) {
        if let Some(wheel) = JAVASCRIPT_TIMER_WHEEL.get() {
            wheel.cancel_registry(&self.timers);
        }
        if let Ok(mut timers) = self.timers.lock() {
            timers.clear();
        }
    }
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
    /// Explicit, trusted host projections supplied by the sidecar. A live
    /// kernel-VFS module reader may fall back only to these roots, never to the
    /// implicit host cwd or sandbox root.
    explicit_mapping_guest_roots: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct LocalPackageJson {
    #[serde(default, deserialize_with = "deserialize_optional_package_string")]
    name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_package_string")]
    main: Option<String>,
    #[serde(
        default,
        rename = "type",
        deserialize_with = "deserialize_optional_package_string"
    )]
    package_type: Option<String>,
    #[serde(default)]
    exports: Option<Value>,
    #[serde(default)]
    imports: Option<Value>,
}

fn deserialize_optional_package_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    Ok(value.and_then(|value| value.as_str().map(str::to_owned)))
}

#[derive(Debug)]
struct LocalTimerEntry {
    delay_ms: u64,
    generation: u64,
    repeat: bool,
    _reservation: Option<agentos_runtime::accounting::Reservation>,
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
    Error(HostServiceError),
    Deferred,
}

/// Upper bound on guest-supplied timer delays, matching the JS `TIMEOUT_MAX`
/// ceiling (`2**31 - 1` ms, ~24.8 days). Guest code can pass a delay up to
/// `u64::MAX` ms; clamping keeps the timer wheel's deadline math and session
/// handle lifetime within Node-compatible bounds.
const MAX_TIMER_DELAY_MS: u64 = 2_147_483_647;
const MAX_TIMERS_PER_EXECUTION: usize = 4_096;
const MAX_TIMER_ACTIONS_PER_TURN: usize = 1_024;

fn timer_delay_ms(value: Option<&Value>) -> u64 {
    let delay = match value {
        Some(Value::Number(number)) => number.as_f64().unwrap_or(0.0),
        Some(Value::String(text)) => text.parse::<f64>().unwrap_or(0.0),
        _ => 0.0,
    };

    if !delay.is_finite() || delay <= 0.0 {
        0
    } else {
        delay.floor().min(MAX_TIMER_DELAY_MS as f64) as u64
    }
}

fn timer_dispatch_error(error: HostServiceError) -> Value {
    json!({
        "__bd_error": {
            "name": "Error",
            "code": error.code,
            "message": error.message,
            "details": error.details,
        }
    })
}

fn javascript_timer_error(code: &'static str, message: impl Into<String>) -> HostServiceError {
    HostServiceError::new(code, message.into())
}

/// Decide whether a woken timer action should fire, and reclaim its tracking
/// entry. Returns `false` (suppressing the callback) when the timer is gone from
/// the map (cleared, or wiped on session teardown) or its generation no longer
/// matches the one captured at scheduling time (re-armed/cancelled). A one-shot
/// (`repeat == false`) timer that does fire is removed from the map so its id is
/// reclaimed. Shared by the kernel-timer and bridge-timer paths so both honor the
/// same cancellation semantics.
fn timer_should_fire(
    timers: &Arc<Mutex<HashMap<u64, LocalTimerEntry>>>,
    timer_id: u64,
    generation: u64,
) -> bool {
    match timers.lock() {
        Ok(mut timers) => {
            let Some((current_generation, repeat)) = timers
                .get(&timer_id)
                .map(|entry| (entry.generation, entry.repeat))
            else {
                return false;
            };
            if current_generation != generation {
                return false;
            }
            if !repeat {
                timers.remove(&timer_id);
            }
            true
        }
        Err(error) => {
            eprintln!(
                "ERR_AGENTOS_TIMER_STATE: failed to inspect timer {timer_id} generation {generation}: {error}"
            );
            false
        }
    }
}

struct TimerWheel {
    state: Mutex<TimerWheelState>,
    ready: Notify,
}

#[derive(Default)]
struct TimerWheelState {
    heap: BinaryHeap<Reverse<(Instant, u64)>>,
    entries: HashMap<u64, ScheduledTimerAction>,
    timer_index: HashMap<(usize, u64), u64>,
    next_seq: u64,
}

struct ScheduledTimerAction {
    deadline: Instant,
    action: TimerAction,
}

enum TimerAction {
    StreamEvent {
        session: V8SessionHandle,
        timer_id: u64,
        generation: u64,
        timers: Arc<Mutex<HashMap<u64, LocalTimerEntry>>>,
    },
    BridgeResponse {
        session: V8SessionHandle,
        call_id: u64,
        timer_id: u64,
        generation: u64,
        timers: Arc<Mutex<HashMap<u64, LocalTimerEntry>>>,
    },
}

fn settle_timer_bridge_response(
    session: &V8SessionHandle,
    call_id: u64,
    status: u8,
    payload: Vec<u8>,
) {
    if let Err(error) = session.send_bridge_response(call_id, status, payload) {
        tracing::warn!(
            call_id,
            error = %error,
            "timer bridge response caller stopped waiting"
        );
    }
}

impl TimerAction {
    fn timer_key(&self) -> (usize, u64) {
        match self {
            Self::StreamEvent {
                timer_id, timers, ..
            }
            | Self::BridgeResponse {
                timer_id, timers, ..
            } => (Arc::as_ptr(timers) as usize, *timer_id),
        }
    }

    fn execute(self) {
        match self {
            Self::StreamEvent {
                session,
                timer_id,
                generation,
                timers,
            } => {
                if !timer_should_fire(&timers, timer_id, generation) {
                    return;
                }

                if let Err(error) = session.publish_timer(timer_id) {
                    tracing::warn!(
                        timer_id,
                        error = %error,
                        "could not publish durable JavaScript timer readiness"
                    );
                }
            }
            Self::BridgeResponse {
                session,
                call_id,
                timer_id,
                generation,
                timers,
            } => {
                if !timer_should_fire(&timers, timer_id, generation) {
                    return;
                }
                settle_timer_bridge_response(&session, call_id, 0, Vec::new());
            }
        }
    }
}

impl TimerWheel {
    fn get(runtime: &RuntimeContext) -> Result<&'static Arc<Self>, HostServiceError> {
        if let Some(wheel) = JAVASCRIPT_TIMER_WHEEL.get() {
            return Ok(wheel);
        }

        let _initializing = JAVASCRIPT_TIMER_WHEEL_INIT.lock().map_err(|_| {
            javascript_timer_error(
                "ERR_AGENTOS_JAVASCRIPT_TIMER_WHEEL_INIT",
                "timer wheel initialization lock poisoned",
            )
        })?;
        if let Some(wheel) = JAVASCRIPT_TIMER_WHEEL.get() {
            return Ok(wheel);
        }

        let wheel = Self::start(runtime.clone())?;
        JAVASCRIPT_TIMER_WHEEL.set(wheel).map_err(|_| {
            javascript_timer_error(
                "ERR_AGENTOS_JAVASCRIPT_TIMER_WHEEL_INIT",
                "timer wheel was concurrently installed despite the initialization lock",
            )
        })?;
        JAVASCRIPT_TIMER_WHEEL.get().ok_or_else(|| {
            javascript_timer_error(
                "ERR_AGENTOS_JAVASCRIPT_TIMER_WHEEL_INIT",
                "timer wheel was not installed",
            )
        })
    }

    fn start(runtime: RuntimeContext) -> Result<Arc<Self>, HostServiceError> {
        let wheel = Arc::new(Self {
            state: Mutex::new(TimerWheelState::default()),
            ready: Notify::new(),
        });
        let worker = Arc::clone(&wheel);
        runtime
            .spawn(agentos_runtime::TaskClass::Timer, async move {
                worker.run().await
            })
            .map_err(|error| {
                javascript_timer_error(
                    "ERR_AGENTOS_TASK_LIMIT",
                    format!("failed to start JavaScript timer wheel: {error}"),
                )
            })?;
        Ok(wheel)
    }

    fn schedule(&self, delay_ms: u64, action: TimerAction) -> Result<(), HostServiceError> {
        let now = Instant::now();
        let deadline = now
            .checked_add(Duration::from_millis(delay_ms))
            .unwrap_or(now);
        let mut state = self.lock_state();
        let timer_key = action.timer_key();
        if let Some(previous_seq) = state.timer_index.remove(&timer_key) {
            state.entries.remove(&previous_seq);
        }
        let old_earliest = state.heap.peek().map(|Reverse((deadline, _))| *deadline);
        let seq = state.next_seq;
        state.next_seq = state.next_seq.checked_add(1).ok_or_else(|| {
            javascript_timer_error(
                "ERR_AGENTOS_JAVASCRIPT_TIMER_SEQUENCE_EXHAUSTED",
                "process timer sequence exhausted",
            )
        })?;
        state.heap.push(Reverse((deadline, seq)));
        state
            .entries
            .insert(seq, ScheduledTimerAction { deadline, action });
        state.timer_index.insert(timer_key, seq);
        Self::compact_heap_if_needed(&mut state);
        if old_earliest.is_none_or(|old| deadline < old) {
            self.ready.notify_one();
        }
        Ok(())
    }

    fn cancel(&self, timers: &Arc<Mutex<HashMap<u64, LocalTimerEntry>>>, timer_id: u64) {
        let key = (Arc::as_ptr(timers) as usize, timer_id);
        let mut state = self.lock_state();
        if let Some(seq) = state.timer_index.remove(&key) {
            state.entries.remove(&seq);
            Self::compact_heap_if_needed(&mut state);
        }
    }

    fn cancel_registry(&self, timers: &Arc<Mutex<HashMap<u64, LocalTimerEntry>>>) {
        let registry = Arc::as_ptr(timers) as usize;
        let mut state = self.lock_state();
        let keys = state
            .timer_index
            .keys()
            .filter(|(candidate, _)| *candidate == registry)
            .copied()
            .collect::<Vec<_>>();
        for key in keys {
            if let Some(seq) = state.timer_index.remove(&key) {
                state.entries.remove(&seq);
            }
        }
        Self::compact_heap_if_needed(&mut state);
    }

    async fn run(&self) {
        loop {
            // Create the notification future before reading the heap so a
            // concurrently inserted earlier deadline cannot be lost.
            let notified = self.ready.notified();
            let next_deadline = self
                .lock_state()
                .heap
                .peek()
                .map(|Reverse((deadline, _))| *deadline);
            match next_deadline {
                Some(deadline) if deadline > Instant::now() => {
                    tokio::select! {
                        _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => {}
                        _ = notified => continue,
                    }
                }
                Some(_) => {}
                None => {
                    notified.await;
                    continue;
                }
            }

            let due = {
                let mut state = self.lock_state();
                let now = Instant::now();
                let mut due = Vec::with_capacity(MAX_TIMER_ACTIONS_PER_TURN);
                while let Some(Reverse((deadline, seq))) = state.heap.peek().copied() {
                    if deadline > now || due.len() >= MAX_TIMER_ACTIONS_PER_TURN {
                        break;
                    }
                    state.heap.pop();
                    if let Some(scheduled) = state.entries.remove(&seq) {
                        let timer_key = scheduled.action.timer_key();
                        if state.timer_index.get(&timer_key) == Some(&seq) {
                            state.timer_index.remove(&timer_key);
                        }
                        due.push(scheduled.action);
                    }
                }
                due
            };

            for action in due {
                if catch_unwind(AssertUnwindSafe(|| action.execute())).is_err() {
                    tracing::warn!("JavaScript timer wheel action panicked");
                }
            }
            tokio::task::yield_now().await;
        }
    }

    fn compact_heap_if_needed(state: &mut TimerWheelState) {
        let compact_above = state.entries.len().saturating_mul(2).saturating_add(1_024);
        if state.heap.len() <= compact_above {
            return;
        }
        state.heap = state
            .entries
            .iter()
            .map(|(&seq, scheduled)| Reverse((scheduled.deadline, seq)))
            .collect();
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, TimerWheelState> {
        match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

impl GuestPathTranslator {
    fn from_host_context(
        env: &BTreeMap<String, String>,
        host_cwd: PathBuf,
        guest_cwd: String,
    ) -> Self {
        let mut mappings = parse_guest_path_mappings_from_env(env)
            .into_iter()
            .filter(|mapping| mapping.guest_path.starts_with('/'))
            .collect::<Vec<_>>();
        let explicit_mapping_guest_roots = mappings
            .iter()
            .map(|mapping| mapping.guest_path.clone())
            .collect();

        if !mappings
            .iter()
            .any(|mapping| mapping.guest_path == guest_cwd && mapping.host_path == host_cwd)
        {
            mappings.push(GuestPathMapping {
                guest_path: guest_cwd.clone(),
                host_path: host_cwd.clone(),
            });
        }

        sort_guest_path_mappings(&mut mappings);

        Self {
            implicit_guest_cwd: guest_cwd,
            implicit_host_cwd: host_cwd,
            sandbox_root: env
                .get(NODE_SANDBOX_ROOT_ENV)
                .filter(|value| Path::new(value.as_str()).is_absolute())
                .map(PathBuf::from),
            mappings,
            explicit_mapping_guest_roots,
        }
    }

    fn explicitly_maps_guest_path(&self, guest_path: &str) -> bool {
        let normalized = normalize_guest_path(guest_path);
        self.explicit_mapping_guest_roots
            .iter()
            .any(|root| strip_guest_prefix(&normalized, root).is_some())
    }

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
        let mut translator = Self::from_host_context(
            &request.env,
            request.cwd.clone(),
            implicit_guest_cwd.clone(),
        );
        translator.mappings.sort_by(|left, right| {
            let left_is_implicit =
                left.guest_path == implicit_guest_cwd && left.host_path == request.cwd;
            let right_is_implicit =
                right.guest_path == implicit_guest_cwd && right.host_path == request.cwd;
            right
                .guest_path
                .len()
                .cmp(&left.guest_path.len())
                .then_with(|| right_is_implicit.cmp(&left_is_implicit))
                .then_with(|| {
                    right
                        .host_path
                        .components()
                        .count()
                        .cmp(&left.host_path.components().count())
                })
        });
        translator
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
                    return self.confine_host_path(candidate);
                }
                if let Ok(real_mapping_path) = fs::canonicalize(&mapping.host_path) {
                    let real_candidate = join_host_path(&real_mapping_path, suffix);
                    if real_candidate.exists() {
                        return self.confine_host_path(real_candidate);
                    }
                    if let Some(sibling_candidate) =
                        resolve_pnpm_sibling_host_path(&real_mapping_path, suffix)
                    {
                        return self.confine_host_path(sibling_candidate);
                    }
                }
                fallback_candidate.get_or_insert(candidate);
            }
        }
        if let Some(suffix) = strip_guest_prefix(&normalized, &self.implicit_guest_cwd) {
            return self.confine_host_path(join_host_path(&self.implicit_host_cwd, suffix));
        }

        if let Some(candidate) = fallback_candidate {
            return self.confine_host_path(candidate);
        }

        if let Some(sandbox_root) = &self.sandbox_root {
            return self.confine_host_path(join_host_path(
                sandbox_root,
                normalized.trim_start_matches('/'),
            ));
        }

        None
    }

    fn confine_host_path(&self, host_path: PathBuf) -> Option<PathBuf> {
        let allowed_roots = self.allowed_canonical_host_roots();
        if allowed_roots.is_empty() {
            return None;
        }

        if let Ok(canonical_path) = fs::canonicalize(&host_path) {
            return canonical_path_is_allowed(&canonical_path, &allowed_roots).then_some(host_path);
        }

        let existing_ancestor = nearest_existing_host_ancestor(&host_path)?;
        let canonical_ancestor = fs::canonicalize(existing_ancestor).ok()?;
        canonical_path_is_allowed(&canonical_ancestor, &allowed_roots).then_some(host_path)
    }

    fn allowed_canonical_host_roots(&self) -> Vec<PathBuf> {
        let mut roots = Vec::new();
        for root in self
            .mappings
            .iter()
            .map(|mapping| mapping.host_path.as_path())
            .chain(std::iter::once(self.implicit_host_cwd.as_path()))
            .chain(self.sandbox_root.as_deref())
        {
            if let Ok(canonical_root) = fs::canonicalize(root) {
                if !roots.iter().any(|existing| existing == &canonical_root) {
                    roots.push(canonical_root);
                }
            }
        }
        roots
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

    /// Typed module-resolution variant of the legacy host translator. Missing
    /// mappings remain a normal resolution miss; permission failures, symlink
    /// loops, and other host filesystem errors cross the bridge as typed errors.
    fn canonical_module_guest_path(
        &self,
        guest_path: &str,
    ) -> Result<Option<String>, HostServiceError> {
        let Some(host_path) = self.module_guest_to_host(guest_path)? else {
            return Ok(None);
        };
        let Some(canonical) =
            module_fs_canonicalize_optional(&host_path, "canonicalize", guest_path)?
        else {
            return Ok(None);
        };

        for mapping in &self.mappings {
            if strip_guest_prefix(guest_path, &mapping.guest_path).is_none() {
                continue;
            }
            if let Ok(stripped) = canonical.strip_prefix(&mapping.host_path) {
                return Ok(Some(join_guest_path(
                    &mapping.guest_path,
                    &stripped.to_string_lossy().replace('\\', "/"),
                )));
            }
            if let Some(real_mapping_path) = module_fs_canonicalize_optional(
                &mapping.host_path,
                "canonicalize mapping",
                guest_path,
            )? {
                if let Ok(stripped) = canonical.strip_prefix(&real_mapping_path) {
                    return Ok(Some(join_guest_path(
                        &mapping.guest_path,
                        &stripped.to_string_lossy().replace('\\', "/"),
                    )));
                }
            }
        }

        if let Ok(stripped) = canonical.strip_prefix(&self.implicit_host_cwd) {
            return Ok(Some(join_guest_path(
                &self.implicit_guest_cwd,
                &stripped.to_string_lossy().replace('\\', "/"),
            )));
        }
        if let Some(sandbox_root) = &self.sandbox_root {
            if let Ok(stripped) = canonical.strip_prefix(sandbox_root) {
                return Ok(Some(join_guest_path(
                    "/",
                    &stripped.to_string_lossy().replace('\\', "/"),
                )));
            }
        }
        Ok(None)
    }

    fn module_guest_to_host(&self, guest_path: &str) -> Result<Option<PathBuf>, HostServiceError> {
        let normalized = normalize_guest_path(guest_path);
        let mut fallback_candidate = None;

        for mapping in &self.mappings {
            if let Some(suffix) = strip_guest_prefix(&normalized, &mapping.guest_path) {
                let candidate = join_host_path(&mapping.host_path, suffix);
                if module_fs_try_exists(&candidate, guest_path)? {
                    return self.confine_module_host_path(candidate, guest_path);
                }
                if let Some(real_mapping_path) = module_fs_canonicalize_optional(
                    &mapping.host_path,
                    "canonicalize mapping",
                    guest_path,
                )? {
                    let real_candidate = join_host_path(&real_mapping_path, suffix);
                    if module_fs_try_exists(&real_candidate, guest_path)? {
                        return self.confine_module_host_path(real_candidate, guest_path);
                    }
                    if let Some(sibling_candidate) = resolve_pnpm_sibling_host_path_typed(
                        &real_mapping_path,
                        suffix,
                        guest_path,
                    )? {
                        return self.confine_module_host_path(sibling_candidate, guest_path);
                    }
                }
                fallback_candidate.get_or_insert(candidate);
            }
        }

        let candidate =
            if let Some(suffix) = strip_guest_prefix(&normalized, &self.implicit_guest_cwd) {
                Some(join_host_path(&self.implicit_host_cwd, suffix))
            } else if let Some(candidate) = fallback_candidate {
                Some(candidate)
            } else {
                self.sandbox_root.as_ref().map(|sandbox_root| {
                    join_host_path(sandbox_root, normalized.trim_start_matches('/'))
                })
            };
        match candidate {
            Some(candidate) => self.confine_module_host_path(candidate, guest_path),
            None => Ok(None),
        }
    }

    fn confine_module_host_path(
        &self,
        host_path: PathBuf,
        guest_path: &str,
    ) -> Result<Option<PathBuf>, HostServiceError> {
        let mut allowed_roots = Vec::new();
        for root in self
            .mappings
            .iter()
            .map(|mapping| mapping.host_path.as_path())
            .chain(std::iter::once(self.implicit_host_cwd.as_path()))
            .chain(self.sandbox_root.as_deref())
        {
            if let Some(canonical_root) =
                module_fs_canonicalize_optional(root, "canonicalize root", guest_path)?
            {
                if !allowed_roots
                    .iter()
                    .any(|existing| existing == &canonical_root)
                {
                    allowed_roots.push(canonical_root);
                }
            }
        }
        if allowed_roots.is_empty() {
            return Ok(None);
        }

        if let Some(canonical_path) =
            module_fs_canonicalize_optional(&host_path, "canonicalize", guest_path)?
        {
            return Ok(
                canonical_path_is_allowed(&canonical_path, &allowed_roots).then_some(host_path)
            );
        }

        let mut ancestor = host_path.as_path();
        loop {
            match fs::symlink_metadata(ancestor) {
                Ok(_) => {
                    let canonical_ancestor = fs::canonicalize(ancestor).map_err(|error| {
                        module_fs_io_error("canonicalize ancestor", guest_path, error)
                    })?;
                    return Ok(
                        canonical_path_is_allowed(&canonical_ancestor, &allowed_roots)
                            .then_some(host_path),
                    );
                }
                Err(error) if module_fs_io_is_missing(&error) => {}
                Err(error) => {
                    return Err(module_fs_io_error("inspect ancestor", guest_path, error));
                }
            }
            let Some(parent) = ancestor.parent() else {
                return Ok(None);
            };
            ancestor = parent;
        }
    }
}

fn module_fs_try_exists(path: &Path, guest_path: &str) -> Result<bool, HostServiceError> {
    path.try_exists()
        .map_err(|error| module_fs_io_error("inspect", guest_path, error))
}

fn module_fs_canonicalize_optional(
    path: &Path,
    operation: &str,
    guest_path: &str,
) -> Result<Option<PathBuf>, HostServiceError> {
    match fs::canonicalize(path) {
        Ok(path) => Ok(Some(path)),
        Err(error) if module_fs_io_is_missing(&error) => Ok(None),
        Err(error) => Err(module_fs_io_error(operation, guest_path, error)),
    }
}

fn resolve_pnpm_sibling_host_path_typed(
    real_mapping_path: &Path,
    suffix: &str,
    guest_path: &str,
) -> Result<Option<PathBuf>, HostServiceError> {
    let Some(trimmed) = suffix.strip_prefix("node_modules/") else {
        return Ok(None);
    };
    let mut current = Some(real_mapping_path);
    while let Some(path) = current {
        if path.file_name().and_then(|name| name.to_str()) == Some("node_modules") {
            let candidate = join_host_path(path, trimmed);
            return module_fs_try_exists(&candidate, guest_path)
                .map(|exists| exists.then_some(candidate));
        }
        current = path.parent();
    }
    Ok(None)
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

fn canonical_path_is_allowed(path: &Path, allowed_roots: &[PathBuf]) -> bool {
    allowed_roots
        .iter()
        .any(|root| path == root || path.starts_with(root))
}

fn nearest_existing_host_ancestor(path: &Path) -> Option<&Path> {
    let mut candidate = Some(path);
    while let Some(current) = candidate {
        if fs::symlink_metadata(current).is_ok() {
            return Some(current);
        }
        candidate = current.parent();
    }
    None
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

        // Build via default + in-place assignment rather than `..default()`:
        // LocalBridgeState implements Drop (to cancel timers on session teardown),
        // and functional-record-update would move fields out of a Drop type (E0509).
        let mut local_bridge = LocalBridgeState::default();
        local_bridge.translator = GuestPathTranslator {
            implicit_guest_cwd: String::from("/root"),
            implicit_host_cwd: host_root,
            sandbox_root: None,
            explicit_mapping_guest_roots: mappings
                .iter()
                .map(|mapping| mapping.guest_path.clone())
                .collect(),
            mappings,
        };
        Self { local_bridge }
    }

    pub fn resolve_import(&mut self, specifier: &str, from_path: &str) -> Option<String> {
        self.try_resolve_import(specifier, from_path)
            .expect("host module filesystem resolution must succeed")
    }

    pub fn try_resolve_import(
        &mut self,
        specifier: &str,
        from_path: &str,
    ) -> Result<Option<String>, HostServiceError> {
        self.local_bridge
            .resolve_module(specifier, from_path, ModuleResolveMode::Import)
    }

    pub fn resolve_require(&mut self, specifier: &str, from_path: &str) -> Option<String> {
        self.try_resolve_require(specifier, from_path)
            .expect("host module filesystem resolution must succeed")
    }

    pub fn try_resolve_require(
        &mut self,
        specifier: &str,
        from_path: &str,
    ) -> Result<Option<String>, HostServiceError> {
        self.local_bridge
            .resolve_module(specifier, from_path, ModuleResolveMode::Require)
    }

    pub fn module_format(&mut self, path: &str) -> Option<&'static str> {
        self.local_bridge
            .module_format(path)
            .expect("host module filesystem format lookup must succeed")
            .map(LocalResolvedModuleFormat::as_str)
    }
}

#[doc(hidden)]
pub fn handle_internal_bridge_call_from_host_context(
    host_cwd: &Path,
    guest_cwd: &str,
    env: &BTreeMap<String, String>,
    method: &str,
    args: &[Value],
) -> Result<Option<Value>, HostServiceError> {
    // default + in-place assign: LocalBridgeState is Drop, so `..default()` (E0509)
    // is not allowed.
    let mut local_bridge = LocalBridgeState::default();
    local_bridge.translator =
        GuestPathTranslator::from_host_context(env, host_cwd.to_path_buf(), guest_cwd.to_owned());

    match local_bridge.handle_internal_bridge_call(0, method, args) {
        Some(LocalBridgeCallResult::Immediate(value)) => Ok(Some(value)),
        Some(LocalBridgeCallResult::Error(error)) => Err(error),
        _ => Ok(None),
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
    parse_guest_path_mappings_from_env(&request.env)
}

fn parse_guest_path_mappings_from_env(env: &BTreeMap<String, String>) -> Vec<GuestPathMapping> {
    env.get(NODE_GUEST_PATH_MAPPINGS_ENV)
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
    if prefix == "/" {
        return path.strip_prefix('/');
    }
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
                "__agentOSType": "bytes",
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
                "__agentOSType": "bytes",
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
        Value::Object(map) if map.get("__agentOSType").and_then(Value::as_str) == Some("bytes") => {
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

fn decode_bridge_output_arg(value: &Value) -> Vec<u8> {
    match value {
        Value::String(s) => s.as_bytes().to_vec(),
        Value::Object(map)
            if map.get("__type").and_then(Value::as_str) == Some("Buffer")
                || map.get("__agentOSType").and_then(Value::as_str) == Some("bytes") =>
        {
            let base64_value = map
                .get("data")
                .or_else(|| map.get("base64"))
                .and_then(Value::as_str);
            if let Some(base64_value) = base64_value {
                if let Some(bytes) = v8_runtime::base64_decode_pub(base64_value) {
                    return bytes;
                }
            }
            value.to_string().into_bytes()
        }
        other => other.to_string().into_bytes(),
    }
}

fn decode_bridge_output_args(args: &[Value]) -> Vec<u8> {
    let mut output = Vec::new();
    for (index, arg) in args.iter().enumerate() {
        if index > 0 {
            output.push(b' ');
        }
        output.extend(decode_bridge_output_arg(arg));
    }
    output
}

#[derive(Debug)]
pub enum JavascriptExecutionError {
    EmptyArgv,
    InvalidLimit(String),
    MissingContext(String),
    VmMismatch { expected: String, found: String },
    PrepareImportCache(std::io::Error),
    Spawn(std::io::Error),
    PendingSyncRpcRequest(u64),
    PendingSyncRpcLimit { limit: usize, observed: usize },
    ExpiredSyncRpcRequest(u64),
    RpcResponse(String),
    BridgeSettlement(agentos_v8_runtime::host_call::BridgeSettlementError),
    Terminate(std::io::Error),
    Control(std::io::Error),
    StdinClosed,
    Stdin(std::io::Error),
    OutputBufferExceeded { stream: &'static str, limit: usize },
    EventChannelClosed,
}

impl fmt::Display for JavascriptExecutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyArgv => f.write_str("guest JavaScript execution requires argv[0]"),
            Self::InvalidLimit(message) => write!(f, "invalid JavaScript limit: {message}"),
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
            Self::PendingSyncRpcLimit { limit, observed } => write!(
                f,
                "ERR_AGENTOS_RESOURCE_LIMIT: pending JavaScript sync RPC calls observed {observed}, exceeding limits.reactor.maxBridgeCalls ({limit})"
            ),
            Self::ExpiredSyncRpcRequest(id) => {
                write!(f, "sync RPC request {id} is no longer pending")
            }
            Self::RpcResponse(message) => {
                write!(
                    f,
                    "failed to reply to guest JavaScript sync RPC request: {message}"
                )
            }
            Self::BridgeSettlement(error) => {
                write!(f, "failed to settle guest JavaScript bridge response: {error}")
            }
            Self::Terminate(err) => {
                write!(f, "failed to terminate guest JavaScript runtime: {err}")
            }
            Self::Control(err) => write!(f, "failed to control guest JavaScript runtime: {err}"),
            Self::StdinClosed => f.write_str("guest JavaScript stdin is already closed"),
            Self::Stdin(err) => write!(f, "failed to write guest stdin: {err}"),
            Self::OutputBufferExceeded { stream, limit } => {
                write!(
                    f,
                    "guest JavaScript {stream} exceeded the captured output limit of {limit} bytes"
                )
            }
            Self::EventChannelClosed => {
                f.write_str("guest JavaScript event channel closed unexpectedly")
            }
        }
    }
}

impl std::error::Error for JavascriptExecutionError {}

fn javascript_bridge_response_error(error: std::io::Error) -> JavascriptExecutionError {
    if let Some(settlement) = error.get_ref().and_then(|source| {
        source.downcast_ref::<agentos_v8_runtime::host_call::BridgeSettlementError>()
    }) {
        return JavascriptExecutionError::BridgeSettlement(settlement.clone());
    }
    JavascriptExecutionError::RpcResponse(error.to_string())
}

#[derive(Debug)]
pub struct JavascriptExecution {
    execution_id: String,
    child_pid: u32,
    // One bounded mailbox supports both the async sidecar pump and standalone
    // blocking consumers. Using a runtime-specific receiver here previously
    // forced blocking compatibility paths through Handle::block_on, which
    // panicked whenever those paths were reached from the unified runtime.
    events: EventReceiver<JavascriptExecutionEvent>,
    pending_sync_rpc: Arc<Mutex<PendingSyncRpcRegistry>>,
    exited: Arc<AtomicBool>,
    termination_requested: AtomicBool,
    kernel_stdin: Arc<LocalKernelStdinBridge>,
    _import_cache_guard: Arc<NodeImportCacheCleanup>,
    v8_session: V8SessionHandle,
    session_destroyed: bool,
    /// Fully prepared V8 execute request. Cross-runtime execve prepares the
    /// replacement isolate and its bridge before committing kernel process
    /// state, but must not enqueue guest code until that commit is complete.
    prepared_execute: Option<PreparedJavascriptExecute>,
    _event_bridge_task: tokio::task::JoinHandle<()>,
    /// Host-direct module resolver state, used ONLY by the standalone `wait()`
    /// loop. The real VM runtime resolves modules against the kernel VFS on the
    /// sidecar service loop and never reaches this; but `wait()` runs without a
    /// kernel (dev/test harness), so it services module-resolution sync RPCs
    /// host-directly from the request's path translator.
    module_resolution: Mutex<(GuestPathTranslator, LocalModuleResolutionCache)>,
}

#[derive(Debug)]
struct PreparedJavascriptExecute {
    mode: u8,
    file_path: String,
    bridge_code: String,
    post_restore_script: String,
    userland_code: String,
    high_resolution_time: bool,
    user_code: String,
    wasm_module_bytes: Option<Arc<Vec<u8>>>,
}

impl JavascriptExecution {
    pub fn execution_id(&self) -> &str {
        &self.execution_id
    }

    pub fn native_process_id(&self) -> Option<u32> {
        (self.child_pid != 0).then_some(self.child_pid)
    }

    pub fn v8_session_handle(&self) -> V8SessionHandle {
        self.v8_session.clone()
    }

    pub fn sync_rpc_responder(&self) -> JavascriptSyncRpcResponder {
        JavascriptSyncRpcResponder {
            pending_sync_rpc: Arc::clone(&self.pending_sync_rpc),
            v8_session: self.v8_session.clone(),
        }
    }

    pub fn has_exited(&self) -> bool {
        self.exited.load(Ordering::Acquire)
    }

    /// Run another sidecar-managed operation in this execution's retained V8
    /// context. Public clients submit semantic language requests; only the
    /// sidecar calls this executor primitive.
    pub fn execute_retained(
        &mut self,
        user_code: String,
        file_path: String,
        module: bool,
    ) -> Result<(), JavascriptExecutionError> {
        self.exited.store(false, Ordering::Release);
        self.kernel_stdin.reset();
        self.v8_session
            .execute(
                2 | u8::from(module),
                file_path,
                String::new(),
                String::new(),
                String::new(),
                false,
                user_code,
                None,
            )
            .map_err(JavascriptExecutionError::Spawn)
    }

    /// Enqueue a replacement image that was fully prepared without running
    /// guest code. This is the final step of an atomic cross-runtime execve and
    /// must only be called after the kernel and sidecar process state commit.
    pub fn start_prepared(&mut self) -> Result<(), JavascriptExecutionError> {
        let prepared = self.prepared_execute.take().ok_or_else(|| {
            JavascriptExecutionError::Spawn(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "JavaScript execution is not awaiting a prepared start",
            ))
        })?;
        self.v8_session
            .execute(
                prepared.mode,
                prepared.file_path,
                prepared.bridge_code,
                prepared.post_restore_script,
                prepared.userland_code,
                prepared.high_resolution_time,
                prepared.user_code,
                prepared.wasm_module_bytes,
            )
            .map_err(JavascriptExecutionError::Spawn)
    }

    #[doc(hidden)]
    pub fn is_prepared_for_start(&self) -> bool {
        self.prepared_execute.is_some()
    }

    pub fn write_stdin(&mut self, chunk: &[u8]) -> Result<(), JavascriptExecutionError> {
        self.kernel_stdin.write(chunk)?;
        let payload = v8_runtime::json_to_cbor_payload(&json!({
            "dataBase64": v8_runtime::base64_encode_pub(chunk),
        }))
        .map_err(JavascriptExecutionError::Stdin)?;
        self.v8_session
            .send_stream_event("stdin", payload)
            .map_err(JavascriptExecutionError::Stdin)
    }

    pub fn close_stdin(&mut self) -> Result<(), JavascriptExecutionError> {
        self.kernel_stdin.close();
        self.v8_session
            .send_stream_event("stdin_end", vec![])
            .map_err(JavascriptExecutionError::Stdin)
    }

    pub(crate) fn write_kernel_stdin_only(
        &mut self,
        chunk: &[u8],
    ) -> Result<(), JavascriptExecutionError> {
        self.kernel_stdin.write(chunk)
    }

    pub(crate) fn close_kernel_stdin_only(&mut self) {
        self.kernel_stdin.close();
    }

    pub fn read_kernel_stdin_sync_rpc(
        &self,
        request: &HostRpcRequest,
    ) -> Result<Value, JavascriptExecutionError> {
        if request.method != "__kernel_stdin_read" {
            return Ok(Value::Null);
        }

        Ok(self.kernel_stdin.read(&request.args))
    }

    pub(crate) fn handle_kernel_stdin_sync_rpc(
        &mut self,
        request: &HostRpcRequest,
    ) -> Result<bool, JavascriptExecutionError> {
        if request.method != "__kernel_stdin_read" {
            return Ok(false);
        }

        let response = self.kernel_stdin.read(&request.args);
        self.respond_sync_rpc_success(request.id, response)?;
        Ok(true)
    }

    pub fn terminate(&self) -> Result<(), JavascriptExecutionError> {
        // Kernel control delivery, explicit cleanup, and the terminal frame can
        // race. Exactly one path may request isolate termination; later
        // idempotent shutdown attempts preserve the first result instead of
        // enqueueing a late TerminateExecution diagnostic into guest stderr.
        if self.has_exited() || self.termination_requested.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let result = self
            .v8_session
            .terminate()
            .map_err(JavascriptExecutionError::Terminate);
        if result.is_err() {
            self.termination_requested.store(false, Ordering::Release);
        }
        result
    }

    pub fn pause(&self) -> Result<(), JavascriptExecutionError> {
        self.v8_session
            .pause()
            .map_err(JavascriptExecutionError::Control)
    }

    pub fn resume(&self) -> Result<(), JavascriptExecutionError> {
        self.v8_session
            .resume()
            .map_err(JavascriptExecutionError::Control)
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
        self.sync_rpc_responder().respond_success(id, result)
    }

    /// Atomically claim the exact pending sync RPC before a caller performs a
    /// destructive operation on its behalf. A timed-out or replaced request
    /// must not consume bytes that belong to the guest's next retry.
    pub fn claim_sync_rpc_response(&mut self, id: u64) -> Result<bool, JavascriptExecutionError> {
        self.sync_rpc_responder().claim(id)
    }

    pub fn respond_claimed_sync_rpc_success(
        &mut self,
        id: u64,
        result: Value,
    ) -> Result<(), JavascriptExecutionError> {
        self.sync_rpc_responder()
            .respond_claimed_success(id, result)
    }

    pub fn respond_sync_rpc_raw_success(
        &mut self,
        id: u64,
        payload: Vec<u8>,
    ) -> Result<(), JavascriptExecutionError> {
        self.sync_rpc_responder().respond_raw_success(id, payload)
    }

    pub fn respond_sync_rpc_error(
        &mut self,
        id: u64,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<(), JavascriptExecutionError> {
        self.sync_rpc_responder().respond_error(id, code, message)
    }

    pub fn respond_claimed_sync_rpc_error(
        &mut self,
        id: u64,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<(), JavascriptExecutionError> {
        self.sync_rpc_responder()
            .respond_claimed_error(id, code, message)
    }

    pub async fn poll_event(
        &self,
        timeout: Duration,
    ) -> Result<Option<JavascriptExecutionEvent>, JavascriptExecutionError> {
        self.poll_event_until(Some(timeout)).await
    }

    /// Probe the durable event queue without registering or discarding a
    /// waker. The sidecar calls this after the execution engine has notified
    /// its coalesced process-event broker.
    pub fn try_poll_event(
        &self,
    ) -> Result<Option<JavascriptExecutionEvent>, JavascriptExecutionError> {
        match self.events.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(flume::TryRecvError::Empty) => Ok(None),
            Err(flume::TryRecvError::Disconnected) => {
                Err(JavascriptExecutionError::EventChannelClosed)
            }
        }
    }

    /// Wait for one event until an optional operation deadline. `None` is a
    /// true readiness wait; it does not install a recurring adapter timer.
    pub async fn poll_event_until(
        &self,
        timeout: Option<Duration>,
    ) -> Result<Option<JavascriptExecutionEvent>, JavascriptExecutionError> {
        if timeout.is_some_and(|timeout| timeout.is_zero()) {
            return match self.events.try_recv() {
                Ok(event) => Ok(Some(event)),
                Err(flume::TryRecvError::Empty) => Ok(None),
                Err(flume::TryRecvError::Disconnected) => {
                    Err(JavascriptExecutionError::EventChannelClosed)
                }
            };
        }

        match timeout {
            Some(timeout) => match time::timeout(timeout, self.events.recv_async()).await {
                Ok(Ok(event)) => Ok(Some(event)),
                Ok(Err(_closed)) => Err(JavascriptExecutionError::EventChannelClosed),
                Err(_) => Ok(None),
            },
            None => self
                .events
                .recv_async()
                .await
                .map(Some)
                .map_err(|_| JavascriptExecutionError::EventChannelClosed),
        }
    }

    pub fn poll_event_blocking(
        &self,
        timeout: Duration,
    ) -> Result<Option<JavascriptExecutionEvent>, JavascriptExecutionError> {
        match self.events.recv_timeout(timeout) {
            Ok(event) => Ok(Some(event)),
            Err(flume::RecvTimeoutError::Timeout) => Ok(None),
            Err(flume::RecvTimeoutError::Disconnected) => {
                Err(JavascriptExecutionError::EventChannelClosed)
            }
        }
    }

    /// Block until the next execution event without a recurring timeout poll.
    /// Adapters that have no deadline use this path so an idle guest consumes
    /// no scheduler turns while it waits for readiness or completion.
    pub(crate) fn next_event_blocking(
        &self,
    ) -> Result<JavascriptExecutionEvent, JavascriptExecutionError> {
        self.events
            .recv()
            .map_err(|_| JavascriptExecutionError::EventChannelClosed)
    }

    pub fn wait(mut self) -> Result<JavascriptExecutionResult, JavascriptExecutionError> {
        self.close_stdin()?;
        let execution_id = std::mem::take(&mut self.execution_id);

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        loop {
            match self.events.recv() {
                Ok(JavascriptExecutionEvent::Stdout(chunk)) => {
                    append_captured_output(&mut stdout, chunk, "stdout")?;
                }
                Ok(JavascriptExecutionEvent::Stderr(chunk)) => {
                    append_captured_output(&mut stderr, chunk, "stderr")?;
                }
                Ok(JavascriptExecutionEvent::SyncRpcRequest(request)) => {
                    // The standalone engine has no kernel/service loop. Service
                    // module-resolution RPCs host-directly (the only FS source
                    // available here) so `wait()` does not deadlock; everything
                    // else is unsupported off the VM path.
                    if self.try_service_standalone_module_sync_rpc(&request)? {
                        continue;
                    }
                    return Err(JavascriptExecutionError::PendingSyncRpcRequest(request.id));
                }
                Ok(JavascriptExecutionEvent::SignalState { .. }) => {}
                Ok(JavascriptExecutionEvent::Exited(exit_code)) => {
                    // Join the V8 executor while this method still owns the
                    // event receiver. That keeps terminal diagnostics
                    // drainable; waiting for Drop would close this local
                    // receiver first during return-value teardown.
                    self.v8_session
                        .destroy()
                        .map_err(JavascriptExecutionError::Terminate)?;
                    self.session_destroyed = true;
                    return Ok(JavascriptExecutionResult {
                        execution_id,
                        exit_code,
                        stdout,
                        stderr,
                    });
                }
                Err(_closed) => return Err(JavascriptExecutionError::EventChannelClosed),
            }
        }
    }

    /// Service a module-resolution sync RPC host-directly, for consumers that
    /// drive the V8 bridge without a kernel/service loop (the standalone
    /// `wait()` loop and the Python/WASM prewarm loops). Uses this execution's
    /// own path translator (captured at start, including any runtime path
    /// mappings) and a persistent cache. Returns `Ok(true)` if the request was a
    /// module method and was answered, `Ok(false)` if it should fall through.
    ///
    /// The real VM runtime resolves modules against the kernel VFS on the
    /// sidecar service loop and never calls this.
    pub fn try_service_standalone_module_sync_rpc(
        &mut self,
        request: &HostRpcRequest,
    ) -> Result<bool, JavascriptExecutionError> {
        if !matches!(
            request.method.as_str(),
            "__resolve_module"
                | "_resolveModule"
                | "_resolveModuleSync"
                | "__load_file"
                | "_loadFile"
                | "_loadFileSync"
                | "__module_format"
                | "_moduleFormat"
                | "__batch_resolve_modules"
                | "_batchResolveModules"
        ) {
            return Ok(false);
        }
        let result: Result<Value, HostServiceError> = {
            let mut guard = self.module_resolution.lock().map_err(|_| {
                JavascriptExecutionError::RpcResponse(String::from(
                    "standalone module resolution state poisoned",
                ))
            })?;
            let (translator, cache) = &mut *guard;
            let mut resolver = ModuleResolver::new(translator, cache);
            (|| match request.method.as_str() {
                "__resolve_module" | "_resolveModule" | "_resolveModuleSync" => {
                    let specifier = request.args.first().and_then(Value::as_str).unwrap_or("");
                    let parent = request.args.get(1).and_then(Value::as_str).unwrap_or("/");
                    let mode = match request.args.get(2).and_then(Value::as_str) {
                        Some("import") => ModuleResolveMode::Import,
                        Some("require") => ModuleResolveMode::Require,
                        _ if request.method == "_resolveModuleSync" => ModuleResolveMode::Require,
                        _ => ModuleResolveMode::Import,
                    };
                    Ok(resolver
                        .resolve_module(specifier, parent, mode)?
                        .map(Value::String)
                        .unwrap_or(Value::Null))
                }
                "__load_file" | "_loadFile" | "_loadFileSync" => Ok(resolver
                    .load_file(request.args.first().and_then(Value::as_str).unwrap_or(""))?
                    .map(Value::String)
                    .unwrap_or(Value::Null)),
                "__module_format" | "_moduleFormat" => Ok(resolver
                    .module_format(request.args.first().and_then(Value::as_str).unwrap_or(""))?
                    .map(|format| Value::String(String::from(format.as_str())))
                    .unwrap_or(Value::Null)),
                "__batch_resolve_modules" | "_batchResolveModules" => {
                    resolver.batch_resolve_modules(&request.args)
                }
                _ => unreachable!("module method was checked before locking resolver state"),
            })()
        };
        match result {
            Ok(result) => self.respond_sync_rpc_success(request.id, result)?,
            Err(error) => self
                .sync_rpc_responder()
                .respond_host_error(request.id, error)?,
        }
        Ok(true)
    }
}

impl ExecutionBackend for JavascriptExecution {
    fn kind(&self) -> ExecutionBackendKind {
        ExecutionBackendKind::Javascript
    }

    fn native_process_id(&self) -> Option<u32> {
        JavascriptExecution::native_process_id(self)
    }

    fn wake_handle(&self, identity: ExecutionWakeIdentity) -> Option<ExecutionWakeHandle> {
        Some(ExecutionWakeHandle::new(
            identity,
            Arc::new(self.v8_session.clone()),
        ))
    }

    fn is_prepared_for_start(&self) -> bool {
        JavascriptExecution::is_prepared_for_start(self)
    }

    fn start_prepared(&mut self) -> Result<(), HostServiceError> {
        JavascriptExecution::start_prepared(self).map_err(|error| {
            HostServiceError::new("ERR_AGENTOS_EXECUTION_START", error.to_string())
        })
    }

    fn begin_shutdown(
        &mut self,
        reason: ShutdownReason,
    ) -> Result<ShutdownOutcome, HostServiceError> {
        if let ShutdownReason::Signal(signal) = reason {
            if let Some(process_id) = self.native_process_id() {
                return Ok(ShutdownOutcome::ForwardSignal { process_id, signal });
            }
            // A shared isolate has no host process whose wait status can
            // preserve the terminating signal. V8 termination only reports a
            // generic runtime exit, so publish the kernel-owned signal status
            // immediately after requesting isolate shutdown.
            self.terminate().map_err(|error| {
                HostServiceError::new("ERR_AGENTOS_EXECUTION_SHUTDOWN", error.to_string())
            })?;
            return Ok(ShutdownOutcome::Exited(ExecutionExit::Signaled {
                signal,
                core_dumped: false,
            }));
        }
        self.terminate().map_err(|error| {
            HostServiceError::new("ERR_AGENTOS_EXECUTION_SHUTDOWN", error.to_string())
        })?;
        Ok(ShutdownOutcome::AwaitExit)
    }

    fn set_paused(&self, paused: bool) -> Result<(), HostServiceError> {
        let result = if paused {
            JavascriptExecution::pause(self)
        } else {
            JavascriptExecution::resume(self)
        };
        result.map_err(|error| {
            HostServiceError::new("ERR_AGENTOS_EXECUTION_CONTROL", error.to_string())
        })
    }

    fn write_stdin(&mut self, bytes: &[u8]) -> Result<(), HostServiceError> {
        JavascriptExecution::write_stdin(self, bytes).map_err(|error| {
            HostServiceError::new("ERR_AGENTOS_EXECUTION_STDIN", error.to_string())
        })
    }

    fn close_stdin(&mut self) -> Result<(), HostServiceError> {
        JavascriptExecution::close_stdin(self).map_err(|error| {
            HostServiceError::new("ERR_AGENTOS_EXECUTION_STDIN", error.to_string())
        })
    }

    fn deliver_signal_checkpoint(
        &self,
        identity: ExecutionWakeIdentity,
        signal: i32,
        delivery_token: u64,
        _flags: u32,
        _thread_id: u32,
    ) -> Result<SignalCheckpointOutcome, HostServiceError> {
        let Some(wake) = self.wake_handle(identity) else {
            return Ok(if let Some(process_id) = self.native_process_id() {
                SignalCheckpointOutcome::ForwardToProcess { process_id }
            } else {
                SignalCheckpointOutcome::Unsupported
            });
        };
        wake.publish_signal(signal, delivery_token)
            .map_err(|error| HostServiceError::new(error.code(), error.to_string()))?;
        Ok(SignalCheckpointOutcome::Published)
    }
}

impl JavascriptSyncRpcResponder {
    pub fn claim(&self, id: u64) -> Result<bool, JavascriptExecutionError> {
        match clear_pending_sync_rpc(&self.pending_sync_rpc, id)? {
            PendingSyncRpcResolution::Pending => Ok(true),
            PendingSyncRpcResolution::TimedOut | PendingSyncRpcResolution::Missing => Ok(false),
        }
    }

    pub fn respond_success(&self, id: u64, result: Value) -> Result<(), JavascriptExecutionError> {
        let phase_start = Instant::now();
        match clear_pending_sync_rpc(&self.pending_sync_rpc, id)? {
            PendingSyncRpcResolution::Pending => {}
            PendingSyncRpcResolution::TimedOut => {
                return Err(JavascriptExecutionError::ExpiredSyncRpcRequest(id));
            }
            PendingSyncRpcResolution::Missing => {}
        }
        record_sync_bridge_phase(
            "sync_rpc_response",
            "response_clear_pending",
            phase_start.elapsed(),
        );
        self.respond_claimed_success(id, result)
    }

    pub fn respond_claimed_success(
        &self,
        id: u64,
        result: Value,
    ) -> Result<(), JavascriptExecutionError> {
        let phase_start = Instant::now();
        let payload = translate_legacy_bridge_value_to_v8(&result);
        record_sync_bridge_phase(
            "sync_rpc_response",
            "response_translate_value",
            phase_start.elapsed(),
        );
        let phase_start = Instant::now();
        let payload = v8_runtime::json_to_cbor_payload(&payload)
            .map_err(|error| JavascriptExecutionError::RpcResponse(error.to_string()))?;
        record_sync_bridge_phase(
            "sync_rpc_response",
            "response_encode_cbor",
            phase_start.elapsed(),
        );
        let phase_start = Instant::now();
        let result = self
            .v8_session
            .send_bridge_response(id, 0, payload)
            .map_err(javascript_bridge_response_error);
        record_sync_bridge_phase("sync_rpc_response", "response_send", phase_start.elapsed());
        result
    }

    pub fn respond_raw_success(
        &self,
        id: u64,
        payload: Vec<u8>,
    ) -> Result<(), JavascriptExecutionError> {
        let phase_start = Instant::now();
        match clear_pending_sync_rpc(&self.pending_sync_rpc, id)? {
            PendingSyncRpcResolution::Pending => {}
            PendingSyncRpcResolution::TimedOut => {
                return Err(JavascriptExecutionError::ExpiredSyncRpcRequest(id));
            }
            PendingSyncRpcResolution::Missing => {}
        }
        record_sync_bridge_phase(
            "sync_rpc_raw_response",
            "response_clear_pending",
            phase_start.elapsed(),
        );
        self.respond_claimed_raw_success(id, payload)
    }

    pub fn respond_claimed_raw_success(
        &self,
        id: u64,
        payload: Vec<u8>,
    ) -> Result<(), JavascriptExecutionError> {
        let phase_start = Instant::now();
        let result = self
            .v8_session
            .send_bridge_response(id, 2, payload)
            .map_err(javascript_bridge_response_error);
        record_sync_bridge_phase(
            "sync_rpc_raw_response",
            "response_send",
            phase_start.elapsed(),
        );
        result
    }

    pub fn respond_error(
        &self,
        id: u64,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<(), JavascriptExecutionError> {
        self.respond_host_error(id, HostServiceError::new(code.into(), message.into()))
    }

    pub fn respond_host_error(
        &self,
        id: u64,
        error: HostServiceError,
    ) -> Result<(), JavascriptExecutionError> {
        match clear_pending_sync_rpc(&self.pending_sync_rpc, id)? {
            PendingSyncRpcResolution::Pending => {}
            PendingSyncRpcResolution::TimedOut => {
                return Err(JavascriptExecutionError::ExpiredSyncRpcRequest(id));
            }
            PendingSyncRpcResolution::Missing => {}
        }
        self.respond_claimed_host_error(id, error)
    }

    pub fn respond_claimed_error(
        &self,
        id: u64,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<(), JavascriptExecutionError> {
        self.respond_claimed_host_error(id, HostServiceError::new(code.into(), message.into()))
    }

    pub(crate) fn respond_claimed_host_error(
        &self,
        id: u64,
        error: HostServiceError,
    ) -> Result<(), JavascriptExecutionError> {
        let payload = encode_host_service_error_payload(&error)?;
        self.v8_session
            .send_bridge_response(id, 1, payload)
            .map_err(javascript_bridge_response_error)
    }
}

impl DirectHostReplyTarget for JavascriptSyncRpcResponder {
    fn claim(&self, call_id: u64) -> Result<bool, HostServiceError> {
        JavascriptSyncRpcResponder::claim(self, call_id).map_err(host_reply_adapter_error)
    }

    fn respond(
        &self,
        call_id: u64,
        claimed: bool,
        result: Result<HostCallReply, HostServiceError>,
    ) -> Result<(), HostServiceError> {
        let response = match result {
            Ok(HostCallReply::Empty) if claimed => {
                self.respond_claimed_success(call_id, Value::Null)
            }
            Ok(HostCallReply::Empty) => self.respond_success(call_id, Value::Null),
            Ok(HostCallReply::Json(value)) if claimed => {
                self.respond_claimed_success(call_id, value)
            }
            Ok(HostCallReply::Json(value)) => self.respond_success(call_id, value),
            Ok(HostCallReply::Raw(bytes)) if claimed => {
                self.respond_claimed_raw_success(call_id, bytes)
            }
            Ok(HostCallReply::Raw(bytes)) => self.respond_raw_success(call_id, bytes),
            Err(error) => {
                if claimed {
                    self.respond_claimed_host_error(call_id, error)
                } else {
                    self.respond_host_error(call_id, error)
                }
            }
        };
        map_host_reply_adapter_response(response)
    }

    fn dismiss_claimed(&self, _call_id: u64) -> Result<(), HostServiceError> {
        // `claim` already removed the exact pending request. A successful
        // exec replaces or destroys the old image, so sending a bridge reply
        // would incorrectly resume instructions following execve(2).
        Ok(())
    }
}

fn clear_pending_sync_rpc(
    pending_sync_rpc: &Arc<Mutex<PendingSyncRpcRegistry>>,
    id: u64,
) -> Result<PendingSyncRpcResolution, JavascriptExecutionError> {
    let mut pending = pending_sync_rpc.lock().map_err(|_| {
        JavascriptExecutionError::RpcResponse(String::from(
            "sync RPC pending-request state lock poisoned",
        ))
    })?;
    let resolution = match pending.states.remove(&id) {
        Some(PendingSyncRpcState::Pending(_)) => PendingSyncRpcResolution::Pending,
        Some(PendingSyncRpcState::TimedOut(_)) => PendingSyncRpcResolution::TimedOut,
        None => PendingSyncRpcResolution::Missing,
    };
    pending.observe_depth();
    Ok(resolution)
}

pub(crate) fn host_reply_adapter_error(error: JavascriptExecutionError) -> HostServiceError {
    match error {
        JavascriptExecutionError::PendingSyncRpcRequest(id) => HostServiceError::new(
            "EBUSY",
            JavascriptExecutionError::PendingSyncRpcRequest(id).to_string(),
        )
        .with_details(json!({ "callId": id })),
        JavascriptExecutionError::PendingSyncRpcLimit { limit, observed } => {
            HostServiceError::limit(
                "ERR_AGENTOS_RESOURCE_LIMIT",
                "limits.reactor.maxBridgeCalls",
                limit as u64,
                observed as u64,
            )
        }
        JavascriptExecutionError::ExpiredSyncRpcRequest(id) => HostServiceError::new(
            "ESTALE",
            JavascriptExecutionError::ExpiredSyncRpcRequest(id).to_string(),
        )
        .with_details(json!({ "callId": id })),
        other => HostServiceError::new("ERR_AGENTOS_ADAPTER_REPLY", other.to_string()),
    }
}

pub(crate) fn map_host_reply_adapter_response(
    response: Result<(), JavascriptExecutionError>,
) -> Result<(), HostServiceError> {
    match response {
        Err(JavascriptExecutionError::BridgeSettlement(error))
            if error.kind()
                == agentos_v8_runtime::host_call::BridgeSettlementErrorKind::StaleCompletion =>
        {
            // The V8 registry emits this marker only after proving that this
            // exact session generation owned a host-visible route which was
            // canceled before its claimed completion arrived. Unknown call IDs
            // and mismatched generations remain fatal and take the branch below.
            eprintln!("INFO_AGENTOS_STALE_BRIDGE_COMPLETION: {error}");
            Ok(())
        }
        Err(error) => Err(host_reply_adapter_error(error)),
        Ok(()) => Ok(()),
    }
}

fn encode_host_service_error_payload(
    error: &HostServiceError,
) -> Result<Vec<u8>, JavascriptExecutionError> {
    v8_runtime::json_to_cbor_payload(&serde_json::json!({
        "code": error.code,
        "message": error.message,
        "details": error.details,
    }))
    .map_err(|encode_error| JavascriptExecutionError::RpcResponse(encode_error.to_string()))
}

fn decode_bridge_call_args(
    method: &str,
    call_id: u64,
    payload: &[u8],
) -> Result<Vec<Value>, HostServiceError> {
    v8_runtime::cbor_payload_to_json_args(payload).map_err(|error| {
        HostServiceError::new(
            "ERR_AGENTOS_BRIDGE_REQUEST_DECODE",
            format!("failed to decode bridge request {method} for call_id={call_id}: {error}"),
        )
    })
}

impl Drop for JavascriptExecution {
    fn drop(&mut self) {
        // Closing the V8 producer lets the bridge task drain any terminal
        // warning/result and then finish when its per-session lane closes.
        // Aborting the task first would drop the lane while the session thread
        // was still completing teardown.
        if self.session_destroyed {
            return;
        }
        if let Err(error) = self.v8_session.destroy() {
            eprintln!(
                "ERR_AGENTOS_JAVASCRIPT_SESSION_TEARDOWN: failed to destroy V8 session during execution drop: {error}"
            );
        }
    }
}

fn append_captured_output(
    target: &mut Vec<u8>,
    chunk: Vec<u8>,
    stream: &'static str,
) -> Result<(), JavascriptExecutionError> {
    let next_len = target.len().checked_add(chunk.len()).ok_or(
        JavascriptExecutionError::OutputBufferExceeded {
            stream,
            limit: JAVASCRIPT_CAPTURED_OUTPUT_LIMIT_BYTES,
        },
    )?;
    if next_len > JAVASCRIPT_CAPTURED_OUTPUT_LIMIT_BYTES {
        return Err(JavascriptExecutionError::OutputBufferExceeded {
            stream,
            limit: JAVASCRIPT_CAPTURED_OUTPUT_LIMIT_BYTES,
        });
    }

    target.extend(chunk);
    Ok(())
}

struct V8SessionRegistrationGuard<'a> {
    v8_host: &'a V8RuntimeHost,
    session_id: String,
    active: bool,
}

impl<'a> V8SessionRegistrationGuard<'a> {
    fn new(v8_host: &'a V8RuntimeHost, session_id: String) -> Self {
        Self {
            v8_host,
            session_id,
            active: true,
        }
    }

    fn disarm(&mut self) {
        self.active = false;
    }
}

impl Drop for V8SessionRegistrationGuard<'_> {
    fn drop(&mut self) {
        if self.active {
            self.v8_host.unregister_session(&self.session_id);
        }
    }
}

struct PendingV8SessionRegistration<'a> {
    frame_receiver: V8SessionFrameReceiver,
    registration_guard: V8SessionRegistrationGuard<'a>,
}

#[allow(clippy::too_many_arguments)] // one session's identity, limits, hint, and creation hook
fn register_v8_session<'a, F>(
    v8_host: &'a V8RuntimeHost,
    runtime: &RuntimeContext,
    session_id: String,
    heap_limit_mb: u32,
    cpu_time_limit_ms: u32,
    wall_clock_limit_ms: u32,
    warm_hint: Option<WarmSessionHint>,
    create_session: F,
) -> Result<PendingV8SessionRegistration<'a>, JavascriptExecutionError>
where
    F: FnOnce(RuntimeCommand) -> std::io::Result<()>,
{
    let frame_receiver = v8_host
        .register_session(&session_id, runtime)
        .map_err(JavascriptExecutionError::Spawn)?;
    let registration_guard = V8SessionRegistrationGuard::new(v8_host, session_id.clone());

    create_session(RuntimeCommand::CreateSession {
        session_id,
        heap_limit_mb: (heap_limit_mb > 0).then_some(heap_limit_mb),
        cpu_time_limit_ms: (cpu_time_limit_ms > 0).then_some(cpu_time_limit_ms),
        wall_clock_limit_ms: (wall_clock_limit_ms > 0).then_some(wall_clock_limit_ms),
        warm_hint,
    })
    .map_err(JavascriptExecutionError::Spawn)?;

    Ok(PendingV8SessionRegistration {
        frame_receiver,
        registration_guard,
    })
}

pub struct JavascriptExecutionEngine {
    runtime: Option<RuntimeContext>,
    next_context_id: usize,
    next_execution_id: usize,
    contexts: BTreeMap<String, JavascriptContext>,
    import_caches: BTreeMap<String, NodeImportCache>,
    v8_host: Option<V8RuntimeHost>,
    event_notify: Option<Arc<Notify>>,
}

impl Default for JavascriptExecutionEngine {
    fn default() -> Self {
        Self {
            runtime: default_test_runtime_context(),
            next_context_id: 0,
            next_execution_id: 0,
            contexts: BTreeMap::new(),
            import_caches: BTreeMap::new(),
            v8_host: None,
            event_notify: None,
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
    pub fn new(runtime: RuntimeContext) -> Self {
        Self {
            runtime: Some(runtime),
            ..Self::default()
        }
    }

    /// Bind this engine to the process-owned runtime before starting work.
    /// This setter exists for embedders that previously constructed via
    /// `Default`; new code should prefer [`Self::new`].
    pub fn set_runtime_context(&mut self, runtime: RuntimeContext) {
        self.runtime = Some(runtime);
    }

    pub(crate) fn runtime_context(&self) -> Result<&RuntimeContext, JavascriptExecutionError> {
        self.runtime.as_ref().ok_or_else(|| {
            JavascriptExecutionError::Spawn(std::io::Error::other(
                "ERR_AGENTOS_RUNTIME_NOT_INJECTED: JavascriptExecutionEngine requires a process RuntimeContext; construct it with JavascriptExecutionEngine::new(runtime)",
            ))
        })
    }

    #[doc(hidden)]
    pub fn set_event_notify(&mut self, notify: Option<Arc<Notify>>) {
        self.event_notify = notify;
    }

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

    /// Dispose an execution context once its final start/prepare operation has
    /// consumed the metadata. Live executions own their resolved runtime state
    /// independently and do not consult this registry after creation.
    pub fn dispose_context(&mut self, context_id: &str) -> bool {
        self.contexts.remove(context_id).is_some()
    }

    #[doc(hidden)]
    pub fn context_count_for_test(&self) -> usize {
        self.contexts.len()
    }

    pub fn start_execution(
        &mut self,
        request: StartJavascriptExecutionRequest,
    ) -> Result<JavascriptExecution, JavascriptExecutionError> {
        let runtime = self.runtime_context()?.clone();
        self.start_execution_with_runtime(request, runtime)
    }

    pub fn start_execution_with_runtime(
        &mut self,
        request: StartJavascriptExecutionRequest,
        runtime: RuntimeContext,
    ) -> Result<JavascriptExecution, JavascriptExecutionError> {
        self.create_execution_with_module_reader_and_runtime(request, None, None, runtime, false)
    }

    pub fn prepare_execution(
        &mut self,
        request: StartJavascriptExecutionRequest,
    ) -> Result<JavascriptExecution, JavascriptExecutionError> {
        let runtime = self.runtime_context()?.clone();
        self.prepare_execution_with_runtime(request, runtime)
    }

    /// Prepare an execution with an explicitly scoped runtime without enqueueing
    /// guest code. Cross-runtime exec uses this to bind the replacement isolate
    /// to the target VM's accounting and reactor state before committing execve.
    pub fn prepare_execution_with_runtime(
        &mut self,
        request: StartJavascriptExecutionRequest,
        runtime: RuntimeContext,
    ) -> Result<JavascriptExecution, JavascriptExecutionError> {
        self.create_execution_with_module_reader_and_runtime(request, None, None, runtime, true)
    }

    fn ensure_v8_host(&mut self) -> Result<(), JavascriptExecutionError> {
        let should_spawn_v8_host = match self.v8_host.as_mut() {
            Some(v8_host) => !v8_host
                .is_alive()
                .map_err(JavascriptExecutionError::Spawn)?,
            None => true,
        };
        if should_spawn_v8_host {
            let runtime = self.runtime_context()?.clone();
            self.v8_host =
                Some(V8RuntimeHost::spawn(&runtime).map_err(JavascriptExecutionError::Spawn)?);
        }
        Ok(())
    }

    pub(crate) fn snapshot_userland_ready(
        &mut self,
        userland_code: &str,
    ) -> Result<bool, JavascriptExecutionError> {
        self.ensure_v8_host()?;
        Ok(self
            .v8_host
            .as_ref()
            .expect("V8 host initialized")
            .snapshot_ready(userland_code))
    }

    pub(crate) fn pre_warm_snapshot(
        &mut self,
        userland_code: &str,
    ) -> Result<(), JavascriptExecutionError> {
        self.ensure_v8_host()?;
        self.v8_host
            .as_ref()
            .expect("V8 host initialized")
            .pre_warm_snapshot(userland_code)
            .map_err(JavascriptExecutionError::Spawn)
    }

    pub(crate) fn pre_warm_workers(
        &mut self,
        userland_code: &str,
        heap_limit_mb: u32,
        count: usize,
    ) -> Result<(), JavascriptExecutionError> {
        self.ensure_v8_host()?;
        self.v8_host
            .as_ref()
            .expect("V8 host initialized")
            .pre_warm_workers(userland_code, heap_limit_mb, count);
        Ok(())
    }

    /// Like [`start_execution`](Self::start_execution) but with an optional
    /// read-only VFS reader over the mounted `node_modules` tree. When supplied,
    /// the bridge thread resolves module-resolution RPCs inline against this
    /// reader (off the service loop, concurrently with it) instead of routing
    /// them through the service loop. The reader must be `Send` because it is
    /// moved onto the bridge thread; it must read the same mount the guest sees.
    pub fn start_execution_with_module_reader(
        &mut self,
        request: StartJavascriptExecutionRequest,
        module_reader: Option<Box<dyn ModuleFsReader + Send>>,
        guest_reader: Option<Box<dyn agentos_v8_runtime::execution::GuestModuleReader>>,
    ) -> Result<JavascriptExecution, JavascriptExecutionError> {
        let runtime = self.runtime_context()?.clone();
        self.create_execution_with_module_reader_and_runtime(
            request,
            module_reader,
            guest_reader,
            runtime,
            false,
        )
    }

    /// Prepare an execution through every fallible image-loading step without
    /// enqueueing guest code in V8. Used by execve when the replacement runtime
    /// differs from the current runtime.
    pub fn prepare_execution_with_module_reader(
        &mut self,
        request: StartJavascriptExecutionRequest,
        module_reader: Option<Box<dyn ModuleFsReader + Send>>,
        guest_reader: Option<Box<dyn agentos_v8_runtime::execution::GuestModuleReader>>,
    ) -> Result<JavascriptExecution, JavascriptExecutionError> {
        let runtime = self.runtime_context()?.clone();
        self.create_execution_with_module_reader_and_runtime(
            request,
            module_reader,
            guest_reader,
            runtime,
            true,
        )
    }

    pub fn start_execution_with_module_reader_and_runtime(
        &mut self,
        request: StartJavascriptExecutionRequest,
        module_reader: Option<Box<dyn ModuleFsReader + Send>>,
        guest_reader: Option<Box<dyn agentos_v8_runtime::execution::GuestModuleReader>>,
        runtime: RuntimeContext,
    ) -> Result<JavascriptExecution, JavascriptExecutionError> {
        self.create_execution_with_module_reader_and_runtime(
            request,
            module_reader,
            guest_reader,
            runtime,
            false,
        )
    }

    pub fn prepare_execution_with_module_reader_and_runtime(
        &mut self,
        request: StartJavascriptExecutionRequest,
        module_reader: Option<Box<dyn ModuleFsReader + Send>>,
        guest_reader: Option<Box<dyn agentos_v8_runtime::execution::GuestModuleReader>>,
        runtime: RuntimeContext,
    ) -> Result<JavascriptExecution, JavascriptExecutionError> {
        self.create_execution_with_module_reader_and_runtime(
            request,
            module_reader,
            guest_reader,
            runtime,
            true,
        )
    }

    fn create_execution_with_module_reader_and_runtime(
        &mut self,
        request: StartJavascriptExecutionRequest,
        module_reader: Option<Box<dyn ModuleFsReader + Send>>,
        guest_reader: Option<Box<dyn agentos_v8_runtime::execution::GuestModuleReader>>,
        runtime: RuntimeContext,
        defer_execute: bool,
    ) -> Result<JavascriptExecution, JavascriptExecutionError> {
        let process_runtime = self.runtime_context()?.clone();
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
        let reactor_work_quantum = javascript_reactor_work_quantum(&request, &runtime)?;
        let bridge_call_timeout = javascript_bridge_call_timeout(&request, &runtime)?;

        let phase_start = Instant::now();
        // Ensure import cache is materialized (still needed for module resolution)
        let import_cache = self.import_caches.entry(context.vm_id.clone()).or_default();
        import_cache
            .ensure_materialized_with_timeout_and_runtime(
                &process_runtime,
                javascript_import_cache_materialize_timeout(&request),
            )
            .map_err(JavascriptExecutionError::PrepareImportCache)?;
        let import_cache_guard = import_cache.cleanup_guard();
        record_js_start_phase("js_start_import_cache", phase_start.elapsed());

        self.next_execution_id += 1;
        let execution_id = format!("exec-{}", self.next_execution_id);

        let phase_start = Instant::now();
        self.ensure_v8_host()?;
        let v8_host = self.v8_host.as_ref().unwrap();
        record_js_start_phase("js_start_v8_host_ready", phase_start.elapsed());

        let phase_start = Instant::now();
        // Create a V8 session
        let session_id = format!(
            "v8-{execution_id}-{}",
            NEXT_V8_SESSION_ID.fetch_add(1, Ordering::Relaxed)
        );
        let heap_limit_mb = javascript_heap_limit_mb(&request);
        let cpu_time_limit_ms = javascript_cpu_time_limit_ms(&request);
        let wall_clock_limit_ms = javascript_wall_clock_limit_ms(&request);
        let snapshot_userland_code = request
            .guest_runtime
            .snapshot_userland_code
            .clone()
            .unwrap_or_default();
        let warm_hint = Some(WarmSessionHint {
            bridge_code: V8RuntimeHost::bridge_code().to_owned(),
            userland_code: snapshot_userland_code.clone(),
            heap_limit_mb: (heap_limit_mb > 0).then_some(heap_limit_mb),
        });
        if snapshot_userland_code.is_empty() && heap_limit_mb == 0 {
            v8_host.seed_default_warm_workers_async();
        }
        let PendingV8SessionRegistration {
            frame_receiver,
            mut registration_guard,
        } = register_v8_session(
            v8_host,
            &runtime,
            session_id.clone(),
            heap_limit_mb,
            cpu_time_limit_ms,
            wall_clock_limit_ms,
            warm_hint,
            |command| {
                v8_host.create_session_from_command_with_runtime(
                    command,
                    &runtime,
                    reactor_work_quantum,
                    bridge_call_timeout,
                )
            },
        )?;
        record_js_start_phase("js_start_v8_session_register", phase_start.elapsed());

        let phase_start = Instant::now();
        // Build user code: prefer inline code, fall back to entrypoint-based
        let translator = GuestPathTranslator::from_request(&request);
        let host_entrypoint = translator.resolve_host_entrypoint(&request.cwd, &request.argv[0]);
        let guest_entrypoint = if request.argv[0] == "-e" || request.argv[0] == "--eval" {
            request.argv[0].clone()
        } else if let Some(explicit_guest_entrypoint) = request
            .env
            .get(NODE_GUEST_ENTRYPOINT_ENV)
            .filter(|value| value.starts_with('/'))
        {
            // Part B (guest-VFS adapter launch): the sidecar already resolved the
            // GUEST entrypoint path (AGENTOS_GUEST_ENTRYPOINT). Use it directly as
            // the sourceURL / module-resolution base instead of translating the
            // host entrypoint — `host_to_guest_string` misses for guest-native
            // mounts (`agentos_packages`, whose host staging dir is not in the
            // translation map) and falls back to `/unknown/<cmd>`, which then
            // poisons the adapter's own relative/bare imports. For host-backed
            // mounts the two values are equal, so this is a no-op there. Applies
            // to child launches too (they set AGENTOS_GUEST_ENTRYPOINT as well),
            // which is the child-process `/unknown` case.
            explicit_guest_entrypoint.clone()
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
        // Node resolves relative imports from `node -e` against process.cwd().
        // Keep the guest-visible argv entry as `-e`, but give V8 an absolute
        // synthetic resource name so its dynamic-import callback has the same
        // resolution base instead of trying to resolve from the literal `-e`.
        let execution_file_path = if matches!(guest_entrypoint.as_str(), "-e" | "--eval") {
            request
                .env
                .get(NODE_INLINE_FILE_PATH_ENV)
                .cloned()
                .unwrap_or_else(|| {
                    let cwd = translator.guest_cwd().trim_end_matches('/');
                    if cwd.is_empty() {
                        String::from("/[eval]")
                    } else {
                        format!("{cwd}/[eval]")
                    }
                })
        } else {
            guest_entrypoint.clone()
        };
        let inline_code = request
            .inline_code
            .clone()
            .map(|inline_code| strip_javascript_hashbang(&inline_code));
        let explicit_module_mode = request
            .env
            .get(NODE_GUEST_ENTRYPOINT_MODULE_MODE_ENV)
            .map(|value| value == "1" || value.eq_ignore_ascii_case("true"));
        let use_module_mode = explicit_module_mode.unwrap_or_else(|| {
            host_entrypoint_uses_module_mode(&host_entrypoint)
                || inline_code
                    .as_deref()
                    .is_some_and(inline_code_uses_module_mode)
        });
        if !matches!(guest_entrypoint.as_str(), "-e" | "--eval") && !use_module_mode {
            if let Some(inline_code) = inline_code.as_ref() {
                if let Some(parent) = host_entrypoint.parent() {
                    fs::create_dir_all(parent)
                        .map_err(JavascriptExecutionError::PrepareImportCache)?;
                }
                fs::write(&host_entrypoint, inline_code)
                    .map_err(JavascriptExecutionError::PrepareImportCache)?;
            }
        }
        let user_code = if matches!(guest_entrypoint.as_str(), "-e" | "--eval") {
            inline_code.unwrap_or_else(|| build_v8_user_code(&guest_entrypoint, &request.env))
        } else if use_module_mode {
            if let Some(inline_code) = inline_code {
                format!("{inline_code}\n//# sourceURL={guest_entrypoint}")
            } else {
                strip_javascript_hashbang(&fs::read_to_string(&host_entrypoint).map_err(
                    |error| {
                        JavascriptExecutionError::PrepareImportCache(std::io::Error::new(
                            error.kind(),
                            format!(
                                "failed to read JavaScript entrypoint {}: {error}",
                                host_entrypoint.display()
                            ),
                        ))
                    },
                )?)
            }
        } else {
            build_v8_user_code(&guest_entrypoint, &request.env)
        };
        let post_restore_script = build_v8_runtime_init_script(
            &guest_entrypoint,
            &process_argv,
            request.argv0.as_deref(),
            translator.guest_cwd(),
            &request.env,
            heap_limit_mb,
            &request.guest_runtime,
        );
        record_js_start_phase("js_start_build_user_code", phase_start.elapsed());

        let phase_start = Instant::now();
        // Create session handle for sending bridge responses
        let v8_session = v8_host.session_handle(session_id.clone());

        // Start the event bridge before execution so early sync bridge calls
        // made during module instantiation/evaluation cannot deadlock waiting
        // for a response while no host thread is draining session frames yet.
        let max_pending_sync_rpcs = runtime
            .resources()
            .configured_limit(ResourceClass::BridgeCalls)
            .map_or(JAVASCRIPT_EVENT_CHANNEL_CAPACITY, |limit| limit.maximum);
        let pending_sync_rpc = Arc::new(Mutex::new(PendingSyncRpcRegistry::new(
            max_pending_sync_rpcs,
        )));
        let exited = Arc::new(AtomicBool::new(false));
        let kernel_stdin = Arc::new(LocalKernelStdinBridge::default());
        let standalone_translator = translator.clone();
        // default + in-place assign: LocalBridgeState is Drop, so `..Default::default()`
        // (E0509) is not allowed.
        let mut local_bridge = LocalBridgeState::default();
        local_bridge.runtime = Some(process_runtime);
        local_bridge.timer_resources = Some(Arc::clone(runtime.resources()));
        local_bridge.max_timers = javascript_max_timers(&request);
        local_bridge.translator = translator;
        local_bridge.kernel_stdin = kernel_stdin.clone();
        local_bridge.v8_session = Some(v8_session.clone());
        local_bridge.module_reader = module_reader;
        if local_bridge.module_reader.is_none()
            && !local_bridge
                .translator
                .explicit_mapping_guest_roots
                .is_empty()
        {
            // Production normally forwards module calls to the kernel when no
            // VFS reader is installed. Trusted runtime projections (npm,
            // Pyodide assets, and similar sidecar-created mappings) need a
            // narrowly scoped host fallback first. This always-missing reader
            // activates the layered resolver: explicit mappings resolve
            // locally, while every other miss still falls through to the
            // kernel service loop.
            local_bridge.module_reader = Some(Box::new(ForwardingModuleFsReader));
            local_bridge.module_reader_forwards_to_kernel = true;
        }
        local_bridge.module_resolution = GuestModuleResolution::from_env(&request.env);
        local_bridge.forward_kernel_stdin_rpc = request
            .env
            .get(FORWARD_KERNEL_STDIN_RPC_ENV)
            .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"));
        let (events, event_bridge_task) = spawn_v8_event_bridge(
            &runtime,
            frame_receiver,
            pending_sync_rpc.clone(),
            exited.clone(),
            v8_session.clone(),
            local_bridge,
            self.event_notify.clone(),
        )?;
        record_js_start_phase("js_start_event_bridge", phase_start.elapsed());

        let phase_start = Instant::now();
        // Install the direct module reader on the session thread BEFORE the Execute
        // frame so the SetModuleReader command (routed through the same dispatch
        // queue) arrives first; module loads then read source directly on the V8
        // thread instead of round-tripping the bridge.
        if let Some(guest_reader) = guest_reader {
            v8_session
                .set_module_reader(guest_reader)
                .map_err(JavascriptExecutionError::Spawn)?;
        }
        record_js_start_phase("js_start_install_module_reader", phase_start.elapsed());

        let phase_start = Instant::now();
        let retain_context = request
            .env
            .get(NODE_RETAIN_CONTEXT_ENV)
            .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"));
        let prepared_execute = PreparedJavascriptExecute {
            mode: u8::from(use_module_mode) | if retain_context { 2 } else { 0 },
            file_path: execution_file_path,
            bridge_code: V8RuntimeHost::bridge_code().to_owned(),
            post_restore_script,
            userland_code: snapshot_userland_code,
            high_resolution_time: request.guest_runtime.high_resolution_time,
            user_code,
            wasm_module_bytes: request.wasm_module_bytes.clone(),
        };
        let prepared_execute = if defer_execute {
            Some(prepared_execute)
        } else {
            v8_session
                .execute(
                    prepared_execute.mode,
                    prepared_execute.file_path,
                    prepared_execute.bridge_code,
                    prepared_execute.post_restore_script,
                    prepared_execute.userland_code,
                    prepared_execute.high_resolution_time,
                    prepared_execute.user_code,
                    prepared_execute.wasm_module_bytes,
                )
                .map_err(JavascriptExecutionError::Spawn)?;
            None
        };
        registration_guard.disarm();
        record_js_start_phase("js_start_send_execute", phase_start.elapsed());

        Ok(JavascriptExecution {
            execution_id,
            child_pid: v8_host.child_pid(),
            events,
            pending_sync_rpc,
            exited,
            termination_requested: AtomicBool::new(false),
            kernel_stdin,
            _import_cache_guard: import_cache_guard,
            v8_session,
            session_destroyed: false,
            prepared_execute,
            _event_bridge_task: event_bridge_task,
            module_resolution: Mutex::new((
                standalone_translator,
                LocalModuleResolutionCache::default(),
            )),
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
        let runtime = self
            .runtime
            .as_ref()
            .ok_or_else(|| std::io::Error::other(
                "ERR_AGENTOS_RUNTIME_NOT_INJECTED: JavascriptExecutionEngine requires a process RuntimeContext",
            ))?;
        let import_cache = self.import_caches.entry(vm_id.to_owned()).or_default();
        import_cache.ensure_materialized_with_runtime(runtime)?;
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
    pending_sync_rpc: &Arc<Mutex<PendingSyncRpcRegistry>>,
    id: u64,
) -> Result<(), JavascriptExecutionError> {
    let mut pending = pending_sync_rpc.lock().map_err(|_| {
        JavascriptExecutionError::RpcResponse(String::from(
            "sync RPC pending-request state lock poisoned",
        ))
    })?;
    if pending.states.contains_key(&id) {
        return Err(JavascriptExecutionError::PendingSyncRpcRequest(id));
    }
    let observed = pending.states.len().saturating_add(1);
    if observed > pending.maximum {
        return Err(JavascriptExecutionError::PendingSyncRpcLimit {
            limit: pending.maximum,
            observed,
        });
    }
    pending.states.insert(id, PendingSyncRpcState::Pending(id));
    pending.observe_depth();
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
        .limits
        .sync_rpc_wait_timeout_ms
        .filter(|value| *value > 0)
        .unwrap_or(NODE_SYNC_RPC_DEFAULT_WAIT_TIMEOUT_MS);
    Duration::from_millis(timeout_ms)
}

fn javascript_heap_limit_mb(request: &StartJavascriptExecutionRequest) -> u32 {
    request
        .limits
        .v8_heap_limit_mb
        .filter(|value| *value > 0)
        .unwrap_or(0)
}

fn javascript_import_cache_materialize_timeout(
    request: &StartJavascriptExecutionRequest,
) -> Duration {
    let timeout_ms = request
        .limits
        .import_cache_materialize_timeout_ms
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_NODE_IMPORT_CACHE_MATERIALIZE_TIMEOUT_MS);
    Duration::from_millis(timeout_ms)
}

fn javascript_max_timers(request: &StartJavascriptExecutionRequest) -> usize {
    request
        .limits
        .max_timers
        .filter(|value| *value > 0)
        .unwrap_or(MAX_TIMERS_PER_EXECUTION)
}

fn javascript_reactor_work_quantum(
    request: &StartJavascriptExecutionRequest,
    runtime: &RuntimeContext,
) -> Result<usize, JavascriptExecutionError> {
    match request.limits.reactor_work_quantum {
        Some(0) => Err(JavascriptExecutionError::InvalidLimit(String::from(
            "limits.reactor.workQuantum must be greater than zero",
        ))),
        Some(limit) => Ok(limit),
        None if runtime.vm_generation().is_some() => Err(JavascriptExecutionError::InvalidLimit(
            String::from("limits.reactor.workQuantum is required for VM-scoped execution"),
        )),
        None => runtime
            .resources()
            .usage(agentos_runtime::accounting::ResourceClass::ReadyHandles)
            .limit
            .ok_or_else(|| {
                JavascriptExecutionError::InvalidLimit(String::from(
                    "standalone runtime.resources.maxReadyHandles must be bounded",
                ))
            }),
    }
}

fn javascript_bridge_call_timeout(
    request: &StartJavascriptExecutionRequest,
    runtime: &RuntimeContext,
) -> Result<Duration, JavascriptExecutionError> {
    match request.limits.bridge_call_timeout_ms {
        Some(0) => Err(JavascriptExecutionError::InvalidLimit(String::from(
            "limits.reactor.operationDeadlineMs must be greater than zero",
        ))),
        Some(timeout_ms) => Ok(Duration::from_millis(timeout_ms)),
        None if runtime.vm_generation().is_some() => Err(JavascriptExecutionError::InvalidLimit(
            String::from("limits.reactor.operationDeadlineMs is required for VM-scoped execution"),
        )),
        None => Ok(Duration::from_secs(30)),
    }
}

/// Resolve the TRUE CPU-time budget (ms) for a JavaScript execution.
///
/// Read from typed `limits.jsRuntime.cpuTimeLimitMs`, falling back to a bounded
/// default when unset. `0` remains an explicit trusted opt-out and is normalized
/// to `None` by the V8 session.
fn javascript_cpu_time_limit_ms(request: &StartJavascriptExecutionRequest) -> u32 {
    request
        .limits
        .cpu_time_limit_ms
        // Generous active-CPU budget: long-lived adapters are still not capped
        // on wall-clock, but CPU-bound runaways no longer pin a core forever by
        // default.
        .unwrap_or(DEFAULT_V8_CPU_TIME_LIMIT_MS)
}

/// Resolve the opt-in WALL-CLOCK backstop (ms) for a JavaScript execution.
///
/// Read from typed `limits.jsRuntime.wallClockLimitMs`, falling back to `0` (no
/// limit). `0` is normalized to `None` by the V8 session, so the wall-clock
/// `TimeoutGuard` is NOT armed and the guest runs without a wall-clock limit.
/// This is INDEPENDENT of the CPU-time budget: setting only one arms only that
/// guard.
fn javascript_wall_clock_limit_ms(request: &StartJavascriptExecutionRequest) -> u32 {
    request
        .limits
        .wall_clock_limit_ms
        .unwrap_or(DEFAULT_V8_WALL_CLOCK_LIMIT_MS)
}

#[cfg(test)]
fn spawn_javascript_sync_rpc_timeout(
    id: u64,
    timeout: Duration,
    pending_state: Arc<Mutex<PendingSyncRpcRegistry>>,
    responses: Option<JavascriptSyncRpcResponseWriter>,
) {
    let Some(responses) = responses else {
        return;
    };

    let runtime = match agentos_runtime::SidecarRuntime::process(
        &agentos_runtime::RuntimeConfig::default(),
    ) {
        Ok(runtime) => runtime.context(),
        Err(error) => {
            eprintln!("ERR_AGENTOS_RUNTIME_UNAVAILABLE: could not arm JavaScript sync RPC timeout: {error}");
            return;
        }
    };
    if let Err(error) = runtime.spawn(agentos_runtime::TaskClass::Timer, async move {
        tokio::time::sleep(timeout).await;

        let should_timeout = pending_state
            .lock()
            .map(|mut guard| match guard.states.get_mut(&id) {
                Some(state @ PendingSyncRpcState::Pending(_)) => {
                    *state = PendingSyncRpcState::TimedOut(id);
                    true
                }
                _ => false,
            })
            .unwrap_or(false);

        if !should_timeout {
            return;
        }

        if let Err(error) = write_javascript_sync_rpc_response(
            &responses,
            json!({
                "id": id,
                "ok": false,
                "error": {
                    "code": "ERR_AGENTOS_NODE_SYNC_RPC_TIMEOUT",
                    "message": format!(
                        "guest JavaScript sync RPC request {id} timed out after {}ms",
                        timeout.as_millis()
                    ),
                },
            }),
        ) {
            eprintln!(
                "ERR_AGENTOS_SYNC_RPC_TIMEOUT_REPLY: failed to publish timeout response for call_id={id}: {error}"
            );
        }
    }) {
        eprintln!("ERR_AGENTOS_TASK_LIMIT: could not arm JavaScript sync RPC timeout: {error}");
    }
}

#[cfg(test)]
fn parse_javascript_sync_rpc_request(line: &str) -> Result<HostRpcRequest, String> {
    let wire: JavascriptSyncRpcRequestWire =
        serde_json::from_str(line).map_err(|error| error.to_string())?;
    Ok(HostRpcRequest {
        id: wire.id,
        method: wire.method,
        args: wire.args,
        raw_bytes_args: HashMap::new(),
    })
}

#[cfg(test)]
fn write_javascript_sync_rpc_response(
    writer: &JavascriptSyncRpcResponseWriter,
    response: Value,
) -> Result<(), JavascriptExecutionError> {
    let mut payload = serde_json::to_vec(&response)
        .map_err(|error| JavascriptExecutionError::RpcResponse(error.to_string()))?;
    payload.push(b'\n');
    writer.send(payload)
}

#[cfg(test)]
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
        env.get("AGENTOS_NODE_EVAL").cloned().unwrap_or_default()
    } else {
        // Module entrypoint - load it as Node's main CommonJS module so
        // require.main, module.parent, and process.mainModule are coherent.
        format!(
            "_moduleModule._load({}, null, true);\n//# sourceURL={}",
            serde_json::to_string(entrypoint).unwrap_or_else(|_| format!("\"{}\"", entrypoint)),
            entrypoint
        )
    }
}

fn host_entrypoint_uses_module_mode(entrypoint: &Path) -> bool {
    // Agent adapters are launched via an extensionless `/opt/agentos/bin/<cmd>`
    // symlink into the packed package's `node_modules/<name>/<entry>`. Resolve it
    // to the real file so the extension check and the nearest-`package.json` walk
    // reflect the package (which carries `"type": "module"`), not the symlink farm
    // (which has neither an extension nor a package.json).
    let resolved = fs::canonicalize(entrypoint).unwrap_or_else(|_| entrypoint.to_path_buf());
    match resolved.extension().and_then(|ext| ext.to_str()) {
        Some("mjs" | "mts") => true,
        Some("js") => nearest_package_json_type(&resolved).as_deref() == Some("module"),
        _ => false,
    }
}

fn inline_code_uses_module_mode(source: &str) -> bool {
    let sanitized = strip_non_code_segments(source);
    let tokens = tokenize_inline_module_source(&sanitized);
    let has_commonjs_signal = tokens.windows(3).any(|window| {
        matches!(
            window,
            [
                InlineModuleToken::Identifier("module"),
                InlineModuleToken::Punct('.'),
                InlineModuleToken::Identifier("exports")
            ]
        )
    }) || tokens.windows(2).any(|window| {
        matches!(
            window,
            [
                InlineModuleToken::Identifier("exports"),
                InlineModuleToken::Punct('.' | '[')
            ] | [
                InlineModuleToken::Identifier("require"),
                InlineModuleToken::Punct('(')
            ]
        )
    });

    if has_commonjs_signal {
        return false;
    }

    tokens.windows(2).any(|window| match window {
        [InlineModuleToken::Identifier("import"), InlineModuleToken::Punct('.')] => true,
        [InlineModuleToken::Identifier("import"), InlineModuleToken::Punct('(' | ':')] => false,
        [InlineModuleToken::Identifier("import"), InlineModuleToken::Identifier(_)
        | InlineModuleToken::Punct('{')
        | InlineModuleToken::Punct('*')
        | InlineModuleToken::StringLiteral] => true,
        [InlineModuleToken::Identifier("export"), InlineModuleToken::Identifier(
            "default" | "const" | "let" | "var" | "function" | "class" | "async" | "enum" | "type"
            | "interface",
        )
        | InlineModuleToken::Punct('{')
        | InlineModuleToken::Punct('*')] => true,
        _ => false,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InlineModuleToken<'a> {
    Identifier(&'a str),
    StringLiteral,
    Punct(char),
}

const INLINE_MODULE_STRING_PLACEHOLDER: char = '\u{1F}';

fn strip_non_code_segments(source: &str) -> String {
    let mut sanitized = String::with_capacity(source.len());
    let bytes = source.as_bytes();
    let mut index = 0;
    sanitize_javascript_code(bytes, &mut index, &mut sanitized, None);
    sanitized
}

fn sanitize_javascript_code(
    bytes: &[u8],
    index: &mut usize,
    output: &mut String,
    until_brace_depth: Option<usize>,
) {
    let mut brace_depth = 0usize;

    while *index < bytes.len() {
        let current = bytes[*index];

        if let Some(target_depth) = until_brace_depth {
            match current {
                b'{' => brace_depth += 1,
                b'}' => {
                    if brace_depth == target_depth {
                        output.push(' ');
                        *index += 1;
                        return;
                    }
                    brace_depth = brace_depth.saturating_sub(1);
                }
                _ => {}
            }
        }

        match current {
            b'/' if bytes.get(*index + 1) == Some(&b'/') => {
                output.push(' ');
                output.push(' ');
                *index += 2;
                while *index < bytes.len() {
                    let comment_byte = bytes[*index];
                    *index += 1;
                    if comment_byte == b'\n' {
                        output.push('\n');
                        break;
                    }
                    output.push(' ');
                }
            }
            b'/' if bytes.get(*index + 1) == Some(&b'*') => {
                output.push(' ');
                output.push(' ');
                *index += 2;
                while *index < bytes.len() {
                    let comment_byte = bytes[*index];
                    if comment_byte == b'*' && bytes.get(*index + 1) == Some(&b'/') {
                        output.push(' ');
                        output.push(' ');
                        *index += 2;
                        break;
                    }
                    output.push(if comment_byte == b'\n' { '\n' } else { ' ' });
                    *index += 1;
                }
            }
            b'\'' | b'"' => sanitize_string_literal(bytes, index, output, current),
            b'`' => sanitize_template_literal(bytes, index, output),
            _ => {
                output.push(char::from(current));
                *index += 1;
            }
        }
    }
}

fn sanitize_string_literal(bytes: &[u8], index: &mut usize, output: &mut String, quote: u8) {
    output.push(INLINE_MODULE_STRING_PLACEHOLDER);
    *index += 1;

    while *index < bytes.len() {
        let current = bytes[*index];
        *index += 1;
        match current {
            b'\\' => {
                if *index < bytes.len() {
                    *index += 1;
                }
            }
            c if c == quote => break,
            _ => {}
        }
    }
}

fn sanitize_template_literal(bytes: &[u8], index: &mut usize, output: &mut String) {
    output.push(INLINE_MODULE_STRING_PLACEHOLDER);
    *index += 1;

    while *index < bytes.len() {
        let current = bytes[*index];
        match current {
            b'\\' => {
                *index += 1;
                if *index < bytes.len() {
                    *index += 1;
                }
            }
            b'`' => {
                *index += 1;
                break;
            }
            b'$' if bytes.get(*index + 1) == Some(&b'{') => {
                output.push(' ');
                output.push(' ');
                *index += 2;
                sanitize_javascript_code(bytes, index, output, Some(0));
                output.push(INLINE_MODULE_STRING_PLACEHOLDER);
            }
            b'\n' => {
                output.push('\n');
                *index += 1;
            }
            _ => {
                *index += 1;
            }
        }
    }
}

fn tokenize_inline_module_source(source: &str) -> Vec<InlineModuleToken<'_>> {
    let mut tokens = Vec::new();
    let bytes = source.as_bytes();
    let mut index = 0;

    while index < bytes.len() {
        let current = bytes[index];
        match current {
            b if b.is_ascii_whitespace() => index += 1,
            b if char::from(b) == INLINE_MODULE_STRING_PLACEHOLDER => {
                tokens.push(InlineModuleToken::StringLiteral);
                index += 1;
            }
            b'a'..=b'z' | b'A'..=b'Z' | b'_' | b'$' => {
                let start = index;
                index += 1;
                while index < bytes.len()
                    && matches!(bytes[index], b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'$')
                {
                    index += 1;
                }
                tokens.push(InlineModuleToken::Identifier(&source[start..index]));
            }
            _ => {
                tokens.push(InlineModuleToken::Punct(char::from(current)));
                index += 1;
            }
        }
    }

    tokens
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

// Keep each injected process/runtime value explicit at this one serialization
// boundary; grouping them would duplicate the already-typed guest config.
#[allow(clippy::too_many_arguments)]
fn build_v8_runtime_init_script(
    entrypoint: &str,
    argv: &[String],
    argv0: Option<&str>,
    cwd: &str,
    env: &BTreeMap<String, String>,
    // V8 heap cap in MB (`0` = engine default). Threaded from the typed wire
    // limit and interpolated into the shim so guest heap-stats reporting no
    // longer depends on an `AGENTOS_V8_HEAP_LIMIT_MB` env var.
    heap_limit_mb: u32,
    // Typed guest-runtime identity, interpolated into the shim so virtual
    // `process.*` identity no longer rides `AGENTOS_VIRTUAL_PROCESS_*` env vars.
    guest_runtime: &GuestRuntimeConfig,
) -> String {
    let argv_json = serde_json::to_string(argv).unwrap_or_else(|_| String::from("[\"node\"]"));
    let argv0_json = serde_json::to_string(&argv0.unwrap_or("node"))
        .unwrap_or_else(|_| String::from("\"node\""));
    let entry_json =
        serde_json::to_string(entrypoint).unwrap_or_else(|_| String::from("\"/<entry>\""));
    let cwd_json = serde_json::to_string(cwd).unwrap_or_else(|_| String::from("\"/\""));
    let env_json = serde_json::to_string(env).unwrap_or_else(|_| String::from("{}"));
    // Virtual process identity object. `Option` fields serialize to `null`, which
    // the shim treats as "unset" (leaving the V8-baked default) — matching the
    // prior behavior when the env var was absent.
    let identity_json = serde_json::json!({
        "pid": guest_runtime.virtual_pid,
        "ppid": guest_runtime.virtual_ppid,
        "uid": guest_runtime.virtual_uid,
        "gid": guest_runtime.virtual_gid,
        "execPath": guest_runtime.virtual_exec_path,
    })
    .to_string();
    // Virtual OS identity (os.cpus/totalmem/freemem/homedir/userInfo/...). Read
    // by the bridge + node-import-cache os polyfill from the `__agentOSVirtualOs`
    // global instead of `AGENTOS_VIRTUAL_OS_*` env vars. Absent fields stay
    // `null`, so the consumers fall back to their built-in defaults.
    let virtual_os_json = serde_json::json!({
        "cpuCount": guest_runtime.os_cpu_count,
        "totalmem": guest_runtime.os_totalmem,
        "freemem": guest_runtime.os_freemem,
        "homedir": guest_runtime.os_homedir,
        "hostname": guest_runtime.os_hostname,
        "tmpdir": guest_runtime.os_tmpdir,
        "type": guest_runtime.os_type,
        "release": guest_runtime.os_release,
        "version": guest_runtime.os_version,
        "machine": guest_runtime.os_machine,
        "shell": guest_runtime.os_shell,
        "user": guest_runtime.os_user,
    })
    .to_string();
    let high_resolution_time = guest_runtime.high_resolution_time;

    format!(
        r#"(function () {{
  const __guestIdentity = {identity_json};
  Object.defineProperty(globalThis, "__agentOSVirtualOs", {{
    configurable: true,
    enumerable: false,
    value: {virtual_os_json},
    writable: true,
  }});
  const nextArgv = {argv_json};
  const nextArgv0 = {argv0_json};
  const entryFile = {entry_json};
  const nextCwd = {cwd_json};
  const nextEnv = {env_json};
  const nextHighResolutionTime = {high_resolution_time};
  const visibleEnv = Object.fromEntries(
    Object.entries(nextEnv).filter(([key]) => !key.startsWith("AGENTOS_"))
  );
  Object.defineProperty(globalThis, "__agentOSProcessConfigEnv", {{
    configurable: true,
    enumerable: false,
    value: nextEnv,
    writable: true,
  }});
  try {{
    const previousProcessConfig =
      typeof globalThis._processConfig === "object" && globalThis._processConfig !== null
        ? globalThis._processConfig
        : {{}};
    Object.defineProperty(globalThis, "_processConfig", {{
      configurable: true,
      enumerable: false,
      value: Object.freeze({{
        ...previousProcessConfig,
        cwd: nextCwd,
        env: visibleEnv,
        argv: nextArgv,
        argv0: nextArgv0,
        high_resolution_time: nextHighResolutionTime,
      }}),
      writable: false,
    }});
  }} catch (_e) {{}}
  // Refresh the process module's closure-backed state before user modules run.
  // Updating only globalThis.process leaves named ESM imports such as
  // `import {{ cwd }} from "process"` reading the warm snapshot's stale cwd.
  if (typeof globalThis.__runtimeRefreshProcessConfig === "function") {{
    globalThis.__runtimeRefreshProcessConfig();
  }}

  if (typeof process !== "undefined") {{
    process.argv = nextArgv;
    process.argv0 = nextArgv0;
    process.env = {{ ...visibleEnv }};
    const configuredHeapLimitMb = {heap_limit_mb};
    if (Number.isFinite(configuredHeapLimitMb) && configuredHeapLimitMb > 0) {{
      Object.defineProperty(globalThis, "__agentOSV8HeapLimitBytes", {{
        configurable: true,
        enumerable: false,
        value: configuredHeapLimitMb * 1024 * 1024,
        writable: true,
      }});
    }}
    if (nextEnv.AGENTOS_ALLOW_PROCESS_BINDINGS === "1" && typeof process.binding === "function") {{
      const originalProcessBinding = process.binding.bind(process);
      let constantsBinding = null;
      try {{
        const bindingRequire = globalThis._moduleModule?.createRequire?.(
          nextCwd === "/"
            ? "/__agentos_runtime__.js"
            : `${{nextCwd.replace(/\/+$/, "")}}/__agentos_runtime__.js`,
        );
        constantsBinding = bindingRequire?.("node:constants") ?? null;
        constantsBinding = constantsBinding?.default ?? constantsBinding;
      }} catch (_e) {{}}
      process.binding = (name) => {{
        const bindingName = String(name);
        if (bindingName === "constants" && constantsBinding !== null) {{
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
    const nextPid = Number(__guestIdentity.pid);
    if (Number.isFinite(nextPid) && nextPid > 0) {{
      process.pid = nextPid;
    }}
    const nextPpid = Number(__guestIdentity.ppid);
    if (Number.isFinite(nextPpid) && nextPpid >= 0) {{
      process.ppid = nextPpid;
    }}
    const nextUid = Number(__guestIdentity.uid);
    if (Number.isFinite(nextUid) && nextUid >= 0) {{
      process.uid = nextUid;
      process.euid = nextUid;
    }}
    const nextGid = Number(__guestIdentity.gid);
    if (Number.isFinite(nextGid) && nextGid >= 0) {{
      process.gid = nextGid;
      process.egid = nextGid;
      process.groups = [nextGid];
    }}
    if (typeof __guestIdentity.execPath === "string" && __guestIdentity.execPath.length > 0) {{
      process.execPath = __guestIdentity.execPath;
    }}
    globalThis.__runtimeStreamStdin = nextEnv.AGENTOS_KEEP_STDIN_OPEN === "1";
    globalThis.__runtimeKernelStdin =
      nextEnv.AGENTOS_FORWARD_KERNEL_STDIN_RPC === "1";
    if (nextEnv.AGENTOS_NODE_IPC === "1" && typeof __runtimeInstallProcessIpcBridge === "function") {{
      process.connected = true;
      __runtimeInstallProcessIpcBridge();
    }}
    if (typeof process.getBuiltinModule !== "function") {{
      process.getBuiltinModule = function(specifier) {{
        return globalThis.require ? globalThis.require(specifier) : undefined;
      }};
    }}
  }}

  const streamStdin = nextEnv.AGENTOS_KEEP_STDIN_OPEN === "1";
  if (typeof globalThis.__runtimeConfigureStreamStdin === "function") {{
    globalThis.__runtimeConfigureStreamStdin(
      streamStdin,
      nextEnv.AGENTOS_EAGER_STDIN_HANDLE === "1",
    );
  }} else {{
    globalThis.__runtimeStreamStdin = streamStdin;
  }}
  globalThis.__runtimeKernelStdin =
    nextEnv.AGENTOS_FORWARD_KERNEL_STDIN_RPC === "1";

  if (
    typeof globalThis.WebAssembly === "object" &&
    globalThis.WebAssembly !== null &&
    typeof globalThis.WebAssembly.instantiateStreaming !== "function"
  ) {{
    globalThis.WebAssembly.instantiateStreaming = async function instantiateStreaming(source, imports) {{
      const response = await source;
      if (response == null || typeof response.arrayBuffer !== "function") {{
        throw new TypeError(
          "WebAssembly.instantiateStreaming requires a Response or promise for one",
        );
      }}
      const bytes = new Uint8Array(await response.arrayBuffer());
      return globalThis.WebAssembly.instantiate(bytes, imports);
    }};
  }}

  if (
    typeof globalThis.require === "undefined" &&
    typeof globalThis._moduleModule?.createRequire === "function"
  ) {{
    const requireEntryFile =
      entryFile === "-e" || entryFile === "--eval"
        ? nextCwd === "/"
          ? "/__agentos_eval__.js"
          : `${{nextCwd.replace(/\/+$/, "")}}/__agentos_eval__.js`
        : entryFile;
    globalThis.require =
      globalThis._moduleModule.createRequire(requireEntryFile);
  }}

  if (typeof globalThis.require === "function") {{
    let preloadModules = [];
    try {{
      const parsed = JSON.parse(nextEnv.AGENTOS_NODE_PRELOAD_MODULES || "[]");
      if (Array.isArray(parsed)) preloadModules = parsed.map(String);
    }} catch (_e) {{}}
    const preloadBase =
      nextCwd === "/"
        ? "/__agentos_preload__.js"
        : `${{nextCwd.replace(/\/+$/, "")}}/__agentos_preload__.js`;
    const preloadRequire =
      typeof globalThis._moduleModule?.createRequire === "function"
        ? globalThis._moduleModule.createRequire(preloadBase)
        : globalThis.require;
    for (const preloadModule of preloadModules) {{
      preloadRequire(preloadModule);
    }}
  }}

  // jsRuntime platform tiering: the guest JS host surface is baked into the
  // shared V8 snapshot, so non-node platforms are produced by subtractively
  // scrubbing baked globals here, per execution.
  const __jsPlatform = nextEnv.AGENTOS_JS_PLATFORM || "node";
  // Install the builtin allow-list gate consulted by the bridge's
  // rejectRestrictedBuiltinRequest (covers require + ESM builtin loads). Present
  // for non-node platforms (empty => deny all) and node + explicit allow-list;
  // absent => unrestricted (node default).
  {{
    const __builtinAllowRaw = nextEnv.AGENTOS_JS_BUILTIN_ALLOWLIST;
    if (typeof __builtinAllowRaw === "string") {{
      let __allowList = [];
      try {{ __allowList = JSON.parse(__builtinAllowRaw); }} catch (_e) {{ __allowList = []; }}
      if (typeof globalThis.__agentOSInitJsRuntime === "function") {{
        globalThis.__agentOSInitJsRuntime(Array.isArray(__allowList) ? __allowList : []);
      }}
      try {{ delete globalThis.__agentOSInitJsRuntime; }} catch (_e) {{}}
    }}
  }}
  if (__jsPlatform !== "node") {{
    const __dropGlobal = (name) => {{
      try {{ delete globalThis[name]; }} catch (_e) {{}}
      if (
        Object.prototype.hasOwnProperty.call(globalThis, name) ||
        typeof globalThis[name] !== "undefined"
      ) {{
        try {{ globalThis[name] = undefined; }} catch (_e) {{}}
        try {{
          Object.defineProperty(globalThis, name, {{
            value: undefined,
            configurable: true,
            writable: true,
          }});
        }} catch (_e) {{}}
        try {{ delete globalThis[name]; }} catch (_e) {{}}
      }}
    }};
    // Node host surface + identity channels — removed on every non-node platform.
    [
      "process", "Buffer", "require", "module", "exports",
      "__dirname", "__filename", "global",
      "_processConfig", "__agentOSProcessConfigEnv", "__agentOSVirtualOs",
    ].forEach(__dropGlobal);
    if (__jsPlatform === "browser") {{
      // Narrow `crypto` from the full node:crypto module to the WebCrypto object
      // (drops randomBytes/createHash/... while keeping subtle/getRandomValues).
      try {{
        const __wc = globalThis.crypto && globalThis.crypto.webcrypto;
        if (__wc) {{ globalThis.crypto = __wc; }}
      }} catch (_e) {{}}
    }}
    if (__jsPlatform === "neutral" || __jsPlatform === "bare") {{
      // Web-platform globals removed at neutral and below.
      [
        "fetch", "Headers", "Request", "Response", "FormData",
        "URL", "URLSearchParams", "Blob", "File", "crypto",
        "atob", "btoa", "structuredClone", "performance",
        "AbortController", "AbortSignal", "Event", "EventTarget",
        "MessageChannel", "MessagePort", "MessageEvent",
        "ReadableStream", "WritableStream", "TransformStream",
      ].forEach(__dropGlobal);
    }}
    if (__jsPlatform === "bare") {{
      // Universal host primitives removed only at the language-only tier.
      [
        "console", "queueMicrotask",
        "setTimeout", "clearTimeout", "setInterval", "clearInterval",
        "setImmediate", "clearImmediate",
      ].forEach(__dropGlobal);
    }}
  }}
}})();"#
    )
}

/// Spawn a supervised task on the process runtime that converts V8 BinaryFrame
/// messages into JavascriptExecutionEvent values for the sidecar event loop.
///
/// Internal bridge calls (module loading, logging, timers) are handled locally
/// by the event bridge. Kernel operations (fs, net, child_process, dns) are
/// forwarded to the sidecar via SyncRpcRequest events.
fn spawn_v8_event_bridge(
    runtime: &RuntimeContext,
    frame_receiver: V8SessionFrameReceiver,
    pending_sync_rpc: Arc<Mutex<PendingSyncRpcRegistry>>,
    exited: Arc<AtomicBool>,
    v8_session: V8SessionHandle,
    mut local_bridge: LocalBridgeState,
    event_notify: Option<Arc<Notify>>,
) -> Result<
    (
        EventReceiver<JavascriptExecutionEvent>,
        tokio::task::JoinHandle<()>,
    ),
    JavascriptExecutionError,
> {
    let (sender, receiver) = flume::bounded(JAVASCRIPT_EVENT_CHANNEL_CAPACITY);
    let event_gauge = register_queue(
        TrackedLimit::JavascriptEventChannel,
        JAVASCRIPT_EVENT_CHANNEL_CAPACITY,
    );

    let task = runtime
        .spawn(agentos_runtime::TaskClass::Vm, async move {
            let mut emitted_exit = false;
            loop {
                let frame_recv_start = Instant::now();
                let Ok(frame) = frame_receiver.recv_async().await else {
                    break;
                };
                let frame_recv_wait = frame_recv_start.elapsed();
                let mut exit_frame_start = None;
                let event = match frame {
                    BinaryFrame::BridgeCall {
                        call_id,
                        method,
                        payload,
                        ..
                    } => {
                        // Convert CBOR payload to JSON args
                        let phase_start = Instant::now();
                        let args = match decode_bridge_call_args(&method, call_id, &payload) {
                            Ok(args) => args,
                            Err(host_error) => {
                                let encoded = match encode_host_service_error_payload(&host_error) {
                                    Ok(encoded) => encoded,
                                    Err(error) => {
                                        terminate_session_after_bridge_encoding_failure(
                                            &v8_session,
                                            call_id,
                                            &error.to_string(),
                                        );
                                        break;
                                    }
                                };
                                if let Err(reply_error) =
                                    v8_session.send_bridge_response(call_id, 1, encoded)
                                {
                                    eprintln!(
                                        "INFO_AGENTOS_STALE_BRIDGE_COMPLETION: failed to send typed decode error for call_id={call_id}: {reply_error}"
                                    );
                                }
                                continue;
                            }
                        };
                        record_sync_bridge_phase(
                            &method,
                            "event_decode_args",
                            phase_start.elapsed(),
                        );

                        // Module resolution / loading must read the mounted
                        // `node_modules` VFS, not host files directly. When the
                        // sidecar supplied a read-only VFS module reader, resolve
                        // these inline on this bridge thread (off the service loop) so
                        // a large cold-start module graph runs concurrently with — and
                        // never serializes behind / starves — the ACP bootstrap that
                        // is itself awaiting the adapter's `session/new` response on
                        // the single service-loop thread. Without a reader (no mount),
                        // they flow to the service loop as SyncRpcRequests (mapped to
                        // `__resolve_module` / `__load_file` / `__module_format` /
                        // `__batch_resolve_modules`) and resolve against `vm.kernel`.
                        let is_module_method = matches!(
                            method.as_str(),
                            "_resolveModule"
                                | "_resolveModuleSync"
                                | "_loadFile"
                                | "_loadFileSync"
                                | "_moduleFormat"
                                | "_batchResolveModules"
                        );
                        let resolve_on_service_loop =
                            is_module_method && !local_bridge.has_module_reader();

                        // Check if this is an internal bridge call we handle locally
                        if !resolve_on_service_loop {
                            if let Some(response) =
                                local_bridge.handle_internal_bridge_call(call_id, &method, &args)
                            {
                                let encoded = match response {
                                    LocalBridgeCallResult::Immediate(response) => {
                                        match v8_runtime::json_to_cbor_payload(&response) {
                                            Ok(payload) => (0, payload),
                                            Err(error) => {
                                                terminate_session_after_bridge_encoding_failure(
                                                    &v8_session,
                                                    call_id,
                                                    &error.to_string(),
                                                );
                                                break;
                                            }
                                        }
                                    }
                                    LocalBridgeCallResult::Error(error) => {
                                        match encode_host_service_error_payload(&error) {
                                            Ok(payload) => (1, payload),
                                            Err(error) => {
                                                terminate_session_after_bridge_encoding_failure(
                                                    &v8_session,
                                                    call_id,
                                                    &error.to_string(),
                                                );
                                                break;
                                            }
                                        }
                                    }
                                    LocalBridgeCallResult::Deferred => continue,
                                };
                                if let Err(error) = v8_session.send_bridge_response(
                                    call_id,
                                    encoded.0,
                                    encoded.1,
                                ) {
                                    eprintln!(
                                        "INFO_AGENTOS_STALE_BRIDGE_COMPLETION: call_id={call_id} error={error}"
                                    );
                                }
                                continue;
                            }
                        }

                        // Handle logging locally (produce stdout/stderr events)
                        if method == "_log" || method == "_error" {
                            let output = decode_bridge_output_args(&args);
                            // Respond to the bridge call
                            let null_payload = match v8_runtime::json_to_cbor_payload(&Value::Null) {
                                Ok(payload) => payload,
                                Err(error) => {
                                    terminate_session_after_bridge_encoding_failure(
                                        &v8_session,
                                        call_id,
                                        &error.to_string(),
                                    );
                                    break;
                                }
                            };
                            if let Err(error) = v8_session.send_bridge_response(
                                call_id,
                                0,
                                null_payload,
                            ) {
                                eprintln!(
                                    "INFO_AGENTOS_STALE_BRIDGE_COMPLETION: call_id={call_id} error={error}"
                                );
                            }
                            if method == "_log" {
                                if !send_javascript_event_async(
                                    &sender,
                                    &event_gauge,
                                    event_notify.as_deref(),
                                    JavascriptExecutionEvent::Stdout(output),
                                )
                                .await
                                {
                                    break;
                                }
                            } else {
                                if !send_javascript_event_async(
                                    &sender,
                                    &event_gauge,
                                    event_notify.as_deref(),
                                    JavascriptExecutionEvent::Stderr(output),
                                )
                                .await
                                {
                                    break;
                                }
                            }
                            continue;
                        }

                        // Map the bridge method name to the sidecar sync RPC method name
                        let phase_start = Instant::now();
                        let (sidecar_method, _needs_translation) =
                            v8_runtime::map_bridge_method(&method);
                        record_sync_bridge_phase(
                            &method,
                            "event_map_method",
                            phase_start.elapsed(),
                        );

                        // Track exactly one pending sync RPC. A malformed or
                        // timed-out guest that submits another call before the
                        // old host operation settles receives a typed busy
                        // reply; the original waiter is never overwritten.
                        let phase_start = Instant::now();
                        if let Err(error) =
                            set_pending_sync_rpc_state(&pending_sync_rpc, call_id)
                        {
                            let host_error = host_reply_adapter_error(error);
                            let payload = match encode_host_service_error_payload(&host_error) {
                                Ok(payload) => payload,
                                Err(error) => {
                                    terminate_session_after_bridge_encoding_failure(
                                        &v8_session,
                                        call_id,
                                        &error.to_string(),
                                    );
                                    break;
                                }
                            };
                            if let Err(reply_error) =
                                v8_session.send_bridge_response(call_id, 1, payload)
                            {
                                eprintln!(
                                    "INFO_AGENTOS_STALE_BRIDGE_COMPLETION: call_id={call_id} error={reply_error}"
                                );
                            }
                            continue;
                        }
                        record_sync_bridge_phase(
                            &method,
                            "event_mark_pending",
                            phase_start.elapsed(),
                        );

                        let phase_start = Instant::now();
                        let request_args = translate_request_args_for_legacy(sidecar_method, &args);
                        let mut raw_bytes_args = HashMap::new();
                        // Preserve every CBOR byte-string argument losslessly.
                        // Host-operation decoders, not the V8 bridge, own the
                        // method schema; a per-method allowlist here silently
                        // converted new binary syscall arguments into opaque
                        // JSON placeholders (for example sendmsg payloads).
                        for index in 0..args.len() {
                            if let Ok(Some(bytes)) =
                                v8_runtime::cbor_payload_raw_byte_arg(&payload, index)
                            {
                                raw_bytes_args.insert(index, bytes);
                            }
                        }
                        if method == "_fsReadRaw" || method == "_fsReadFileRangeRaw" {
                            raw_bytes_args.insert(usize::MAX, Vec::new());
                        }
                        record_sync_bridge_phase(
                            &method,
                            "event_translate_args",
                            phase_start.elapsed(),
                        );
                        Some(JavascriptExecutionEvent::SyncRpcRequest(
                            HostRpcRequest {
                                id: call_id,
                                method: sidecar_method.to_owned(),
                                args: request_args,
                                raw_bytes_args,
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
                        exited.store(true, Ordering::Release);
                        let phase_start = Instant::now();
                        exit_frame_start = Some(phase_start);
                        record_js_event_phase("js_exit_frame_recv_wait", frame_recv_wait);
                        let is_process_exit_error = error.as_ref().is_some_and(|err| {
                            err.error_type == "ProcessExitError"
                                || err.message.starts_with("process.exit(")
                        });
                        let resolved_exit_code = error
                            .as_ref()
                            .and_then(|err| {
                                if is_process_exit_error {
                                    parse_process_exit_code_message(&err.message)
                                } else {
                                    None
                                }
                            })
                            .unwrap_or(exit_code);
                        let should_emit_error = error.is_some() && !is_process_exit_error;
                        if should_emit_error {
                            let err = error.as_ref().expect("checked above");
                            let error_msg = if err.stack.is_empty() {
                                format!("{}: {}\n", err.error_type, err.message)
                            } else {
                                format!("{}\n", err.stack)
                            };
                            if !send_javascript_event_async(
                                &sender,
                                &event_gauge,
                                event_notify.as_deref(),
                                JavascriptExecutionEvent::Stderr(error_msg.into_bytes()),
                            )
                            .await
                            {
                                break;
                            }
                        }
                        emitted_exit = true;
                        record_js_event_phase(
                            "js_exit_frame_to_event_construct",
                            phase_start.elapsed(),
                        );
                        Some(JavascriptExecutionEvent::Exited(resolved_exit_code))
                    }
                    BinaryFrame::StreamCallback { .. } => None,
                    _ => None,
                };

                if let Some(event) = event {
                    let sync_rpc = match &event {
                        JavascriptExecutionEvent::SyncRpcRequest(request) => {
                            Some((request.id, request.method.clone()))
                        }
                        _ => None,
                    };
                    if let Some((call_id, method)) = sync_rpc.as_ref() {
                        record_sync_bridge_request_enqueued(*call_id, method);
                    }
                    let phase_start = sync_rpc.as_ref().map(|_| Instant::now());
                    let exit_send_start = if matches!(&event, JavascriptExecutionEvent::Exited(_)) {
                        Some(Instant::now())
                    } else {
                        None
                    };
                    if !send_javascript_event_async(
                        &sender,
                        &event_gauge,
                        event_notify.as_deref(),
                        event,
                    )
                    .await
                    {
                        break;
                    }
                    if let (Some((_, method)), Some(start)) = (sync_rpc, phase_start) {
                        record_sync_bridge_phase(&method, "event_enqueue", start.elapsed());
                    }
                    if let Some(start) = exit_send_start {
                        record_js_event_phase("js_exit_event_send", start.elapsed());
                    }
                    if let Some(start) = exit_frame_start {
                        record_js_event_phase("js_exit_frame_to_event_sent", start.elapsed());
                    }
                }
            }

            if !emitted_exit {
                exited.store(true, Ordering::Release);
                let phase_start = Instant::now();
                let sent = send_javascript_event_async(
                    &sender,
                    &event_gauge,
                    event_notify.as_deref(),
                    JavascriptExecutionEvent::Exited(1),
                )
                .await;
                if sent {
                    record_js_event_phase("js_exit_fallback_event_send", phase_start.elapsed());
                }
            }
        })
        .map_err(|error| {
            JavascriptExecutionError::Spawn(std::io::Error::other(error.to_string()))
        })?;

    Ok((receiver, task))
}

fn terminate_session_after_bridge_encoding_failure(
    session: &V8SessionHandle,
    call_id: u64,
    error: &str,
) {
    eprintln!(
        "ERR_AGENTOS_BRIDGE_RESPONSE_ENCODE: failed to encode bridge response for call_id={call_id}: {error}; terminating the JavaScript session"
    );
    if let Err(destroy_error) = session.destroy() {
        eprintln!(
            "ERR_AGENTOS_BRIDGE_RESPONSE_TEARDOWN: failed to terminate JavaScript session after bridge encoding failure for call_id={call_id}: {destroy_error}"
        );
    }
}

async fn send_javascript_event_async(
    sender: &EventSender<JavascriptExecutionEvent>,
    gauge: &agentos_bridge::queue_tracker::QueueGauge,
    notify: Option<&Notify>,
    event: JavascriptExecutionEvent,
) -> bool {
    match event {
        JavascriptExecutionEvent::Stdout(chunk)
            if chunk.len() > JAVASCRIPT_EVENT_PAYLOAD_LIMIT_BYTES =>
        {
            for chunk in chunk.chunks(JAVASCRIPT_EVENT_PAYLOAD_LIMIT_BYTES) {
                if !send_single_javascript_event_async(
                    sender,
                    gauge,
                    notify,
                    JavascriptExecutionEvent::Stdout(chunk.to_vec()),
                )
                .await
                {
                    return false;
                }
            }
            true
        }
        JavascriptExecutionEvent::Stderr(chunk)
            if chunk.len() > JAVASCRIPT_EVENT_PAYLOAD_LIMIT_BYTES =>
        {
            for chunk in chunk.chunks(JAVASCRIPT_EVENT_PAYLOAD_LIMIT_BYTES) {
                if !send_single_javascript_event_async(
                    sender,
                    gauge,
                    notify,
                    JavascriptExecutionEvent::Stderr(chunk.to_vec()),
                )
                .await
                {
                    return false;
                }
            }
            true
        }
        event => send_single_javascript_event_async(sender, gauge, notify, event).await,
    }
}

async fn send_single_javascript_event_async(
    sender: &EventSender<JavascriptExecutionEvent>,
    gauge: &agentos_bridge::queue_tracker::QueueGauge,
    notify: Option<&Notify>,
    event: JavascriptExecutionEvent,
) -> bool {
    match sender.send_async(event).await {
        Ok(()) => {
            gauge.observe_depth(sender.len());
            if let Some(notify) = notify {
                notify.notify_one();
            }
            true
        }
        Err(_closed) => false,
    }
}

#[cfg(test)]
fn send_javascript_event(
    sender: &EventSender<JavascriptExecutionEvent>,
    gauge: &agentos_bridge::queue_tracker::QueueGauge,
    notify: Option<&Notify>,
    event: JavascriptExecutionEvent,
) -> bool {
    match event {
        JavascriptExecutionEvent::Stdout(chunk)
            if chunk.len() > JAVASCRIPT_EVENT_PAYLOAD_LIMIT_BYTES =>
        {
            for chunk in chunk.chunks(JAVASCRIPT_EVENT_PAYLOAD_LIMIT_BYTES) {
                if !send_single_javascript_event(
                    sender,
                    gauge,
                    notify,
                    JavascriptExecutionEvent::Stdout(chunk.to_vec()),
                ) {
                    return false;
                }
            }
            true
        }
        JavascriptExecutionEvent::Stderr(chunk)
            if chunk.len() > JAVASCRIPT_EVENT_PAYLOAD_LIMIT_BYTES =>
        {
            for chunk in chunk.chunks(JAVASCRIPT_EVENT_PAYLOAD_LIMIT_BYTES) {
                if !send_single_javascript_event(
                    sender,
                    gauge,
                    notify,
                    JavascriptExecutionEvent::Stderr(chunk.to_vec()),
                ) {
                    return false;
                }
            }
            true
        }
        event => send_single_javascript_event(sender, gauge, notify, event),
    }
}

#[cfg(test)]
fn send_single_javascript_event(
    sender: &EventSender<JavascriptExecutionEvent>,
    gauge: &agentos_bridge::queue_tracker::QueueGauge,
    notify: Option<&Notify>,
    event: JavascriptExecutionEvent,
) -> bool {
    // Apply backpressure instead of self-destructing when the host consumer is
    // slow. A burst of guest events that briefly outpaces the host draining this
    // channel is normal; previously a single `try_send` returning `Full` tore the
    // whole session down (`destroy()` -> Shutdown -> `Exited(1)`), turning a
    // transient backlog into a fatal crash. This test-only synchronous helper
    // parks its calling thread until the host drains capacity. Production uses
    // `send_javascript_event_async`, which yields the shared runtime task.
    match sender.send(event) {
        Ok(()) => {
            // Sample the live channel depth so the centralized queue tracker can
            // warn before the host consumer falls far enough behind to stall the
            // session (and surface the high-water mark for debugging).
            gauge.observe_depth(sender.len());
            if let Some(notify) = notify {
                notify.notify_one();
            }
            true
        }
        Err(_closed) => false,
    }
}

/// Handle internal bridge calls that don't need to go to the sidecar.
/// Returns Some(response) if handled locally, None if it should be forwarded.
impl LocalBridgeState {
    fn package_module_request_requires_kernel(&self, parent: &str) -> bool {
        if !self.module_reader_forwards_to_kernel {
            return false;
        }
        let parent = guest_path_from_file_url(parent).unwrap_or_else(|| parent.to_owned());
        normalize_guest_path(&parent).starts_with("/opt/agentos/pkgs/")
    }

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
                if self.js_runtime_denies_specifier(specifier) {
                    return Some(LocalBridgeCallResult::Immediate(Value::Null));
                }
                if self.package_module_request_requires_kernel(parent) {
                    return None;
                }
                let resolved = self.with_module_resolver(|resolver| {
                    resolver.resolve_module(specifier, parent, mode)
                });
                let resolved = match resolved {
                    Ok(resolved) => resolved,
                    Err(error) => return Some(LocalBridgeCallResult::Error(error)),
                };
                if resolved.is_none() && self.has_module_reader() {
                    return None;
                }
                Some(LocalBridgeCallResult::Immediate(
                    resolved.map(Value::String).unwrap_or(Value::Null),
                ))
            }
            "_moduleFormat" => {
                let path = args.first().and_then(Value::as_str).unwrap_or("");
                // A forwarding reader means the live kernel VFS owns paths
                // outside the explicitly admitted host projections. The local
                // resolver cannot distinguish a missing package.json there
                // from Node's real default-CommonJS classification, so a
                // locally inferred `commonjs` result would hide the
                // authoritative package scope (notably projected `.aospkg`
                // packages). Forward those paths to the kernel just like
                // resolve/load misses; explicit trusted host mappings remain
                // eligible for the fast local path.
                if self.has_module_reader() && !self.translator.explicitly_maps_guest_path(path) {
                    return None;
                }
                let format = match self.module_format(path) {
                    Ok(format) => format,
                    Err(error) => return Some(LocalBridgeCallResult::Error(error)),
                };
                if format.is_none() && self.has_module_reader() {
                    return None;
                }
                Some(LocalBridgeCallResult::Immediate(
                    format
                        .map(|format| Value::String(String::from(format.as_str())))
                        .unwrap_or(Value::Null),
                ))
            }
            "_loadFile" | "_loadFileSync" => {
                let source =
                    match self.load_file(args.first().and_then(Value::as_str).unwrap_or("")) {
                        Ok(source) => source,
                        Err(error) => return Some(LocalBridgeCallResult::Error(error)),
                    };
                if source.is_none() && self.has_module_reader() {
                    return None;
                }
                Some(LocalBridgeCallResult::Immediate(
                    source.map(Value::String).unwrap_or(Value::Null),
                ))
            }
            "_batchResolveModules" => {
                if args
                    .first()
                    .and_then(Value::as_array)
                    .is_some_and(|requests| {
                        requests.iter().any(|request| {
                            request
                                .as_array()
                                .and_then(|pair| pair.get(1))
                                .and_then(Value::as_str)
                                .is_some_and(|parent| {
                                    self.package_module_request_requires_kernel(parent)
                                })
                        })
                    })
                {
                    return None;
                }
                let resolved = match self.batch_resolve_modules(args) {
                    Ok(resolved) => resolved,
                    Err(error) => return Some(LocalBridgeCallResult::Error(error)),
                };
                if self.has_module_reader()
                    && resolved
                        .as_array()
                        .is_some_and(|items| items.iter().any(Value::is_null))
                {
                    return None;
                }
                Some(LocalBridgeCallResult::Immediate(resolved))
            }
            "_loadPolyfill" => Some(LocalBridgeCallResult::Immediate(
                self.handle_polyfill_dispatch(args),
            )),
            "_cryptoRandomFill" => {
                let size = args.first().and_then(Value::as_u64).unwrap_or(16) as usize;
                let mut bytes = vec![0u8; size];
                if let Err(error) = getrandom(&mut bytes) {
                    return Some(LocalBridgeCallResult::Error(HostServiceError::new(
                        "ERR_AGENTOS_ENTROPY_UNAVAILABLE",
                        format!("failed to fill the guest random buffer: {error}"),
                    )));
                }
                Some(LocalBridgeCallResult::Immediate(Value::String(
                    v8_runtime::base64_encode_pub(&bytes),
                )))
            }
            "_cryptoRandomUUID" => {
                let mut bytes = [0u8; 16];
                if let Err(error) = getrandom(&mut bytes) {
                    return Some(LocalBridgeCallResult::Error(HostServiceError::new(
                        "ERR_AGENTOS_ENTROPY_UNAVAILABLE",
                        format!("failed to generate a guest UUID: {error}"),
                    )));
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
            "_kernelStdinRead" | "_kernelStdinReadRaw" if self.forward_kernel_stdin_rpc => None,
            "_kernelStdinRead" | "_kernelStdinReadRaw" => Some(LocalBridgeCallResult::Immediate(
                self.kernel_stdin.read(args),
            )),
            "_pythonStdinRead" => Some(LocalBridgeCallResult::Immediate(
                self.kernel_stdin.read_python_raw(args),
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
        let Some((dispatch_method, payload_json)) = dispatch
            .strip_prefix("__bd:")
            .and_then(|value| value.split_once(':'))
        else {
            return Value::String(
                timer_dispatch_error(javascript_timer_error(
                    "ERR_AGENTOS_BRIDGE_DISPATCH_DECODE",
                    "malformed bridge dispatch envelope",
                ))
                .to_string(),
            );
        };
        let payload = match serde_json::from_str::<Value>(payload_json) {
            Ok(payload) => payload,
            Err(error) => {
                return Value::String(
                    timer_dispatch_error(javascript_timer_error(
                        "ERR_AGENTOS_BRIDGE_DISPATCH_DECODE",
                        format!("invalid dispatch payload: {error}"),
                    ))
                    .to_string(),
                );
            }
        };
        let Some(args) = payload.as_array() else {
            return Value::String(
                timer_dispatch_error(javascript_timer_error(
                    "ERR_AGENTOS_BRIDGE_DISPATCH_DECODE",
                    "dispatch payload must be an array",
                ))
                .to_string(),
            );
        };
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
                match self.create_kernel_timer(delay_ms, repeat) {
                    Ok(timer_id) => json!(timer_id),
                    Err(error) => timer_dispatch_error(error),
                }
            }
            "kernelTimerArm" => {
                if let Some(timer_id) = args.first().and_then(Value::as_u64) {
                    if let Err(error) = self.arm_kernel_timer(timer_id) {
                        return timer_dispatch_error(error);
                    }
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

        if result.get("__bd_error").is_some() {
            Value::String(serde_json::to_string(&result).unwrap_or_else(|_| {
                String::from(
                    "{\"__bd_error\":{\"name\":\"Error\",\"message\":\"dispatch failed\"}}",
                )
            }))
        } else if dispatch_method.starts_with("kernel") {
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

    fn create_kernel_timer(
        &mut self,
        delay_ms: u64,
        repeat: bool,
    ) -> Result<u64, HostServiceError> {
        self.register_timer(delay_ms, repeat)
    }

    /// Allocate a fresh timer id and register a one-shot (`repeat == false`)
    /// tracking entry at generation 0. Used by the bridge-timer path so the
    /// queued wheel action can be cancelled (its entry removed) on `clear`/teardown.
    fn register_oneshot_timer(&mut self, delay_ms: u64) -> Result<u64, HostServiceError> {
        self.register_timer(delay_ms, false)
    }

    fn register_timer(&mut self, delay_ms: u64, repeat: bool) -> Result<u64, HostServiceError> {
        let mut timers = self.timers.lock().map_err(|_| {
            javascript_timer_error(
                "ERR_AGENTOS_JAVASCRIPT_TIMER_STATE",
                "JavaScript timer registry lock poisoned",
            )
        })?;
        if timers.len() >= self.max_timers {
            return Err(HostServiceError::limit(
                "ERR_AGENTOS_JAVASCRIPT_TIMER_LIMIT",
                "limits.jsRuntime.maxTimers",
                self.max_timers as u64,
                timers.len().saturating_add(1) as u64,
            ));
        }
        let reservation = self
            .timer_resources
            .as_ref()
            .ok_or_else(|| {
                javascript_timer_error(
                    "ERR_AGENTOS_RUNTIME_NOT_INJECTED",
                    "JavaScript timers require a resource ledger",
                )
            })?
            .reserve(agentos_runtime::accounting::ResourceClass::Timers, 1)
            .map_err(|error| {
                javascript_timer_error("ERR_AGENTOS_RESOURCE_LIMIT", error.to_string())
                    .with_details(json!({
                        "limitName": "limits.jsRuntime.maxTimers",
                        "requested": 1,
                    }))
            })?;
        let timer_id = self.next_timer_id.checked_add(1).ok_or_else(|| {
            javascript_timer_error(
                "ERR_AGENTOS_JAVASCRIPT_TIMER_ID_EXHAUSTED",
                "execution exhausted timer IDs",
            )
        })?;
        self.next_timer_id = timer_id;
        timers.insert(
            timer_id,
            LocalTimerEntry {
                delay_ms,
                generation: 0,
                repeat,
                _reservation: Some(reservation),
            },
        );
        Ok(timer_id)
    }

    fn arm_kernel_timer(&self, timer_id: u64) -> Result<(), HostServiceError> {
        let Some(session) = self.v8_session.clone() else {
            return Err(javascript_timer_error(
                "ERR_AGENTOS_JAVASCRIPT_TIMER_SESSION",
                "timer has no live V8 session",
            ));
        };

        let (delay_ms, generation, timers) = {
            let mut timers = self.timers.lock().map_err(|_| {
                javascript_timer_error(
                    "ERR_AGENTOS_JAVASCRIPT_TIMER_STATE",
                    "JavaScript timer registry lock poisoned",
                )
            })?;
            let entry = timers.get_mut(&timer_id).ok_or_else(|| {
                javascript_timer_error(
                    "ERR_AGENTOS_JAVASCRIPT_TIMER_UNKNOWN",
                    format!("unknown timer {timer_id}"),
                )
            })?;
            entry.generation = entry.generation.checked_add(1).ok_or_else(|| {
                javascript_timer_error(
                    "ERR_AGENTOS_JAVASCRIPT_TIMER_GENERATION_EXHAUSTED",
                    format!("timer {timer_id} exhausted generations"),
                )
            })?;
            (entry.delay_ms, entry.generation, self.timers.clone())
        };

        let runtime = self.runtime.as_ref().ok_or_else(|| {
            javascript_timer_error(
                "ERR_AGENTOS_RUNTIME_NOT_INJECTED",
                "JavaScript timers require a process RuntimeContext",
            )
        })?;
        TimerWheel::get(runtime)?.schedule(
            delay_ms,
            TimerAction::StreamEvent {
                session,
                timer_id,
                generation,
                timers,
            },
        )
    }

    fn clear_kernel_timer(&self, timer_id: u64) {
        if let Ok(mut timers) = self.timers.lock() {
            timers.remove(&timer_id);
        }
        if let Some(wheel) = JAVASCRIPT_TIMER_WHEEL.get() {
            wheel.cancel(&self.timers, timer_id);
        }
    }

    fn schedule_bridge_timer_response(&mut self, call_id: u64, delay_ms: u64) {
        let Some(session) = self.v8_session.clone() else {
            return;
        };

        // Register the bridge timer in the shared `timers` map with a generation,
        // mirroring the kernel-timer cancellation path. Tracking it means that
        // when `LocalBridgeState` is dropped on session teardown (which clears the
        // map) or the entry is otherwise removed, the timer wheel observes the
        // missing/mismatched generation via `timer_should_fire` and suppresses the
        // response instead of touching the torn-down session.
        let timer_id = match self.register_oneshot_timer(delay_ms) {
            Ok(timer_id) => timer_id,
            Err(error) => {
                settle_timer_bridge_response(&session, call_id, 1, error.to_string().into_bytes());
                return;
            }
        };
        let generation = 0;
        let timers = self.timers.clone();

        let Some(runtime) = self.runtime.as_ref() else {
            self.clear_kernel_timer(timer_id);
            let error = "ERR_AGENTOS_RUNTIME_NOT_INJECTED: JavaScript timers require a process RuntimeContext";
            settle_timer_bridge_response(&session, call_id, 1, error.as_bytes().to_vec());
            return;
        };
        let wheel = match TimerWheel::get(runtime) {
            Ok(wheel) => wheel,
            Err(error) => {
                self.clear_kernel_timer(timer_id);
                settle_timer_bridge_response(&session, call_id, 1, error.to_string().into_bytes());
                return;
            }
        };
        if let Err(error) = wheel.schedule(
            delay_ms,
            TimerAction::BridgeResponse {
                session: session.clone(),
                call_id,
                timer_id,
                generation,
                timers,
            },
        ) {
            self.clear_kernel_timer(timer_id);
            settle_timer_bridge_response(&session, call_id, 1, error.to_string().into_bytes());
        }
    }

    fn has_module_reader(&self) -> bool {
        self.module_reader.is_some()
    }

    fn batch_resolve_modules(&mut self, args: &[Value]) -> Result<Value, HostServiceError> {
        self.with_module_resolver(|resolver| resolver.batch_resolve_modules(args))
    }

    fn resolve_module(
        &mut self,
        specifier: &str,
        from_dir: &str,
        mode: ModuleResolveMode,
    ) -> Result<Option<String>, HostServiceError> {
        if self.js_runtime_denies_specifier(specifier) {
            if std::env::var("AGENTOS_MODULE_READER_TRACE").is_ok() {
                eprintln!("resolve DENIED: {specifier} from {from_dir}");
            }
            return Ok(None);
        }
        let resolved = self
            .with_module_resolver(|resolver| resolver.resolve_module(specifier, from_dir, mode));
        if resolved.as_ref().is_ok_and(Option::is_none)
            && std::env::var("AGENTOS_MODULE_READER_TRACE").is_ok()
        {
            eprintln!("resolve MISS: {specifier} from {from_dir} mode={mode:?}");
        }
        resolved
    }

    /// jsRuntime resolution gate. Denies builtin and bare/relative imports per the
    /// configured `moduleResolution` and builtin allow-list, before the resolver
    /// touches the VFS. This is the authoritative chokepoint for the live
    /// shared-V8 path (both `import`/`import()` and `require`/`createRequire`
    /// route through `_resolveModule` -> here).
    fn js_runtime_denies_specifier(&self, specifier: &str) -> bool {
        let is_local = specifier.starts_with("./")
            || specifier.starts_with("../")
            || specifier == "."
            || specifier == ".."
            || specifier.starts_with('/')
            || specifier.starts_with("file:");
        match self.module_resolution {
            GuestModuleResolution::Node => false,
            // Relative permits local files only; bare specifiers and package
            // imports (`#...`) do not resolve.
            GuestModuleResolution::Relative => !is_local,
            // None denies every specifier, local included.
            GuestModuleResolution::None => true,
        }
    }

    fn module_format(
        &mut self,
        path: &str,
    ) -> Result<Option<LocalResolvedModuleFormat>, HostServiceError> {
        self.with_module_resolver(|resolver| resolver.module_format(path))
    }

    fn load_file(&mut self, path: &str) -> Result<Option<String>, HostServiceError> {
        self.with_module_resolver(|resolver| resolver.load_file(path))
    }

    /// Run `f` against a resolver bound to this bridge's resolution cache, reading
    /// through the supplied VFS `module_reader` when present (the live VM path:
    /// resolution executes here on the bridge thread, reading the mounted
    /// `node_modules` filesystem in parallel with the service loop), or through
    /// the host-backed path translator otherwise (the legacy host-direct path
    /// used by `handle_internal_bridge_call_from_host_context`).
    fn with_module_resolver<T>(
        &mut self,
        f: impl FnOnce(&mut ModuleResolver<'_, &mut dyn ModuleFsReader>) -> T,
    ) -> T {
        let cache = &mut self.resolution_cache;
        if let Some(reader) = self.module_reader.as_deref_mut() {
            let mut layered = LayeredModuleFsReader {
                primary: reader,
                explicit_host_mappings: &mut self.translator,
            };
            let reader: &mut dyn ModuleFsReader = &mut layered;
            let mut resolver = ModuleResolver { reader, cache };
            f(&mut resolver)
        } else {
            let mut translator = &mut self.translator;
            let reader: &mut dyn ModuleFsReader = &mut translator;
            let mut resolver = ModuleResolver { reader, cache };
            f(&mut resolver)
        }
    }
}

struct LayeredModuleFsReader<'a> {
    primary: &'a mut dyn ModuleFsReader,
    explicit_host_mappings: &'a mut GuestPathTranslator,
}

struct ForwardingModuleFsReader;

impl ModuleFsReader for ForwardingModuleFsReader {
    fn canonical_guest_path(
        &mut self,
        _guest_path: &str,
    ) -> Result<Option<String>, HostServiceError> {
        Ok(None)
    }

    fn read_to_string(&mut self, _guest_path: &str) -> Result<Option<String>, HostServiceError> {
        Ok(None)
    }

    fn path_is_dir(&mut self, _guest_path: &str) -> Result<Option<bool>, HostServiceError> {
        Ok(None)
    }

    fn path_exists(&mut self, _guest_path: &str) -> Result<bool, HostServiceError> {
        Ok(false)
    }
}

impl ModuleFsReader for LayeredModuleFsReader<'_> {
    fn canonical_guest_path(
        &mut self,
        guest_path: &str,
    ) -> Result<Option<String>, HostServiceError> {
        if let Some(path) = self.primary.canonical_guest_path(guest_path)? {
            return Ok(Some(path));
        }
        if !self
            .explicit_host_mappings
            .explicitly_maps_guest_path(guest_path)
        {
            return Ok(None);
        }
        self.explicit_host_mappings
            .canonical_module_guest_path(guest_path)
    }

    fn read_to_string(&mut self, guest_path: &str) -> Result<Option<String>, HostServiceError> {
        if let Some(source) = self.primary.read_to_string(guest_path)? {
            return Ok(Some(source));
        }
        if !self
            .explicit_host_mappings
            .explicitly_maps_guest_path(guest_path)
        {
            return Ok(None);
        }
        ModuleFsReader::read_to_string(&mut self.explicit_host_mappings, guest_path)
    }

    fn path_is_dir(&mut self, guest_path: &str) -> Result<Option<bool>, HostServiceError> {
        if let Some(is_dir) = self.primary.path_is_dir(guest_path)? {
            return Ok(Some(is_dir));
        }
        if !self
            .explicit_host_mappings
            .explicitly_maps_guest_path(guest_path)
        {
            return Ok(None);
        }
        ModuleFsReader::path_is_dir(&mut self.explicit_host_mappings, guest_path)
    }

    fn path_exists(&mut self, guest_path: &str) -> Result<bool, HostServiceError> {
        if self.primary.path_exists(guest_path)? {
            return Ok(true);
        }
        if !self
            .explicit_host_mappings
            .explicitly_maps_guest_path(guest_path)
        {
            return Ok(false);
        }
        ModuleFsReader::path_exists(&mut self.explicit_host_mappings, guest_path)
    }
}

impl ModuleFsReader for &mut dyn ModuleFsReader {
    fn canonical_guest_path(
        &mut self,
        guest_path: &str,
    ) -> Result<Option<String>, HostServiceError> {
        (**self).canonical_guest_path(guest_path)
    }

    fn read_to_string(&mut self, guest_path: &str) -> Result<Option<String>, HostServiceError> {
        (**self).read_to_string(guest_path)
    }

    fn path_is_dir(&mut self, guest_path: &str) -> Result<Option<bool>, HostServiceError> {
        (**self).path_is_dir(guest_path)
    }

    fn path_exists(&mut self, guest_path: &str) -> Result<bool, HostServiceError> {
        (**self).path_exists(guest_path)
    }
}

impl ModuleFsReader for &mut GuestPathTranslator {
    fn canonical_guest_path(
        &mut self,
        guest_path: &str,
    ) -> Result<Option<String>, HostServiceError> {
        self.canonical_module_guest_path(guest_path)
    }

    fn read_to_string(&mut self, guest_path: &str) -> Result<Option<String>, HostServiceError> {
        let Some(host_path) = self.module_guest_to_host(guest_path)? else {
            return Ok(None);
        };
        match fs::read_to_string(&host_path) {
            Ok(contents) => Ok(Some(contents)),
            Err(error) if module_fs_io_is_missing(&error) => Ok(None),
            Err(error) => Err(module_fs_io_error("read", guest_path, error)),
        }
    }

    fn path_is_dir(&mut self, guest_path: &str) -> Result<Option<bool>, HostServiceError> {
        let Some(host_path) = self.module_guest_to_host(guest_path)? else {
            return Ok(None);
        };
        match fs::metadata(&host_path) {
            Ok(metadata) => Ok(Some(metadata.is_dir())),
            Err(error) if module_fs_io_is_missing(&error) => Ok(None),
            Err(error) => Err(module_fs_io_error("stat", guest_path, error)),
        }
    }

    fn path_exists(&mut self, guest_path: &str) -> Result<bool, HostServiceError> {
        Ok(self.path_is_dir(guest_path)?.is_some())
    }
}

fn module_fs_io_is_missing(error: &std::io::Error) -> bool {
    matches!(error.raw_os_error(), Some(2 | 20)) || error.kind() == std::io::ErrorKind::NotFound
}

fn module_fs_io_error(
    operation: &str,
    guest_path: &str,
    error: std::io::Error,
) -> HostServiceError {
    let code = match error.raw_os_error() {
        Some(2) => "ENOENT",
        Some(13) => "EACCES",
        Some(20) => "ENOTDIR",
        Some(40) => "ELOOP",
        _ => "EIO",
    };
    HostServiceError::new(
        code,
        format!("module filesystem {operation} failed for {guest_path}: {error}"),
    )
}

/// Standard Node module resolution executed as pure path algebra over a
/// [`ModuleFsReader`]. The same algorithm backs both the legacy host-direct
/// path (reader = host path translator) and the live VM path (reader = kernel
/// VFS), guaranteeing they resolve identically.
pub struct ModuleResolver<'a, R: ModuleFsReader> {
    reader: R,
    cache: &'a mut LocalModuleResolutionCache,
}

impl<'a, R: ModuleFsReader> ModuleResolver<'a, R> {
    /// Construct a resolver over `reader`, reusing `cache` across calls. The
    /// cache must be persisted per-VM so cold-start resolution does not rebuild
    /// it on every dispatch.
    pub fn new(reader: R, cache: &'a mut LocalModuleResolutionCache) -> Self {
        Self { reader, cache }
    }

    pub fn batch_resolve_modules(&mut self, args: &[Value]) -> Result<Value, HostServiceError> {
        let requests = args
            .first()
            .and_then(Value::as_array)
            .ok_or_else(|| {
                HostServiceError::new(
                    "EINVAL",
                    "batch module resolution requires an array of [specifier, referrer] pairs",
                )
            })?
            .clone();
        let mut results = Vec::with_capacity(requests.len());
        for (index, request) in requests.into_iter().enumerate() {
            let pair = request.as_array().ok_or_else(|| {
                HostServiceError::new(
                    "EINVAL",
                    format!("batch module request {index} must be an array pair"),
                )
            })?;
            let specifier = pair.first().and_then(Value::as_str).ok_or_else(|| {
                HostServiceError::new(
                    "EINVAL",
                    format!("batch module request {index} requires a string specifier"),
                )
            })?;
            let referrer = pair.get(1).and_then(Value::as_str).ok_or_else(|| {
                HostServiceError::new(
                    "EINVAL",
                    format!("batch module request {index} requires a string referrer"),
                )
            })?;
            let value = if let Some(resolved) =
                self.resolve_module(specifier, referrer, ModuleResolveMode::Import)?
            {
                self.load_file(&resolved)?.map_or(Value::Null, |source| {
                    json!({
                        "resolved": resolved,
                        "source": source,
                    })
                })
            } else {
                Value::Null
            };
            results.push(value);
        }
        Ok(Value::Array(results))
    }

    pub fn resolve_module(
        &mut self,
        specifier: &str,
        from_dir: &str,
        mode: ModuleResolveMode,
    ) -> Result<Option<String>, HostServiceError> {
        let normalized_from_path = self
            .reader
            .canonical_guest_path(from_dir)?
            .unwrap_or_else(|| normalize_guest_path(from_dir));
        let normalized_from = if self.cached_stat(&normalized_from_path)? == Some(false) {
            dirname_guest_path(&normalized_from_path)
        } else {
            normalize_module_resolve_context(&normalized_from_path)
        };
        let cache_key = (specifier.to_owned(), normalized_from.clone(), mode);
        if let Some(cached) = self.cache.resolve_results.get(&cache_key) {
            return Ok(cached.clone());
        }

        let resolved = if let Some(builtin) = normalize_builtin_specifier(specifier) {
            Some(builtin)
        } else if specifier.starts_with("file:") {
            match guest_path_from_file_url(specifier) {
                Some(file_path) => self.resolve_path(&file_path, mode)?,
                None => None,
            }
        } else if specifier.starts_with('/') {
            self.resolve_path(specifier, mode)?
        } else if specifier.starts_with("./")
            || specifier.starts_with("../")
            || specifier == "."
            || specifier == ".."
        {
            self.resolve_path(&join_guest_path(&normalized_from, specifier), mode)?
        } else if specifier.starts_with('#') {
            self.resolve_package_imports(specifier, &normalized_from, mode)?
        } else {
            let own = self.resolve_package_self_reference(specifier, &normalized_from, mode)?;
            match own {
                Some(resolved) => Some(resolved),
                None => self.resolve_node_modules(specifier, &normalized_from, mode)?,
            }
        };

        if resolved.is_some() || module_resolution_miss_is_stable(&normalized_from) {
            self.cache
                .resolve_results
                .insert(cache_key, resolved.clone());
        }
        Ok(resolved)
    }

    pub fn load_file(&mut self, path: &str) -> Result<Option<String>, HostServiceError> {
        let bare = path.trim_start_matches("node:");
        if is_builtin_specifier(path) {
            return Ok(Some(build_builtin_module_wrapper(bare)));
        }

        let Some(source) = self.reader.read_to_string(path)? else {
            return Ok(None);
        };
        Ok(Some(
            if matches!(
                Path::new(path).extension().and_then(|ext| ext.to_str()),
                Some("js" | "mjs" | "cjs")
            ) {
                strip_javascript_hashbang(&source)
            } else {
                source
            },
        ))
    }

    pub fn module_format(
        &mut self,
        path: &str,
    ) -> Result<Option<LocalResolvedModuleFormat>, HostServiceError> {
        if let Some(cached) = self.cache.module_format_results.get(path) {
            return Ok(*cached);
        }

        let format = self.detect_module_format(path)?;
        self.cache
            .module_format_results
            .insert(path.to_owned(), format);
        Ok(format)
    }

    fn detect_module_format(
        &mut self,
        path: &str,
    ) -> Result<Option<LocalResolvedModuleFormat>, HostServiceError> {
        if is_builtin_specifier(path) {
            return Ok(Some(LocalResolvedModuleFormat::Module));
        }

        let normalized = normalize_guest_path(path);
        Ok(
            match Path::new(&normalized)
                .extension()
                .and_then(|ext| ext.to_str())
            {
                Some("mjs" | "mts") => Some(LocalResolvedModuleFormat::Module),
                Some("cjs" | "cts") => Some(LocalResolvedModuleFormat::Commonjs),
                Some("json") => Some(LocalResolvedModuleFormat::Json),
                Some("js") => Some(
                    if self
                        .nearest_package_json_type_for_guest_path(&normalized)?
                        .as_deref()
                        == Some("module")
                    {
                        LocalResolvedModuleFormat::Module
                    } else {
                        LocalResolvedModuleFormat::Commonjs
                    },
                ),
                _ => None,
            },
        )
    }

    fn nearest_package_json_type_for_guest_path(
        &mut self,
        guest_path: &str,
    ) -> Result<Option<String>, HostServiceError> {
        let mut dir = dirname_guest_path(guest_path);
        loop {
            let package_json_path = join_guest_path(&dir, "package.json");
            if let Some(package_json) = self.read_package_json(&package_json_path)? {
                return Ok(package_json.package_type);
            }
            // Node package scopes do not inherit `type` across a node_modules
            // boundary. This also matters for pnpm's nested symlink layout: if
            // a package.json read is unavailable at the symlinked package root,
            // climbing into the fixture's `type: module` package would
            // incorrectly classify a dependency's CommonJS `.js` files as ESM.
            if dir == "/" || dir.rsplit('/').next() == Some("node_modules") {
                break;
            }
            dir = dirname_guest_path(&dir);
        }
        Ok(None)
    }

    fn resolve_package_imports(
        &mut self,
        request: &str,
        from_dir: &str,
        mode: ModuleResolveMode,
    ) -> Result<Option<String>, HostServiceError> {
        let mut dir = normalize_guest_path(from_dir);
        loop {
            let pkg_json_path = join_guest_path(&dir, "package.json");
            if let Some(pkg_json) = self.read_package_json(&pkg_json_path)? {
                if let Some(imports) = &pkg_json.imports {
                    if let Some(target) = resolve_imports_target(imports, request, mode) {
                        let target_path = if target.starts_with('/') {
                            target
                        } else {
                            join_guest_path(&dir, &target)
                        };
                        return self.resolve_path(&target_path, mode);
                    }
                    return Ok(None);
                }
            }
            if dir == "/" {
                break;
            }
            dir = dirname_guest_path(&dir);
        }
        Ok(None)
    }

    fn resolve_package_self_reference(
        &mut self,
        request: &str,
        from_dir: &str,
        mode: ModuleResolveMode,
    ) -> Result<Option<String>, HostServiceError> {
        let Some((package_name, subpath)) = split_package_request(request) else {
            return Ok(None);
        };
        let mut dir = normalize_guest_path(from_dir);
        loop {
            let pkg_json_path = join_guest_path(&dir, "package.json");
            if let Some(pkg_json) = self.read_package_json(&pkg_json_path)? {
                if pkg_json.name.as_deref() == Some(package_name) {
                    return self.resolve_package_entry_from_dir(&dir, subpath, mode);
                }
            }
            if dir == "/" {
                break;
            }
            dir = dirname_guest_path(&dir);
        }
        Ok(None)
    }

    fn resolve_node_modules(
        &mut self,
        request: &str,
        from_dir: &str,
        mode: ModuleResolveMode,
    ) -> Result<Option<String>, HostServiceError> {
        let Some((package_name, subpath)) = split_package_request(request) else {
            return Ok(None);
        };

        // Standard Node resolution over the faithful VFS: walk ancestor
        // `node_modules` directories (following symlinks via the importer's
        // realpath). pnpm/yarn layouts resolve because the VFS exposes their
        // symlinks, not because the resolver understands package-manager
        // internals (see CLAUDE.md npm Compatibility).
        let mut dir = normalize_guest_path(from_dir);
        loop {
            for package_dir in node_modules_direct_candidate_dirs(&dir, package_name) {
                if let Some(entry) =
                    self.resolve_package_entry_from_dir(&package_dir, subpath, mode)?
                {
                    return Ok(Some(entry));
                }
            }
            if dir == "/" {
                break;
            }
            dir = dirname_guest_path(&dir);
        }

        for root in ["/root/node_modules", "/node_modules"] {
            if let Some(entry) = self.resolve_package_entry_from_dir(
                &join_guest_path(root, package_name),
                subpath,
                mode,
            )? {
                return Ok(Some(entry));
            }
        }
        Ok(None)
    }

    fn resolve_package_entry_from_dir(
        &mut self,
        package_dir: &str,
        subpath: &str,
        mode: ModuleResolveMode,
    ) -> Result<Option<String>, HostServiceError> {
        let package_json_path = join_guest_path(package_dir, "package.json");
        let pkg_json = self.read_package_json(&package_json_path)?;
        if pkg_json.is_none() && !self.cached_exists(package_dir)? {
            return Ok(None);
        }

        if let Some(pkg_json) = pkg_json.as_ref() {
            if let Some(exports) = &pkg_json.exports {
                let exports_subpath = if subpath.is_empty() {
                    String::from(".")
                } else {
                    format!("./{subpath}")
                };
                let Some(exports_target) = resolve_exports_target(exports, &exports_subpath, mode)
                else {
                    return Ok(None);
                };
                let target_path = join_guest_path(package_dir, &exports_target);
                let resolved = self.resolve_path(&target_path, mode)?;
                return Ok(resolved.or(Some(target_path)));
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
        if let Some(resolved) = self.resolve_path(&entry_path, mode)? {
            return Ok(Some(resolved));
        }
        self.resolve_path(&join_guest_path(package_dir, "index"), mode)
    }

    fn resolve_path(
        &mut self,
        base_path: &str,
        mode: ModuleResolveMode,
    ) -> Result<Option<String>, HostServiceError> {
        if self.cached_stat(base_path)? == Some(false) {
            return Ok(Some(normalize_guest_path(base_path)));
        }

        for extension in [".js", ".json", ".mjs", ".cjs"] {
            let candidate = format!("{}{}", normalize_guest_path(base_path), extension);
            if self.cached_exists(&candidate)? {
                return Ok(Some(candidate));
            }
        }

        if self.cached_stat(base_path)? == Some(true) {
            let pkg_json_path = join_guest_path(base_path, "package.json");
            if let Some(pkg_json) = self.read_package_json(&pkg_json_path)? {
                if let Some(main) = pkg_json.main.as_deref() {
                    let entry_path = join_guest_path(base_path, main);
                    if entry_path != normalize_guest_path(base_path) {
                        if let Some(entry) = self.resolve_path(&entry_path, mode)? {
                            return Ok(Some(entry));
                        }
                    }
                }
                if mode == ModuleResolveMode::Import
                    && pkg_json.package_type.as_deref() == Some("module")
                    && self.cached_exists(&join_guest_path(base_path, "index.js"))?
                {
                    return Ok(Some(join_guest_path(base_path, "index.js")));
                }
            }

            for extension in [".js", ".json", ".mjs", ".cjs"] {
                let index_path = join_guest_path(base_path, &format!("index{extension}"));
                if self.cached_exists(&index_path)? {
                    return Ok(Some(index_path));
                }
            }
        }

        Ok(None)
    }

    fn read_package_json(
        &mut self,
        guest_path: &str,
    ) -> Result<Option<LocalPackageJson>, HostServiceError> {
        if let Some(cached) = self.cache.package_json_results.get(guest_path).cloned() {
            return Ok(cached);
        }

        let parsed = match self.reader.read_to_string(guest_path)? {
            Some(contents) => Some(serde_json::from_str::<LocalPackageJson>(&contents).map_err(
                |error| {
                    HostServiceError::new(
                        "ERR_INVALID_PACKAGE_CONFIG",
                        format!("invalid package configuration {guest_path}: {error}"),
                    )
                },
            )?),
            None => None,
        };
        if parsed.is_some() || module_path_miss_is_stable(guest_path) {
            self.cache
                .package_json_results
                .insert(guest_path.to_owned(), parsed.clone());
        }
        Ok(parsed)
    }

    fn cached_exists(&mut self, guest_path: &str) -> Result<bool, HostServiceError> {
        if let Some(cached) = self.cache.exists_results.get(guest_path) {
            return Ok(*cached);
        }
        let exists = self.reader.path_exists(guest_path)?;
        if exists || module_path_miss_is_stable(guest_path) {
            self.cache
                .exists_results
                .insert(guest_path.to_owned(), exists);
        }
        Ok(exists)
    }

    fn cached_stat(&mut self, guest_path: &str) -> Result<Option<bool>, HostServiceError> {
        if let Some(cached) = self.cache.stat_results.get(guest_path) {
            return Ok(*cached);
        }
        let result = self.reader.path_is_dir(guest_path)?;
        if result.is_some() || module_path_miss_is_stable(guest_path) {
            self.cache
                .stat_results
                .insert(guest_path.to_owned(), result);
        }
        Ok(result)
    }
}

fn module_resolution_miss_is_stable(from_dir: &str) -> bool {
    module_path_miss_is_stable(from_dir)
}

fn module_path_miss_is_stable(guest_path: &str) -> bool {
    guest_path == "/node_modules"
        || guest_path.ends_with("/node_modules")
        || guest_path.contains("/node_modules/")
}

fn guest_path_from_file_url(specifier: &str) -> Option<String> {
    if !specifier.starts_with("file:") {
        return None;
    }

    let mut pathname = if let Some(stripped) = specifier.strip_prefix("file://") {
        stripped
    } else {
        specifier.strip_prefix("file:")?
    };

    if let Some(terminator_index) = pathname.find(['?', '#']) {
        pathname = &pathname[..terminator_index];
    }

    if !pathname.starts_with('/') {
        let slash_index = pathname.find('/')?;
        let host = &pathname[..slash_index];
        if !host.is_empty() && host != "localhost" {
            return None;
        }
        pathname = &pathname[slash_index..];
    }

    Some(normalize_guest_path(&percent_decode(pathname)?))
}

fn percent_decode(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut index = 0;
    let mut decoded = Vec::with_capacity(bytes.len());
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                if let (Some(high), Some(low)) =
                    (hex_digit(bytes[index + 1]), hex_digit(bytes[index + 2]))
                {
                    let value = (high << 4) | low;
                    decoded.push(value);
                    index += 3;
                } else {
                    decoded.push(bytes[index]);
                    index += 1;
                }
            }
            byte => {
                decoded.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(decoded).ok()
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

impl LocalKernelStdinBridge {
    fn reset(&self) {
        let mut state = self.state.lock().expect("kernel stdin state poisoned");
        state.bytes.clear();
        state.closed = false;
    }

    fn write(&self, chunk: &[u8]) -> Result<(), JavascriptExecutionError> {
        let mut state = self.state.lock().expect("kernel stdin state poisoned");
        if state.closed {
            return Err(JavascriptExecutionError::StdinClosed);
        }
        let next_len = state.bytes.len().checked_add(chunk.len()).ok_or_else(|| {
            JavascriptExecutionError::Stdin(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("guest stdin buffer exceeded {KERNEL_STDIN_BUFFER_LIMIT_BYTES} bytes"),
            ))
        })?;
        if next_len > KERNEL_STDIN_BUFFER_LIMIT_BYTES {
            return Err(JavascriptExecutionError::Stdin(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("guest stdin buffer exceeded {KERNEL_STDIN_BUFFER_LIMIT_BYTES} bytes"),
            )));
        }

        state.bytes.extend(chunk.iter().copied());
        self.ready.notify_all();
        Ok(())
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
        let deadline = if args.get(1).is_some_and(Value::is_null) {
            None
        } else {
            let timeout = Duration::from_millis(args.get(1).and_then(Value::as_u64).unwrap_or(100));
            Some(Instant::now() + timeout)
        };
        let mut state = self.state.lock().expect("kernel stdin state poisoned");

        while state.bytes.is_empty() && !state.closed {
            if let Some(deadline) = deadline {
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
            } else {
                state = self.ready.wait(state).expect("kernel stdin wait poisoned");
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

    fn read_python_raw(&self, args: &[Value]) -> Value {
        const PYTHON_STDIN_DONE_SENTINEL: &str = "__AGENTOS_PYTHON_STDIN_DONE__";

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
            return Value::String(v8_runtime::base64_encode_pub(&bytes));
        }

        Value::String(String::from(PYTHON_STDIN_DONE_SENTINEL))
    }
}

fn normalize_module_resolve_context(path: &str) -> String {
    let normalized = normalize_guest_path(path);
    if normalized == "/[eval]"
        || normalized.ends_with("/[eval]")
        || normalized.ends_with(".js")
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
        | "assert/strict"
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
        | "dns/promises"
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
        | "test"
        | "test/reporters"
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
                    "node:{normalized} is not available in the agentos guest runtime"
                ))
                .unwrap_or_else(|_| format!(
                    "\"node:{normalized} is not available in the agentos guest runtime\""
                )),
                code = serde_json::to_string(error_code)
                    .unwrap_or_else(|_| "\"ERR_ACCESS_DENIED\"".to_owned())
            )
        }
    })
}

fn build_builtin_module_wrapper(module_name: &str) -> String {
    if matches!(
        module_name,
        "assert"
            | "assert/strict"
            | "path"
            | "path/posix"
            | "path/win32"
            | "string_decoder"
            | "url"
    ) {
        return build_delegating_builtin_module_wrapper(module_name);
    }

    if module_name == "test" {
        return String::from(
            r#"const state = globalThis.__agentOSNodeTestState ??= {
  tests: [],
  suite: [],
  before: [],
  after: [],
  beforeEach: [],
  afterEach: [],
  ran: false,
};

function normalizeTest(name, optionsOrFn, maybeFn) {
  const options = typeof optionsOrFn === "object" && optionsOrFn !== null ? optionsOrFn : {};
  const fn = typeof optionsOrFn === "function" ? optionsOrFn : maybeFn;
  return { name: String(name), options, fn };
}

function test(name, optionsOrFn, maybeFn) {
  const record = normalizeTest(name, optionsOrFn, maybeFn);
  state.tests.push({
    ...record,
    name: [...state.suite, record.name].join(" > "),
  });
}
test.skip = (name, optionsOrFn, maybeFn) => {
  const record = normalizeTest(name, optionsOrFn, maybeFn);
  test(record.name, { ...record.options, skip: true }, record.fn);
};
test.todo = (name, optionsOrFn, maybeFn) => {
  const record = normalizeTest(name, optionsOrFn, maybeFn);
  test(record.name, { ...record.options, todo: true }, record.fn);
};
test.only = test;

function describe(name, optionsOrFn, maybeFn) {
  const record = normalizeTest(name, optionsOrFn, maybeFn);
  state.suite.push(record.name);
  try {
    record.fn?.();
  } finally {
    state.suite.pop();
  }
}
describe.skip = (_name, _optionsOrFn, _maybeFn) => {};
describe.only = describe;

function before(fn) { state.before.push(fn); }
function after(fn) { state.after.push(fn); }
function beforeEach(fn) { state.beforeEach.push(fn); }
function afterEach(fn) { state.afterEach.push(fn); }

async function __agentOSRunTests(namePattern) {
  if (state.ran) throw new Error("node:test runner was already consumed");
  state.ran = true;
  const pattern = namePattern ? new RegExp(namePattern) : null;
  const records = pattern ? state.tests.filter((record) => pattern.test(record.name)) : state.tests;
  let passed = 0;
  let failed = 0;
  let skipped = 0;
  console.log("TAP version 13");
  for (const hook of state.before) await hook();
  for (let index = 0; index < records.length; index += 1) {
    const record = records[index];
    if (record.options.skip || record.options.todo || typeof record.fn !== "function") {
      skipped += 1;
      console.log(`ok ${index + 1} - ${record.name} # SKIP`);
      continue;
    }
    try {
      for (const hook of state.beforeEach) await hook();
      const context = { test, skip() { throw Object.assign(new Error("skip"), { __agentOSTestSkip: true }); } };
      await record.fn(context);
      passed += 1;
      console.log(`ok ${index + 1} - ${record.name}`);
    } catch (error) {
      if (error?.__agentOSTestSkip) {
        skipped += 1;
        console.log(`ok ${index + 1} - ${record.name} # SKIP`);
      } else {
        failed += 1;
        console.log(`not ok ${index + 1} - ${record.name}`);
        console.log(`  error: ${JSON.stringify(String(error?.stack ?? error))}`);
      }
    } finally {
      for (const hook of state.afterEach) await hook();
    }
  }
  for (const hook of state.after) await hook();
  console.log(`1..${records.length}`);
  console.log(`# tests ${records.length}`);
  console.log(`# pass ${passed}`);
  console.log(`# fail ${failed}`);
  console.log(`# skipped ${skipped}`);
  return { total: records.length, passed, failed, skipped };
}

const it = test;
const suite = describe;
const mock = {};
export {
  __agentOSRunTests,
  after,
  afterEach,
  before,
  beforeEach,
  describe,
  it,
  mock,
  suite,
  test as default,
  test,
};
"#,
        );
    }

    if module_name == "test/reporters" {
        return String::from(
            r#"const empty = async function* (source) { for await (const event of source) yield event; };
export { empty as dot, empty as junit, empty as spec, empty as tap };
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
  const pendingQuestionResolves = [];

  const enqueueLine = (line) => {
    if (pendingQuestionResolves.length > 0) {
      const resolve = pendingQuestionResolves.shift();
      resolve(line);
      return;
    }
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
    while (pendingQuestionResolves.length > 0) {
      const resolve = pendingQuestionResolves.shift();
      resolve("");
    }
    if (pendingResolve) {
      const resolve = pendingResolve;
      pendingResolve = null;
      resolve({ done: true, value: void 0 });
    }
  };

  if (input && typeof input.on === "function") {
    input.on("data", onData);
    input.on("end", onEnd);
    if (typeof input.resume === "function") {
      input.resume();
    }
  }

  emitter.close = () => {
    if (closed) return;
    closed = true;
    if (input && typeof input.off === "function") {
      input.off("data", onData);
      input.off("end", onEnd);
    }
    flush();
    emitter.emit("close");
    while (pendingQuestionResolves.length > 0) {
      const resolve = pendingQuestionResolves.shift();
      resolve("");
    }
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
    const readLine = () => {
      if (queuedLines.length > 0) {
        return Promise.resolve(queuedLines.shift());
      }
      if (closed || ended) {
        return Promise.resolve("");
      }
      return new Promise((resolve) => {
        pendingQuestionResolves.push(resolve);
      });
    };
    if (typeof callback === "function") {
      void readLine().then((line) => {
        callback(line);
      });
      return;
    }
    return readLine();
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

    // Historical embedded-only stream classes live below only as source text
    // for compatibility archaeology. The active `node:stream` wrapper must
    // fall through to the runtime builtin so ESM and CommonJS share constructor
    // identity (including the Duplex used by node:net.Socket).
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
export const BrotliCompress = _m.BrotliCompress;
export const BrotliDecompress = _m.BrotliDecompress;
export const Deflate = _m.Deflate;
export const DeflateRaw = _m.DeflateRaw;
export const Gunzip = _m.Gunzip;
export const Gzip = _m.Gzip;
export const Inflate = _m.Inflate;
export const InflateRaw = _m.InflateRaw;
export const Unzip = _m.Unzip;
export const brotliCompress = _m.brotliCompress;
export const brotliCompressSync = _m.brotliCompressSync;
export const brotliDecompress = _m.brotliDecompress;
export const brotliDecompressSync = _m.brotliDecompressSync;
export const createBrotliCompress = _m.createBrotliCompress;
export const createBrotliDecompress = _m.createBrotliDecompress;
export const createDeflate = _m.createDeflate;
export const createDeflateRaw = _m.createDeflateRaw;
export const createGunzip = _m.createGunzip;
export const createGzip = _m.createGzip;
export const createInflate = _m.createInflate;
export const createInflateRaw = _m.createInflateRaw;
export const createUnzip = _m.createUnzip;
export const deflate = _m.deflate;
export const deflateRaw = _m.deflateRaw;
export const deflateRawSync = _m.deflateRawSync;
export const deflateSync = _m.deflateSync;
export const gunzip = _m.gunzip;
export const gunzipSync = _m.gunzipSync;
export const gzip = _m.gzip;
export const gzipSync = _m.gzipSync;
export const inflate = _m.inflate;
export const inflateRaw = _m.inflateRaw;
export const inflateRawSync = _m.inflateRawSync;
export const inflateSync = _m.inflateSync;
export const unzip = _m.unzip;
export const unzipSync = _m.unzipSync;
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
  if (interfaceValue && typeof interfaceValue === "object") {
    if (interfaceValue.__agentOSReadlineWrapped === true) {
      return interfaceValue;
    }
    Object.defineProperty(interfaceValue, "__agentOSReadlineWrapped", {
      value: true,
      configurable: true,
      enumerable: false,
      writable: false,
    });
    const options = args[0] && typeof args[0] === "object" ? args[0] : {};
    const output = options.output ?? null;
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
    const pendingQuestionResolves = [];
    let pendingResolve = null;
    let done = false;
    const enqueue = (line) => {
      if (pendingQuestionResolves.length > 0) {
        const resolve = pendingQuestionResolves.shift();
        resolve(line);
        return;
      }
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
      while (pendingQuestionResolves.length > 0) {
        const resolve = pendingQuestionResolves.shift();
        resolve("");
      }
      if (pendingResolve) {
        const resolve = pendingResolve;
        pendingResolve = null;
        resolve({ done: true, value: void 0 });
      }
    };
    const readLine = () => {
      if (queued.length > 0) {
        return Promise.resolve(queued.shift());
      }
      if (done) {
        return Promise.resolve("");
      }
      return new Promise((resolve) => {
        pendingQuestionResolves.push(resolve);
      });
    };
    originalOn?.("line", enqueue);
    originalOn?.("close", finish);
    interfaceValue.question = (prompt, callback) => {
      if (output && typeof output.write === "function" && prompt) {
        output.write(String(prompt));
      }
      if (typeof callback === "function") {
        void readLine().then((line) => {
          callback(line);
        });
        return;
      }
      return readLine();
    };
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
  const configured = Number(globalThis.__agentOSV8HeapLimitBytes);
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
            r#"const VM_CONTEXT_TAG = typeof Symbol === "function" ? Symbol.for("agentos.vm.context") : "__agentos_vm_context__";
const VM_CONTEXT_ID = typeof Symbol === "function" ? Symbol.for("agentos.vm.context.id") : "__agentos_vm_context_id__";

function createVmNotImplementedError(feature) {
  const error = new Error(`node:vm ${feature} is not implemented in the agentos guest runtime`);
  error.code = "ERR_NOT_IMPLEMENTED";
  return error;
}

function isVmContextCandidate(value) {
  return value !== null && (typeof value === "object" || typeof value === "function");
}

function normalizeVmOptions(options = undefined) {
  if (typeof options === "string") {
    return { filename: options };
  }
  if (!options || typeof options !== "object") {
    return {};
  }
  const normalized = {};
  if (typeof options.filename === "string") {
    normalized.filename = options.filename;
  }
  if (Number.isInteger(options.lineOffset)) {
    normalized.lineOffset = options.lineOffset;
  }
  if (Number.isInteger(options.columnOffset)) {
    normalized.columnOffset = options.columnOffset;
  }
  if (Number.isInteger(options.timeout) && options.timeout > 0) {
    normalized.timeout = options.timeout;
  }
  if (options.cachedData !== undefined) {
    normalized.cachedData = options.cachedData;
  }
  if (options.produceCachedData === true) {
    normalized.produceCachedData = true;
  }
  return normalized;
}

function mergeVmOptions(baseOptions, overrideOptions) {
  return { ...normalizeVmOptions(baseOptions), ...normalizeVmOptions(overrideOptions) };
}

function createContext(context = {}) {
  if (!isVmContextCandidate(context)) {
    throw new TypeError('The "object" argument must be of type object.');
  }
  if (context[VM_CONTEXT_TAG] === true && Number.isInteger(context[VM_CONTEXT_ID])) {
    return context;
  }
  const contextId = globalThis._vmCreateContext(context);
  Object.defineProperty(context, VM_CONTEXT_TAG, {
    value: true,
    configurable: true,
    enumerable: false,
    writable: false,
  });
  Object.defineProperty(context, VM_CONTEXT_ID, {
    value: contextId,
    configurable: false,
    enumerable: false,
    writable: false,
  });
  return context;
}

function isContext(context) {
  return isVmContextCandidate(context) && context[VM_CONTEXT_TAG] === true && Number.isInteger(context[VM_CONTEXT_ID]);
}

function assertContext(context) {
  if (!isContext(context)) {
    throw new TypeError('The "contextifiedObject" argument must be a vm context.');
  }
  return context;
}

function runInThisContext(code, options = undefined) {
  return globalThis._vmRunInThisContext(String(code), normalizeVmOptions(options));
}

function runInContext(code, contextifiedObject, options = undefined) {
  const context = assertContext(contextifiedObject);
  return globalThis._vmRunInContext(context[VM_CONTEXT_ID], String(code), normalizeVmOptions(options), context);
}

function runInNewContext(code, contextOrOptions = {}, maybeOptions = undefined) {
  const hasExplicitContext = isVmContextCandidate(contextOrOptions);
  const context = hasExplicitContext ? contextOrOptions : {};
  const options = hasExplicitContext ? maybeOptions : contextOrOptions;
  return runInContext(code, createContext(context), options);
}

class Script {
  constructor(code, options = undefined) {
    this.code = String(code);
    this.options = normalizeVmOptions(options);
    this.filename = this.options.filename ?? "evalmachine.<anonymous>";
    this.lineOffset = this.options.lineOffset ?? 0;
    this.columnOffset = this.options.columnOffset ?? 0;
    this.cachedData = this.options.cachedData;
    this.cachedDataProduced = false;
    this.cachedDataRejected = false;
  }

  createCachedData() {
    return typeof Buffer === "function" ? Buffer.alloc(0) : new Uint8Array(0);
  }

  runInThisContext(options = undefined) {
    return runInThisContext(this.code, mergeVmOptions(this.options, options));
  }

  runInContext(contextifiedObject, options = undefined) {
    return runInContext(this.code, contextifiedObject, mergeVmOptions(this.options, options));
  }

  runInNewContext(context = {}, options = undefined) {
    return runInNewContext(this.code, context, mergeVmOptions(this.options, options));
  }
}

function compileFunction() {
  throw createVmNotImplementedError("compileFunction");
}

function measureMemory() {
  throw createVmNotImplementedError("measureMemory");
}

export { Script, compileFunction, createContext, isContext, measureMemory, runInContext, runInNewContext, runInThisContext };
export default { Script, compileFunction, createContext, isContext, measureMemory, runInContext, runInNewContext, runInThisContext };
"#,
        );
    }

    if module_name == "worker_threads" {
        return String::from(
            r#"function createNotImplementedError(feature) {
  const error = new Error(`node:worker_threads ${feature} is not available in the agentos guest runtime`);
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
export const SHARE_ENV = Symbol.for("agentos.worker_threads.SHARE_ENV");
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

    build_delegating_builtin_module_wrapper(module_name)
}

fn build_delegating_builtin_module_wrapper(module_name: &str) -> String {
    let default_target = format!(
        "globalThis._requireFrom({}, \"/\")",
        serde_json::to_string(&format!("node:{module_name}"))
            .unwrap_or_else(|_| format!("\"node:{module_name}\""))
    );
    let mut exports = builtin_named_exports(module_name)
        .iter()
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
        "assert" | "assert/strict" => &[
            "AssertionError",
            "CallTracker",
            "deepEqual",
            "deepStrictEqual",
            "doesNotMatch",
            "doesNotReject",
            "doesNotThrow",
            "equal",
            "fail",
            "ifError",
            "match",
            "notDeepEqual",
            "notDeepStrictEqual",
            "notEqual",
            "notStrictEqual",
            "ok",
            "partialDeepStrictEqual",
            "rejects",
            "strict",
            "strictEqual",
            "throws",
        ],
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
            "isAscii",
            "isUtf8",
            "resolveObjectURL",
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
        "constants" => &[
            "COPYFILE_EXCL",
            "COPYFILE_FICLONE",
            "COPYFILE_FICLONE_FORCE",
            "F_OK",
            "R_OK",
            "W_OK",
            "X_OK",
            "O_RDONLY",
            "O_WRONLY",
            "O_RDWR",
            "O_CREAT",
            "O_EXCL",
            "O_TRUNC",
            "O_APPEND",
            "O_DIRECTORY",
            "O_NOFOLLOW",
            "O_SYNC",
            "O_DSYNC",
            "O_NONBLOCK",
            "S_IFMT",
            "S_IFREG",
            "S_IFDIR",
            "S_IFCHR",
            "S_IFBLK",
            "S_IFIFO",
            "S_IFLNK",
            "S_IFSOCK",
        ],
        "crypto" => &[
            "DiffieHellman",
            "ECDH",
            "KeyObject",
            "constants",
            "createCipheriv",
            "createDecipheriv",
            "createDiffieHellman",
            "createECDH",
            "createHash",
            "createHmac",
            "createPrivateKey",
            "createPublicKey",
            "createSecretKey",
            "createSign",
            "createVerify",
            "diffieHellman",
            "generateKeyPair",
            "generateKeyPairSync",
            "generateKeySync",
            "generatePrime",
            "generatePrimeSync",
            "getCiphers",
            "getCurves",
            "getDiffieHellman",
            "getFips",
            "getHashes",
            "getRandomValues",
            "pbkdf2",
            "pbkdf2Sync",
            "privateDecrypt",
            "privateEncrypt",
            "publicDecrypt",
            "publicEncrypt",
            "randomBytes",
            "randomFill",
            "randomFillSync",
            "randomUUID",
            "scrypt",
            "scryptSync",
            "sign",
            "subtle",
            "timingSafeEqual",
            "verify",
            "webcrypto",
        ],
        "diagnostics_channel" => &[
            "Channel",
            "channel",
            "hasSubscribers",
            "subscribe",
            "tracingChannel",
            "unsubscribe",
        ],
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
        "dns" => &[
            "ADDRCONFIG",
            "ALL",
            "Resolver",
            "V4MAPPED",
            "getServers",
            "lookup",
            "promises",
            "resolve",
            "resolve4",
            "resolve6",
            "setServers",
        ],
        "dns/promises" => &[
            "Resolver",
            "lookup",
            "resolve",
            "resolve4",
            "resolve6",
            "resolveAny",
            "resolveMx",
            "resolveTxt",
            "resolveSrv",
            "resolveCname",
            "resolvePtr",
            "resolveNs",
            "resolveSoa",
            "resolveNaptr",
            "resolveCaa",
        ],
        "fs" => &[
            "Dir",
            "Dirent",
            "ReadStream",
            "Stats",
            "WriteStream",
            "access",
            "accessSync",
            "appendFile",
            "appendFileSync",
            "chmod",
            "chmodSync",
            "chown",
            "chownSync",
            "close",
            "closeSync",
            "constants",
            "copyFile",
            "copyFileSync",
            "cp",
            "cpSync",
            "createReadStream",
            "createWriteStream",
            "exists",
            "existsSync",
            "lchmod",
            "lchmodSync",
            "lchown",
            "lchownSync",
            "link",
            "linkSync",
            "fstat",
            "fstatSync",
            "fsyncSync",
            "lstat",
            "lstatSync",
            "lutimes",
            "lutimesSync",
            "mkdir",
            "mkdirSync",
            "mkdtemp",
            "mkdtempSync",
            "open",
            "openSync",
            "opendir",
            "opendirSync",
            "read",
            "readFile",
            "promises",
            "readFileSync",
            "readdir",
            "readSync",
            "readdirSync",
            "readlink",
            "readlinkSync",
            "realpath",
            "realpathSync",
            "rename",
            "renameSync",
            "rmdir",
            "rmdirSync",
            "rm",
            "rmSync",
            "rmdir",
            "rmdirSync",
            "stat",
            "statSync",
            "statfs",
            "statfsSync",
            "symlink",
            "symlinkSync",
            "truncate",
            "truncateSync",
            "unlink",
            "unlinkSync",
            "utimes",
            "utimesSync",
            "watch",
            "watchFile",
            "unwatchFile",
            "write",
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
            "ClientRequest",
            "IncomingMessage",
            "METHODS",
            "Server",
            "ServerResponse",
            "STATUS_CODES",
            "_checkInvalidHeaderChar",
            "_checkIsHttpToken",
            "createServer",
            "get",
            "globalAgent",
            "maxHeaderSize",
            "request",
            "validateHeaderName",
            "validateHeaderValue",
        ],
        "http2" => &["connect", "createServer", "createSecureServer"],
        "https" => &[
            "Agent",
            "ClientRequest",
            "IncomingMessage",
            "Server",
            "ServerResponse",
            "_checkInvalidHeaderChar",
            "_checkIsHttpToken",
            "createServer",
            "get",
            "globalAgent",
            "maxHeaderSize",
            "request",
            "validateHeaderName",
            "validateHeaderValue",
        ],
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
            "toNamespacedPath",
            "win32",
        ],
        "process" => &[
            "abort",
            "allowedNodeEnvironmentFlags",
            "arch",
            "argv",
            "argv0",
            "availableMemory",
            "chdir",
            "config",
            "constrainedMemory",
            "cpuUsage",
            "cwd",
            "debugPort",
            "dlopen",
            "emitWarning",
            "env",
            "execArgv",
            "execPath",
            "execve",
            "exit",
            "exitCode",
            "features",
            "finalization",
            "getActiveResourcesInfo",
            "getBuiltinModule",
            "getegid",
            "geteuid",
            "getgid",
            "getgroups",
            "getuid",
            "hasUncaughtExceptionCaptureCallback",
            "hrtime",
            "initgroups",
            "kill",
            "loadEnvFile",
            "memoryUsage",
            "moduleLoadList",
            "nextTick",
            "openStdin",
            "pid",
            "platform",
            "ppid",
            "reallyExit",
            "ref",
            "release",
            "report",
            "resourceUsage",
            "setSourceMapsEnabled",
            "setUncaughtExceptionCaptureCallback",
            "setegid",
            "seteuid",
            "setgid",
            "setgroups",
            "setuid",
            "sourceMapsEnabled",
            "stderr",
            "stdin",
            "stdout",
            "threadCpuUsage",
            "title",
            "umask",
            "unref",
            "uptime",
            "version",
            "versions",
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
            "getDefaultHighWaterMark",
            "isDisturbed",
            "isErrored",
            "isReadable",
            "isWritable",
            "pipeline",
            "setDefaultHighWaterMark",
        ],
        "stream/consumers" => &["arrayBuffer", "blob", "buffer", "json", "text"],
        "sys" => &[
            "MIMEType",
            "MIMEParams",
            "TextDecoder",
            "TextEncoder",
            "aborted",
            "callbackify",
            "debug",
            "debuglog",
            "deprecate",
            "format",
            "formatWithOptions",
            "inherits",
            "inspect",
            "parseEnv",
            "parseArgs",
            "promisify",
            "styleText",
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
            "DEFAULT_MAX_VERSION",
            "DEFAULT_MIN_VERSION",
            "TLSSocket",
            "Server",
            "checkServerIdentity",
            "connect",
            "createSecureContext",
            "createServer",
            "getCiphers",
            "rootCertificates",
        ],
        "stream/promises" => &["finished", "pipeline"],
        "string_decoder" => &["StringDecoder"],
        "timers/promises" => &["scheduler", "setImmediate", "setInterval", "setTimeout"],
        "url" => &[
            "URL",
            "URLSearchParams",
            "Url",
            "domainToASCII",
            "domainToUnicode",
            "fileURLToPath",
            "format",
            "parse",
            "pathToFileURL",
            "resolve",
            "resolveObject",
            "urlToHttpOptions",
        ],
        "util" => &[
            "MIMEType",
            "MIMEParams",
            "TextDecoder",
            "TextEncoder",
            "aborted",
            "callbackify",
            "debug",
            "debuglog",
            "deprecate",
            "format",
            "formatWithOptions",
            "inherits",
            "inspect",
            "isDeepStrictEqual",
            "parseEnv",
            "parseArgs",
            "promisify",
            "styleText",
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
            "compileFunction",
            "createContext",
            "isContext",
            "measureMemory",
            "runInContext",
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
            "BrotliCompress",
            "BrotliDecompress",
            "Deflate",
            "DeflateRaw",
            "Gunzip",
            "Gzip",
            "Inflate",
            "InflateRaw",
            "Unzip",
            "brotliCompress",
            "brotliCompressSync",
            "brotliDecompress",
            "brotliDecompressSync",
            "constants",
            "createBrotliCompress",
            "createBrotliDecompress",
            "createDeflate",
            "createDeflateRaw",
            "createGunzip",
            "createGzip",
            "createInflate",
            "createInflateRaw",
            "createUnzip",
            "deflate",
            "deflateRaw",
            "deflateRawSync",
            "deflateSync",
            "gunzip",
            "gunzipSync",
            "gzip",
            "gzipSync",
            "inflate",
            "inflateRaw",
            "inflateRawSync",
            "inflateSync",
            "unzip",
            "unzipSync",
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
        request.split_once('/').or(Some((request, "")))
    }
}

fn node_modules_direct_candidate_dirs(dir: &str, package_name: &str) -> Vec<String> {
    let mut candidates = HashSet::new();
    candidates.insert(join_guest_path(
        dir,
        &format!("node_modules/{package_name}"),
    ));
    if dir == "/node_modules" || dir.ends_with("/node_modules") {
        candidates.insert(join_guest_path(dir, package_name));
    }
    let mut candidates = candidates.into_iter().collect::<Vec<_>>();
    candidates.sort();
    candidates
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
            let mut best_match = None;
            for (key, value) in record {
                if let Some((prefix, suffix)) = key.split_once('*') {
                    if subpath.starts_with(prefix) && subpath.ends_with(suffix) {
                        let wildcard = &subpath[prefix.len()..subpath.len() - suffix.len()];
                        let specificity = (prefix.len(), suffix.len());
                        if best_match
                            .as_ref()
                            .is_none_or(|(_, _, current)| specificity > *current)
                        {
                            best_match = Some((value, wildcard, specificity));
                        }
                    }
                }
            }
            if let Some((value, wildcard, _)) = best_match {
                let resolved = resolve_exports_target(value, ".", mode)?;
                return Some(resolved.replace('*', wildcard));
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
            let mut best_match = None;
            for (key, value) in record {
                if let Some((prefix, suffix)) = key.split_once('*') {
                    if specifier.starts_with(prefix) && specifier.ends_with(suffix) {
                        let wildcard = &specifier[prefix.len()..specifier.len() - suffix.len()];
                        let specificity = (prefix.len(), suffix.len());
                        if best_match
                            .as_ref()
                            .is_none_or(|(_, _, current)| specificity > *current)
                        {
                            best_match = Some((value, wildcard, specificity));
                        }
                    }
                }
            }
            best_match.and_then(|(value, wildcard, _)| {
                resolve_exports_target(value, ".", mode)
                    .map(|resolved| resolved.replace('*', wildcard))
            })
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
    use std::time::{SystemTime, UNIX_EPOCH};
    use tempfile::tempdir;

    struct MissingModuleReader;

    impl ModuleFsReader for MissingModuleReader {
        fn canonical_guest_path(
            &mut self,
            _guest_path: &str,
        ) -> Result<Option<String>, HostServiceError> {
            Ok(None)
        }

        fn read_to_string(
            &mut self,
            _guest_path: &str,
        ) -> Result<Option<String>, HostServiceError> {
            Ok(None)
        }

        fn path_is_dir(&mut self, _guest_path: &str) -> Result<Option<bool>, HostServiceError> {
            Ok(None)
        }

        fn path_exists(&mut self, _guest_path: &str) -> Result<bool, HostServiceError> {
            Ok(false)
        }
    }

    #[test]
    fn malformed_bridge_payload_is_a_typed_decode_error() {
        let error = decode_bridge_call_args("_resolveModule", 17, &[0xff])
            .expect_err("malformed CBOR must not become an empty argument list");
        assert_eq!(error.code, "ERR_AGENTOS_BRIDGE_REQUEST_DECODE");
        assert!(error.message.contains("call_id=17"));
    }

    #[test]
    fn malformed_batch_module_request_is_a_typed_validation_error() {
        let mut bridge = LocalBridgeState::default();
        let error = bridge
            .batch_resolve_modules(&[Value::Null])
            .expect_err("malformed batch request must not become an empty result");
        assert_eq!(error.code, "EINVAL");
    }

    #[test]
    fn malformed_internal_dispatch_payload_returns_a_typed_error() {
        let mut bridge = LocalBridgeState::default();
        let response = bridge
            .handle_polyfill_dispatch(&[Value::String(String::from("__bd:kernelTimerCreate:{"))]);
        assert!(response
            .as_str()
            .expect("dispatch error response")
            .contains("ERR_AGENTOS_BRIDGE_DISPATCH_DECODE"));
    }

    #[test]
    fn live_module_reader_falls_back_only_to_explicit_host_mappings() {
        let root = tempdir().expect("create explicit mapping root");
        let mapped_root = root.path().join("npm");
        fs::create_dir_all(mapped_root.join("lib/utils")).expect("create mapped module tree");
        fs::write(
            mapped_root.join("lib/utils/display.js"),
            "module.exports = 1;\n",
        )
        .expect("write mapped module");
        fs::write(root.path().join("hidden.js"), "module.exports = 2;\n")
            .expect("write implicit-cwd module");
        let env = BTreeMap::from([(
            String::from(NODE_GUEST_PATH_MAPPINGS_ENV),
            serde_json::to_string(&vec![json!({
                "guestPath": "/__agentos/node-runtime/npm",
                "hostPath": mapped_root,
            })])
            .expect("serialize explicit mapping"),
        )]);
        let mut bridge = LocalBridgeState::default();
        bridge.translator = GuestPathTranslator::from_host_context(
            &env,
            root.path().to_path_buf(),
            String::from("/workspace"),
        );
        bridge.module_reader = Some(Box::new(MissingModuleReader));

        assert_eq!(
            bridge
                .resolve_module(
                    "/__agentos/node-runtime/npm/lib/utils/display.js",
                    "/workspace/[eval]",
                    ModuleResolveMode::Require,
                )
                .expect("resolve explicit host mapping"),
            Some(String::from(
                "/__agentos/node-runtime/npm/lib/utils/display.js"
            ))
        );
        assert_eq!(
            bridge
                .resolve_module(
                    "/workspace/hidden.js",
                    "/workspace/[eval]",
                    ModuleResolveMode::Require,
                )
                .expect("implicit host cwd remains unavailable"),
            None
        );
    }

    #[test]
    fn partial_host_reader_defers_agentos_package_resolution_to_kernel_vfs() {
        let mut bridge = LocalBridgeState::default();
        bridge.module_reader = Some(Box::new(MissingModuleReader));
        bridge.module_reader_forwards_to_kernel = true;
        let parent =
            "/opt/agentos/pkgs/codex/0.0.1/node_modules/@agentos-software/codex/dist/adapter.js";

        assert!(
            bridge
                .handle_internal_bridge_call(
                    1,
                    "_resolveModule",
                    &[
                        Value::String(String::from("@agentclientprotocol/sdk")),
                        Value::String(parent.to_owned()),
                        Value::String(String::from("import")),
                    ],
                )
                .is_none(),
            "a /root/node_modules host mapping must not shadow package-local dependencies"
        );
        assert!(
            bridge
                .handle_internal_bridge_call(
                    2,
                    "_resolveModule",
                    &[
                        Value::String(String::from("@agentclientprotocol/sdk")),
                        Value::String(format!("file://{parent}")),
                        Value::String(String::from("import")),
                    ],
                )
                .is_none(),
            "file URL referrers inside projected packages must also use the kernel VFS"
        );
        assert!(
            bridge
                .handle_internal_bridge_call(
                    3,
                    "_batchResolveModules",
                    &[Value::Array(vec![Value::Array(vec![
                        Value::String(String::from("@agentclientprotocol/sdk")),
                        Value::String(parent.to_owned()),
                    ])])],
                )
                .is_none(),
            "batched package resolution must preserve the same kernel ownership"
        );
    }

    #[test]
    fn vfs_owned_module_format_falls_through_to_the_authoritative_reader() {
        let mut bridge = LocalBridgeState::default();
        bridge.module_reader = Some(Box::new(MissingModuleReader));

        assert!(
            bridge
                .handle_internal_bridge_call(
                    1,
                    "_moduleFormat",
                    &[Value::String(String::from(
                        "/opt/agentos/pkgs/demo/0.0.1/node_modules/demo/index.js",
                    ))],
                )
                .is_none(),
            "a local default-CommonJS guess must not hide live VFS package metadata"
        );
    }

    #[test]
    fn dispose_context_reclaims_one_shot_metadata_without_reusing_ids() {
        let mut engine = JavascriptExecutionEngine::default();
        let baseline = engine.context_count_for_test();
        let first = engine.create_context(CreateJavascriptContextRequest {
            vm_id: String::from("vm-context-dispose"),
            bootstrap_module: None,
            compile_cache_root: None,
        });
        assert_eq!(engine.context_count_for_test(), baseline + 1);
        assert!(engine.dispose_context(&first.context_id));
        assert_eq!(engine.context_count_for_test(), baseline);
        assert!(!engine.dispose_context(&first.context_id));

        let second = engine.create_context(CreateJavascriptContextRequest {
            vm_id: String::from("vm-context-dispose"),
            bootstrap_module: None,
            compile_cache_root: None,
        });
        assert_ne!(first.context_id, second.context_id);
    }

    #[test]
    fn javascript_limits_are_read_from_typed_fields_and_env_is_inert() {
        // Misleading env values: a reader that still consulted `AGENTOS_*` would
        // observe these instead of the typed wire limits.
        let env = std::collections::BTreeMap::from([
            (
                String::from("AGENTOS_V8_HEAP_LIMIT_MB"),
                String::from("999999"),
            ),
            (
                String::from("AGENTOS_V8_CPU_TIME_LIMIT_MS"),
                String::from("999999"),
            ),
            (
                String::from("AGENTOS_V8_WALL_CLOCK_LIMIT_MS"),
                String::from("999999"),
            ),
            (
                String::from("AGENTOS_NODE_IMPORT_CACHE_MATERIALIZE_TIMEOUT_MS"),
                String::from("999999"),
            ),
            (
                String::from(NODE_SYNC_RPC_WAIT_TIMEOUT_MS_ENV),
                String::from("999999"),
            ),
        ]);
        let request = StartJavascriptExecutionRequest {
            argv0: None,
            guest_runtime: Default::default(),
            vm_id: String::from("vm-js"),
            context_id: String::from("ctx-js"),
            argv: vec![String::from("/entry.mjs")],
            env,
            cwd: std::path::PathBuf::from("/tmp"),
            limits: JavascriptExecutionLimits {
                v8_heap_limit_mb: Some(64),
                sync_rpc_wait_timeout_ms: Some(2_000),
                cpu_time_limit_ms: Some(750),
                wall_clock_limit_ms: Some(500),
                import_cache_materialize_timeout_ms: Some(125),
                max_timers: Some(321),
                reactor_work_quantum: Some(64),
                bridge_call_timeout_ms: Some(15_000),
            },
            wasm_module_bytes: None,
            inline_code: None,
        };

        assert_eq!(
            javascript_heap_limit_mb(&request),
            64,
            "heap must come from the typed wire limit, not AGENTOS_V8_HEAP_LIMIT_MB"
        );
        assert_eq!(
            javascript_sync_rpc_timeout(&request),
            std::time::Duration::from_millis(2_000),
            "sync-rpc wait must come from the typed wire limit, not env"
        );
        assert_eq!(
            javascript_cpu_time_limit_ms(&request),
            750,
            "CPU budget must come from the typed wire limit, not env"
        );
        assert_eq!(
            javascript_wall_clock_limit_ms(&request),
            500,
            "wall-clock budget must come from the typed wire limit, not env"
        );
        assert_eq!(
            javascript_import_cache_materialize_timeout(&request),
            std::time::Duration::from_millis(125),
            "import-cache timeout must come from the typed wire limit, not env"
        );
        assert_eq!(javascript_max_timers(&request), 321);
        assert_eq!(
            javascript_reactor_work_quantum(
                &request,
                &default_test_runtime_context().expect("test runtime")
            )
            .expect("typed reactor work quantum"),
            64
        );
    }

    #[test]
    fn vm_scoped_reactor_work_quantum_is_required_and_nonzero() {
        let process = default_test_runtime_context().expect("test runtime context");
        let resources = Arc::new(agentos_runtime::accounting::ResourceLedger::child(
            "javascript-reactor-work-quantum-test",
            std::iter::empty::<(
                agentos_runtime::accounting::ResourceClass,
                agentos_runtime::accounting::ResourceLimit,
            )>(),
            Arc::clone(process.resources()),
        ));
        let runtime = process.scoped_for_vm(resources, 9_001);
        let mut request = StartJavascriptExecutionRequest {
            guest_runtime: Default::default(),
            vm_id: String::from("vm-js"),
            context_id: String::from("ctx-js"),
            argv0: None,
            argv: vec![String::from("/entry.mjs")],
            env: BTreeMap::new(),
            cwd: PathBuf::from("/tmp"),
            limits: JavascriptExecutionLimits::default(),
            wasm_module_bytes: None,
            inline_code: None,
        };

        let missing = javascript_reactor_work_quantum(&request, &runtime)
            .expect_err("VM execution must carry its work quantum");
        assert!(missing
            .to_string()
            .contains("limits.reactor.workQuantum is required"));

        request.limits.reactor_work_quantum = Some(0);
        let zero = javascript_reactor_work_quantum(&request, &runtime)
            .expect_err("zero VM work quantum must fail closed");
        assert!(zero
            .to_string()
            .contains("limits.reactor.workQuantum must be greater than zero"));
    }

    #[test]
    fn javascript_limits_fall_back_to_defaults_when_unset() {
        let request = StartJavascriptExecutionRequest {
            argv0: None,
            guest_runtime: Default::default(),
            vm_id: String::from("vm-js"),
            context_id: String::from("ctx-js"),
            argv: vec![String::from("/entry.mjs")],
            env: std::collections::BTreeMap::new(),
            cwd: std::path::PathBuf::from("/tmp"),
            limits: JavascriptExecutionLimits::default(),
            wasm_module_bytes: None,
            inline_code: None,
        };

        assert_eq!(
            javascript_heap_limit_mb(&request),
            0,
            "0 selects the engine default heap"
        );
        assert_eq!(
            javascript_sync_rpc_timeout(&request),
            std::time::Duration::from_millis(NODE_SYNC_RPC_DEFAULT_WAIT_TIMEOUT_MS),
        );
        assert_eq!(
            javascript_cpu_time_limit_ms(&request),
            DEFAULT_V8_CPU_TIME_LIMIT_MS
        );
        assert_eq!(
            javascript_wall_clock_limit_ms(&request),
            DEFAULT_V8_WALL_CLOCK_LIMIT_MS
        );
        assert_eq!(javascript_max_timers(&request), MAX_TIMERS_PER_EXECUTION);
        assert_eq!(
            javascript_import_cache_materialize_timeout(&request),
            std::time::Duration::from_millis(DEFAULT_NODE_IMPORT_CACHE_MATERIALIZE_TIMEOUT_MS)
        );
    }

    #[test]
    fn inline_code_module_detection_prefers_commonjs_when_import_only_appears_in_comment() {
        let source = "// import { x } from 'y';\nmodule.exports = { foo: 1 };";
        assert!(!inline_code_uses_module_mode(source));
    }

    #[test]
    fn inline_code_module_detection_ignores_import_inside_string_literal() {
        let source = "const msg = \"run: import x from 'y'\";\nmodule.exports.msg = msg;";
        assert!(!inline_code_uses_module_mode(source));
    }

    #[test]
    fn inline_code_module_detection_accepts_multiline_import_statements() {
        let source = "import\n  { default as foo }\nfrom 'bar';\nconsole.log(foo);";
        assert!(inline_code_uses_module_mode(source));
    }

    #[test]
    fn inline_code_module_detection_accepts_real_esm_source() {
        let source = "import { foo } from 'bar';\nexport const baz = 1;\nconsole.log(foo, baz);";
        assert!(inline_code_uses_module_mode(source));
    }

    #[test]
    fn inline_code_module_detection_is_deterministic_for_empty_comment_only_and_template_cases() {
        assert!(!inline_code_uses_module_mode(""));
        assert!(!inline_code_uses_module_mode(
            "// import x from 'y';\n/* export const z = 1; */"
        ));
        assert!(!inline_code_uses_module_mode(
            "const msg = `export const nope = 1;`;"
        ));
    }

    #[test]
    fn javascript_sync_rpc_timeout_writes_clear_error_response() {
        let (reader_fd, writer_fd) = pipe2(OFlag::O_CLOEXEC).expect("create pipe");
        let reader = File::from(reader_fd);
        let writer = File::from(writer_fd);
        let response_writer =
            JavascriptSyncRpcResponseWriter::new(writer, Duration::from_millis(50));
        let pending = Arc::new(Mutex::new(PendingSyncRpcRegistry::new(2)));
        set_pending_sync_rpc_state(&pending, 7).expect("register timeout test call");

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
            Value::String(String::from("ERR_AGENTOS_NODE_SYNC_RPC_TIMEOUT"))
        );
        assert!(response["error"]["message"]
            .as_str()
            .expect("timeout message")
            .contains("timed out after 20ms"));
        assert_eq!(
            pending.lock().expect("pending state lock").states.get(&7),
            Some(&PendingSyncRpcState::TimedOut(7))
        );
    }

    #[test]
    fn pending_sync_rpc_registry_is_bounded_keyed_and_nonlossy() {
        let pending = Arc::new(Mutex::new(PendingSyncRpcRegistry::new(2)));
        set_pending_sync_rpc_state(&pending, 7).expect("first concurrent waiter");
        set_pending_sync_rpc_state(&pending, 8).expect("second concurrent waiter");

        let duplicate = set_pending_sync_rpc_state(&pending, 7)
            .expect_err("duplicate call ID must not replace its waiter");
        assert!(matches!(
            duplicate,
            JavascriptExecutionError::PendingSyncRpcRequest(7)
        ));
        assert_eq!(host_reply_adapter_error(duplicate).code, "EBUSY");

        let over_limit = set_pending_sync_rpc_state(&pending, 9)
            .expect_err("third distinct waiter exceeds configured admission");
        assert!(matches!(
            over_limit,
            JavascriptExecutionError::PendingSyncRpcLimit {
                limit: 2,
                observed: 3,
            }
        ));
        let host_error = host_reply_adapter_error(over_limit);
        assert_eq!(host_error.code, "ERR_AGENTOS_RESOURCE_LIMIT");
        assert_eq!(
            host_error.details.as_ref().unwrap()["limitName"],
            "limits.reactor.maxBridgeCalls"
        );

        let guard = pending.lock().expect("pending state lock");
        assert_eq!(guard.states.len(), 2, "rejection preserves both waiters");
        assert_eq!(guard.states.get(&7), Some(&PendingSyncRpcState::Pending(7)));
        assert_eq!(guard.states.get(&8), Some(&PendingSyncRpcState::Pending(8)));
        drop(guard);

        assert_eq!(
            clear_pending_sync_rpc(&pending, 8).expect("settle exact waiter"),
            PendingSyncRpcResolution::Pending
        );
        let guard = pending.lock().expect("pending state lock");
        assert_eq!(guard.states.len(), 1);
        assert!(guard.states.contains_key(&7));
    }

    #[test]
    fn direct_host_reply_ignores_only_proven_stale_bridge_completions() {
        map_host_reply_adapter_response(Err(JavascriptExecutionError::BridgeSettlement(
            agentos_v8_runtime::host_call::BridgeSettlementError::stale_completion(String::from(
                "ERR_AGENTOS_BRIDGE_STALE_COMPLETION: response for canceled host-visible bridge call_id 7 in session v8-exec-1 generation Some(3)",
            )),
        )))
        .expect("exact retired-route completion is benign");

        for fatal in [
            "ERR_AGENTOS_BRIDGE_UNKNOWN_CALL_ID: response for unknown bridge call_id 7",
            "ERR_AGENTOS_BRIDGE_STALE_GENERATION: response call_id 7 named the wrong generation",
            "prefix ERR_AGENTOS_BRIDGE_STALE_COMPLETION: guest-controlled text is not proof",
        ] {
            let error = map_host_reply_adapter_response(Err(
                JavascriptExecutionError::RpcResponse(String::from(fatal)),
            ))
            .expect_err("unproven bridge response failure must stay fatal");
            assert_eq!(error.code, "ERR_AGENTOS_ADAPTER_REPLY");
        }
    }

    #[test]
    fn javascript_host_error_payload_preserves_fields_without_parsing_message() {
        let payload = encode_host_service_error_payload(
            &HostServiceError::new("EIO", "EACCES: guest-controlled diagnostic").with_details(
                serde_json::json!({
                    "path": "/guest/file",
                    "retryable": false,
                }),
            ),
        )
        .expect("encode structured host error");
        let ciborium::Value::Map(entries) =
            ciborium::from_reader::<ciborium::Value, _>(payload.as_slice())
                .expect("decode structured host error")
        else {
            panic!("structured host error must be a CBOR map");
        };
        let field = |name: &str| {
            entries.iter().find_map(|(key, value)| match key {
                ciborium::Value::Text(key) if key == name => Some(value),
                _ => None,
            })
        };
        assert_eq!(
            field("code"),
            Some(&ciborium::Value::Text(String::from("EIO")))
        );
        assert_eq!(
            field("message"),
            Some(&ciborium::Value::Text(String::from(
                "EACCES: guest-controlled diagnostic"
            )))
        );
        assert!(matches!(field("details"), Some(ciborium::Value::Map(_))));
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

    #[test]
    fn javascript_wait_capture_rejects_output_over_limit() {
        let mut stdout = vec![b'x'; JAVASCRIPT_CAPTURED_OUTPUT_LIMIT_BYTES - 1];
        append_captured_output(&mut stdout, vec![b'y'], "stdout").expect("fill to limit");
        assert_eq!(stdout.len(), JAVASCRIPT_CAPTURED_OUTPUT_LIMIT_BYTES);

        let error = append_captured_output(&mut stdout, vec![b'z'], "stdout")
            .expect_err("captured output over limit should fail");
        assert!(matches!(
            error,
            JavascriptExecutionError::OutputBufferExceeded {
                stream: "stdout",
                limit: JAVASCRIPT_CAPTURED_OUTPUT_LIMIT_BYTES,
            }
        ));
    }

    #[test]
    fn kernel_stdin_bridge_rejects_buffer_over_limit_and_closed_writes() {
        let bridge = LocalKernelStdinBridge::default();
        bridge
            .write(&vec![b'x'; KERNEL_STDIN_BUFFER_LIMIT_BYTES])
            .expect("fill stdin buffer to limit");

        let error = bridge
            .write(b"y")
            .expect_err("stdin buffer over limit should fail");
        assert!(matches!(error, JavascriptExecutionError::Stdin(_)));

        let bridge = LocalKernelStdinBridge::default();
        bridge.close();
        let error = bridge
            .write(b"x")
            .expect_err("write after stdin close should fail");
        assert!(matches!(error, JavascriptExecutionError::StdinClosed));
    }

    #[test]
    fn kernel_stdin_bridge_null_timeout_waits_for_readiness_without_polling() {
        let bridge = Arc::new(LocalKernelStdinBridge::default());
        let reader = Arc::clone(&bridge);
        let (sender, receiver) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            sender
                .send(reader.read(&[json!(64), Value::Null]))
                .expect("publish stdin result");
        });

        assert!(receiver.recv_timeout(Duration::from_millis(20)).is_err());
        bridge.write(b"ready").expect("make stdin readable");
        let value = receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("readiness should wake the parked read");
        assert_eq!(
            value["dataBase64"],
            Value::String(v8_runtime::base64_encode_pub(b"ready"))
        );
        thread.join().expect("stdin reader exits");
    }

    #[test]
    fn javascript_event_sender_reports_closed_receiver() {
        let (sender, receiver) = flume::bounded(1);
        drop(receiver);
        let gauge = register_queue(TrackedLimit::JavascriptEventChannel, 1);
        assert!(!send_javascript_event(
            &sender,
            &gauge,
            None,
            JavascriptExecutionEvent::Exited(1)
        ));
    }

    // Regression: a full event channel must apply backpressure, not destroy the
    // session. The old code called `v8_session.destroy()` on the first `Full`,
    // truncating the stream and tearing the session down.
    #[test]
    fn javascript_event_sender_backpressures_instead_of_destroying_when_full() {
        let gauge = register_queue(TrackedLimit::JavascriptEventChannel, 1);
        let (sender, event_receiver) = flume::bounded(1);

        // Drain slowly on another thread so the producer is forced onto the
        // blocking-backpressure path the old destroy-on-full code never reached.
        let drainer = std::thread::spawn(move || {
            let mut drained = 0usize;
            while event_receiver.recv().is_ok() {
                drained += 1;
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            drained
        });

        // Far more events than the 1-slot channel holds; every send must succeed.
        const SENDS: usize = 16;
        for _ in 0..SENDS {
            assert!(send_javascript_event(
                &sender,
                &gauge,
                None,
                JavascriptExecutionEvent::Stdout(Vec::new())
            ));
        }
        drop(sender);
        let drained = drainer.join().expect("drainer thread panicked");
        assert_eq!(drained, SENDS, "every event must survive backpressure");
    }

    #[test]
    fn javascript_event_sender_chunks_oversized_output_without_data_loss() {
        let (sender, event_receiver) = flume::bounded(JAVASCRIPT_EVENT_CHANNEL_CAPACITY);
        let gauge = register_queue(
            TrackedLimit::JavascriptEventChannel,
            JAVASCRIPT_EVENT_CHANNEL_CAPACITY,
        );
        let payload = vec![b'x'; JAVASCRIPT_EVENT_PAYLOAD_LIMIT_BYTES + 17];

        assert!(send_javascript_event(
            &sender,
            &gauge,
            None,
            JavascriptExecutionEvent::Stdout(payload.clone())
        ));

        let first = event_receiver.recv().expect("first chunk");
        let second = event_receiver.recv().expect("second chunk");
        let joined = [first, second]
            .into_iter()
            .flat_map(|event| match event {
                JavascriptExecutionEvent::Stdout(chunk) => chunk,
                other => panic!("unexpected event: {other:?}"),
            })
            .collect::<Vec<_>>();
        assert_eq!(joined, payload);
    }

    #[test]
    fn internal_bridge_host_context_resolves_relative_module_path() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "agentos-module-bridge-{}-{unique}",
            std::process::id()
        ));
        let bin_dir = root.join("node_modules/next/dist/bin");
        let cli_dir = root.join("node_modules/next/dist/cli");
        fs::create_dir_all(&bin_dir).expect("create bin dir");
        fs::create_dir_all(&cli_dir).expect("create cli dir");
        fs::write(
            root.join("node_modules/next/package.json"),
            r#"{"name":"next"}"#,
        )
        .expect("write package.json");
        fs::write(bin_dir.join("next"), "#!/usr/bin/env node\n").expect("write next bin");
        fs::write(cli_dir.join("next-build.js"), "module.exports = 1;\n")
            .expect("write next-build.js");

        let env = BTreeMap::new();
        let result = handle_internal_bridge_call_from_host_context(
            &root,
            "/",
            &env,
            "_resolveModule",
            &[
                Value::String(String::from("../cli/next-build.js")),
                Value::String(String::from("/node_modules/next/dist/bin/next")),
                Value::String(String::from("import")),
            ],
        );

        assert_eq!(
            result,
            Ok(Some(Value::String(String::from(
                "/node_modules/next/dist/cli/next-build.js"
            ))))
        );

        fs::remove_dir_all(&root).expect("remove temp module tree");
    }

    #[test]
    fn register_v8_session_deregisters_on_create_session_failure() {
        let runtime = default_test_runtime_context().expect("test runtime context");
        let host = V8RuntimeHost::spawn(&runtime).expect("spawn V8 runtime host");
        let session_id = format!(
            "v8-register-failure-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time")
                .as_nanos()
        );

        let error = match register_v8_session(
            &host,
            &runtime,
            session_id.clone(),
            0,
            0,
            0,
            None,
            |_command| {
                Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "simulated CreateSession send failure",
                ))
            },
        ) {
            Ok(_) => panic!("register_v8_session should surface create-session send failures"),
            Err(error) => error,
        };

        match error {
            JavascriptExecutionError::Spawn(inner) => {
                assert_eq!(inner.kind(), std::io::ErrorKind::BrokenPipe);
            }
            other => panic!("unexpected error: {other:?}"),
        }
        let receiver = host
            .register_session(&session_id, &runtime)
            .expect("failed registration should not leak the session output receiver");
        drop(receiver);
        host.unregister_session(&session_id);
    }

    #[test]
    fn javascript_cpu_time_limit_defaults_to_bounded_value() {
        let request = StartJavascriptExecutionRequest {
            limits: Default::default(),
            argv0: None,
            guest_runtime: Default::default(),
            vm_id: String::from("vm-js-default-cpu"),
            context_id: String::from("ctx-js-default-cpu"),
            argv: vec![String::from("./entry.mjs")],
            env: BTreeMap::new(),
            cwd: std::path::PathBuf::from("/tmp"),
            wasm_module_bytes: None,
            inline_code: None,
        };

        assert_eq!(
            javascript_cpu_time_limit_ms(&request),
            30_000,
            "unset JavaScript CPU budget must be bounded by default"
        );
    }

    #[test]
    fn javascript_execution_drop_keeps_normal_v8_session_cleanup() {
        let temp = tempdir().expect("create temp dir");
        let mut engine = JavascriptExecutionEngine::default();
        let context = engine.create_context(CreateJavascriptContextRequest {
            vm_id: String::from("vm-drop-cleanup"),
            bootstrap_module: None,
            compile_cache_root: None,
        });

        let execution = engine
            .start_execution(StartJavascriptExecutionRequest {
                limits: Default::default(),
                argv0: None,
                guest_runtime: Default::default(),
                vm_id: String::from("vm-drop-cleanup"),
                context_id: context.context_id,
                argv: vec![String::from("./entry.mjs")],
                env: BTreeMap::new(),
                cwd: temp.path().to_path_buf(),
                wasm_module_bytes: None,
                inline_code: Some(String::from("globalThis.__agentOSDropCleanup = true;")),
            })
            .expect("start JavaScript execution");
        let session_id = execution.v8_session.session_id().to_owned();
        let runtime = engine.runtime_context().expect("engine runtime").clone();
        let host = engine.v8_host.as_ref().expect("shared V8 runtime host");

        drop(execution);

        let receiver = host
            .register_session(&session_id, &runtime)
            .expect("execution drop should still destroy and deregister the session");
        drop(receiver);
        host.unregister_session(&session_id);
    }

    #[test]
    fn prepared_execution_does_not_enqueue_guest_code_until_started() {
        let temp = tempdir().expect("create temp dir");
        let mut engine = JavascriptExecutionEngine::default();
        let context = engine.create_context(CreateJavascriptContextRequest {
            vm_id: String::from("vm-deferred-exec"),
            bootstrap_module: None,
            compile_cache_root: None,
        });

        let mut execution = engine
            .prepare_execution(StartJavascriptExecutionRequest {
                limits: Default::default(),
                argv0: None,
                guest_runtime: Default::default(),
                vm_id: String::from("vm-deferred-exec"),
                context_id: context.context_id,
                argv: vec![String::from("./entry.mjs")],
                env: BTreeMap::new(),
                cwd: temp.path().to_path_buf(),
                wasm_module_bytes: None,
                inline_code: Some(String::from("process.stdout.write('started\\n');")),
            })
            .expect("prepare JavaScript execution");

        assert!(execution.is_prepared_for_start());
        assert_eq!(
            execution
                .poll_event_blocking(Duration::ZERO)
                .expect("poll prepared execution"),
            None,
            "preparation must not enqueue any guest code"
        );

        execution
            .start_prepared()
            .expect("start prepared execution");
        assert!(!execution.is_prepared_for_start());
        let result = execution.wait().expect("wait for prepared execution");
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout, b"started\n");
    }

    // --- Timer cancellation / cap regression tests (U4: H2 bridge timers, M3
    // kernel timers). These assert the *safeguards firing* (delay clamped, timer
    // entry reclaimed, callback suppressed) and never spawn unbounded threads. ---

    #[test]
    fn timer_delay_is_clamped_to_the_cap() {
        // A guest can pass an arbitrarily large delay (up to u64::MAX ms); without
        // a cap the timer wheel could retain a session Arc behind a deadline that
        // is effectively forever away. The cap bounds that lifetime.
        assert_eq!(
            timer_delay_ms(Some(&json!(u64::MAX))),
            MAX_TIMER_DELAY_MS,
            "a u64::MAX delay must be clamped to MAX_TIMER_DELAY_MS"
        );
        assert_eq!(
            timer_delay_ms(Some(&json!(1.0e308_f64))),
            MAX_TIMER_DELAY_MS,
            "an enormous float delay must be clamped to the cap"
        );
        assert_eq!(
            timer_delay_ms(Some(&json!(MAX_TIMER_DELAY_MS + 1))),
            MAX_TIMER_DELAY_MS,
            "a delay one past the cap must clamp down to the cap"
        );
        // Below-cap values pass through unchanged so normal timers are unaffected.
        assert_eq!(timer_delay_ms(Some(&json!(250))), 250);
        assert_eq!(timer_delay_ms(Some(&json!(0))), 0);
    }

    #[test]
    fn cleared_timer_is_suppressed_and_entry_reclaimed() {
        // Mirrors what a woken bridge/kernel timer action does after waiting: it
        // consults the shared map via `timer_should_fire`. When the entry has been
        // removed (clear or session teardown), the callback must be suppressed.
        let timers: Arc<Mutex<HashMap<u64, LocalTimerEntry>>> =
            Arc::new(Mutex::new(HashMap::new()));
        timers.lock().unwrap().insert(
            7,
            LocalTimerEntry {
                delay_ms: 1_000,
                generation: 0,
                repeat: false,
                _reservation: None,
            },
        );

        // Simulate `kernelTimerClear` / teardown removing the entry before the
        // action wakes.
        timers.lock().unwrap().remove(&7);

        assert!(
            !timer_should_fire(&timers, 7, 0),
            "a cleared timer must not fire"
        );
        assert!(
            timers.lock().unwrap().is_empty(),
            "tracking map stays empty after a cleared timer is evaluated"
        );
    }

    #[test]
    fn rearmed_timer_generation_mismatch_suppresses_stale_action() {
        // The bridge/kernel timer action captures the generation at schedule time.
        // If the timer is re-armed (generation bumped) before the stale action
        // wakes, the stale action must observe the mismatch and suppress, while the
        // entry survives for the live generation.
        let timers: Arc<Mutex<HashMap<u64, LocalTimerEntry>>> =
            Arc::new(Mutex::new(HashMap::new()));
        timers.lock().unwrap().insert(
            3,
            LocalTimerEntry {
                delay_ms: 10,
                generation: 1,
                repeat: false,
                _reservation: None,
            },
        );

        // Stale action captured generation 0; current entry is at generation 1.
        assert!(
            !timer_should_fire(&timers, 3, 0),
            "a stale generation must be suppressed"
        );
        assert!(
            timers.lock().unwrap().contains_key(&3),
            "the live entry must survive a stale-generation evaluation"
        );

        // The matching (current) generation fires and reclaims the one-shot entry.
        assert!(
            timer_should_fire(&timers, 3, 1),
            "the current generation must fire"
        );
        assert!(
            timers.lock().unwrap().is_empty(),
            "a fired one-shot timer must reclaim its id from the map"
        );
    }

    #[test]
    fn timer_registration_reserves_before_insert_and_releases_on_remove() {
        use agentos_runtime::accounting::{ResourceClass, ResourceLedger, ResourceLimit};

        let ledger = Arc::new(ResourceLedger::root(
            "vm=test",
            [(
                ResourceClass::Timers,
                ResourceLimit::new(1, "limits.jsRuntime.maxTimers"),
            )],
        ));
        let mut state = LocalBridgeState::default();
        state.timer_resources = Some(Arc::clone(&ledger));
        state.max_timers = 2;

        let first = state.register_timer(10, false).expect("first timer");
        assert_eq!(ledger.usage(ResourceClass::Timers).used, 1);
        let error = state
            .register_timer(10, false)
            .expect_err("second timer must hit the ledger bound");
        assert!(
            error.message.contains("limits.jsRuntime.maxTimers"),
            "{error:?}"
        );
        assert_eq!(state.timers.lock().unwrap().len(), 1);

        state.clear_kernel_timer(first);
        assert_eq!(ledger.usage(ResourceClass::Timers).used, 0);
        state
            .register_timer(10, false)
            .expect("released admission is reusable");
    }

    #[test]
    fn bridge_timer_registration_is_tracked_and_drop_clears_timers() {
        // H2: the bridge-timer path must register its timer (so it is cancellable)
        // before queuing a wheel action, and session teardown
        // (dropping LocalBridgeState) must wipe the tracking map so in-flight timer
        // actions are cancelled.
        let mut state = LocalBridgeState::default();
        // Observe the same map the queued actions would consult.
        let timers = state.timers.clone();

        let id_a = state
            .register_oneshot_timer(MAX_TIMER_DELAY_MS)
            .expect("register first timer");
        let id_b = state
            .register_oneshot_timer(500)
            .expect("register second timer");
        assert_ne!(id_a, id_b, "each bridge timer gets a fresh id");
        assert_eq!(
            timers.lock().unwrap().len(),
            2,
            "registered bridge timers are tracked in the shared map"
        );
        // A registered timer would fire for its captured generation (proving the
        // entry is real and consultable) ...
        assert!(timer_should_fire(&timers, id_a, 0));
        // ... and seeding a still-pending one before teardown:
        let id_c = state
            .register_oneshot_timer(1_000)
            .expect("register third timer");

        // Session teardown: dropping the bridge state must clear every timer so any
        // queued action wakes to a missing entry and suppresses its callback.
        drop(state);

        assert!(
            timers.lock().unwrap().is_empty(),
            "dropping LocalBridgeState must clear the timers map on teardown"
        );
        assert!(
            !timer_should_fire(&timers, id_c, 0),
            "a pending bridge timer is suppressed after teardown"
        );
    }
}
