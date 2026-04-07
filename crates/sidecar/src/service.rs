use crate::bridge::{build_mount_plugin_registry, MountPluginContext};
pub(crate) use crate::execution::{
    build_javascript_socket_path_context, error_code, format_dns_resource, format_tcp_resource,
    ignore_stale_javascript_sync_rpc_response, javascript_sync_rpc_arg_str,
    javascript_sync_rpc_arg_u32, javascript_sync_rpc_arg_u32_optional, javascript_sync_rpc_arg_u64,
    javascript_sync_rpc_arg_u64_optional, javascript_sync_rpc_bytes_arg,
    javascript_sync_rpc_bytes_value, javascript_sync_rpc_encoding, javascript_sync_rpc_error_code,
    javascript_sync_rpc_option_bool, javascript_sync_rpc_option_u32, parse_signal,
    runtime_child_is_alive, sanitize_javascript_child_process_internal_bootstrap_env,
    service_javascript_net_sync_rpc, service_javascript_sync_rpc, signal_runtime_process,
    vm_network_resource_counts,
};
use crate::filesystem::{
    guest_filesystem_call as filesystem_guest_filesystem_call,
    handle_python_vfs_rpc_request as filesystem_handle_python_vfs_rpc_request,
    service_javascript_fs_sync_rpc,
};
use crate::protocol::{
    AuthenticatedResponse, BoundUdpSnapshotResponse, CloseStdinRequest, DisposeReason, EventFrame,
    EventPayload, ExecuteRequest, FindBoundUdpRequest, FindListenerRequest, GetSignalStateRequest,
    GetZombieTimerCountRequest, GuestFilesystemCallRequest, GuestRuntimeKind,
    JavascriptChildProcessSpawnRequest, JavascriptDgramBindRequest,
    JavascriptDgramCreateSocketRequest, JavascriptDgramSendRequest, JavascriptDnsLookupRequest,
    JavascriptDnsResolveRequest, JavascriptNetConnectRequest, JavascriptNetListenRequest,
    KillProcessRequest, ListenerSnapshotResponse, OpenSessionRequest, OwnershipScope,
    ProcessExitedEvent, ProcessKilledResponse, ProcessOutputEvent, ProcessStartedResponse,
    ProtocolSchema, RejectedResponse, RequestFrame, RequestId, RequestPayload, ResponseFrame,
    ResponsePayload, SessionOpenedResponse, SidecarRequestFrame, SidecarRequestPayload,
    SidecarResponseFrame, SidecarResponseTracker, SidecarResponseTrackerError,
    SignalDispositionAction, SignalHandlerRegistration, SignalStateResponse, SocketStateEntry,
    StdinClosedResponse, StdinWrittenResponse, StreamChannel, VmLifecycleEvent, VmLifecycleState,
    WasmPermissionTier, WriteStdinRequest, ZombieTimerCountResponse,
};
use crate::state::{
    ActiveExecution, ActiveExecutionEvent, ActiveProcess, ActiveTcpListener, ActiveTcpSocket,
    ActiveUdpSocket, ActiveUnixListener, ActiveUnixSocket, BridgeError, ConnectionState,
    DnsResolutionSource, JavascriptSocketFamily, JavascriptSocketPathContext,
    JavascriptTcpListenerEvent, JavascriptTcpSocketEvent, JavascriptUdpFamily,
    JavascriptUdpSocketEvent, JavascriptUnixListenerEvent, NetworkResourceCounts, PendingTcpSocket,
    PendingUnixSocket, ProcNetEntry, ProcessEventEnvelope, ResolvedChildProcessExecution,
    ResolvedTcpConnectAddr, SessionState, SharedBridge, SidecarKernel, SocketQueryKind,
    VmDnsConfig, VmListenPolicy, VmState, DEFAULT_JAVASCRIPT_NET_BACKLOG, EXECUTION_DRIVER_NAME,
    EXECUTION_SANDBOX_ROOT_ENV, JAVASCRIPT_COMMAND, LOOPBACK_EXEMPT_PORTS_ENV, PYTHON_COMMAND,
    VM_LISTEN_ALLOW_PRIVILEGED_METADATA_KEY, VM_LISTEN_PORT_MAX_METADATA_KEY,
    VM_LISTEN_PORT_MIN_METADATA_KEY, WASM_COMMAND,
};
use crate::NativeSidecarBridge;
use agent_os_bridge::{
    CommandPermissionRequest, EnvironmentAccess, EnvironmentPermissionRequest, FilesystemAccess,
    FilesystemPermissionRequest, LifecycleEventRecord, LifecycleState, LogLevel, LogRecord,
    NetworkAccess, NetworkPermissionRequest, StructuredEventRecord,
};
use agent_os_execution::wasm::{
    WASM_MAX_FUEL_ENV, WASM_MAX_MEMORY_BYTES_ENV, WASM_MAX_STACK_BYTES_ENV,
};
use agent_os_execution::{
    CreateJavascriptContextRequest, CreatePythonContextRequest, CreateWasmContextRequest,
    JavascriptExecutionEngine, JavascriptExecutionError, JavascriptExecutionEvent,
    JavascriptSyncRpcRequest, NodeSignalDispositionAction, NodeSignalHandlerRegistration,
    PythonExecutionEngine, PythonExecutionError, PythonExecutionEvent, PythonVfsRpcRequest,
    PythonVfsRpcResponsePayload, StartJavascriptExecutionRequest, StartPythonExecutionRequest,
    StartWasmExecutionRequest, WasmExecutionEngine, WasmExecutionError, WasmExecutionEvent,
    WasmPermissionTier as ExecutionWasmPermissionTier,
};
use agent_os_kernel::kernel::{KernelError, KernelProcessHandle, SpawnOptions};
use agent_os_kernel::mount_plugin::{FileSystemPluginRegistry, PluginError};
use agent_os_kernel::permissions::{
    CommandAccessRequest, EnvAccessRequest, EnvironmentOperation, NetworkAccessRequest,
    NetworkOperation, PermissionDecision,
};
use agent_os_kernel::process_table::{SIGKILL, SIGTERM};
use agent_os_kernel::resource_accounting::ResourceLimits;
// root_fs types moved to crate::vm
use agent_os_kernel::vfs::VfsError;
use base64::Engine;
use hickory_resolver::config::{NameServerConfig, ResolverConfig};
use hickory_resolver::net::runtime::TokioRuntimeProvider;
use hickory_resolver::TokioResolver;
use nix::libc;
use nix::sys::signal::{kill as send_signal, Signal};
use nix::sys::wait::{waitid as wait_on_child, Id as WaitId, WaitPidFlag, WaitStatus};
use nix::unistd::Pid;
use serde::Deserialize;
use serde_json::json;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::fs;
use std::io::{Read, Write};
use std::net::{
    IpAddr, Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs,
    UdpSocket,
};
use std::os::unix::net::{SocketAddr as UnixSocketAddr, UnixListener, UnixStream};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Wake, Waker};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::time;

// Constants and type aliases moved to crate::state

// NativeSidecarConfig, DispatchResult, SidecarError moved to crate::state
pub use crate::state::{DispatchResult, NativeSidecarConfig, SidecarError};

// SharedBridge struct and Clone impl moved to crate::state

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
        if let Some(decision) =
            self.static_permission_decision(vm_id, filesystem_permission_capability(access), "fs")
        {
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
        if let Some(decision) =
            self.static_permission_decision(vm_id, "child_process.spawn", "child_process")
        {
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
        permissions: &[crate::protocol::PermissionDescriptor],
    ) -> Result<(), SidecarError> {
        let mut stored = self.permissions.lock().map_err(|_| {
            SidecarError::Bridge(String::from(
                "native sidecar permission policy lock poisoned",
            ))
        })?;
        stored.insert(
            vm_id.to_owned(),
            normalize_permission_descriptors(permissions),
        );
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
    ) -> Option<PermissionDecision> {
        let stored = self.permissions.lock().ok()?;
        let permissions = stored.get(vm_id)?;
        let mode = permissions
            .get(capability)
            .or_else(|| permissions.get(domain))
            .cloned()
            .unwrap_or(crate::protocol::PermissionMode::Deny);
        Some(permission_mode_to_kernel_decision(mode, capability))
    }
}

fn default_allow_all_permissions() -> BTreeMap<String, crate::protocol::PermissionMode> {
    BTreeMap::from([
        (String::from("fs"), crate::protocol::PermissionMode::Allow),
        (
            String::from("network"),
            crate::protocol::PermissionMode::Allow,
        ),
        (
            String::from("child_process"),
            crate::protocol::PermissionMode::Allow,
        ),
        (String::from("env"), crate::protocol::PermissionMode::Allow),
    ])
}

fn normalize_permission_descriptors(
    permissions: &[crate::protocol::PermissionDescriptor],
) -> BTreeMap<String, crate::protocol::PermissionMode> {
    if permissions.is_empty() {
        return default_allow_all_permissions();
    }

    let mut normalized = BTreeMap::new();
    for permission in permissions {
        normalized.insert(permission.capability.clone(), permission.mode.clone());
    }
    normalized
}

fn permission_mode_to_kernel_decision(
    mode: crate::protocol::PermissionMode,
    capability: &str,
) -> PermissionDecision {
    match mode {
        crate::protocol::PermissionMode::Allow => PermissionDecision::allow(),
        crate::protocol::PermissionMode::Ask => {
            PermissionDecision::deny(format!("permission prompt required for {capability}"))
        }
        crate::protocol::PermissionMode::Deny => {
            PermissionDecision::deny(format!("blocked by {capability} policy"))
        }
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
    pub(crate) next_sidecar_request_id: RequestId,
    pub(crate) connections: BTreeMap<String, ConnectionState>,
    pub(crate) sessions: BTreeMap<String, SessionState>,
    pub(crate) vms: BTreeMap<String, VmState>,
    pub(crate) process_event_sender: UnboundedSender<ProcessEventEnvelope>,
    pub(crate) process_event_receiver: Option<UnboundedReceiver<ProcessEventEnvelope>>,
    pub(crate) pending_process_events: VecDeque<ProcessEventEnvelope>,
    pub(crate) pending_sidecar_responses: SidecarResponseTracker,
    pub(crate) outbound_sidecar_requests: VecDeque<SidecarRequestFrame>,
    pub(crate) completed_sidecar_responses: BTreeMap<RequestId, SidecarResponseFrame>,
}

impl<B> fmt::Debug for NativeSidecar<B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NativeSidecar")
            .field("config", &self.config)
            .field("cache_root", &self.cache_root)
            .field("next_connection_id", &self.next_connection_id)
            .field("next_session_id", &self.next_session_id)
            .field("next_vm_id", &self.next_vm_id)
            .field("connection_count", &self.connections.len())
            .field("session_count", &self.sessions.len())
            .field("vm_count", &self.vms.len())
            .finish()
    }
}

impl<B> NativeSidecar<B>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
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
            next_sidecar_request_id: -1,
            connections: BTreeMap::new(),
            sessions: BTreeMap::new(),
            vms: BTreeMap::new(),
            process_event_sender,
            process_event_receiver: Some(process_event_receiver),
            pending_process_events: VecDeque::new(),
            pending_sidecar_responses: SidecarResponseTracker::default(),
            outbound_sidecar_requests: VecDeque::new(),
            completed_sidecar_responses: BTreeMap::new(),
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
            RequestPayload::DisposeVm(payload) => self.dispose_vm(&request, payload).await,
            RequestPayload::BootstrapRootFilesystem(payload) => {
                self.bootstrap_root_filesystem(&request, payload.entries)
                    .await
            }
            RequestPayload::ConfigureVm(payload) => self.configure_vm(&request, payload).await,
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
        self.handle_execution_event(&envelope.vm_id, &envelope.process_id, envelope.event)
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
                let payload = request
                    .args
                    .first()
                    .cloned()
                    .ok_or_else(|| {
                        SidecarError::InvalidState(String::from(
                            "child_process.spawn requires a request payload",
                        ))
                    })
                    .and_then(|value| {
                        serde_json::from_value::<JavascriptChildProcessSpawnRequest>(value).map_err(
                            |error| {
                                SidecarError::InvalidState(format!(
                                    "invalid child_process.spawn payload: {error}"
                                ))
                            },
                        )
                    })?;
                self.spawn_javascript_child_process(vm_id, process_id, payload)
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
            _ => {
                let vm = self.vms.get_mut(vm_id).expect("VM should exist");
                let resource_limits = vm.kernel.resource_limits().clone();
                let network_counts = vm_network_resource_counts(vm);
                let socket_paths = build_javascript_socket_path_context(vm)?;
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
                )
            }
        };

        let vm = self.vms.get_mut(vm_id).expect("VM should exist");
        let process = vm
            .active_processes
            .get_mut(process_id)
            .expect("process should still exist");

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
