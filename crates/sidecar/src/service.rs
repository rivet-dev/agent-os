use crate::acp::compat::{
    is_cancel_method_not_found, maybe_normalize_permission_response,
    normalize_inbound_permission_request, summarize_inbound_notification,
    summarize_inbound_request, summarize_inbound_response, to_record, ACP_CANCEL_METHOD,
    LEGACY_PERMISSION_METHOD,
};
use crate::acp::session::{AcpSessionState, AcpTerminalState};
use crate::acp::{
    deserialize_message, serialize_message, AcpTimeoutDiagnostics, JsonRpcError, JsonRpcId,
    JsonRpcMessage, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse,
};
use crate::bridge::{build_mount_plugin_registry, MountPluginContext};
pub(crate) use crate::execution::{
    build_javascript_socket_path_context, error_code, ignore_stale_javascript_sync_rpc_response,
    javascript_sync_rpc_arg_str, javascript_sync_rpc_arg_u32, javascript_sync_rpc_arg_u32_optional,
    javascript_sync_rpc_arg_u64, javascript_sync_rpc_arg_u64_optional,
    javascript_sync_rpc_bytes_arg, javascript_sync_rpc_bytes_value, javascript_sync_rpc_encoding,
    javascript_sync_rpc_error_code, javascript_sync_rpc_option_bool,
    javascript_sync_rpc_option_u32, parse_signal, runtime_child_is_alive,
    sanitize_javascript_child_process_internal_bootstrap_env, service_javascript_sync_rpc,
    signal_runtime_process, vm_network_resource_counts, write_kernel_process_stdin,
};
use crate::filesystem::guest_filesystem_call as filesystem_guest_filesystem_call;
use crate::protocol::{
    AgentSessionClosedResponse, AuthenticatedResponse, CloseAgentSessionRequest,
    CreateSessionRequest, DisposeReason, EventFrame, EventPayload, ExecuteRequest,
    FsPermissionScope, GetSessionStateRequest, GuestFilesystemCallRequest,
    JavascriptChildProcessSpawnOptions, JavascriptChildProcessSpawnRequest, OpenSessionRequest,
    OwnershipScope, PatternPermissionRule, PatternPermissionScope, PermissionMode,
    PermissionsPolicy, ProtocolSchema, RejectedResponse, RequestFrame, RequestId, RequestPayload,
    ResponseFrame, ResponsePayload, SessionOpenedResponse, SessionRequest as AgentSessionRequest,
    SessionRpcResponse, SidecarPermissionRequest, SidecarRequestFrame, SidecarRequestPayload,
    SidecarResponseFrame, SidecarResponsePayload, SidecarResponseTracker,
    SidecarResponseTrackerError, SignalDispositionAction, SignalHandlerRegistration,
    StructuredEvent, VmLifecycleEvent, VmLifecycleState,
};
use crate::state::{
    ActiveExecution, ActiveExecutionEvent, BridgeError, ConnectionState, JavascriptSocketFamily,
    JavascriptSocketPathContext, ProcessEventEnvelope, SessionState, SharedBridge,
    SharedSidecarRequestClient, SidecarRequestTransport, VmState, EXECUTION_DRIVER_NAME,
};
use crate::tools::register_toolkit;
use crate::NativeSidecarBridge;
use agent_os_bridge::{
    CommandPermissionRequest, EnvironmentAccess, EnvironmentPermissionRequest, FilesystemAccess,
    FilesystemPermissionRequest, LifecycleEventRecord, LifecycleState, LogLevel, LogRecord,
    NetworkAccess, NetworkPermissionRequest, StructuredEventRecord,
};
use agent_os_execution::{
    JavascriptExecutionEngine, JavascriptExecutionError, JavascriptSyncRpcRequest,
    PythonExecutionEngine, PythonExecutionError, WasmExecutionEngine, WasmExecutionError,
};
use agent_os_kernel::kernel::KernelError;
use agent_os_kernel::mount_plugin::{FileSystemPluginRegistry, PluginError};
use agent_os_kernel::permissions::{
    CommandAccessRequest, EnvAccessRequest, EnvironmentOperation, NetworkAccessRequest,
    NetworkOperation, PermissionDecision,
};
use agent_os_kernel::process_table::SIGKILL;
// root_fs types moved to crate::vm
use agent_os_kernel::vfs::VfsError;
use nix::sys::wait::{waitid as wait_on_child, Id as WaitId, WaitPidFlag, WaitStatus};
use nix::unistd::Pid;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Wake, Waker};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::time;

// Constants and type aliases moved to crate::state

// NativeSidecarConfig, DispatchResult, SidecarError moved to crate::state
pub use crate::state::{DispatchResult, NativeSidecarConfig, SidecarError};

// SharedBridge struct and Clone impl moved to crate::state

#[derive(Debug, Default, Deserialize)]
struct LegacyJavascriptChildProcessSpawnOptions {
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    input: Option<Value>,
    #[serde(default)]
    shell: bool,
    #[serde(default)]
    detached: bool,
    #[serde(default, rename = "maxBuffer")]
    max_buffer: Option<usize>,
}

#[derive(Debug)]
enum AcpRequestError {
    Sidecar(SidecarError),
    Timeout(AcpTimeoutDiagnostics),
}

impl AcpRequestError {
    fn into_sidecar_error(self) -> SidecarError {
        match self {
            Self::Sidecar(error) => error,
            Self::Timeout(diagnostics) => SidecarError::InvalidState(diagnostics.message()),
        }
    }
}

pub(crate) fn parse_javascript_child_process_spawn_request(
    vm: &VmState,
    args: &[Value],
) -> Result<(JavascriptChildProcessSpawnRequest, Option<usize>), SidecarError> {
    if let Some(value) = args.first().cloned() {
        if let Ok(request) = serde_json::from_value::<JavascriptChildProcessSpawnRequest>(value) {
            return Ok((request, None));
        }
    }

    let command = javascript_sync_rpc_arg_str(args, 0, "child_process.spawn command")?.to_owned();
    let raw_args = javascript_sync_rpc_arg_str(args, 1, "child_process.spawn args")?;
    let raw_options = javascript_sync_rpc_arg_str(args, 2, "child_process.spawn options")?;

    let parsed_args = serde_json::from_str::<Vec<String>>(raw_args).map_err(|error| {
        SidecarError::InvalidState(format!("invalid child_process.spawn args payload: {error}"))
    })?;
    let parsed_options = serde_json::from_str::<LegacyJavascriptChildProcessSpawnOptions>(
        raw_options,
    )
    .map_err(|error| {
        SidecarError::InvalidState(format!(
            "invalid child_process.spawn options payload: {error}"
        ))
    })?;

    Ok((
        JavascriptChildProcessSpawnRequest {
            command,
            args: parsed_args,
            options: JavascriptChildProcessSpawnOptions {
                cwd: parsed_options.cwd,
                env: parsed_options.env,
                internal_bootstrap_env: sanitize_javascript_child_process_internal_bootstrap_env(
                    &vm.guest_env,
                ),
                input: parsed_options.input,
                shell: parsed_options.shell,
                detached: parsed_options.detached,
            },
        },
        parsed_options.max_buffer,
    ))
}

impl<B> SharedBridge<B> {
    fn new(bridge: B) -> Self {
        Self {
            inner: Arc::new(Mutex::new(bridge)),
            permissions: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }
}

impl<B> SharedBridge<B>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    pub(crate) fn with_mut<T>(
        &self,
        operation: impl FnOnce(&mut B) -> Result<T, BridgeError<B>>,
    ) -> Result<T, SidecarError> {
        let mut bridge = self.inner.lock().map_err(|_| {
            SidecarError::Bridge(String::from("native sidecar bridge lock poisoned"))
        })?;
        operation(&mut bridge).map_err(|error| SidecarError::Bridge(format!("{error:?}")))
    }

    fn inspect<T>(&self, operation: impl FnOnce(&mut B) -> T) -> Result<T, SidecarError> {
        let mut bridge = self.inner.lock().map_err(|_| {
            SidecarError::Bridge(String::from("native sidecar bridge lock poisoned"))
        })?;
        Ok(operation(&mut bridge))
    }

    pub(crate) fn emit_lifecycle(
        &self,
        vm_id: &str,
        state: LifecycleState,
    ) -> Result<(), SidecarError> {
        self.with_mut(|bridge| {
            bridge.emit_lifecycle(LifecycleEventRecord {
                vm_id: vm_id.to_owned(),
                state,
                detail: None,
            })
        })
    }

    pub(crate) fn emit_log(
        &self,
        vm_id: &str,
        message: impl Into<String>,
    ) -> Result<(), SidecarError> {
        self.with_mut(|bridge| {
            bridge.emit_log(LogRecord {
                vm_id: vm_id.to_owned(),
                level: LogLevel::Info,
                message: message.into(),
            })
        })
    }

    pub(crate) fn filesystem_decision(
        &self,
        vm_id: &str,
        path: &str,
        access: FilesystemAccess,
    ) -> PermissionDecision {
        if let Some(decision) = self.static_permission_decision(
            vm_id,
            filesystem_permission_capability(access),
            "fs",
            Some(path),
        ) {
            return decision;
        }
        match self.with_mut(|bridge| {
            bridge.check_filesystem_access(FilesystemPermissionRequest {
                vm_id: vm_id.to_owned(),
                path: path.to_owned(),
                access,
            })
        }) {
            Ok(decision) => map_bridge_permission(decision),
            Err(error) => PermissionDecision::deny(error.to_string()),
        }
    }

    pub(crate) fn command_decision(
        &self,
        vm_id: &str,
        request: &CommandAccessRequest,
    ) -> PermissionDecision {
        if let Some(decision) = self.static_permission_decision(
            vm_id,
            "child_process.spawn",
            "child_process",
            Some(&request.command),
        ) {
            return decision;
        }
        match self.with_mut(|bridge| {
            bridge.check_command_execution(CommandPermissionRequest {
                vm_id: vm_id.to_owned(),
                command: request.command.clone(),
                args: request.args.clone(),
                cwd: request.cwd.clone(),
                env: request.env.clone(),
            })
        }) {
            Ok(decision) => map_bridge_permission(decision),
            Err(error) => PermissionDecision::deny(error.to_string()),
        }
    }

    pub(crate) fn environment_decision(
        &self,
        vm_id: &str,
        request: &EnvAccessRequest,
    ) -> PermissionDecision {
        if let Some(decision) = self.static_permission_decision(
            vm_id,
            environment_permission_capability(request.op),
            "env",
            Some(&request.key),
        ) {
            return decision;
        }
        match self.with_mut(|bridge| {
            bridge.check_environment_access(EnvironmentPermissionRequest {
                vm_id: vm_id.to_owned(),
                access: match request.op {
                    EnvironmentOperation::Read => EnvironmentAccess::Read,
                    EnvironmentOperation::Write => EnvironmentAccess::Write,
                },
                key: request.key.clone(),
                value: request.value.clone(),
            })
        }) {
            Ok(decision) => map_bridge_permission(decision),
            Err(error) => PermissionDecision::deny(error.to_string()),
        }
    }

    pub(crate) fn network_decision(
        &self,
        vm_id: &str,
        request: &NetworkAccessRequest,
    ) -> PermissionDecision {
        if let Some(decision) = self.static_permission_decision(
            vm_id,
            network_permission_capability(request.op),
            "network",
            Some(&request.resource),
        ) {
            return decision;
        }
        match self.with_mut(|bridge| {
            bridge.check_network_access(NetworkPermissionRequest {
                vm_id: vm_id.to_owned(),
                access: match request.op {
                    NetworkOperation::Fetch => NetworkAccess::Fetch,
                    NetworkOperation::Http => NetworkAccess::Http,
                    NetworkOperation::Dns => NetworkAccess::Dns,
                    NetworkOperation::Listen => NetworkAccess::Listen,
                },
                resource: request.resource.clone(),
            })
        }) {
            Ok(decision) => map_bridge_permission(decision),
            Err(error) => PermissionDecision::deny(error.to_string()),
        }
    }

    pub(crate) fn require_network_access(
        &self,
        vm_id: &str,
        op: NetworkOperation,
        resource: impl Into<String>,
    ) -> Result<(), SidecarError> {
        let resource = resource.into();
        let decision = self.network_decision(
            vm_id,
            &NetworkAccessRequest {
                vm_id: vm_id.to_owned(),
                op,
                resource: resource.clone(),
            },
        );
        if decision.allow {
            return Ok(());
        }

        let message = match decision.reason.as_deref() {
            Some(reason) => format!("EACCES: permission denied, {resource}: {reason}"),
            None => format!("EACCES: permission denied, {resource}"),
        };
        Err(SidecarError::Execution(message))
    }

    pub(crate) fn set_vm_permissions(
        &self,
        vm_id: &str,
        permissions: &PermissionsPolicy,
    ) -> Result<(), SidecarError> {
        let mut stored = self.permissions.lock().map_err(|_| {
            SidecarError::Bridge(String::from(
                "native sidecar permission policy lock poisoned",
            ))
        })?;
        stored.insert(vm_id.to_owned(), permissions.clone());
        Ok(())
    }

    pub(crate) fn clear_vm_permissions(&self, vm_id: &str) -> Result<(), SidecarError> {
        let mut stored = self.permissions.lock().map_err(|_| {
            SidecarError::Bridge(String::from(
                "native sidecar permission policy lock poisoned",
            ))
        })?;
        stored.remove(vm_id);
        Ok(())
    }

    pub(crate) fn static_permission_decision(
        &self,
        vm_id: &str,
        capability: &str,
        domain: &str,
        resource: Option<&str>,
    ) -> Option<PermissionDecision> {
        let stored = self.permissions.lock().ok()?;
        let permissions = stored.get(vm_id)?;
        let mode = evaluate_permissions_policy(permissions, domain, capability, resource);
        Some(permission_mode_to_kernel_decision(mode, capability))
    }
}

fn evaluate_permissions_policy(
    permissions: &PermissionsPolicy,
    domain: &str,
    capability: &str,
    resource: Option<&str>,
) -> PermissionMode {
    match domain {
        "fs" => evaluate_fs_permission_scope(
            permissions.fs.as_ref(),
            capability_operation(capability, domain),
            resource,
        ),
        "network" => evaluate_pattern_permission_scope(
            permissions.network.as_ref(),
            capability_operation(capability, domain),
            resource,
        ),
        "child_process" => evaluate_pattern_permission_scope(
            permissions.child_process.as_ref(),
            capability_operation(capability, domain),
            resource,
        ),
        "env" => evaluate_pattern_permission_scope(
            permissions.env.as_ref(),
            capability_operation(capability, domain),
            resource,
        ),
        _ => PermissionMode::Deny,
    }
}

fn evaluate_fs_permission_scope(
    scope: Option<&FsPermissionScope>,
    operation: &str,
    resource: Option<&str>,
) -> PermissionMode {
    match scope {
        Some(FsPermissionScope::Mode(mode)) => mode.clone(),
        Some(FsPermissionScope::Rules(rules)) => {
            let mut mode = rules.default.clone().unwrap_or(PermissionMode::Deny);
            for rule in &rules.rules {
                if fs_rule_matches(rule, operation, resource) {
                    mode = rule.mode.clone();
                }
            }
            mode
        }
        None => PermissionMode::Deny,
    }
}

fn evaluate_pattern_permission_scope(
    scope: Option<&PatternPermissionScope>,
    operation: &str,
    resource: Option<&str>,
) -> PermissionMode {
    match scope {
        Some(PatternPermissionScope::Mode(mode)) => mode.clone(),
        Some(PatternPermissionScope::Rules(rules)) => {
            let mut mode = rules.default.clone().unwrap_or(PermissionMode::Deny);
            for rule in &rules.rules {
                if pattern_rule_matches(rule, operation, resource) {
                    mode = rule.mode.clone();
                }
            }
            mode
        }
        None => PermissionMode::Deny,
    }
}

fn fs_rule_matches(
    rule: &crate::protocol::FsPermissionRule,
    operation: &str,
    resource: Option<&str>,
) -> bool {
    let operations_match = rule.operations.is_empty()
        || rule
            .operations
            .iter()
            .any(|candidate| candidate == operation);
    let paths_match = rule.paths.is_empty()
        || resource
            .is_some_and(|path| rule.paths.iter().any(|pattern| glob_matches(pattern, path)));
    operations_match && paths_match
}

fn pattern_rule_matches(
    rule: &PatternPermissionRule,
    operation: &str,
    resource: Option<&str>,
) -> bool {
    let operations_match = rule.operations.is_empty()
        || rule
            .operations
            .iter()
            .any(|candidate| candidate == operation);
    let patterns_match = rule.patterns.is_empty()
        || resource.is_some_and(|value| {
            rule.patterns
                .iter()
                .any(|pattern| glob_matches(pattern, value))
        });
    operations_match && patterns_match
}

fn capability_operation<'a>(capability: &'a str, domain: &str) -> &'a str {
    capability
        .strip_prefix(domain)
        .and_then(|value| value.strip_prefix('.'))
        .unwrap_or("")
}

fn glob_matches(pattern: &str, value: &str) -> bool {
    let pattern = pattern.as_bytes();
    let value = value.as_bytes();
    let mut pattern_index = 0usize;
    let mut value_index = 0usize;
    let mut star_pattern_index = None;
    let mut star_value_index = 0usize;

    while value_index < value.len() {
        if pattern_index < pattern.len()
            && (pattern[pattern_index] == b'?' || pattern[pattern_index] == value[value_index])
        {
            pattern_index += 1;
            value_index += 1;
            continue;
        }

        if pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
            while pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
                pattern_index += 1;
            }
            if pattern_index == pattern.len() {
                return true;
            }
            star_pattern_index = Some(pattern_index);
            star_value_index = value_index;
            continue;
        }

        let Some(saved_pattern_index) = star_pattern_index else {
            return false;
        };
        star_value_index += 1;
        value_index = star_value_index;
        pattern_index = saved_pattern_index;
    }

    while pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
        pattern_index += 1;
    }

    pattern_index == pattern.len()
}

fn permission_mode_to_kernel_decision(
    mode: PermissionMode,
    capability: &str,
) -> PermissionDecision {
    match mode {
        PermissionMode::Allow => PermissionDecision::allow(),
        PermissionMode::Ask => {
            PermissionDecision::deny(format!("permission prompt required for {capability}"))
        }
        PermissionMode::Deny => PermissionDecision::deny(format!("blocked by {capability} policy")),
    }
}

pub(crate) fn filesystem_permission_capability(access: FilesystemAccess) -> &'static str {
    match access {
        FilesystemAccess::Read => "fs.read",
        FilesystemAccess::Write => "fs.write",
        FilesystemAccess::Stat => "fs.stat",
        FilesystemAccess::ReadDir => "fs.readdir",
        FilesystemAccess::CreateDir => "fs.create_dir",
        FilesystemAccess::Remove => "fs.rm",
        FilesystemAccess::Rename => "fs.rename",
        FilesystemAccess::Symlink => "fs.symlink",
        FilesystemAccess::ReadLink => "fs.readlink",
        FilesystemAccess::Chmod => "fs.chmod",
        FilesystemAccess::Truncate => "fs.truncate",
    }
}

fn network_permission_capability(operation: NetworkOperation) -> &'static str {
    match operation {
        NetworkOperation::Fetch => "network.fetch",
        NetworkOperation::Http => "network.http",
        NetworkOperation::Dns => "network.dns",
        NetworkOperation::Listen => "network.listen",
    }
}

fn environment_permission_capability(operation: EnvironmentOperation) -> &'static str {
    match operation {
        EnvironmentOperation::Read => "env.read",
        EnvironmentOperation::Write => "env.write",
    }
}

fn ownership_matches_process_event(
    ownership: &OwnershipScope,
    event: &ProcessEventEnvelope,
) -> bool {
    match ownership {
        OwnershipScope::Connection { connection_id } => connection_id == &event.connection_id,
        OwnershipScope::Session {
            connection_id,
            session_id,
        } => connection_id == &event.connection_id && session_id == &event.session_id,
        OwnershipScope::Vm {
            connection_id,
            session_id,
            vm_id,
        } => {
            connection_id == &event.connection_id
                && session_id == &event.session_id
                && vm_id == &event.vm_id
        }
    }
}

fn poll_future_once<F: std::future::Future>(future: std::pin::Pin<&mut F>) -> Option<F::Output> {
    let waker = noop_waker();
    let mut context = Context::from_waker(&waker);
    match future.poll(&mut context) {
        Poll::Ready(output) => Some(output),
        Poll::Pending => None,
    }
}

fn noop_waker() -> Waker {
    Waker::from(Arc::new(NoopWake))
}

struct NoopWake;

impl Wake for NoopWake {
    fn wake(self: Arc<Self>) {}
}

// ConnectionState, SessionState, VmConfiguration, VmState moved to crate::state

// JavascriptSocketPathContext, JavascriptSocketFamily, VmListenPolicy moved to crate::state

impl JavascriptSocketPathContext {
    pub(crate) fn loopback_port_allowed(&self, port: u16) -> bool {
        self.loopback_exempt_ports.contains(&port)
            || self
                .tcp_loopback_guest_to_host_ports
                .keys()
                .any(|(_, guest_port)| *guest_port == port)
    }

    pub(crate) fn translate_tcp_loopback_port(
        &self,
        family: JavascriptSocketFamily,
        port: u16,
    ) -> Option<u16> {
        self.tcp_loopback_guest_to_host_ports
            .get(&(family, port))
            .copied()
    }

    pub(crate) fn translate_udp_loopback_port(
        &self,
        family: JavascriptSocketFamily,
        port: u16,
    ) -> Option<u16> {
        self.udp_loopback_guest_to_host_ports
            .get(&(family, port))
            .copied()
    }

    pub(crate) fn guest_udp_port_for_host_port(
        &self,
        family: JavascriptSocketFamily,
        port: u16,
    ) -> Option<u16> {
        self.udp_loopback_host_to_guest_ports
            .get(&(family, port))
            .copied()
    }
}

// ActiveProcess, NetworkResourceCounts moved to crate::state

pub struct NativeSidecar<B> {
    pub(crate) config: NativeSidecarConfig,
    pub(crate) bridge: SharedBridge<B>,
    pub(crate) mount_plugins: FileSystemPluginRegistry<MountPluginContext<B>>,
    pub(crate) cache_root: PathBuf,
    pub(crate) javascript_engine: JavascriptExecutionEngine,
    pub(crate) python_engine: PythonExecutionEngine,
    pub(crate) wasm_engine: WasmExecutionEngine,
    pub(crate) next_connection_id: usize,
    pub(crate) next_session_id: usize,
    pub(crate) next_vm_id: usize,
    pub(crate) next_agent_process_id: usize,
    pub(crate) next_sidecar_request_id: RequestId,
    pub(crate) connections: BTreeMap<String, ConnectionState>,
    pub(crate) sessions: BTreeMap<String, SessionState>,
    pub(crate) vms: BTreeMap<String, VmState>,
    pub(crate) acp_sessions: BTreeMap<String, AcpSessionState>,
    pub(crate) acp_process_stdout_buffers: BTreeMap<String, String>,
    pub(crate) process_event_sender: UnboundedSender<ProcessEventEnvelope>,
    pub(crate) process_event_receiver: Option<UnboundedReceiver<ProcessEventEnvelope>>,
    pub(crate) pending_process_events: VecDeque<ProcessEventEnvelope>,
    pub(crate) pending_sidecar_responses: SidecarResponseTracker,
    pub(crate) outbound_sidecar_requests: VecDeque<SidecarRequestFrame>,
    pub(crate) completed_sidecar_responses: BTreeMap<RequestId, SidecarResponseFrame>,
    pub(crate) sidecar_requests: SharedSidecarRequestClient,
}

impl<B> fmt::Debug for NativeSidecar<B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NativeSidecar")
            .field("config", &self.config)
            .field("cache_root", &self.cache_root)
            .field("next_connection_id", &self.next_connection_id)
            .field("next_session_id", &self.next_session_id)
            .field("next_vm_id", &self.next_vm_id)
            .field("next_agent_process_id", &self.next_agent_process_id)
            .field("connection_count", &self.connections.len())
            .field("session_count", &self.sessions.len())
            .field("vm_count", &self.vms.len())
            .field("acp_session_count", &self.acp_sessions.len())
            .field(
                "acp_process_stdout_buffer_count",
                &self.acp_process_stdout_buffers.len(),
            )
            .finish()
    }
}

impl<B> NativeSidecar<B>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    const ACP_REQUEST_TIMEOUT_MS: u64 = 120_000;

    pub fn new(bridge: B) -> Result<Self, SidecarError> {
        Self::with_config(bridge, NativeSidecarConfig::default())
    }

    pub fn with_config(bridge: B, config: NativeSidecarConfig) -> Result<Self, SidecarError> {
        if matches!(config.expected_auth_token.as_deref(), Some("")) {
            return Err(SidecarError::InvalidState(String::from(
                "native sidecar expected_auth_token must not be empty",
            )));
        }

        let cache_root = config.compile_cache_root.clone().unwrap_or_else(|| {
            std::env::temp_dir().join(format!(
                "{}-{}",
                config.sidecar_id,
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("system time before unix epoch")
                    .as_nanos()
            ))
        });
        fs::create_dir_all(&cache_root).map_err(|error| {
            SidecarError::Io(format!("failed to prepare sidecar cache root: {error}"))
        })?;

        let bridge = SharedBridge::new(bridge);
        let mount_plugins = build_mount_plugin_registry::<B>()?;
        let (process_event_sender, process_event_receiver) = unbounded_channel();

        Ok(Self {
            config,
            bridge,
            mount_plugins,
            cache_root,
            javascript_engine: JavascriptExecutionEngine::default(),
            python_engine: PythonExecutionEngine::default(),
            wasm_engine: WasmExecutionEngine::default(),
            next_connection_id: 0,
            next_session_id: 0,
            next_vm_id: 0,
            next_agent_process_id: 0,
            next_sidecar_request_id: -1,
            connections: BTreeMap::new(),
            sessions: BTreeMap::new(),
            vms: BTreeMap::new(),
            acp_sessions: BTreeMap::new(),
            acp_process_stdout_buffers: BTreeMap::new(),
            process_event_sender,
            process_event_receiver: Some(process_event_receiver),
            pending_process_events: VecDeque::new(),
            pending_sidecar_responses: SidecarResponseTracker::default(),
            outbound_sidecar_requests: VecDeque::new(),
            completed_sidecar_responses: BTreeMap::new(),
            sidecar_requests: SharedSidecarRequestClient::default(),
        })
    }

    pub fn sidecar_id(&self) -> &str {
        &self.config.sidecar_id
    }

    pub fn with_bridge_mut<T>(
        &self,
        operation: impl FnOnce(&mut B) -> T,
    ) -> Result<T, SidecarError> {
        self.bridge.inspect(operation)
    }

    pub fn set_sidecar_request_transport(&mut self, transport: Arc<dyn SidecarRequestTransport>) {
        self.sidecar_requests.set_transport(transport);
    }

    pub fn set_sidecar_request_handler<F>(&mut self, handler: F)
    where
        F: Fn(SidecarRequestFrame) -> Result<SidecarResponsePayload, SidecarError>
            + Send
            + Sync
            + 'static,
    {
        struct HandlerTransport<F>(F);

        impl<F> SidecarRequestTransport for HandlerTransport<F>
        where
            F: Fn(SidecarRequestFrame) -> Result<SidecarResponsePayload, SidecarError>
                + Send
                + Sync
                + 'static,
        {
            fn send_request(
                &self,
                request: SidecarRequestFrame,
                _timeout: Duration,
            ) -> Result<SidecarResponseFrame, SidecarError> {
                let payload = (self.0)(request.clone())?;
                Ok(SidecarResponseFrame::new(
                    request.request_id,
                    request.ownership,
                    payload,
                ))
            }
        }

        self.set_sidecar_request_transport(Arc::new(HandlerTransport(handler)));
    }

    pub fn dispatch_blocking(
        &mut self,
        request: RequestFrame,
    ) -> Result<DispatchResult, SidecarError> {
        if matches!(request.payload, RequestPayload::DisposeVm(_)) {
            return tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("sidecar dispatch runtime")
                .block_on(self.dispatch(request));
        }

        let mut future = std::pin::pin!(self.dispatch(request));
        match poll_future_once(future.as_mut()) {
            Some(result) => result,
            None => tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("sidecar dispatch runtime")
                .block_on(future),
        }
    }

    pub fn poll_event_blocking(
        &mut self,
        ownership: &OwnershipScope,
        timeout: Duration,
    ) -> Result<Option<EventFrame>, SidecarError> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("sidecar poll runtime")
            .block_on(self.poll_event(ownership, timeout))
    }

    pub fn close_session_blocking(
        &mut self,
        connection_id: &str,
        session_id: &str,
    ) -> Result<Vec<EventFrame>, SidecarError> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("sidecar close-session runtime")
            .block_on(self.close_session(connection_id, session_id))
    }

    pub fn remove_connection_blocking(
        &mut self,
        connection_id: &str,
    ) -> Result<Vec<EventFrame>, SidecarError> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("sidecar remove-connection runtime")
            .block_on(self.remove_connection(connection_id))
    }

    pub fn dispose_vm_internal_blocking(
        &mut self,
        connection_id: &str,
        session_id: &str,
        vm_id: &str,
        reason: DisposeReason,
    ) -> Result<Vec<EventFrame>, SidecarError> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("sidecar dispose-vm runtime")
            .block_on(self.dispose_vm_internal(connection_id, session_id, vm_id, reason))
    }

    pub async fn dispatch(
        &mut self,
        request: RequestFrame,
    ) -> Result<DispatchResult, SidecarError> {
        if let Err(error) = self.ensure_request_within_frame_limit(&request) {
            return Ok(DispatchResult {
                response: self.reject(&request, error_code(&error), &error.to_string()),
                events: Vec::new(),
            });
        }

        let result = match request.payload.clone() {
            RequestPayload::Authenticate(payload) => {
                self.authenticate_connection(&request, payload).await
            }
            RequestPayload::OpenSession(payload) => self.open_session(&request, payload).await,
            RequestPayload::CreateVm(payload) => self.create_vm(&request, payload).await,
            RequestPayload::CreateSession(payload) => self.create_session(&request, payload).await,
            RequestPayload::SessionRequest(payload) => {
                self.session_request(&request, payload).await
            }
            RequestPayload::GetSessionState(payload) => {
                self.get_session_state(&request, payload).await
            }
            RequestPayload::CloseAgentSession(payload) => {
                self.close_agent_session(&request, payload).await
            }
            RequestPayload::DisposeVm(payload) => self.dispose_vm(&request, payload).await,
            RequestPayload::BootstrapRootFilesystem(payload) => {
                self.bootstrap_root_filesystem(&request, payload.entries)
                    .await
            }
            RequestPayload::ConfigureVm(payload) => self.configure_vm(&request, payload).await,
            RequestPayload::RegisterToolkit(payload) => register_toolkit(self, &request, payload),
            RequestPayload::CreateLayer(payload) => self.create_layer(&request, payload).await,
            RequestPayload::SealLayer(payload) => self.seal_layer(&request, payload).await,
            RequestPayload::ImportSnapshot(payload) => {
                self.import_snapshot(&request, payload).await
            }
            RequestPayload::ExportSnapshot(payload) => {
                self.export_snapshot(&request, payload).await
            }
            RequestPayload::CreateOverlay(payload) => self.create_overlay(&request, payload).await,
            RequestPayload::GuestFilesystemCall(payload) => {
                self.guest_filesystem_call(&request, payload).await
            }
            RequestPayload::SnapshotRootFilesystem(payload) => {
                self.snapshot_root_filesystem(&request, payload).await
            }
            RequestPayload::Execute(payload) => self.execute(&request, payload).await,
            RequestPayload::WriteStdin(payload) => self.write_stdin(&request, payload).await,
            RequestPayload::CloseStdin(payload) => self.close_stdin(&request, payload).await,
            RequestPayload::KillProcess(payload) => self.kill_process(&request, payload).await,
            RequestPayload::GetProcessSnapshot(payload) => {
                self.get_process_snapshot(&request, payload).await
            }
            RequestPayload::FindListener(payload) => self.find_listener(&request, payload).await,
            RequestPayload::FindBoundUdp(payload) => self.find_bound_udp(&request, payload).await,
            RequestPayload::GetSignalState(payload) => {
                self.get_signal_state(&request, payload).await
            }
            RequestPayload::GetZombieTimerCount(payload) => {
                self.get_zombie_timer_count(&request, payload).await
            }
            RequestPayload::HostFilesystemCall(_)
            | RequestPayload::PermissionRequest(_)
            | RequestPayload::PersistenceLoad(_)
            | RequestPayload::PersistenceFlush(_) => Ok(DispatchResult {
                response: self.reject(
                    &request,
                    "unsupported_direction",
                    "host callback request categories are sidecar-to-host only in this scaffold",
                ),
                events: Vec::new(),
            }),
        };

        match result {
            Ok(dispatch) => Ok(dispatch),
            Err(error @ SidecarError::Io(_)) => Err(error),
            Err(error) => Ok(DispatchResult {
                response: self.reject(&request, error_code(&error), &error.to_string()),
                events: Vec::new(),
            }),
        }
    }

    pub async fn poll_event(
        &mut self,
        ownership: &OwnershipScope,
        timeout: Duration,
    ) -> Result<Option<EventFrame>, SidecarError> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(index) = self
                .pending_process_events
                .iter()
                .position(|event| ownership_matches_process_event(ownership, event))
            {
                let envelope = self
                    .pending_process_events
                    .remove(index)
                    .expect("pending process event index should exist");
                if let Some(frame) = self.handle_process_event_envelope(envelope)? {
                    return Ok(Some(frame));
                }
                continue;
            }

            if !timeout.is_zero() {
                let _ = self.pump_process_events(ownership).await?;
            }

            let matching_envelope = {
                let receiver = self.process_event_receiver.as_mut().ok_or_else(|| {
                    SidecarError::InvalidState(String::from("process event receiver unavailable"))
                })?;
                let mut matching_envelope = None;
                while let Ok(envelope) = receiver.try_recv() {
                    if ownership_matches_process_event(ownership, &envelope) {
                        matching_envelope = Some(envelope);
                        break;
                    }
                    self.pending_process_events.push_back(envelope);
                }
                matching_envelope
            };

            if let Some(envelope) = matching_envelope {
                if let Some(frame) = self.handle_process_event_envelope(envelope)? {
                    return Ok(Some(frame));
                }
                continue;
            }

            if Instant::now() >= deadline {
                return Ok(None);
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            time::sleep(remaining.min(Duration::from_millis(10))).await;
        }
    }

    pub(crate) fn handle_process_event_envelope(
        &mut self,
        envelope: ProcessEventEnvelope,
    ) -> Result<Option<EventFrame>, SidecarError> {
        let ProcessEventEnvelope {
            connection_id,
            session_id,
            vm_id,
            process_id,
            event,
        } = envelope;

        if let Some((acp_session_id, terminal_id)) =
            self.acp_terminal_owner_for_process(&vm_id, &process_id)
        {
            self.handle_acp_terminal_execution_event(&vm_id, &acp_session_id, &terminal_id, event)?;
            return Ok(None);
        }

        if matches!(event, ActiveExecutionEvent::Exited(_)) {
            let mut trailing = Vec::new();
            let mut deferred = VecDeque::new();
            while let Some(pending) = self.pending_process_events.pop_front() {
                if pending.vm_id == vm_id
                    && pending.process_id == process_id
                    && !matches!(pending.event, ActiveExecutionEvent::Exited(_))
                {
                    trailing.push(pending.event);
                } else {
                    deferred.push_back(pending);
                }
            }
            self.pending_process_events = deferred;
            trailing.extend(
                self.drain_process_events_blocking(&vm_id, &process_id)?
                    .into_iter()
                    .filter(|event| !matches!(event, ActiveExecutionEvent::Exited(_))),
            );

            if !trailing.is_empty() {
                self.pending_process_events
                    .push_front(ProcessEventEnvelope {
                        connection_id: connection_id.clone(),
                        session_id: session_id.clone(),
                        vm_id: vm_id.clone(),
                        process_id: process_id.clone(),
                        event,
                    });
                for event in trailing.into_iter().rev() {
                    self.pending_process_events
                        .push_front(ProcessEventEnvelope {
                            connection_id: connection_id.clone(),
                            session_id: session_id.clone(),
                            vm_id: vm_id.clone(),
                            process_id: process_id.clone(),
                            event,
                        });
                }
                return Ok(None);
            }
        }

        self.handle_execution_event(&vm_id, &process_id, event)
    }

    // try_poll_event moved to crate::execution

    pub async fn close_session(
        &mut self,
        connection_id: &str,
        session_id: &str,
    ) -> Result<Vec<EventFrame>, SidecarError> {
        self.dispose_session(connection_id, session_id, DisposeReason::Requested)
            .await
    }

    pub async fn remove_connection(
        &mut self,
        connection_id: &str,
    ) -> Result<Vec<EventFrame>, SidecarError> {
        self.require_authenticated_connection(connection_id)?;

        let session_ids = self
            .connections
            .get(connection_id)
            .expect("authenticated connection should exist")
            .sessions
            .iter()
            .cloned()
            .collect::<Vec<_>>();

        let mut events = Vec::new();
        for session_id in session_ids {
            events.extend(
                self.dispose_session(connection_id, &session_id, DisposeReason::ConnectionClosed)
                    .await?,
            );
        }

        self.connections.remove(connection_id);
        Ok(events)
    }

    async fn authenticate_connection(
        &mut self,
        request: &RequestFrame,
        payload: crate::protocol::AuthenticateRequest,
    ) -> Result<DispatchResult, SidecarError> {
        let _ = self.connection_id_for(&request.ownership)?;
        if let Err(error) = self.validate_auth_token(&payload.auth_token) {
            let mut fields = audit_fields([
                (String::from("source"), payload.client_name.clone()),
                (String::from("reason"), error.to_string()),
            ]);
            if let OwnershipScope::Connection { connection_id } = &request.ownership {
                fields.insert(String::from("connection_id"), connection_id.clone());
            }
            emit_security_audit_event(
                &self.bridge,
                &self.config.sidecar_id,
                "security.auth.failed",
                fields,
            );
            return Err(error);
        }

        let connection_id = self.allocate_connection_id();
        self.connections.insert(
            connection_id.clone(),
            ConnectionState {
                auth_token: payload.auth_token,
                sessions: BTreeSet::new(),
            },
        );

        let response = self.response_with_ownership(
            request.request_id,
            OwnershipScope::connection(&connection_id),
            ResponsePayload::Authenticated(AuthenticatedResponse {
                sidecar_id: self.config.sidecar_id.clone(),
                connection_id,
                max_frame_bytes: self.config.max_frame_bytes as u32,
            }),
        );
        Ok(DispatchResult {
            response,
            events: Vec::new(),
        })
    }

    async fn open_session(
        &mut self,
        request: &RequestFrame,
        payload: OpenSessionRequest,
    ) -> Result<DispatchResult, SidecarError> {
        let connection_id = self.connection_id_for(&request.ownership)?;
        self.require_authenticated_connection(&connection_id)?;

        self.next_session_id += 1;
        let session_id = format!("session-{}", self.next_session_id);
        self.sessions.insert(
            session_id.clone(),
            SessionState {
                connection_id: connection_id.clone(),
                placement: payload.placement,
                metadata: payload.metadata,
                vm_ids: BTreeSet::new(),
            },
        );
        self.connections
            .get_mut(&connection_id)
            .expect("authenticated connection should exist")
            .sessions
            .insert(session_id.clone());

        Ok(DispatchResult {
            response: self.respond(
                request,
                ResponsePayload::SessionOpened(SessionOpenedResponse {
                    session_id,
                    owner_connection_id: connection_id,
                }),
            ),
            events: Vec::new(),
        })
    }

    async fn create_session(
        &mut self,
        request: &RequestFrame,
        payload: CreateSessionRequest,
    ) -> Result<DispatchResult, SidecarError> {
        let (connection_id, session_id, vm_id) = self.vm_scope_for(&request.ownership)?;
        self.require_owned_vm(&connection_id, &session_id, &vm_id)?;

        self.next_agent_process_id += 1;
        let process_id = format!("acp-agent-{}", self.next_agent_process_id);
        let mut env = payload.env.clone();
        env.insert(String::from("AGENT_OS_KEEP_STDIN_OPEN"), String::from("1"));
        let execute_result = self
            .execute(
                request,
                ExecuteRequest {
                    process_id: process_id.clone(),
                    command: None,
                    runtime: Some(payload.runtime.clone()),
                    entrypoint: Some(payload.adapter_entrypoint.clone()),
                    args: payload.args.clone(),
                    env,
                    cwd: Some(payload.cwd.clone()),
                    wasm_permission_tier: None,
                },
            )
            .await?;
        let mut events = execute_result.events;
        let session_pid = match &execute_result.response.payload {
            ResponsePayload::ProcessStarted(payload) => payload.pid,
            _ => None,
        };

        let initialize = JsonRpcRequest {
            jsonrpc: String::from("2.0"),
            id: JsonRpcId::Number(1),
            method: String::from("initialize"),
            params: Some(json!({
                "protocolVersion": 1,
                "clientCapabilities": {
                    "fs": {
                        "readTextFile": true,
                        "writeTextFile": true,
                    },
                    "terminal": true,
                }
            })),
        };
        let initialize_response = match self
            .send_acp_request_and_collect(
                &vm_id,
                &process_id,
                &payload.agent_type,
                None,
                initialize,
            )
            .await
        {
            Ok((response, response_events)) => {
                events.extend(response_events);
                response
            }
            Err(error) => {
                self.kill_acp_process(&vm_id, &process_id);
                return Err(error.into_sidecar_error());
            }
        };
        if let Some(error) = &initialize_response.error {
            self.kill_acp_process(&vm_id, &process_id);
            return Err(SidecarError::InvalidState(format!(
                "ACP initialize failed: {}",
                error.message
            )));
        }
        let init_result = to_record(initialize_response.result);

        let session_new = JsonRpcRequest {
            jsonrpc: String::from("2.0"),
            id: JsonRpcId::Number(2),
            method: String::from("session/new"),
            params: Some(json!({
                "cwd": payload.cwd,
                "mcpServers": payload.mcp_servers,
            })),
        };
        let session_response = match self
            .send_acp_request_and_collect(
                &vm_id,
                &process_id,
                &payload.agent_type,
                None,
                session_new,
            )
            .await
        {
            Ok((response, response_events)) => {
                events.extend(response_events);
                response
            }
            Err(error) => {
                self.kill_acp_process(&vm_id, &process_id);
                return Err(error.into_sidecar_error());
            }
        };
        if let Some(error) = &session_response.error {
            self.kill_acp_process(&vm_id, &process_id);
            return Err(SidecarError::InvalidState(format!(
                "ACP session/new failed: {}",
                error.message
            )));
        }
        let session_result = to_record(session_response.result);
        let acp_session_id = session_result
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                SidecarError::InvalidState(String::from(
                    "ACP session/new response missing sessionId",
                ))
            })?
            .to_owned();

        let mut session = AcpSessionState::new(
            acp_session_id.clone(),
            vm_id.clone(),
            payload.agent_type,
            process_id,
            session_pid,
            &init_result,
            &session_result,
        );
        if let Some(buffer) = self.acp_process_stdout_buffers.remove(&session.process_id) {
            session.stdout_buffer = buffer;
        }
        let created = session.created_response();
        self.acp_sessions.insert(acp_session_id, session);

        Ok(DispatchResult {
            response: self.respond(request, ResponsePayload::SessionCreated(created)),
            events,
        })
    }

    async fn session_request(
        &mut self,
        request: &RequestFrame,
        payload: AgentSessionRequest,
    ) -> Result<DispatchResult, SidecarError> {
        let (connection_id, session_id, vm_id) = self.vm_scope_for(&request.ownership)?;
        self.require_owned_vm(&connection_id, &session_id, &vm_id)?;

        let (process_id, agent_type) = {
            let session = self.require_acp_session(&payload.session_id, &vm_id)?;
            (session.process_id.clone(), session.agent_type.clone())
        };

        let normalized = {
            let session = self
                .acp_sessions
                .get_mut(&payload.session_id)
                .expect("ACP session should exist");
            maybe_normalize_permission_response(
                &payload.method,
                payload.params.clone(),
                &mut session.pending_permission_requests,
            )
        };
        if let Some((response_id, result)) = normalized {
            let response = JsonRpcResponse {
                jsonrpc: String::from("2.0"),
                id: response_id.clone(),
                result: Some(result.clone()),
                error: None,
            };
            self.write_json_rpc_message(
                &vm_id,
                &process_id,
                JsonRpcMessage::Response(response.clone()),
            )?;
            return Ok(DispatchResult {
                response: self.respond(
                    request,
                    ResponsePayload::SessionRpc(SessionRpcResponse {
                        session_id: payload.session_id,
                        response: serde_json::to_value(response)
                            .expect("serialize ACP permission response"),
                    }),
                ),
                events: Vec::new(),
            });
        }

        let event_count_before = self
            .acp_sessions
            .get(&payload.session_id)
            .expect("ACP session should exist")
            .events
            .len();
        let rpc_id = {
            let session = self
                .acp_sessions
                .get_mut(&payload.session_id)
                .expect("ACP session should exist");
            let rpc_id = session.next_request_id;
            session.next_request_id += 1;
            session.record_activity(format!("sent request {} id={}", payload.method, rpc_id));
            rpc_id
        };
        let merged_params = {
            let mut params = to_record(payload.params.clone());
            params.insert(
                String::from("sessionId"),
                Value::String(payload.session_id.clone()),
            );
            params
        };
        let outbound = JsonRpcRequest {
            jsonrpc: String::from("2.0"),
            id: JsonRpcId::Number(rpc_id),
            method: payload.method.clone(),
            params: Some(Value::Object(merged_params.clone())),
        };

        let (mut response, mut events) = match self
            .send_acp_request_and_collect(
                &vm_id,
                &process_id,
                &agent_type,
                Some(&payload.session_id),
                outbound.clone(),
            )
            .await
        {
            Ok(result) => result,
            Err(AcpRequestError::Timeout(diagnostics)) => (
                Self::session_timeout_response(outbound.id, diagnostics),
                Vec::new(),
            ),
            Err(AcpRequestError::Sidecar(error)) => return Err(error),
        };
        if payload.method == ACP_CANCEL_METHOD && is_cancel_method_not_found(&response) {
            let notification = JsonRpcNotification {
                jsonrpc: String::from("2.0"),
                method: payload.method.clone(),
                params: Some(Value::Object(merged_params.clone())),
            };
            self.write_json_rpc_message(
                &vm_id,
                &process_id,
                JsonRpcMessage::Notification(notification),
            )?;
            response = JsonRpcResponse {
                jsonrpc: String::from("2.0"),
                id: response.id,
                result: Some(json!({
                    "cancelled": false,
                    "requested": true,
                    "via": "notification-fallback",
                })),
                error: None,
            };
        }
        if response.error.is_none() {
            let synthetic = {
                let session = self
                    .acp_sessions
                    .get_mut(&payload.session_id)
                    .expect("ACP session should exist");
                session.apply_request_success(&payload.method, &merged_params, event_count_before)
            };
            if let Some(notification) = synthetic {
                events.push(
                    self.build_acp_event_frame(
                        &request.ownership,
                        &payload.session_id,
                        self.acp_sessions
                            .get(&payload.session_id)
                            .expect("ACP session should exist")
                            .next_sequence_number
                            - 1,
                        &notification,
                    )?,
                );
            }
        }

        Ok(DispatchResult {
            response: self.respond(
                request,
                ResponsePayload::SessionRpc(SessionRpcResponse {
                    session_id: payload.session_id,
                    response: serde_json::to_value(response)
                        .expect("serialize ACP JSON-RPC response"),
                }),
            ),
            events,
        })
    }

    async fn get_session_state(
        &mut self,
        request: &RequestFrame,
        payload: GetSessionStateRequest,
    ) -> Result<DispatchResult, SidecarError> {
        let (_, _, vm_id) = self.vm_scope_for(&request.ownership)?;
        let session = self.require_acp_session(&payload.session_id, &vm_id)?;
        Ok(DispatchResult {
            response: self.respond(
                request,
                ResponsePayload::SessionState(session.state_response()),
            ),
            events: Vec::new(),
        })
    }

    async fn close_agent_session(
        &mut self,
        request: &RequestFrame,
        payload: CloseAgentSessionRequest,
    ) -> Result<DispatchResult, SidecarError> {
        let (_, _, vm_id) = self.vm_scope_for(&request.ownership)?;
        let (process_id, terminal_ids) = {
            let session = self.require_acp_session(&payload.session_id, &vm_id)?;
            (
                session.process_id.clone(),
                session.terminals.keys().cloned().collect::<Vec<_>>(),
            )
        };
        for terminal_id in terminal_ids {
            let _ = self.sync_acp_terminal(
                &vm_id,
                &payload.session_id,
                &terminal_id,
                false,
                Duration::ZERO,
            );
            let should_kill = self
                .acp_sessions
                .get(&payload.session_id)
                .and_then(|session| session.terminals.get(&terminal_id))
                .is_some_and(|terminal| terminal.exit_code.is_none());
            if should_kill {
                if let Some(process_id) = self
                    .acp_sessions
                    .get(&payload.session_id)
                    .and_then(|session| session.terminals.get(&terminal_id))
                    .map(|terminal| terminal.process_id.clone())
                {
                    let _ = self.kill_process_internal(&vm_id, &process_id, "SIGTERM");
                    let _ = self.sync_acp_terminal(
                        &vm_id,
                        &payload.session_id,
                        &terminal_id,
                        true,
                        Duration::from_secs(5),
                    );
                }
            }
        }
        self.terminate_acp_process(&vm_id, &process_id).await?;
        self.acp_sessions.remove(&payload.session_id);
        Ok(DispatchResult {
            response: self.respond(
                request,
                ResponsePayload::AgentSessionClosed(AgentSessionClosedResponse {
                    session_id: payload.session_id,
                }),
            ),
            events: Vec::new(),
        })
    }

    // create_vm, dispose_vm, bootstrap_root_filesystem, configure_vm moved to crate::vm

    async fn guest_filesystem_call(
        &mut self,
        request: &RequestFrame,
        payload: GuestFilesystemCallRequest,
    ) -> Result<DispatchResult, SidecarError> {
        filesystem_guest_filesystem_call(self, request, payload).await
    }

    // snapshot_root_filesystem moved to crate::vm

    // execute, write_stdin, close_stdin, kill_process, find_listener, find_bound_udp,
    // get_signal_state, get_zombie_timer_count moved to crate::execution

    async fn dispose_session(
        &mut self,
        connection_id: &str,
        session_id: &str,
        reason: DisposeReason,
    ) -> Result<Vec<EventFrame>, SidecarError> {
        self.require_owned_session(connection_id, session_id)?;

        let vm_ids = self
            .sessions
            .get(session_id)
            .expect("owned session should exist")
            .vm_ids
            .iter()
            .cloned()
            .collect::<Vec<_>>();

        let mut events = Vec::new();
        for vm_id in vm_ids {
            events.extend(
                self.dispose_vm_internal(connection_id, session_id, &vm_id, reason.clone())
                    .await?,
            );
        }

        self.sessions.remove(session_id);
        if let Some(connection) = self.connections.get_mut(connection_id) {
            connection.sessions.remove(session_id);
        }
        Ok(events)
    }

    // dispose_vm_internal, terminate_vm_processes, wait_for_vm_processes_to_exit moved to crate::vm

    // kill_process_internal, handle_execution_event, handle_python_vfs_rpc_request,
    // resolve_javascript_child_process_execution, spawn_javascript_child_process,
    // poll_javascript_child_process, write_javascript_child_process_stdin,
    // close_javascript_child_process_stdin, kill_javascript_child_process moved to crate::execution

    pub(crate) fn handle_javascript_sync_rpc_request(
        &mut self,
        vm_id: &str,
        process_id: &str,
        request: JavascriptSyncRpcRequest,
    ) -> Result<(), SidecarError> {
        let response: Result<Value, SidecarError> = match request.method.as_str() {
            "child_process.spawn" => {
                let vm = self.vms.get(vm_id).expect("VM should exist");
                let (payload, _) = parse_javascript_child_process_spawn_request(vm, &request.args)?;
                self.spawn_javascript_child_process(vm_id, process_id, payload)
            }
            "child_process.spawn_sync" => {
                let vm = self.vms.get(vm_id).expect("VM should exist");
                let (payload, max_buffer) =
                    parse_javascript_child_process_spawn_request(vm, &request.args)?;
                self.spawn_javascript_child_process_sync(vm_id, process_id, payload, max_buffer)
            }
            "child_process.poll" => {
                let child_process_id =
                    javascript_sync_rpc_arg_str(&request.args, 0, "child_process.poll child id")?;
                let wait_ms = javascript_sync_rpc_arg_u64_optional(
                    &request.args,
                    1,
                    "child_process.poll wait ms",
                )?
                .unwrap_or_default();
                self.poll_javascript_child_process(vm_id, process_id, child_process_id, wait_ms)
            }
            "child_process.write_stdin" => {
                let child_process_id = javascript_sync_rpc_arg_str(
                    &request.args,
                    0,
                    "child_process.write_stdin child id",
                )?;
                let chunk = javascript_sync_rpc_bytes_arg(
                    &request.args,
                    1,
                    "child_process.write_stdin chunk",
                )?;
                self.write_javascript_child_process_stdin(
                    vm_id,
                    process_id,
                    child_process_id,
                    &chunk,
                )?;
                Ok(Value::Null)
            }
            "child_process.close_stdin" => {
                let child_process_id = javascript_sync_rpc_arg_str(
                    &request.args,
                    0,
                    "child_process.close_stdin child id",
                )?;
                self.close_javascript_child_process_stdin(vm_id, process_id, child_process_id)?;
                Ok(Value::Null)
            }
            "child_process.kill" => {
                let child_process_id =
                    javascript_sync_rpc_arg_str(&request.args, 0, "child_process.kill child id")?;
                let signal =
                    javascript_sync_rpc_arg_str(&request.args, 1, "child_process.kill signal")?;
                self.kill_javascript_child_process(vm_id, process_id, child_process_id, signal)?;
                Ok(Value::Null)
            }
            "process.kill" => {
                let target_pid =
                    javascript_sync_rpc_arg_u32(&request.args, 0, "process.kill target pid")?;
                let signal = javascript_sync_rpc_arg_str(&request.args, 1, "process.kill signal")?;
                let parsed_signal = parse_signal(signal)?;
                enum ProcessKillTarget {
                    SelfProcess(SignalDispositionAction),
                    Child(String),
                    TopLevel(String),
                }
                let target = {
                    let vm = self.vms.get(vm_id).expect("VM should exist");
                    let caller = vm
                        .active_processes
                        .get(process_id)
                        .expect("process should still exist");
                    if caller.kernel_pid == target_pid {
                        let action = vm
                            .signal_states
                            .get(process_id)
                            .and_then(|handlers| handlers.get(&(parsed_signal as u32)))
                            .map(|registration| registration.action)
                            .unwrap_or(SignalDispositionAction::Default);
                        Some(ProcessKillTarget::SelfProcess(action))
                    } else if let Some((child_process_id, _)) = caller
                        .child_processes
                        .iter()
                        .find(|(_, child)| child.kernel_pid == target_pid)
                    {
                        Some(ProcessKillTarget::Child(child_process_id.clone()))
                    } else {
                        vm.active_processes
                            .iter()
                            .find(|(_, process)| process.kernel_pid == target_pid)
                            .map(|(target_process_id, _)| {
                                ProcessKillTarget::TopLevel(target_process_id.clone())
                            })
                    }
                };
                match target {
                    Some(ProcessKillTarget::SelfProcess(action)) => Ok(json!({
                        "self": true,
                        "action": match action {
                            SignalDispositionAction::Default => "default",
                            SignalDispositionAction::Ignore => "ignore",
                            SignalDispositionAction::User => "user",
                        },
                    })),
                    Some(ProcessKillTarget::Child(child_process_id)) => {
                        self.kill_javascript_child_process(
                            vm_id,
                            process_id,
                            &child_process_id,
                            signal,
                        )?;
                        Ok(Value::Null)
                    }
                    Some(ProcessKillTarget::TopLevel(target_process_id)) => {
                        self.kill_process_internal(vm_id, &target_process_id, signal)?;
                        Ok(Value::Null)
                    }
                    None => Err(SidecarError::InvalidState(format!(
                        "unknown process pid {target_pid}"
                    ))),
                }
            }
            "process.signal_state" => {
                let signal =
                    javascript_sync_rpc_arg_u32(&request.args, 0, "process.signal_state signal")?;
                let action =
                    javascript_sync_rpc_arg_str(&request.args, 1, "process.signal_state action")?;
                let mask_json =
                    javascript_sync_rpc_arg_str(&request.args, 2, "process.signal_state mask")?;
                let flags =
                    javascript_sync_rpc_arg_u32(&request.args, 3, "process.signal_state flags")?;
                let mask: Vec<u32> = serde_json::from_str(mask_json).map_err(|error| {
                    SidecarError::InvalidState(format!(
                        "process.signal_state mask must be valid JSON: {error}"
                    ))
                })?;
                let action = match action.trim().to_ascii_lowercase().as_str() {
                    "default" => SignalDispositionAction::Default,
                    "ignore" => SignalDispositionAction::Ignore,
                    "user" => SignalDispositionAction::User,
                    other => {
                        return Err(SidecarError::InvalidState(format!(
                            "unsupported process.signal_state action {other}"
                        )));
                    }
                };
                let vm = self.vms.get_mut(vm_id).expect("VM should exist");
                if action == SignalDispositionAction::Default && mask.is_empty() && flags == 0 {
                    let remove_process_entry = vm
                        .signal_states
                        .get_mut(process_id)
                        .map(|handlers| {
                            handlers.remove(&signal);
                            handlers.is_empty()
                        })
                        .unwrap_or(false);
                    if remove_process_entry {
                        vm.signal_states.remove(process_id);
                    }
                } else {
                    vm.signal_states
                        .entry(process_id.to_owned())
                        .or_default()
                        .insert(
                            signal,
                            SignalHandlerRegistration {
                                action,
                                mask,
                                flags,
                            },
                        );
                }
                Ok(Value::Null)
            }
            _ => {
                let vm = self.vms.get_mut(vm_id).expect("VM should exist");
                let resource_limits = vm.kernel.resource_limits().clone();
                let network_counts = vm_network_resource_counts(vm);
                let socket_paths = build_javascript_socket_path_context(vm)?;
                let enable_transform = vm.configuration.enable_http_request_transform;
                let ownership = OwnershipScope::vm(
                    vm.connection_id.clone(),
                    vm.session_id.clone(),
                    vm_id.to_owned(),
                );
                let http_transform = if enable_transform {
                    Some((&self.sidecar_requests, &ownership))
                } else {
                    None
                };
                let process = vm
                    .active_processes
                    .get_mut(process_id)
                    .expect("process should still exist");
                service_javascript_sync_rpc(
                    &self.bridge,
                    vm_id,
                    &vm.dns,
                    &socket_paths,
                    &mut vm.kernel,
                    process,
                    &request,
                    &resource_limits,
                    network_counts,
                    http_transform,
                )
            }
        };

        let vm = self.vms.get_mut(vm_id).expect("VM should exist");
        let shadow_root = vm.cwd.clone();
        let process = vm
            .active_processes
            .get_mut(process_id)
            .expect("process should still exist");

        if response.is_ok()
            && matches!(
                request.method.as_str(),
                "fs.chmodSync" | "fs.promises.chmod"
            )
        {
            let guest_path =
                javascript_sync_rpc_arg_str(&request.args, 0, "filesystem chmod path")?;
            let mode =
                javascript_sync_rpc_arg_u32(&request.args, 1, "filesystem chmod mode")? & 0o7777;
            let host_path =
                shadow_host_path_for_process(&shadow_root, &process.guest_cwd, guest_path);
            if host_path.exists() {
                fs::set_permissions(&host_path, fs::Permissions::from_mode(mode)).map_err(
                    |error| {
                        SidecarError::Io(format!(
                            "failed to mirror chmod to shadow path {}: {error}",
                            host_path.display()
                        ))
                    },
                )?;
            }
        }

        match response {
            Ok(result) => process
                .execution
                .respond_javascript_sync_rpc_success(request.id, result)
                .or_else(ignore_stale_javascript_sync_rpc_response),
            Err(error) => process
                .execution
                .respond_javascript_sync_rpc_error(
                    request.id,
                    javascript_sync_rpc_error_code(&error),
                    error.to_string(),
                )
                .or_else(ignore_stale_javascript_sync_rpc_response),
        }
    }

    pub(crate) fn vm_ids_for_scope(
        &self,
        ownership: &OwnershipScope,
    ) -> Result<Vec<String>, SidecarError> {
        match ownership {
            OwnershipScope::Session {
                connection_id,
                session_id,
            } => {
                self.require_owned_session(connection_id, session_id)?;
                Ok(self
                    .sessions
                    .get(session_id)
                    .expect("owned session should exist")
                    .vm_ids
                    .iter()
                    .cloned()
                    .collect())
            }
            OwnershipScope::Vm {
                connection_id,
                session_id,
                vm_id,
            } => {
                self.require_owned_vm(connection_id, session_id, vm_id)?;
                Ok(vec![vm_id.clone()])
            }
            OwnershipScope::Connection { .. } => Err(SidecarError::InvalidState(String::from(
                "event polling requires session or VM ownership scope",
            ))),
        }
    }

    pub(crate) fn vm_ownership(&self, vm_id: &str) -> Result<OwnershipScope, SidecarError> {
        let vm = self
            .vms
            .get(vm_id)
            .ok_or_else(|| SidecarError::InvalidState(format!("unknown sidecar VM {vm_id}")))?;
        Ok(OwnershipScope::vm(&vm.connection_id, &vm.session_id, vm_id))
    }

    pub(crate) fn vm_has_active_processes(&self, vm_id: &str) -> bool {
        self.vms
            .get(vm_id)
            .is_some_and(|vm| !vm.active_processes.is_empty())
    }

    fn require_authenticated_connection(&self, connection_id: &str) -> Result<(), SidecarError> {
        if self.connections.contains_key(connection_id) {
            Ok(())
        } else {
            Err(SidecarError::InvalidState(format!(
                "connection {connection_id} has not authenticated"
            )))
        }
    }

    pub(crate) fn require_owned_session(
        &self,
        connection_id: &str,
        session_id: &str,
    ) -> Result<(), SidecarError> {
        self.require_authenticated_connection(connection_id)?;
        let session = self.sessions.get(session_id).ok_or_else(|| {
            SidecarError::InvalidState(format!("unknown sidecar session {session_id}"))
        })?;
        if session.connection_id == connection_id {
            Ok(())
        } else {
            Err(SidecarError::InvalidState(format!(
                "session {session_id} is not owned by connection {connection_id}"
            )))
        }
    }

    pub(crate) fn require_owned_vm(
        &self,
        connection_id: &str,
        session_id: &str,
        vm_id: &str,
    ) -> Result<(), SidecarError> {
        self.require_owned_session(connection_id, session_id)?;
        let vm = self
            .vms
            .get(vm_id)
            .ok_or_else(|| SidecarError::InvalidState(format!("unknown sidecar VM {vm_id}")))?;
        if vm.connection_id != connection_id || vm.session_id != session_id {
            return Err(SidecarError::InvalidState(format!(
                "VM {vm_id} is not owned by {connection_id}/{session_id}"
            )));
        }
        Ok(())
    }

    fn connection_id_for(&self, ownership: &OwnershipScope) -> Result<String, SidecarError> {
        match ownership {
            OwnershipScope::Connection { connection_id } => Ok(connection_id.clone()),
            OwnershipScope::Session { .. } | OwnershipScope::Vm { .. } => {
                Err(SidecarError::InvalidState(String::from(
                    "request requires connection ownership scope",
                )))
            }
        }
    }

    fn validate_auth_token(&self, auth_token: &str) -> Result<(), SidecarError> {
        let Some(expected_auth_token) = self.config.expected_auth_token.as_deref() else {
            return Ok(());
        };

        if auth_token == expected_auth_token {
            Ok(())
        } else {
            Err(SidecarError::Unauthorized(String::from(
                "authenticate request provided an invalid auth token",
            )))
        }
    }

    fn allocate_connection_id(&mut self) -> String {
        self.next_connection_id += 1;
        format!("conn-{}", self.next_connection_id)
    }

    fn require_acp_session(
        &self,
        acp_session_id: &str,
        vm_id: &str,
    ) -> Result<&AcpSessionState, SidecarError> {
        let session = self.acp_sessions.get(acp_session_id).ok_or_else(|| {
            SidecarError::InvalidState(format!("unknown ACP session {acp_session_id}"))
        })?;
        if session.vm_id == vm_id {
            Ok(session)
        } else {
            Err(SidecarError::InvalidState(format!(
                "ACP session {acp_session_id} is not owned by VM {vm_id}"
            )))
        }
    }

    pub(crate) fn acp_terminal_owner_for_process(
        &self,
        vm_id: &str,
        process_id: &str,
    ) -> Option<(String, String)> {
        self.acp_sessions.iter().find_map(|(session_id, session)| {
            if session.vm_id != vm_id {
                return None;
            }
            session
                .terminals
                .iter()
                .find_map(|(terminal_id, terminal)| {
                    (terminal.process_id == process_id)
                        .then(|| (session_id.clone(), terminal_id.clone()))
                })
        })
    }

    fn require_visible_acp_terminal<'a>(
        &'a self,
        session_id: &str,
        terminal_id: &str,
    ) -> Result<&'a AcpTerminalState, SidecarError> {
        let session = self.acp_sessions.get(session_id).ok_or_else(|| {
            SidecarError::InvalidState(format!("unknown ACP session {session_id}"))
        })?;
        let terminal = session.terminals.get(terminal_id).ok_or_else(|| {
            SidecarError::InvalidState(format!("ACP terminal not found: {terminal_id}"))
        })?;
        if terminal.released {
            return Err(SidecarError::InvalidState(format!(
                "ACP terminal not found: {terminal_id}"
            )));
        }
        Ok(terminal)
    }

    fn handle_acp_terminal_execution_event(
        &mut self,
        vm_id: &str,
        session_id: &str,
        terminal_id: &str,
        event: ActiveExecutionEvent,
    ) -> Result<(), SidecarError> {
        match event {
            ActiveExecutionEvent::Stdout(chunk) | ActiveExecutionEvent::Stderr(chunk) => {
                if let Some(session) = self.acp_sessions.get_mut(session_id) {
                    if let Some(terminal) = session.terminals.get_mut(terminal_id) {
                        terminal.append_output(&chunk);
                    }
                }
                Ok(())
            }
            ActiveExecutionEvent::Exited(exit_code) => {
                let (process_id, released) = {
                    let session = self.acp_sessions.get_mut(session_id).ok_or_else(|| {
                        SidecarError::InvalidState(format!("unknown ACP session {session_id}"))
                    })?;
                    let terminal = session.terminals.get_mut(terminal_id).ok_or_else(|| {
                        SidecarError::InvalidState(format!("ACP terminal not found: {terminal_id}"))
                    })?;
                    terminal.exit_code = Some(exit_code);
                    (terminal.process_id.clone(), terminal.released)
                };
                let _ = self.handle_execution_event(
                    vm_id,
                    &process_id,
                    ActiveExecutionEvent::Exited(exit_code),
                )?;
                if released {
                    if let Some(session) = self.acp_sessions.get_mut(session_id) {
                        session.terminals.remove(terminal_id);
                    }
                }
                Ok(())
            }
            other => {
                let process_id = self
                    .acp_sessions
                    .get(session_id)
                    .and_then(|session| session.terminals.get(terminal_id))
                    .map(|terminal| terminal.process_id.clone())
                    .ok_or_else(|| {
                        SidecarError::InvalidState(format!("ACP terminal not found: {terminal_id}"))
                    })?;
                let _ = self.handle_execution_event(vm_id, &process_id, other)?;
                Ok(())
            }
        }
    }

    fn drain_queued_acp_terminal_events(
        &mut self,
        vm_id: &str,
        session_id: &str,
        terminal_id: &str,
    ) -> Result<(), SidecarError> {
        let process_id = self
            .acp_sessions
            .get(session_id)
            .and_then(|session| session.terminals.get(terminal_id))
            .map(|terminal| terminal.process_id.clone())
            .ok_or_else(|| {
                SidecarError::InvalidState(format!("ACP terminal not found: {terminal_id}"))
            })?;

        let mut deferred = VecDeque::new();
        while let Some(envelope) = self.pending_process_events.pop_front() {
            if envelope.vm_id == vm_id && envelope.process_id == process_id {
                self.handle_acp_terminal_execution_event(
                    vm_id,
                    session_id,
                    terminal_id,
                    envelope.event,
                )?;
            } else {
                deferred.push_back(envelope);
            }
        }
        self.pending_process_events = deferred;

        let mut queued = Vec::new();
        {
            let receiver = self.process_event_receiver.as_mut().ok_or_else(|| {
                SidecarError::InvalidState(String::from("process event receiver unavailable"))
            })?;
            while let Ok(envelope) = receiver.try_recv() {
                queued.push(envelope);
            }
        }
        for envelope in queued {
            if envelope.vm_id == vm_id && envelope.process_id == process_id {
                self.handle_acp_terminal_execution_event(
                    vm_id,
                    session_id,
                    terminal_id,
                    envelope.event,
                )?;
            } else {
                self.pending_process_events.push_back(envelope);
            }
        }

        Ok(())
    }

    fn sync_acp_terminal(
        &mut self,
        vm_id: &str,
        session_id: &str,
        terminal_id: &str,
        wait_for_exit: bool,
        timeout: Duration,
    ) -> Result<(), SidecarError> {
        let deadline = Instant::now() + timeout;
        loop {
            self.drain_queued_acp_terminal_events(vm_id, session_id, terminal_id)?;
            let (process_id, exit_code) = self
                .acp_sessions
                .get(session_id)
                .and_then(|session| session.terminals.get(terminal_id))
                .map(|terminal| (terminal.process_id.clone(), terminal.exit_code))
                .ok_or_else(|| {
                    SidecarError::InvalidState(format!("ACP terminal not found: {terminal_id}"))
                })?;
            if exit_code.is_some() {
                return Ok(());
            }

            let wait = if wait_for_exit {
                deadline
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_millis(25))
            } else {
                Duration::ZERO
            };
            let event = {
                let vm = self.vms.get_mut(vm_id).ok_or_else(|| {
                    SidecarError::InvalidState(format!("unknown sidecar VM {vm_id}"))
                })?;
                let process = vm.active_processes.get_mut(&process_id).ok_or_else(|| {
                    SidecarError::InvalidState(format!(
                        "VM {vm_id} has no active process {process_id}"
                    ))
                })?;
                process.execution.poll_event_blocking(wait)?
            };

            match event {
                Some(event) => {
                    self.handle_acp_terminal_execution_event(vm_id, session_id, terminal_id, event)?
                }
                None if wait_for_exit && Instant::now() >= deadline => {
                    return Err(SidecarError::InvalidState(format!(
                        "ACP terminal {terminal_id} did not exit before timeout"
                    )));
                }
                None if wait_for_exit => continue,
                None => return Ok(()),
            }
        }
    }

    fn handle_inbound_acp_request(
        &mut self,
        session_id: &str,
        request: &JsonRpcRequest,
    ) -> Result<Option<Value>, SidecarError> {
        let params = to_record(request.params.clone());
        let (vm_id, kernel_pid, connection_id, sidecar_session_id) = {
            let session = self.acp_sessions.get(session_id).ok_or_else(|| {
                SidecarError::InvalidState(format!("unknown ACP session {session_id}"))
            })?;
            let vm = self.vms.get(&session.vm_id).ok_or_else(|| {
                SidecarError::InvalidState(format!("unknown sidecar VM {}", session.vm_id))
            })?;
            let process = vm
                .active_processes
                .get(&session.process_id)
                .ok_or_else(|| {
                    SidecarError::InvalidState(format!(
                        "VM {} has no active process {}",
                        session.vm_id, session.process_id
                    ))
                })?;
            (
                session.vm_id.clone(),
                process.kernel_pid,
                vm.connection_id.clone(),
                vm.session_id.clone(),
            )
        };

        match request.method.as_str() {
            "fs/read_text_file" => {
                let path = params.get("path").and_then(Value::as_str).ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "fs/read_text_file requires a string path",
                    ))
                })?;
                let path = normalize_path(path);
                let bytes = {
                    let vm = self.vms.get_mut(&vm_id).ok_or_else(|| {
                        SidecarError::InvalidState(format!("unknown sidecar VM {vm_id}"))
                    })?;
                    vm.kernel
                        .read_file_for_process(EXECUTION_DRIVER_NAME, kernel_pid, &path)
                        .map_err(kernel_error)?
                };
                let content = String::from_utf8_lossy(&bytes).into_owned();
                let start_line = params
                    .get("line")
                    .and_then(Value::as_u64)
                    .unwrap_or(1)
                    .max(1) as usize;
                let limit = params
                    .get("limit")
                    .and_then(Value::as_u64)
                    .map(|value| value as usize);
                let lines = content.split('\n').collect::<Vec<_>>();
                let sliced = lines
                    .into_iter()
                    .skip(start_line.saturating_sub(1))
                    .take(limit.unwrap_or(usize::MAX))
                    .collect::<Vec<_>>()
                    .join("\n");
                Ok(Some(json!({ "content": sliced })))
            }
            "fs/write_text_file" => {
                let path = params.get("path").and_then(Value::as_str).ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "fs/write_text_file requires string path and content",
                    ))
                })?;
                let content = params
                    .get("content")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        SidecarError::InvalidState(String::from(
                            "fs/write_text_file requires string path and content",
                        ))
                    })?;
                let path = normalize_path(path);
                {
                    let vm = self.vms.get_mut(&vm_id).ok_or_else(|| {
                        SidecarError::InvalidState(format!("unknown sidecar VM {vm_id}"))
                    })?;
                    vm.kernel
                        .write_file_for_process(
                            EXECUTION_DRIVER_NAME,
                            kernel_pid,
                            &path,
                            content.as_bytes().to_vec(),
                            None,
                        )
                        .map_err(kernel_error)?;
                }
                Ok(Some(Value::Null))
            }
            "terminal/create" => {
                let command = params
                    .get("command")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        SidecarError::InvalidState(String::from(
                            "terminal/create requires a command",
                        ))
                    })?
                    .to_owned();
                let args = params
                    .get("args")
                    .and_then(Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(Value::as_str)
                            .map(String::from)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let env = params
                    .get("env")
                    .and_then(Value::as_array)
                    .map(|items| {
                        items.iter().fold(BTreeMap::new(), |mut acc, item| {
                            let Some(entry) = item.as_object() else {
                                return acc;
                            };
                            let (Some(name), Some(value)) = (
                                entry.get("name").and_then(Value::as_str),
                                entry.get("value").and_then(Value::as_str),
                            ) else {
                                return acc;
                            };
                            acc.insert(String::from(name), String::from(value));
                            acc
                        })
                    })
                    .unwrap_or_default();
                let cwd = params
                    .get("cwd")
                    .and_then(Value::as_str)
                    .map(normalize_path);
                let output_byte_limit = params
                    .get("outputByteLimit")
                    .and_then(Value::as_u64)
                    .unwrap_or(1_048_576) as usize;

                self.next_agent_process_id += 1;
                let process_id = format!("acp-terminal-{}", self.next_agent_process_id);
                let ownership = OwnershipScope::vm(&connection_id, &sidecar_session_id, &vm_id);
                let execute_payload = ExecuteRequest {
                    process_id: process_id.clone(),
                    command: Some(command),
                    runtime: None,
                    entrypoint: None,
                    args,
                    env,
                    cwd,
                    wasm_permission_tier: None,
                };
                let request = RequestFrame::new(
                    0,
                    ownership,
                    RequestPayload::Execute(execute_payload.clone()),
                );
                {
                    let mut execute = std::pin::pin!(self.execute(&request, execute_payload));
                    let _ = poll_future_once(execute.as_mut()).ok_or_else(|| {
                        SidecarError::InvalidState(String::from(
                            "ACP terminal/create unexpectedly required async dispatch",
                        ))
                    })??;
                }
                let terminal_id = {
                    let session = self.acp_sessions.get_mut(session_id).ok_or_else(|| {
                        SidecarError::InvalidState(format!("unknown ACP session {session_id}"))
                    })?;
                    let terminal_id = session.allocate_terminal_id();
                    session.terminals.insert(
                        terminal_id.clone(),
                        AcpTerminalState::new(process_id, output_byte_limit),
                    );
                    terminal_id
                };
                Ok(Some(json!({ "terminalId": terminal_id })))
            }
            "terminal/output" => {
                let terminal_id = params
                    .get("terminalId")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        SidecarError::InvalidState(String::from(
                            "terminal/output requires a terminalId",
                        ))
                    })?;
                self.sync_acp_terminal(&vm_id, session_id, terminal_id, false, Duration::ZERO)?;
                let terminal = self.require_visible_acp_terminal(session_id, terminal_id)?;
                let mut result = Map::from_iter([
                    (
                        String::from("output"),
                        Value::String(terminal.output.clone()),
                    ),
                    (String::from("truncated"), Value::Bool(terminal.truncated)),
                ]);
                if let Some(exit_code) = terminal.exit_code {
                    result.insert(
                        String::from("exitStatus"),
                        json!({ "exitCode": exit_code, "signal": Value::Null }),
                    );
                }
                Ok(Some(Value::Object(result)))
            }
            "terminal/wait_for_exit" => {
                let terminal_id = params
                    .get("terminalId")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        SidecarError::InvalidState(String::from(
                            "terminal/wait_for_exit requires a terminalId",
                        ))
                    })?;
                self.sync_acp_terminal(
                    &vm_id,
                    session_id,
                    terminal_id,
                    true,
                    Duration::from_millis(120_000),
                )?;
                let exit_code = self
                    .require_visible_acp_terminal(session_id, terminal_id)?
                    .exit_code
                    .ok_or_else(|| {
                        SidecarError::InvalidState(format!(
                            "ACP terminal {terminal_id} did not report an exit code"
                        ))
                    })?;
                Ok(Some(
                    json!({ "exitCode": exit_code, "signal": Value::Null }),
                ))
            }
            "terminal/kill" => {
                let terminal_id = params
                    .get("terminalId")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        SidecarError::InvalidState(String::from(
                            "terminal/kill requires a terminalId",
                        ))
                    })?;
                let terminal = self.require_visible_acp_terminal(session_id, terminal_id)?;
                if terminal.exit_code.is_none() {
                    let process_id = terminal.process_id.clone();
                    self.kill_process_internal(&vm_id, &process_id, "SIGTERM")?;
                }
                Ok(Some(Value::Null))
            }
            "terminal/release" => {
                let terminal_id = params
                    .get("terminalId")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        SidecarError::InvalidState(String::from(
                            "terminal/release requires a terminalId",
                        ))
                    })?;
                let (process_id, exit_code) = {
                    let terminal = self.require_visible_acp_terminal(session_id, terminal_id)?;
                    (terminal.process_id.clone(), terminal.exit_code)
                };
                if exit_code.is_none() {
                    self.kill_process_internal(&vm_id, &process_id, "SIGTERM")?;
                }
                if let Some(session) = self.acp_sessions.get_mut(session_id) {
                    if let Some(terminal) = session.terminals.get_mut(terminal_id) {
                        terminal.released = true;
                    }
                    if exit_code.is_some() {
                        session.terminals.remove(terminal_id);
                    }
                }
                Ok(Some(Value::Null))
            }
            _ => Ok(None),
        }
    }

    fn build_acp_event_frame(
        &self,
        ownership: &OwnershipScope,
        session_id: &str,
        sequence_number: u64,
        notification: &JsonRpcNotification,
    ) -> Result<EventFrame, SidecarError> {
        Ok(EventFrame::new(
            ownership.clone(),
            EventPayload::Structured(StructuredEvent {
                name: String::from("acp.session_event"),
                detail: BTreeMap::from([
                    (String::from("session_id"), String::from(session_id)),
                    (String::from("sequence_number"), sequence_number.to_string()),
                    (String::from("method"), notification.method.clone()),
                    (
                        String::from("notification"),
                        serde_json::to_string(notification).map_err(|error| {
                            SidecarError::InvalidState(format!(
                                "failed to serialize ACP notification: {error}"
                            ))
                        })?,
                    ),
                ]),
            }),
        ))
    }

    fn write_json_rpc_message(
        &mut self,
        vm_id: &str,
        process_id: &str,
        message: JsonRpcMessage,
    ) -> Result<(), SidecarError> {
        let encoded = serialize_message(&message).map_err(|error| {
            SidecarError::InvalidState(format!("failed to serialize ACP frame: {error}"))
        })?;
        let vm = self
            .vms
            .get_mut(vm_id)
            .ok_or_else(|| SidecarError::InvalidState(format!("unknown sidecar VM {vm_id}")))?;
        let process = vm.active_processes.get_mut(process_id).ok_or_else(|| {
            SidecarError::InvalidState(format!("VM {vm_id} has no active process {process_id}"))
        })?;
        process.execution.write_stdin(encoded.as_bytes())?;
        write_kernel_process_stdin(&mut vm.kernel, process, encoded.as_bytes())
    }

    fn kill_acp_process(&mut self, vm_id: &str, process_id: &str) {
        let _ = self.kill_process_internal(vm_id, process_id, "SIGKILL");
        self.acp_process_stdout_buffers.remove(process_id);
        if let Some(vm) = self.vms.get_mut(vm_id) {
            vm.active_processes.remove(process_id);
            vm.signal_states.remove(process_id);
        }
    }

    async fn terminate_acp_process(
        &mut self,
        vm_id: &str,
        process_id: &str,
    ) -> Result<(), SidecarError> {
        for session in self.acp_sessions.values_mut() {
            if session.vm_id == vm_id && session.process_id == process_id {
                session.mark_termination_requested();
            }
        }
        let shared_runtime_child_pid = self.vms.get(vm_id).and_then(|vm| {
            vm.active_processes
                .get(process_id)
                .and_then(|process| match &process.execution {
                    ActiveExecution::Javascript(execution)
                        if execution.uses_shared_v8_runtime() && execution.child_pid() != 0 =>
                    {
                        Some(execution.child_pid())
                    }
                    _ => None,
                })
        });
        if !self
            .vms
            .get(vm_id)
            .is_some_and(|vm| vm.active_processes.contains_key(process_id))
        {
            self.acp_process_stdout_buffers.remove(process_id);
            if let Some(vm) = self.vms.get_mut(vm_id) {
                vm.signal_states.remove(process_id);
            }
            return Ok(());
        }

        let _ = self.kill_process_internal(vm_id, process_id, "SIGKILL");
        let ownership = self.vm_ownership(vm_id)?;
        let deadline = Instant::now() + Duration::from_secs(5);

        while self
            .vms
            .get(vm_id)
            .is_some_and(|vm| vm.active_processes.contains_key(process_id))
            && Instant::now() < deadline
        {
            let remaining = deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(10));
            let _ = self.poll_event(&ownership, remaining).await?;
        }

        if let Some(child_pid) = shared_runtime_child_pid {
            let other_shared_runtime_users = self.vms.get(vm_id).is_some_and(|vm| {
                vm.active_processes.iter().any(|(candidate_id, process)| {
                    candidate_id != process_id && process.execution.child_pid() == child_pid
                })
            });
            if !other_shared_runtime_users {
                if runtime_child_is_alive(child_pid)? {
                    signal_runtime_process(child_pid, SIGKILL)?;
                    let child_deadline = Instant::now() + Duration::from_secs(5);
                    while runtime_child_is_alive(child_pid)? && Instant::now() < child_deadline {
                        time::sleep(Duration::from_millis(10)).await;
                    }
                }
                reap_runtime_child_if_exited(child_pid)?;
            }
        }

        self.acp_process_stdout_buffers.remove(process_id);
        if let Some(vm) = self.vms.get_mut(vm_id) {
            vm.active_processes.remove(process_id);
            vm.signal_states.remove(process_id);
        }
        Ok(())
    }

    fn session_timeout_diagnostics(
        session: &AcpSessionState,
        method: &str,
        id: &JsonRpcId,
    ) -> AcpTimeoutDiagnostics {
        session.timeout_diagnostics(method, id, Self::ACP_REQUEST_TIMEOUT_MS, None)
    }

    fn session_timeout_response(
        id: JsonRpcId,
        diagnostics: AcpTimeoutDiagnostics,
    ) -> JsonRpcResponse {
        JsonRpcResponse {
            jsonrpc: String::from("2.0"),
            id,
            result: None,
            error: Some(JsonRpcError {
                code: -32000,
                message: diagnostics.message(),
                data: Some(diagnostics.to_json()),
            }),
        }
    }

    async fn send_acp_request_and_collect(
        &mut self,
        vm_id: &str,
        process_id: &str,
        agent_type: &str,
        session_id: Option<&str>,
        request: JsonRpcRequest,
    ) -> Result<(JsonRpcResponse, Vec<EventFrame>), AcpRequestError> {
        self.write_json_rpc_message(vm_id, process_id, JsonRpcMessage::Request(request.clone()))
            .map_err(AcpRequestError::Sidecar)?;

        let ownership = self.vm_ownership(vm_id).map_err(AcpRequestError::Sidecar)?;
        let deadline = Instant::now() + Duration::from_millis(Self::ACP_REQUEST_TIMEOUT_MS);
        let mut events = Vec::new();

        loop {
            let _ = self
                .pump_process_events(&ownership)
                .await
                .map_err(AcpRequestError::Sidecar)?;

            while let Some(envelope) = self
                .take_matching_process_event_envelope(vm_id, process_id)
                .map_err(AcpRequestError::Sidecar)?
            {
                let exited = match envelope.event {
                    ActiveExecutionEvent::Exited(exit_code) => Some(exit_code),
                    _ => None,
                };
                if let Some(response) = self
                    .handle_acp_process_event(
                        vm_id,
                        process_id,
                        session_id,
                        &ownership,
                        envelope.event,
                        &mut events,
                    )
                    .map_err(AcpRequestError::Sidecar)?
                {
                    if response.id == request.id {
                        return Ok((response, events));
                    }
                }
                if let Some(exit_code) = exited {
                    self.terminate_acp_process(vm_id, process_id)
                        .await
                        .map_err(AcpRequestError::Sidecar)?;
                    return Ok((
                        JsonRpcResponse {
                            jsonrpc: String::from("2.0"),
                            id: request.id.clone(),
                            result: None,
                            error: Some(JsonRpcError {
                                code: -32000,
                                message: format!(
                                    "ACP process exited while handling {} (exit code {exit_code})",
                                    request.method
                                ),
                                data: None,
                            }),
                        },
                        events,
                    ));
                }
            }

            let event = {
                let vm = self
                    .vms
                    .get_mut(vm_id)
                    .ok_or_else(|| {
                        SidecarError::InvalidState(format!("unknown sidecar VM {vm_id}"))
                    })
                    .map_err(AcpRequestError::Sidecar)?;
                let process = vm
                    .active_processes
                    .get_mut(process_id)
                    .ok_or_else(|| {
                        SidecarError::InvalidState(format!(
                            "VM {vm_id} has no active process {process_id}"
                        ))
                    })
                    .map_err(AcpRequestError::Sidecar)?;
                process
                    .execution
                    .poll_event(Duration::from_millis(10))
                    .await
                    .map_err(AcpRequestError::Sidecar)?
            };

            if let Some(event) = event {
                let exited = match event {
                    ActiveExecutionEvent::Exited(exit_code) => Some(exit_code),
                    _ => None,
                };
                if let Some(response) = self
                    .handle_acp_process_event(
                        vm_id,
                        process_id,
                        session_id,
                        &ownership,
                        event,
                        &mut events,
                    )
                    .map_err(AcpRequestError::Sidecar)?
                {
                    if response.id == request.id {
                        return Ok((response, events));
                    }
                }
                if let Some(exit_code) = exited {
                    self.terminate_acp_process(vm_id, process_id)
                        .await
                        .map_err(AcpRequestError::Sidecar)?;
                    return Ok((
                        JsonRpcResponse {
                            jsonrpc: String::from("2.0"),
                            id: request.id.clone(),
                            result: None,
                            error: Some(JsonRpcError {
                                code: -32000,
                                message: format!(
                                    "ACP process exited while handling {} (exit code {exit_code})",
                                    request.method
                                ),
                                data: None,
                            }),
                        },
                        events,
                    ));
                }
            }

            if Instant::now() >= deadline {
                let session = session_id
                    .and_then(|session_id| self.acp_sessions.get(session_id))
                    .cloned()
                    .unwrap_or_else(|| {
                        AcpSessionState::new(
                            String::new(),
                            String::from(vm_id),
                            String::from(agent_type),
                            String::from(process_id),
                            None,
                            &Map::new(),
                            &Map::new(),
                        )
                    });
                return Err(AcpRequestError::Timeout(Self::session_timeout_diagnostics(
                    &session,
                    &request.method,
                    &request.id,
                )));
            }
        }
    }

    fn take_matching_process_event_envelope(
        &mut self,
        vm_id: &str,
        process_id: &str,
    ) -> Result<Option<ProcessEventEnvelope>, SidecarError> {
        if let Some(index) = self
            .pending_process_events
            .iter()
            .position(|event| event.vm_id == vm_id && event.process_id == process_id)
        {
            return Ok(self.pending_process_events.remove(index));
        }

        let receiver = self.process_event_receiver.as_mut().ok_or_else(|| {
            SidecarError::InvalidState(String::from("process event receiver unavailable"))
        })?;
        let mut matching_envelope = None;
        while let Ok(envelope) = receiver.try_recv() {
            if matching_envelope.is_none()
                && envelope.vm_id == vm_id
                && envelope.process_id == process_id
            {
                matching_envelope = Some(envelope);
                break;
            }
            self.pending_process_events.push_back(envelope);
        }

        Ok(matching_envelope)
    }

    fn handle_acp_process_event(
        &mut self,
        vm_id: &str,
        process_id: &str,
        session_id: Option<&str>,
        ownership: &OwnershipScope,
        event: ActiveExecutionEvent,
        events: &mut Vec<EventFrame>,
    ) -> Result<Option<JsonRpcResponse>, SidecarError> {
        match event {
            ActiveExecutionEvent::Stdout(chunk) => {
                let mut matched_response = None;
                let chunk = String::from_utf8_lossy(&chunk);
                let buffer = if let Some(session_id) = session_id {
                    self.acp_sessions
                        .get_mut(session_id)
                        .map(|session| {
                            session.stdout_buffer.push_str(&chunk);
                            std::mem::take(&mut session.stdout_buffer)
                        })
                        .unwrap_or_else(|| chunk.into_owned())
                } else {
                    let buffer = self
                        .acp_process_stdout_buffers
                        .entry(String::from(process_id))
                        .or_default();
                    buffer.push_str(&chunk);
                    std::mem::take(buffer)
                };
                let mut pending = buffer;
                while let Some(index) = pending.find('\n') {
                    let line = pending[..index].trim().to_owned();
                    pending = pending[index + 1..].to_owned();
                    if line.is_empty() {
                        continue;
                    }
                    let Some(message) = deserialize_message(&line) else {
                        if let Some(session_id) = session_id {
                            if let Some(session) = self.acp_sessions.get_mut(session_id) {
                                session.record_activity(format!("non_json {}", line));
                            }
                        }
                        continue;
                    };
                    match message {
                        JsonRpcMessage::Response(response) => {
                            if let Some(session_id) = session_id {
                                if let Some(session) = self.acp_sessions.get_mut(session_id) {
                                    session.record_activity(summarize_inbound_response(&response));
                                }
                            }
                            matched_response = Some(response);
                        }
                        JsonRpcMessage::Notification(notification) => {
                            if let Some(session_id) = session_id {
                                let sequence_number = {
                                    let session = self
                                        .acp_sessions
                                        .get_mut(session_id)
                                        .expect("ACP session should exist");
                                    session.record_activity(summarize_inbound_notification(
                                        &notification,
                                    ));
                                    let sequence_number = session.next_sequence_number;
                                    session.record_notification(notification.clone());
                                    sequence_number
                                };
                                events.push(self.build_acp_event_frame(
                                    ownership,
                                    session_id,
                                    sequence_number,
                                    &notification,
                                )?);
                            }
                        }
                        JsonRpcMessage::Request(request) => {
                            if let Some(session_id) = session_id {
                                let (normalized, duplicate) = {
                                    let session = self
                                        .acp_sessions
                                        .get_mut(session_id)
                                        .expect("ACP session should exist");
                                    session.record_activity(summarize_inbound_request(&request));
                                    let duplicate =
                                        session.seen_inbound_request_ids.contains(&request.id);
                                    let normalized = normalize_inbound_permission_request(
                                        &request,
                                        &mut session.seen_inbound_request_ids,
                                        &mut session.pending_permission_requests,
                                    );
                                    if normalized.is_none() && !duplicate {
                                        session.seen_inbound_request_ids.insert(request.id.clone());
                                    }
                                    (normalized, duplicate)
                                };
                                if let Some(notification) = normalized {
                                    let notification_params: Map<String, Value> =
                                        to_record(notification.params.clone());
                                    let permission_id =
                                        match notification_params.get("permissionId") {
                                            Some(Value::String(value)) => Some(value.clone()),
                                            Some(Value::Number(value)) => Some(value.to_string()),
                                            _ => None,
                                        };
                                    if let Some(permission_id) = permission_id {
                                        let sidecar_response = self.sidecar_requests.invoke(
                                            ownership.clone(),
                                            SidecarRequestPayload::PermissionRequest(
                                                SidecarPermissionRequest {
                                                    session_id: session_id.to_string(),
                                                    permission_id: permission_id.clone(),
                                                    params: Value::Object(
                                                        notification_params.clone(),
                                                    ),
                                                },
                                            ),
                                            Duration::from_millis(120_000),
                                        )?;
                                        let reply = match sidecar_response {
                                            SidecarResponsePayload::PermissionRequestResult(
                                                result,
                                            ) => result
                                                .reply
                                                .unwrap_or_else(|| String::from("reject")),
                                            other => {
                                                return Err(SidecarError::InvalidState(format!(
                                                    "unexpected sidecar permission response: {other:?}",
                                                )));
                                            }
                                        };
                                        let normalized_response = {
                                            let session = self
                                                .acp_sessions
                                                .get_mut(session_id)
                                                .expect("ACP session should exist");
                                            maybe_normalize_permission_response(
                                                LEGACY_PERMISSION_METHOD,
                                                Some(json!({
                                                    "permissionId": permission_id,
                                                    "reply": reply,
                                                })),
                                                &mut session.pending_permission_requests,
                                            )
                                        };
                                        if let Some((response_id, result)) = normalized_response {
                                            self.write_json_rpc_message(
                                                vm_id,
                                                process_id,
                                                JsonRpcMessage::Response(JsonRpcResponse {
                                                    jsonrpc: String::from("2.0"),
                                                    id: response_id,
                                                    result: Some(result),
                                                    error: None,
                                                }),
                                            )?;
                                        }
                                        continue;
                                    }

                                    let sequence_number = {
                                        let session = self
                                            .acp_sessions
                                            .get_mut(session_id)
                                            .expect("ACP session should exist");
                                        let sequence_number = session.next_sequence_number;
                                        session.record_notification(notification.clone());
                                        sequence_number
                                    };
                                    events.push(self.build_acp_event_frame(
                                        ownership,
                                        session_id,
                                        sequence_number,
                                        &notification,
                                    )?);
                                } else if !duplicate {
                                    let response = match self
                                        .handle_inbound_acp_request(session_id, &request)
                                    {
                                        Ok(Some(result)) => JsonRpcResponse {
                                            jsonrpc: String::from("2.0"),
                                            id: request.id,
                                            result: Some(result),
                                            error: None,
                                        },
                                        Ok(None) => JsonRpcResponse {
                                            jsonrpc: String::from("2.0"),
                                            id: request.id,
                                            result: None,
                                            error: Some(JsonRpcError {
                                                code: -32601,
                                                message: format!(
                                                    "Method not found: {}",
                                                    request.method
                                                ),
                                                data: None,
                                            }),
                                        },
                                        Err(error) => JsonRpcResponse {
                                            jsonrpc: String::from("2.0"),
                                            id: request.id,
                                            result: None,
                                            error: Some(JsonRpcError {
                                                code: -32000,
                                                message: error.to_string(),
                                                data: None,
                                            }),
                                        },
                                    };
                                    self.write_json_rpc_message(
                                        vm_id,
                                        process_id,
                                        JsonRpcMessage::Response(response),
                                    )?;
                                }
                            }
                        }
                    }
                }
                if let Some(session_id) = session_id {
                    if let Some(session) = self.acp_sessions.get_mut(session_id) {
                        session.stdout_buffer = pending;
                    }
                } else {
                    self.acp_process_stdout_buffers
                        .insert(String::from(process_id), pending);
                }
                Ok(matched_response)
            }
            ActiveExecutionEvent::Stderr(chunk) => {
                if let Some(session_id) = session_id {
                    if let Some(session) = self.acp_sessions.get_mut(session_id) {
                        session
                            .record_activity(format!("stderr {}", String::from_utf8_lossy(&chunk)));
                    }
                }
                Ok(None)
            }
            ActiveExecutionEvent::JavascriptSyncRpcRequest(request) => {
                self.handle_javascript_sync_rpc_request(vm_id, process_id, request)?;
                Ok(None)
            }
            ActiveExecutionEvent::PythonVfsRpcRequest(request) => {
                self.handle_python_vfs_rpc_request(vm_id, process_id, request)?;
                Ok(None)
            }
            ActiveExecutionEvent::SignalState {
                signal,
                registration,
            } => {
                let vm = self.vms.get_mut(vm_id).ok_or_else(|| {
                    SidecarError::InvalidState(format!("unknown sidecar VM {vm_id}"))
                })?;
                vm.signal_states
                    .entry(String::from(process_id))
                    .or_default()
                    .insert(signal, registration);
                Ok(None)
            }
            ActiveExecutionEvent::Exited(exit_code) => {
                if let Some(session_id) = session_id {
                    if let Some(session) = self.acp_sessions.get_mut(session_id) {
                        session.closed = true;
                        session.exit_code = Some(exit_code);
                    }
                }
                Ok(None)
            }
        }
    }

    fn allocate_sidecar_request_id(&mut self) -> RequestId {
        let request_id = self.next_sidecar_request_id;
        self.next_sidecar_request_id -= 1;
        request_id
    }

    pub(crate) fn session_scope_for(
        &self,
        ownership: &OwnershipScope,
    ) -> Result<(String, String), SidecarError> {
        match ownership {
            OwnershipScope::Session {
                connection_id,
                session_id,
            } => Ok((connection_id.clone(), session_id.clone())),
            OwnershipScope::Connection { .. } | OwnershipScope::Vm { .. } => {
                Err(SidecarError::InvalidState(String::from(
                    "request requires session ownership scope",
                )))
            }
        }
    }

    pub(crate) fn vm_scope_for(
        &self,
        ownership: &OwnershipScope,
    ) -> Result<(String, String, String), SidecarError> {
        match ownership {
            OwnershipScope::Vm {
                connection_id,
                session_id,
                vm_id,
            } => Ok((connection_id.clone(), session_id.clone(), vm_id.clone())),
            OwnershipScope::Connection { .. } | OwnershipScope::Session { .. } => Err(
                SidecarError::InvalidState(String::from("request requires VM ownership scope")),
            ),
        }
    }

    fn response_with_ownership(
        &self,
        request_id: RequestId,
        ownership: OwnershipScope,
        payload: ResponsePayload,
    ) -> ResponseFrame {
        ResponseFrame {
            schema: ProtocolSchema::current(),
            request_id,
            ownership,
            payload,
        }
    }

    pub(crate) fn respond(
        &self,
        request: &RequestFrame,
        payload: ResponsePayload,
    ) -> ResponseFrame {
        self.response_with_ownership(request.request_id, request.ownership.clone(), payload)
    }

    fn reject(&self, request: &RequestFrame, code: &str, message: &str) -> ResponseFrame {
        self.respond(
            request,
            ResponsePayload::Rejected(RejectedResponse {
                code: code.to_owned(),
                message: message.to_owned(),
            }),
        )
    }

    pub fn queue_sidecar_request(
        &mut self,
        ownership: OwnershipScope,
        payload: SidecarRequestPayload,
    ) -> Result<RequestId, SidecarError> {
        let request_id = self.allocate_sidecar_request_id();
        let request = SidecarRequestFrame::new(request_id, ownership, payload);
        self.pending_sidecar_responses
            .register_request(&request)
            .map_err(sidecar_response_tracker_error)?;
        self.outbound_sidecar_requests.push_back(request);
        Ok(request_id)
    }

    pub fn pop_sidecar_request(&mut self) -> Option<SidecarRequestFrame> {
        self.outbound_sidecar_requests.pop_front()
    }

    pub fn accept_sidecar_response(
        &mut self,
        response: SidecarResponseFrame,
    ) -> Result<(), SidecarError> {
        self.pending_sidecar_responses
            .accept_response(&response)
            .map_err(sidecar_response_tracker_error)?;
        self.completed_sidecar_responses
            .insert(response.request_id, response);
        Ok(())
    }

    pub fn take_sidecar_response(&mut self, request_id: RequestId) -> Option<SidecarResponseFrame> {
        self.completed_sidecar_responses.remove(&request_id)
    }

    pub(crate) fn vm_lifecycle_event(
        &self,
        connection_id: &str,
        session_id: &str,
        vm_id: &str,
        state: VmLifecycleState,
    ) -> EventFrame {
        EventFrame::new(
            OwnershipScope::vm(connection_id, session_id, vm_id),
            EventPayload::VmLifecycle(VmLifecycleEvent { state }),
        )
    }

    fn ensure_request_within_frame_limit(
        &self,
        request: &RequestFrame,
    ) -> Result<(), SidecarError> {
        let frame = crate::protocol::ProtocolFrame::Request(request.clone());
        let size = serde_json::to_vec(&frame)
            .map_err(|error| {
                SidecarError::InvalidState(format!("failed to serialize request frame: {error}"))
            })?
            .len();

        if size > self.config.max_frame_bytes {
            return Err(SidecarError::FrameTooLarge(format!(
                "request frame is {size} bytes, limit is {}",
                self.config.max_frame_bytes
            )));
        }

        Ok(())
    }
}

fn shadow_host_path_for_process(
    shadow_root: &Path,
    process_guest_cwd: &str,
    guest_path: &str,
) -> PathBuf {
    let normalized_guest_path = if guest_path.starts_with('/') {
        normalize_path(guest_path)
    } else {
        normalize_path(&format!(
            "{}/{}",
            process_guest_cwd.trim_end_matches('/'),
            guest_path
        ))
    };
    if normalized_guest_path == "/" {
        shadow_root.to_path_buf()
    } else {
        shadow_root.join(normalized_guest_path.trim_start_matches('/'))
    }
}

fn sidecar_response_tracker_error(error: SidecarResponseTrackerError) -> SidecarError {
    SidecarError::InvalidState(format!(
        "invalid sidecar response correlation state: {error}"
    ))
}

fn map_bridge_permission(decision: agent_os_bridge::PermissionDecision) -> PermissionDecision {
    match decision.verdict {
        agent_os_bridge::PermissionVerdict::Allow => PermissionDecision::allow(),
        agent_os_bridge::PermissionVerdict::Deny => PermissionDecision::deny(
            decision
                .reason
                .unwrap_or_else(|| String::from("denied by host")),
        ),
        agent_os_bridge::PermissionVerdict::Prompt => PermissionDecision::deny(
            decision
                .reason
                .unwrap_or_else(|| String::from("permission prompt required")),
        ),
    }
}

fn audit_timestamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_millis()
        .to_string()
}

fn reap_runtime_child_if_exited(child_pid: u32) -> Result<(), SidecarError> {
    if child_pid == 0 {
        return Ok(());
    }

    let wait_flags = WaitPidFlag::WNOHANG
        | WaitPidFlag::WEXITED
        | WaitPidFlag::WUNTRACED
        | WaitPidFlag::WCONTINUED;
    match wait_on_child(WaitId::Pid(Pid::from_raw(child_pid as i32)), wait_flags) {
        Ok(WaitStatus::StillAlive)
        | Ok(WaitStatus::Stopped(_, _))
        | Ok(WaitStatus::Continued(_)) => Ok(()),
        Ok(WaitStatus::Exited(_, _)) | Ok(WaitStatus::Signaled(_, _, _)) => Ok(()),
        #[cfg(any(target_os = "linux", target_os = "android"))]
        Ok(WaitStatus::PtraceEvent(_, _, _) | WaitStatus::PtraceSyscall(_)) => Ok(()),
        Err(nix::errno::Errno::ECHILD) => Ok(()),
        Err(error) => Err(SidecarError::Execution(format!(
            "failed to reap guest runtime process {child_pid}: {error}"
        ))),
    }
}

pub(crate) fn audit_fields<I, K, V>(fields: I) -> BTreeMap<String, String>
where
    I: IntoIterator<Item = (K, V)>,
    K: Into<String>,
    V: Into<String>,
{
    let mut mapped = BTreeMap::from([(String::from("timestamp"), audit_timestamp())]);
    for (key, value) in fields {
        mapped.insert(key.into(), value.into());
    }
    mapped
}

pub(crate) fn emit_structured_event<B>(
    bridge: &SharedBridge<B>,
    vm_id: &str,
    name: &str,
    fields: BTreeMap<String, String>,
) -> Result<(), SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    bridge.with_mut(|bridge| {
        bridge.emit_structured_event(StructuredEventRecord {
            vm_id: vm_id.to_owned(),
            name: name.to_owned(),
            fields,
        })
    })
}

pub(crate) fn emit_security_audit_event<B>(
    bridge: &SharedBridge<B>,
    vm_id: &str,
    name: &str,
    fields: BTreeMap<String, String>,
) where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let _ = emit_structured_event(bridge, vm_id, name, fields);
}

// filesystem_operation_label moved to crate::vm

pub(crate) fn root_filesystem_error(error: impl std::fmt::Display) -> SidecarError {
    SidecarError::InvalidState(format!("root filesystem: {error}"))
}

pub(crate) fn normalize_path(path: &str) -> String {
    let mut segments = Vec::new();
    for component in Path::new(path).components() {
        match component {
            Component::RootDir => segments.clear(),
            Component::ParentDir => {
                segments.pop();
            }
            Component::CurDir => {}
            Component::Normal(value) => segments.push(value.to_string_lossy().into_owned()),
            Component::Prefix(prefix) => {
                segments.push(prefix.as_os_str().to_string_lossy().into_owned());
            }
        }
    }

    let normalized = format!("/{}", segments.join("/"));
    if normalized.is_empty() {
        String::from("/")
    } else {
        normalized
    }
}

pub(crate) fn normalize_host_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();

    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(Path::new("/")),
            Component::CurDir => {}
            Component::ParentDir => {
                if normalized != Path::new("/") {
                    normalized.pop();
                }
            }
            Component::Normal(part) => normalized.push(part),
        }
    }

    if normalized.as_os_str().is_empty() {
        if path.is_absolute() {
            PathBuf::from("/")
        } else {
            PathBuf::from(".")
        }
    } else {
        normalized
    }
}

pub(crate) fn path_is_within_root(path: &Path, root: &Path) -> bool {
    path == root || path.starts_with(root)
}

pub(crate) fn dirname(path: &str) -> String {
    let normalized = normalize_path(path);
    let parent = Path::new(&normalized)
        .parent()
        .unwrap_or_else(|| Path::new("/"));
    let value = parent.to_string_lossy();
    if value.is_empty() {
        String::from("/")
    } else {
        value.into_owned()
    }
}

pub(crate) fn kernel_error(error: KernelError) -> SidecarError {
    SidecarError::Kernel(error.to_string())
}

pub(crate) fn plugin_error(error: PluginError) -> SidecarError {
    SidecarError::Plugin(error.to_string())
}

pub(crate) fn javascript_error(error: JavascriptExecutionError) -> SidecarError {
    SidecarError::Execution(error.to_string())
}

pub(crate) fn wasm_error(error: WasmExecutionError) -> SidecarError {
    SidecarError::Execution(error.to_string())
}

pub(crate) fn python_error(error: PythonExecutionError) -> SidecarError {
    SidecarError::Execution(error.to_string())
}

pub(crate) fn vfs_error(error: VfsError) -> SidecarError {
    SidecarError::Kernel(error.to_string())
}
