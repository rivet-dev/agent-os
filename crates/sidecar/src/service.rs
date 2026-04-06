use crate::NativeSidecarBridge;
use crate::google_drive_plugin::GoogleDriveMountPlugin;
use crate::host_dir_plugin::HostDirMountPlugin;
use crate::protocol::{
    AuthenticatedResponse, BoundUdpSnapshotResponse, CloseStdinRequest, ConfigureVmRequest,
    DEFAULT_MAX_FRAME_BYTES, DisposeReason, DisposeVmRequest, EventFrame, EventPayload,
    ExecuteRequest, FindBoundUdpRequest, FindListenerRequest, GetSignalStateRequest,
    GetZombieTimerCountRequest, GuestFilesystemCallRequest, GuestFilesystemOperation,
    GuestFilesystemResultResponse, GuestFilesystemStat, GuestRuntimeKind, KillProcessRequest,
    ListenerSnapshotResponse, OpenSessionRequest, OwnershipScope, ProcessExitedEvent,
    ProcessKilledResponse, ProcessOutputEvent, ProcessSnapshotEntry, ProcessSnapshotResponse,
    ProcessSnapshotStatus, ProcessStartedResponse, ProtocolSchema, RejectedResponse, RequestFrame,
    RequestPayload, ResponseFrame, ResponsePayload, RootFilesystemBootstrappedResponse,
    RootFilesystemDescriptor, RootFilesystemEntry, RootFilesystemEntryEncoding,
    RootFilesystemEntryKind, RootFilesystemLowerDescriptor, RootFilesystemMode,
    RootFilesystemSnapshotResponse, SessionOpenedResponse, SidecarPlacement,
    SignalDispositionAction, SignalHandlerRegistration, SignalStateResponse,
    SnapshotRootFilesystemRequest, SocketStateEntry, StdinClosedResponse, StdinWrittenResponse,
    StreamChannel, VmConfiguredResponse, VmCreatedResponse, VmDisposedResponse, VmLifecycleEvent,
    VmLifecycleState, WasmPermissionTier, WriteStdinRequest, ZombieTimerCountResponse,
};
use crate::s3_plugin::S3MountPlugin;
use crate::sandbox_agent_plugin::SandboxAgentMountPlugin;
use agent_os_bridge::{
    BridgeTypes, ChmodRequest, CommandPermissionRequest, CreateDirRequest, EnvironmentAccess,
    EnvironmentPermissionRequest, FileKind, FileMetadata, FilesystemAccess,
    FilesystemPermissionRequest, FilesystemSnapshot, FlushFilesystemStateRequest,
    LifecycleEventRecord, LifecycleState, LoadFilesystemStateRequest, LogLevel, LogRecord,
    NetworkAccess, NetworkPermissionRequest, PathRequest, ReadDirRequest, ReadFileRequest,
    RenameRequest, StructuredEventRecord, SymlinkRequest, TruncateRequest, WriteFileRequest,
};
use agent_os_execution::wasm::{
    WASM_MAX_FUEL_ENV, WASM_MAX_MEMORY_BYTES_ENV, WASM_MAX_STACK_BYTES_ENV,
};
use agent_os_execution::{
    CreateJavascriptContextRequest, CreatePythonContextRequest, CreateWasmContextRequest,
    JavascriptExecution, JavascriptExecutionEngine, JavascriptExecutionError,
    JavascriptExecutionEvent, JavascriptSyncRpcRequest, NodeSignalDispositionAction,
    NodeSignalHandlerRegistration, PythonExecution, PythonExecutionEngine, PythonExecutionError,
    PythonExecutionEvent, PythonVfsRpcMethod, PythonVfsRpcRequest, PythonVfsRpcResponsePayload,
    PythonVfsRpcStat, StartJavascriptExecutionRequest, StartPythonExecutionRequest,
    StartWasmExecutionRequest, WasmExecution, WasmExecutionEngine, WasmExecutionError,
    WasmExecutionEvent, WasmPermissionTier as ExecutionWasmPermissionTier,
};
use agent_os_kernel::command_registry::CommandDriver;
use agent_os_kernel::kernel::{
    KernelError, KernelProcessHandle, KernelVm, KernelVmConfig, SpawnOptions,
};
use agent_os_kernel::mount_plugin::{
    FileSystemPluginFactory, FileSystemPluginRegistry, OpenFileSystemPluginRequest, PluginError,
};
use agent_os_kernel::mount_table::{MountOptions, MountTable, MountedVirtualFileSystem};
use agent_os_kernel::permissions::{
    CommandAccessRequest, EnvAccessRequest, EnvironmentOperation, FsAccessRequest, FsOperation,
    NetworkAccessRequest, NetworkOperation, PermissionDecision, Permissions, filter_env,
};
use agent_os_kernel::process_table::{SIGKILL, SIGTERM};
use agent_os_kernel::resource_accounting::ResourceLimits;
use agent_os_kernel::root_fs::{
    FilesystemEntry as KernelFilesystemEntry, FilesystemEntryKind as KernelFilesystemEntryKind,
    ROOT_FILESYSTEM_SNAPSHOT_FORMAT, RootFileSystem,
    RootFilesystemDescriptor as KernelRootFilesystemDescriptor,
    RootFilesystemMode as KernelRootFilesystemMode, RootFilesystemSnapshot,
    decode_snapshot as decode_root_snapshot, encode_snapshot as encode_root_snapshot,
};
use agent_os_kernel::vfs::{
    MemoryFileSystem, VfsError, VfsResult, VirtualDirEntry, VirtualFileSystem, VirtualStat,
};
use base64::Engine;
use hickory_resolver::TokioResolver;
use hickory_resolver::config::{NameServerConfig, ResolverConfig};
use hickory_resolver::net::runtime::TokioRuntimeProvider;
use nix::libc;
use nix::sys::signal::{Signal, kill as send_signal};
use nix::sys::wait::{Id as WaitId, WaitPidFlag, WaitStatus, waitid as wait_on_child};
use nix::unistd::Pid;
use serde::Deserialize;
use serde_json::Value;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
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
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const EXECUTION_DRIVER_NAME: &str = "agent-os-sidecar-execution";
const JAVASCRIPT_COMMAND: &str = "node";
const PYTHON_COMMAND: &str = "python";
const WASM_COMMAND: &str = "wasm";
const PYTHON_VFS_RPC_GUEST_ROOT: &str = "/workspace";
const EXECUTION_SANDBOX_ROOT_ENV: &str = "AGENT_OS_SANDBOX_ROOT";
const HOST_REALPATH_MAX_SYMLINK_DEPTH: usize = 40;
const DISPOSE_VM_SIGTERM_GRACE: Duration = Duration::from_millis(100);
const DISPOSE_VM_SIGKILL_GRACE: Duration = Duration::from_millis(100);
const VM_DNS_SERVERS_METADATA_KEY: &str = "network.dns.servers";
const VM_DNS_OVERRIDE_METADATA_PREFIX: &str = "network.dns.override.";
const VM_LISTEN_PORT_MIN_METADATA_KEY: &str = "network.listen.port_min";
const VM_LISTEN_PORT_MAX_METADATA_KEY: &str = "network.listen.port_max";
const VM_LISTEN_ALLOW_PRIVILEGED_METADATA_KEY: &str = "network.listen.allow_privileged";
const DEFAULT_JAVASCRIPT_NET_BACKLOG: u32 = 511;
const LOOPBACK_EXEMPT_PORTS_ENV: &str = "AGENT_OS_LOOPBACK_EXEMPT_PORTS";

type BridgeError<B> = <B as BridgeTypes>::Error;
type SidecarKernel = KernelVm<MountTable>;

#[derive(Debug, Clone)]
pub struct NativeSidecarConfig {
    pub sidecar_id: String,
    pub max_frame_bytes: usize,
    pub compile_cache_root: Option<PathBuf>,
    pub expected_auth_token: Option<String>,
}

impl Default for NativeSidecarConfig {
    fn default() -> Self {
        Self {
            sidecar_id: String::from("agent-os-sidecar"),
            max_frame_bytes: DEFAULT_MAX_FRAME_BYTES,
            compile_cache_root: None,
            expected_auth_token: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DispatchResult {
    pub response: ResponseFrame,
    pub events: Vec<EventFrame>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidecarError {
    InvalidState(String),
    Unauthorized(String),
    Unsupported(String),
    FrameTooLarge(String),
    Kernel(String),
    Plugin(String),
    Execution(String),
    Bridge(String),
    Io(String),
}

impl fmt::Display for SidecarError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidState(message)
            | Self::Unauthorized(message)
            | Self::Unsupported(message)
            | Self::FrameTooLarge(message)
            | Self::Kernel(message)
            | Self::Plugin(message)
            | Self::Execution(message)
            | Self::Bridge(message)
            | Self::Io(message) => f.write_str(message),
        }
    }
}

impl Error for SidecarError {}

struct SharedBridge<B> {
    inner: Arc<Mutex<B>>,
    permissions: Arc<Mutex<BTreeMap<String, BTreeMap<String, crate::protocol::PermissionMode>>>>,
}

impl<B> SharedBridge<B> {
    fn new(bridge: B) -> Self {
        Self {
            inner: Arc::new(Mutex::new(bridge)),
            permissions: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }
}

impl<B> Clone for SharedBridge<B> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            permissions: Arc::clone(&self.permissions),
        }
    }
}

impl<B> SharedBridge<B>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    fn with_mut<T>(
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

    fn emit_lifecycle(&self, vm_id: &str, state: LifecycleState) -> Result<(), SidecarError> {
        self.with_mut(|bridge| {
            bridge.emit_lifecycle(LifecycleEventRecord {
                vm_id: vm_id.to_owned(),
                state,
                detail: None,
            })
        })
    }

    fn emit_log(&self, vm_id: &str, message: impl Into<String>) -> Result<(), SidecarError> {
        self.with_mut(|bridge| {
            bridge.emit_log(LogRecord {
                vm_id: vm_id.to_owned(),
                level: LogLevel::Info,
                message: message.into(),
            })
        })
    }

    fn filesystem_decision(
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

    fn command_decision(&self, vm_id: &str, request: &CommandAccessRequest) -> PermissionDecision {
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

    fn environment_decision(&self, vm_id: &str, request: &EnvAccessRequest) -> PermissionDecision {
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

    fn network_decision(&self, vm_id: &str, request: &NetworkAccessRequest) -> PermissionDecision {
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

    fn require_network_access(
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

    fn require_command_access(
        &self,
        vm_id: &str,
        request: CommandAccessRequest,
    ) -> Result<(), SidecarError> {
        let static_decision =
            self.static_permission_decision(vm_id, "child_process.spawn", "child_process");
        if let Some(decision) = static_decision.as_ref() {
            if !decision.allow {
                let message = match decision.reason.as_deref() {
                    Some(reason) => {
                        format!("EACCES: permission denied, {}: {reason}", request.command)
                    }
                    None => format!("EACCES: permission denied, {}", request.command),
                };
                return Err(SidecarError::Execution(message));
            }
        }

        let bridge_decision = match self.with_mut(|bridge| {
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
        };
        if bridge_decision.allow {
            return Ok(());
        }
        if static_decision
            .as_ref()
            .is_some_and(|decision| decision.allow)
            && bridge_decision.reason.as_deref().is_some_and(|reason| {
                reason.starts_with("no static child_process policy registered for ")
            })
        {
            return Ok(());
        }

        let message = match bridge_decision.reason.as_deref() {
            Some(reason) => format!("EACCES: permission denied, {}: {reason}", request.command),
            None => format!("EACCES: permission denied, {}", request.command),
        };
        Err(SidecarError::Execution(message))
    }

    fn set_vm_permissions(
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

    fn clear_vm_permissions(&self, vm_id: &str) -> Result<(), SidecarError> {
        let mut stored = self.permissions.lock().map_err(|_| {
            SidecarError::Bridge(String::from(
                "native sidecar permission policy lock poisoned",
            ))
        })?;
        stored.remove(vm_id);
        Ok(())
    }

    fn static_permission_decision(
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

fn filesystem_permission_capability(access: FilesystemAccess) -> &'static str {
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

#[derive(Clone)]
struct HostFilesystem<B> {
    bridge: SharedBridge<B>,
    vm_id: String,
    links: Arc<Mutex<HostFilesystemLinkState>>,
}

#[derive(Debug, Clone, Default)]
struct HostFilesystemMetadataState {
    uid: Option<u32>,
    gid: Option<u32>,
    atime_ms: Option<u64>,
    mtime_ms: Option<u64>,
    ctime_ms: Option<u64>,
    birthtime_ms: Option<u64>,
}

impl HostFilesystemMetadataState {
    fn apply_to_stat(&self, stat: &mut VirtualStat) {
        if let Some(uid) = self.uid {
            stat.uid = uid;
        }
        if let Some(gid) = self.gid {
            stat.gid = gid;
        }
        if let Some(atime_ms) = self.atime_ms {
            stat.atime_ms = atime_ms;
        }
        if let Some(mtime_ms) = self.mtime_ms {
            stat.mtime_ms = mtime_ms;
        }
        if let Some(ctime_ms) = self.ctime_ms {
            stat.ctime_ms = ctime_ms;
        }
        if let Some(birthtime_ms) = self.birthtime_ms {
            stat.birthtime_ms = birthtime_ms;
        }
    }
}

#[derive(Debug, Clone)]
struct HostFilesystemLinkedInode {
    canonical_path: String,
    paths: BTreeSet<String>,
    metadata: HostFilesystemMetadataState,
}

#[derive(Debug, Default)]
struct HostFilesystemLinkState {
    next_ino: u64,
    path_to_ino: BTreeMap<String, u64>,
    inodes: BTreeMap<u64, HostFilesystemLinkedInode>,
}

#[derive(Debug, Clone)]
struct HostFilesystemTrackedIdentity {
    canonical_path: String,
    ino: u64,
    nlink: u64,
    metadata: HostFilesystemMetadataState,
}

impl<B> HostFilesystem<B> {
    fn new(bridge: SharedBridge<B>, vm_id: impl Into<String>) -> Self {
        Self {
            bridge,
            vm_id: vm_id.into(),
            links: Arc::new(Mutex::new(HostFilesystemLinkState {
                next_ino: 1,
                ..HostFilesystemLinkState::default()
            })),
        }
    }

    fn vfs_error(error: SidecarError) -> VfsError {
        VfsError::io(error.to_string())
    }

    fn link_state_error() -> VfsError {
        VfsError::io("native sidecar host filesystem link state lock poisoned")
    }

    fn current_time_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }

    fn file_metadata_to_stat(
        metadata: FileMetadata,
        identity: Option<&HostFilesystemTrackedIdentity>,
    ) -> VirtualStat {
        let mut stat = VirtualStat {
            mode: metadata.mode,
            size: metadata.size,
            blocks: if metadata.size == 0 {
                0
            } else {
                metadata.size.div_ceil(512)
            },
            dev: 1,
            rdev: 0,
            is_directory: metadata.kind == FileKind::Directory,
            is_symbolic_link: metadata.kind == FileKind::SymbolicLink,
            atime_ms: 0,
            mtime_ms: 0,
            ctime_ms: 0,
            birthtime_ms: 0,
            ino: identity.map_or(0, |tracked| tracked.ino),
            nlink: identity.map_or(1, |tracked| tracked.nlink),
            uid: 0,
            gid: 0,
        };
        if let Some(identity) = identity {
            identity.metadata.apply_to_stat(&mut stat);
        }
        stat
    }

    fn tracked_identity(&self, path: &str) -> VfsResult<Option<HostFilesystemTrackedIdentity>> {
        let normalized = normalize_path(path);
        let links = self.links.lock().map_err(|_| Self::link_state_error())?;
        Ok(links.path_to_ino.get(&normalized).and_then(|ino| {
            links
                .inodes
                .get(ino)
                .map(|inode| HostFilesystemTrackedIdentity {
                    canonical_path: inode.canonical_path.clone(),
                    ino: *ino,
                    nlink: inode.paths.len() as u64,
                    metadata: inode.metadata.clone(),
                })
        }))
    }

    fn tracked_identity_for_stat(
        &self,
        path: &str,
    ) -> VfsResult<Option<HostFilesystemTrackedIdentity>>
    where
        B: NativeSidecarBridge + Send + 'static,
        BridgeError<B>: fmt::Debug + Send + Sync + 'static,
    {
        let normalized = normalize_path(path);
        if let Some(identity) = self.tracked_identity(&normalized)? {
            return Ok(Some(identity));
        }

        let resolved = self.realpath(&normalized)?;
        if resolved == normalized {
            return Ok(None);
        }

        self.tracked_identity(&resolved)
    }

    fn tracked_successor(&self, path: &str) -> VfsResult<Option<String>> {
        let normalized = normalize_path(path);
        let links = self.links.lock().map_err(|_| Self::link_state_error())?;
        Ok(links
            .path_to_ino
            .get(&normalized)
            .and_then(|ino| links.inodes.get(ino))
            .and_then(|inode| {
                inode
                    .paths
                    .iter()
                    .find(|candidate| **candidate != normalized)
                    .cloned()
            }))
    }

    fn ensure_tracked_path(&self, path: &str) -> VfsResult<u64> {
        let normalized = normalize_path(path);
        let mut links = self.links.lock().map_err(|_| Self::link_state_error())?;
        if let Some(ino) = links.path_to_ino.get(&normalized).copied() {
            return Ok(ino);
        }

        let ino = links.next_ino;
        links.next_ino += 1;
        links.path_to_ino.insert(normalized.clone(), ino);
        links.inodes.insert(
            ino,
            HostFilesystemLinkedInode {
                canonical_path: normalized.clone(),
                paths: BTreeSet::from([normalized]),
                metadata: HostFilesystemMetadataState::default(),
            },
        );
        Ok(ino)
    }

    fn track_link(&self, old_path: &str, new_path: &str) -> VfsResult<()> {
        let normalized_old = normalize_path(old_path);
        let normalized_new = normalize_path(new_path);
        let ino = self.ensure_tracked_path(&normalized_old)?;
        let mut links = self.links.lock().map_err(|_| Self::link_state_error())?;
        links.path_to_ino.insert(normalized_new.clone(), ino);
        links
            .inodes
            .get_mut(&ino)
            .expect("tracked inode should exist")
            .paths
            .insert(normalized_new);
        Ok(())
    }

    fn metadata_target_path(&self, path: &str) -> VfsResult<String>
    where
        B: NativeSidecarBridge + Send + 'static,
        BridgeError<B>: fmt::Debug + Send + Sync + 'static,
    {
        if let Some(identity) = self.tracked_identity(path)? {
            return Ok(identity.canonical_path);
        }

        let normalized = normalize_path(path);
        self.bridge
            .with_mut(|bridge| {
                bridge.stat(PathRequest {
                    vm_id: self.vm_id.clone(),
                    path: normalized.clone(),
                })
            })
            .map_err(Self::vfs_error)?;
        self.realpath(&normalized)
    }

    fn update_metadata(
        &self,
        path: &str,
        update: impl FnOnce(&mut HostFilesystemMetadataState),
    ) -> VfsResult<()>
    where
        B: NativeSidecarBridge + Send + 'static,
        BridgeError<B>: fmt::Debug + Send + Sync + 'static,
    {
        let target = self.metadata_target_path(path)?;
        let ino = self.ensure_tracked_path(&target)?;
        let mut links = self.links.lock().map_err(|_| Self::link_state_error())?;
        let inode = links
            .inodes
            .get_mut(&ino)
            .expect("tracked inode should exist");
        update(&mut inode.metadata);
        Ok(())
    }

    fn apply_remove(&self, path: &str) -> VfsResult<()> {
        let normalized = normalize_path(path);
        let mut links = self.links.lock().map_err(|_| Self::link_state_error())?;
        let Some(ino) = links.path_to_ino.remove(&normalized) else {
            return Ok(());
        };
        let remove_inode = {
            let inode = links
                .inodes
                .get_mut(&ino)
                .expect("tracked inode should exist");
            inode.paths.remove(&normalized);
            if inode.paths.is_empty() {
                true
            } else {
                if inode.canonical_path == normalized {
                    inode.canonical_path = inode
                        .paths
                        .iter()
                        .next()
                        .expect("tracked inode should retain at least one path")
                        .clone();
                }
                false
            }
        };
        if remove_inode {
            links.inodes.remove(&ino);
        }
        Ok(())
    }

    fn apply_rename(&self, old_path: &str, new_path: &str) -> VfsResult<()> {
        let normalized_old = normalize_path(old_path);
        let normalized_new = normalize_path(new_path);
        let mut links = self.links.lock().map_err(|_| Self::link_state_error())?;
        let Some(ino) = links.path_to_ino.remove(&normalized_old) else {
            return Ok(());
        };
        links.path_to_ino.insert(normalized_new.clone(), ino);
        let inode = links
            .inodes
            .get_mut(&ino)
            .expect("tracked inode should exist");
        inode.paths.remove(&normalized_old);
        inode.paths.insert(normalized_new.clone());
        if inode.canonical_path == normalized_old {
            inode.canonical_path = normalized_new;
        }
        Ok(())
    }

    fn apply_rename_prefix(&self, old_prefix: &str, new_prefix: &str) -> VfsResult<()> {
        let normalized_old = normalize_path(old_prefix);
        let normalized_new = normalize_path(new_prefix);
        let prefix = if normalized_old == "/" {
            String::from("/")
        } else {
            format!("{}/", normalized_old.trim_end_matches('/'))
        };

        let mut links = self.links.lock().map_err(|_| Self::link_state_error())?;
        let affected = links
            .path_to_ino
            .keys()
            .filter(|path| *path == &normalized_old || path.starts_with(&prefix))
            .cloned()
            .collect::<Vec<_>>();

        for old_path in affected {
            let suffix = old_path
                .strip_prefix(&normalized_old)
                .expect("tracked path should match renamed prefix");
            let new_path = if normalized_new == "/" {
                normalize_path(&format!("/{}", suffix.trim_start_matches('/')))
            } else if suffix.is_empty() {
                normalized_new.clone()
            } else {
                normalize_path(&format!(
                    "{}/{}",
                    normalized_new.trim_end_matches('/'),
                    suffix.trim_start_matches('/')
                ))
            };
            let ino = links
                .path_to_ino
                .remove(&old_path)
                .expect("tracked path should exist");
            links.path_to_ino.insert(new_path.clone(), ino);
            let inode = links
                .inodes
                .get_mut(&ino)
                .expect("tracked inode should exist");
            inode.paths.remove(&old_path);
            inode.paths.insert(new_path.clone());
            if inode.canonical_path == old_path {
                inode.canonical_path = new_path;
            }
        }
        Ok(())
    }
}

impl<B> VirtualFileSystem for HostFilesystem<B>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    fn read_file(&mut self, path: &str) -> VfsResult<Vec<u8>> {
        let normalized = self
            .tracked_identity(path)?
            .map(|identity| identity.canonical_path)
            .unwrap_or_else(|| normalize_path(path));
        self.bridge
            .with_mut(|bridge| {
                bridge.read_file(ReadFileRequest {
                    vm_id: self.vm_id.clone(),
                    path: normalized,
                })
            })
            .map_err(Self::vfs_error)
    }

    fn read_dir(&mut self, path: &str) -> VfsResult<Vec<String>> {
        let normalized = normalize_path(path);
        let mut entries = self
            .bridge
            .with_mut(|bridge| {
                bridge.read_dir(ReadDirRequest {
                    vm_id: self.vm_id.clone(),
                    path: normalized.clone(),
                })
            })
            .map_err(Self::vfs_error)?;
        let links = self.links.lock().map_err(|_| Self::link_state_error())?;
        for linked_path in links.path_to_ino.keys() {
            if dirname(linked_path) != normalized {
                continue;
            }
            let name = Path::new(linked_path)
                .file_name()
                .map(|value| value.to_string_lossy().into_owned())
                .unwrap_or_else(|| linked_path.trim_start_matches('/').to_owned());
            if entries.iter().all(|entry| entry.name != name) {
                entries.push(agent_os_bridge::DirectoryEntry {
                    name,
                    kind: FileKind::File,
                });
            }
        }
        Ok(entries.into_iter().map(|entry| entry.name).collect())
    }

    fn read_dir_with_types(&mut self, path: &str) -> VfsResult<Vec<VirtualDirEntry>> {
        let normalized = normalize_path(path);
        let mut entries = self
            .bridge
            .with_mut(|bridge| {
                bridge.read_dir(ReadDirRequest {
                    vm_id: self.vm_id.clone(),
                    path: normalized.clone(),
                })
            })
            .map_err(Self::vfs_error)?;
        let links = self.links.lock().map_err(|_| Self::link_state_error())?;
        for linked_path in links.path_to_ino.keys() {
            if dirname(linked_path) != normalized {
                continue;
            }
            let name = Path::new(linked_path)
                .file_name()
                .map(|value| value.to_string_lossy().into_owned())
                .unwrap_or_else(|| linked_path.trim_start_matches('/').to_owned());
            if entries.iter().all(|entry| entry.name != name) {
                entries.push(agent_os_bridge::DirectoryEntry {
                    name,
                    kind: FileKind::File,
                });
            }
        }
        Ok(entries
            .into_iter()
            .map(|entry| VirtualDirEntry {
                name: entry.name,
                is_directory: entry.kind == FileKind::Directory,
                is_symbolic_link: entry.kind == FileKind::SymbolicLink,
            })
            .collect())
    }

    fn write_file(&mut self, path: &str, content: impl Into<Vec<u8>>) -> VfsResult<()> {
        let normalized = self
            .tracked_identity(path)?
            .map(|identity| identity.canonical_path)
            .unwrap_or_else(|| normalize_path(path));
        self.bridge
            .with_mut(|bridge| {
                bridge.write_file(WriteFileRequest {
                    vm_id: self.vm_id.clone(),
                    path: normalized,
                    contents: content.into(),
                })
            })
            .map_err(Self::vfs_error)
    }

    fn create_dir(&mut self, path: &str) -> VfsResult<()> {
        let normalized = normalize_path(path);
        self.bridge
            .with_mut(|bridge| {
                bridge.create_dir(CreateDirRequest {
                    vm_id: self.vm_id.clone(),
                    path: normalized,
                    recursive: false,
                })
            })
            .map_err(Self::vfs_error)
    }

    fn mkdir(&mut self, path: &str, recursive: bool) -> VfsResult<()> {
        let normalized = normalize_path(path);
        self.bridge
            .with_mut(|bridge| {
                bridge.create_dir(CreateDirRequest {
                    vm_id: self.vm_id.clone(),
                    path: normalized,
                    recursive,
                })
            })
            .map_err(Self::vfs_error)
    }

    fn exists(&self, path: &str) -> bool {
        if self.tracked_identity(path).ok().flatten().is_some() {
            return true;
        }
        let normalized = normalize_path(path);
        self.bridge
            .with_mut(|bridge| {
                bridge.exists(PathRequest {
                    vm_id: self.vm_id.clone(),
                    path: normalized,
                })
            })
            .unwrap_or(false)
    }

    fn stat(&mut self, path: &str) -> VfsResult<VirtualStat> {
        let identity = self.tracked_identity_for_stat(path)?;
        let normalized = identity
            .as_ref()
            .map(|identity| identity.canonical_path.clone())
            .unwrap_or_else(|| normalize_path(path));
        let metadata = self
            .bridge
            .with_mut(|bridge| {
                bridge.stat(PathRequest {
                    vm_id: self.vm_id.clone(),
                    path: normalized,
                })
            })
            .map_err(Self::vfs_error)?;
        Ok(Self::file_metadata_to_stat(metadata, identity.as_ref()))
    }

    fn remove_file(&mut self, path: &str) -> VfsResult<()> {
        let normalized = normalize_path(path);
        if let Some(identity) = self.tracked_identity(&normalized)? {
            let canonical = identity.canonical_path;
            let nlink = identity.nlink;
            if canonical == normalized {
                if nlink > 1 {
                    let successor = self
                        .tracked_successor(&normalized)?
                        .expect("tracked inode should retain a successor path");
                    self.bridge
                        .with_mut(|bridge| {
                            bridge.rename(RenameRequest {
                                vm_id: self.vm_id.clone(),
                                from_path: canonical.clone(),
                                to_path: successor,
                            })
                        })
                        .map_err(Self::vfs_error)?;
                } else {
                    self.bridge
                        .with_mut(|bridge| {
                            bridge.remove_file(PathRequest {
                                vm_id: self.vm_id.clone(),
                                path: canonical,
                            })
                        })
                        .map_err(Self::vfs_error)?;
                }
            }
            self.apply_remove(&normalized)?;
            return Ok(());
        }

        self.bridge
            .with_mut(|bridge| {
                bridge.remove_file(PathRequest {
                    vm_id: self.vm_id.clone(),
                    path: normalized,
                })
            })
            .map_err(Self::vfs_error)
    }

    fn remove_dir(&mut self, path: &str) -> VfsResult<()> {
        let normalized = normalize_path(path);
        self.bridge
            .with_mut(|bridge| {
                bridge.remove_dir(PathRequest {
                    vm_id: self.vm_id.clone(),
                    path: normalized,
                })
            })
            .map_err(Self::vfs_error)
    }

    fn rename(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        let normalized_old = normalize_path(old_path);
        let normalized_new = normalize_path(new_path);
        let tracked = self.tracked_identity(&normalized_old)?;
        if let Some(identity) = tracked {
            let canonical = identity.canonical_path;
            if self.exists(&normalized_new) {
                return Err(VfsError::new(
                    "EEXIST",
                    format!("file already exists, rename '{new_path}'"),
                ));
            }
            if canonical == normalized_old {
                self.bridge
                    .with_mut(|bridge| {
                        bridge.rename(RenameRequest {
                            vm_id: self.vm_id.clone(),
                            from_path: canonical,
                            to_path: normalized_new.clone(),
                        })
                    })
                    .map_err(Self::vfs_error)?;
            }
            self.apply_rename(&normalized_old, &normalized_new)?;
            return Ok(());
        }

        let old_kind = self
            .bridge
            .with_mut(|bridge| {
                bridge.lstat(PathRequest {
                    vm_id: self.vm_id.clone(),
                    path: normalized_old.clone(),
                })
            })
            .ok()
            .map(|metadata| metadata.kind);
        self.bridge
            .with_mut(|bridge| {
                bridge.rename(RenameRequest {
                    vm_id: self.vm_id.clone(),
                    from_path: normalized_old.clone(),
                    to_path: normalized_new.clone(),
                })
            })
            .map_err(Self::vfs_error)?;
        if old_kind == Some(FileKind::Directory) {
            self.apply_rename_prefix(&normalized_old, &normalized_new)?;
        }
        Ok(())
    }

    fn realpath(&self, path: &str) -> VfsResult<String> {
        let original = normalize_path(path);
        let mut normalized = original.clone();

        for _ in 0..HOST_REALPATH_MAX_SYMLINK_DEPTH {
            match self.lstat(&normalized) {
                Ok(stat) if stat.is_symbolic_link => {
                    let target = self.read_link(&normalized)?;
                    normalized = if target.starts_with('/') {
                        normalize_path(&target)
                    } else {
                        normalize_path(&format!("{}/{}", dirname(&normalized), target))
                    };
                }
                Ok(_) | Err(_) => return Ok(normalized),
            }
        }

        Err(VfsError::new(
            "ELOOP",
            format!("too many levels of symbolic links, '{original}'"),
        ))
    }

    fn symlink(&mut self, target: &str, link_path: &str) -> VfsResult<()> {
        self.bridge
            .with_mut(|bridge| {
                bridge.symlink(SymlinkRequest {
                    vm_id: self.vm_id.clone(),
                    target_path: normalize_path(target),
                    link_path: normalize_path(link_path),
                })
            })
            .map_err(Self::vfs_error)
    }

    fn read_link(&self, path: &str) -> VfsResult<String> {
        let normalized = normalize_path(path);
        self.bridge
            .with_mut(|bridge| {
                bridge.read_link(PathRequest {
                    vm_id: self.vm_id.clone(),
                    path: normalized,
                })
            })
            .map_err(Self::vfs_error)
    }

    fn lstat(&self, path: &str) -> VfsResult<VirtualStat> {
        let identity = self.tracked_identity(path)?;
        let normalized = identity
            .as_ref()
            .map(|identity| identity.canonical_path.clone())
            .unwrap_or_else(|| normalize_path(path));
        let metadata = self
            .bridge
            .with_mut(|bridge| {
                bridge.lstat(PathRequest {
                    vm_id: self.vm_id.clone(),
                    path: normalized,
                })
            })
            .map_err(Self::vfs_error)?;
        Ok(Self::file_metadata_to_stat(metadata, identity.as_ref()))
    }

    fn link(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        let normalized_old = normalize_path(old_path);
        let normalized_new = normalize_path(new_path);
        if self.exists(&normalized_new) {
            return Err(VfsError::new(
                "EEXIST",
                format!("file already exists, link '{new_path}'"),
            ));
        }

        let old_stat = self.stat(&normalized_old)?;
        if old_stat.is_directory || old_stat.is_symbolic_link {
            return Err(VfsError::new(
                "EPERM",
                format!("operation not permitted, link '{old_path}'"),
            ));
        }
        let parent = self.lstat(&dirname(&normalized_new))?;
        if !parent.is_directory {
            return Err(VfsError::new(
                "ENOENT",
                format!("no such file or directory, link '{new_path}'"),
            ));
        }

        self.track_link(&normalized_old, &normalized_new)
    }

    fn chmod(&mut self, path: &str, mode: u32) -> VfsResult<()> {
        let normalized = normalize_path(path);
        self.bridge
            .with_mut(|bridge| {
                bridge.chmod(ChmodRequest {
                    vm_id: self.vm_id.clone(),
                    path: normalized,
                    mode,
                })
            })
            .map_err(Self::vfs_error)
    }

    fn chown(&mut self, path: &str, uid: u32, gid: u32) -> VfsResult<()> {
        let now = Self::current_time_ms();
        self.update_metadata(path, |metadata| {
            metadata.uid = Some(uid);
            metadata.gid = Some(gid);
            metadata.ctime_ms = Some(now);
        })
    }

    fn utimes(&mut self, path: &str, atime_ms: u64, mtime_ms: u64) -> VfsResult<()> {
        let now = Self::current_time_ms();
        self.update_metadata(path, |metadata| {
            metadata.atime_ms = Some(atime_ms);
            metadata.mtime_ms = Some(mtime_ms);
            metadata.ctime_ms = Some(now);
        })
    }

    fn truncate(&mut self, path: &str, length: u64) -> VfsResult<()> {
        let normalized = self
            .tracked_identity(path)?
            .map(|identity| identity.canonical_path)
            .unwrap_or_else(|| normalize_path(path));
        self.bridge
            .with_mut(|bridge| {
                bridge.truncate(TruncateRequest {
                    vm_id: self.vm_id.clone(),
                    path: normalized,
                    len: length,
                })
            })
            .map_err(Self::vfs_error)
    }

    fn pread(&mut self, path: &str, offset: u64, length: usize) -> VfsResult<Vec<u8>> {
        let bytes = self.read_file(path)?;
        let start = offset as usize;
        if start >= bytes.len() {
            return Ok(Vec::new());
        }
        let end = start.saturating_add(length).min(bytes.len());
        Ok(bytes[start..end].to_vec())
    }
}

#[derive(Clone)]
struct ScopedHostFilesystem<B> {
    inner: HostFilesystem<B>,
    guest_root: String,
}

impl<B> ScopedHostFilesystem<B> {
    fn new(inner: HostFilesystem<B>, guest_root: impl Into<String>) -> Self {
        Self {
            inner,
            guest_root: normalize_path(&guest_root.into()),
        }
    }

    fn scoped_path(&self, path: &str) -> String {
        let normalized = normalize_path(path);
        if self.guest_root == "/" {
            return normalized;
        }
        if normalized == "/" {
            return self.guest_root.clone();
        }
        format!(
            "{}/{}",
            self.guest_root.trim_end_matches('/'),
            normalized.trim_start_matches('/')
        )
    }

    fn scoped_target(&self, target: &str) -> String {
        if target.starts_with('/') {
            self.scoped_path(target)
        } else {
            target.to_owned()
        }
    }

    fn strip_guest_root_prefix<'a>(&self, target: &'a str) -> Option<&'a str> {
        if target == self.guest_root {
            Some("")
        } else {
            target
                .strip_prefix(self.guest_root.as_str())
                .filter(|stripped| stripped.starts_with('/'))
        }
    }

    fn unscoped_target(&self, target: String) -> String {
        if !target.starts_with('/') || self.guest_root == "/" {
            return target;
        }
        match self.strip_guest_root_prefix(&target) {
            Some(stripped) => format!("/{}", stripped.trim_start_matches('/')),
            None => target,
        }
    }
}

impl<B> VirtualFileSystem for ScopedHostFilesystem<B>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    fn read_file(&mut self, path: &str) -> VfsResult<Vec<u8>> {
        self.inner.read_file(&self.scoped_path(path))
    }

    fn read_dir(&mut self, path: &str) -> VfsResult<Vec<String>> {
        self.inner.read_dir(&self.scoped_path(path))
    }

    fn read_dir_with_types(&mut self, path: &str) -> VfsResult<Vec<VirtualDirEntry>> {
        self.inner.read_dir_with_types(&self.scoped_path(path))
    }

    fn write_file(&mut self, path: &str, content: impl Into<Vec<u8>>) -> VfsResult<()> {
        self.inner.write_file(&self.scoped_path(path), content)
    }

    fn create_dir(&mut self, path: &str) -> VfsResult<()> {
        self.inner.create_dir(&self.scoped_path(path))
    }

    fn mkdir(&mut self, path: &str, recursive: bool) -> VfsResult<()> {
        self.inner.mkdir(&self.scoped_path(path), recursive)
    }

    fn exists(&self, path: &str) -> bool {
        self.inner.exists(&self.scoped_path(path))
    }

    fn stat(&mut self, path: &str) -> VfsResult<VirtualStat> {
        self.inner.stat(&self.scoped_path(path))
    }

    fn remove_file(&mut self, path: &str) -> VfsResult<()> {
        self.inner.remove_file(&self.scoped_path(path))
    }

    fn remove_dir(&mut self, path: &str) -> VfsResult<()> {
        self.inner.remove_dir(&self.scoped_path(path))
    }

    fn rename(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        self.inner
            .rename(&self.scoped_path(old_path), &self.scoped_path(new_path))
    }

    fn realpath(&self, path: &str) -> VfsResult<String> {
        let resolved = self.inner.realpath(&self.scoped_path(path))?;
        Ok(self.unscoped_target(resolved))
    }

    fn symlink(&mut self, target: &str, link_path: &str) -> VfsResult<()> {
        self.inner
            .symlink(&self.scoped_target(target), &self.scoped_path(link_path))
    }

    fn read_link(&self, path: &str) -> VfsResult<String> {
        self.inner
            .read_link(&self.scoped_path(path))
            .map(|target| self.unscoped_target(target))
    }

    fn lstat(&self, path: &str) -> VfsResult<VirtualStat> {
        self.inner.lstat(&self.scoped_path(path))
    }

    fn link(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        self.inner
            .link(&self.scoped_path(old_path), &self.scoped_path(new_path))
    }

    fn chmod(&mut self, path: &str, mode: u32) -> VfsResult<()> {
        self.inner.chmod(&self.scoped_path(path), mode)
    }

    fn chown(&mut self, path: &str, uid: u32, gid: u32) -> VfsResult<()> {
        self.inner.chown(&self.scoped_path(path), uid, gid)
    }

    fn utimes(&mut self, path: &str, atime_ms: u64, mtime_ms: u64) -> VfsResult<()> {
        self.inner
            .utimes(&self.scoped_path(path), atime_ms, mtime_ms)
    }

    fn truncate(&mut self, path: &str, length: u64) -> VfsResult<()> {
        self.inner.truncate(&self.scoped_path(path), length)
    }

    fn pread(&mut self, path: &str, offset: u64, length: usize) -> VfsResult<Vec<u8>> {
        self.inner.pread(&self.scoped_path(path), offset, length)
    }
}

#[derive(Clone)]
struct MountPluginContext<B> {
    bridge: SharedBridge<B>,
    vm_id: String,
}

#[derive(Debug)]
struct MemoryMountPlugin;

impl<Context> FileSystemPluginFactory<Context> for MemoryMountPlugin {
    fn plugin_id(&self) -> &'static str {
        "memory"
    }

    fn open(
        &self,
        _request: OpenFileSystemPluginRequest<'_, Context>,
    ) -> Result<Box<dyn agent_os_kernel::mount_table::MountedFileSystem>, PluginError> {
        Ok(Box::new(MountedVirtualFileSystem::new(
            MemoryFileSystem::new(),
        )))
    }
}

#[derive(Debug)]
struct JsBridgeMountPlugin;

impl<B> FileSystemPluginFactory<MountPluginContext<B>> for JsBridgeMountPlugin
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    fn plugin_id(&self) -> &'static str {
        "js_bridge"
    }

    fn open(
        &self,
        request: OpenFileSystemPluginRequest<'_, MountPluginContext<B>>,
    ) -> Result<Box<dyn agent_os_kernel::mount_table::MountedFileSystem>, PluginError> {
        if !matches!(request.config, Value::Null | Value::Object(_)) {
            return Err(PluginError::invalid_input(
                "js_bridge mount config must be an object or null",
            ));
        }

        Ok(Box::new(MountedVirtualFileSystem::new(
            ScopedHostFilesystem::new(
                HostFilesystem::new(request.context.bridge.clone(), &request.context.vm_id),
                request.guest_path,
            ),
        )))
    }
}

#[allow(dead_code)]
#[derive(Debug)]
struct ConnectionState {
    auth_token: String,
    sessions: BTreeSet<String>,
}

#[allow(dead_code)]
#[derive(Debug)]
struct SessionState {
    connection_id: String,
    placement: SidecarPlacement,
    metadata: BTreeMap<String, String>,
    vm_ids: BTreeSet<String>,
}

#[allow(dead_code)]
#[derive(Debug, Default, Clone)]
struct VmConfiguration {
    mounts: Vec<crate::protocol::MountDescriptor>,
    software: Vec<crate::protocol::SoftwareDescriptor>,
    permissions: Vec<crate::protocol::PermissionDescriptor>,
    instructions: Vec<String>,
    projected_modules: Vec<crate::protocol::ProjectedModuleDescriptor>,
    command_permissions: BTreeMap<String, WasmPermissionTier>,
}

#[allow(dead_code)]
struct VmState {
    connection_id: String,
    session_id: String,
    metadata: BTreeMap<String, String>,
    dns: VmDnsConfig,
    guest_env: BTreeMap<String, String>,
    requested_runtime: GuestRuntimeKind,
    cwd: PathBuf,
    kernel: SidecarKernel,
    loaded_snapshot: Option<FilesystemSnapshot>,
    configuration: VmConfiguration,
    command_guest_paths: BTreeMap<String, String>,
    command_permissions: BTreeMap<String, WasmPermissionTier>,
    active_processes: BTreeMap<String, ActiveProcess>,
    signal_states: BTreeMap<String, BTreeMap<u32, SignalHandlerRegistration>>,
}

#[derive(Debug, Clone)]
struct JavascriptSocketPathContext {
    sandbox_root: PathBuf,
    mounts: Vec<crate::protocol::MountDescriptor>,
    listen_policy: VmListenPolicy,
    loopback_exempt_ports: BTreeSet<u16>,
    tcp_loopback_guest_to_host_ports: BTreeMap<(JavascriptSocketFamily, u16), u16>,
    udp_loopback_guest_to_host_ports: BTreeMap<(JavascriptSocketFamily, u16), u16>,
    udp_loopback_host_to_guest_ports: BTreeMap<(JavascriptSocketFamily, u16), u16>,
    used_tcp_guest_ports: BTreeMap<JavascriptSocketFamily, BTreeSet<u16>>,
    used_udp_guest_ports: BTreeMap<JavascriptSocketFamily, BTreeSet<u16>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum JavascriptSocketFamily {
    Ipv4,
    Ipv6,
}

impl JavascriptSocketFamily {
    fn from_ip(ip: IpAddr) -> Self {
        match ip {
            IpAddr::V4(_) => Self::Ipv4,
            IpAddr::V6(_) => Self::Ipv6,
        }
    }
}

impl From<JavascriptUdpFamily> for JavascriptSocketFamily {
    fn from(value: JavascriptUdpFamily) -> Self {
        match value {
            JavascriptUdpFamily::Ipv4 => Self::Ipv4,
            JavascriptUdpFamily::Ipv6 => Self::Ipv6,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct VmListenPolicy {
    port_min: u16,
    port_max: u16,
    allow_privileged: bool,
}

impl Default for VmListenPolicy {
    fn default() -> Self {
        Self {
            port_min: 1,
            port_max: u16::MAX,
            allow_privileged: false,
        }
    }
}

impl JavascriptSocketPathContext {
    fn loopback_port_allowed(&self, port: u16) -> bool {
        self.loopback_exempt_ports.contains(&port)
            || self
                .tcp_loopback_guest_to_host_ports
                .keys()
                .any(|(_, guest_port)| *guest_port == port)
    }

    fn translate_tcp_loopback_port(
        &self,
        family: JavascriptSocketFamily,
        port: u16,
    ) -> Option<u16> {
        self.tcp_loopback_guest_to_host_ports
            .get(&(family, port))
            .copied()
    }

    fn translate_udp_loopback_port(
        &self,
        family: JavascriptSocketFamily,
        port: u16,
    ) -> Option<u16> {
        self.udp_loopback_guest_to_host_ports
            .get(&(family, port))
            .copied()
    }

    fn guest_udp_port_for_host_port(
        &self,
        family: JavascriptSocketFamily,
        port: u16,
    ) -> Option<u16> {
        self.udp_loopback_host_to_guest_ports
            .get(&(family, port))
            .copied()
    }
}

#[allow(dead_code)]
struct ActiveProcess {
    kernel_pid: u32,
    kernel_handle: KernelProcessHandle,
    runtime: GuestRuntimeKind,
    execution: ActiveExecution,
    host_cwd: PathBuf,
    child_processes: BTreeMap<String, ActiveProcess>,
    next_child_process_id: usize,
    tcp_listeners: BTreeMap<String, ActiveTcpListener>,
    next_tcp_listener_id: usize,
    tcp_sockets: BTreeMap<String, ActiveTcpSocket>,
    next_tcp_socket_id: usize,
    unix_listeners: BTreeMap<String, ActiveUnixListener>,
    next_unix_listener_id: usize,
    unix_sockets: BTreeMap<String, ActiveUnixSocket>,
    next_unix_socket_id: usize,
    udp_sockets: BTreeMap<String, ActiveUdpSocket>,
    next_udp_socket_id: usize,
}

#[derive(Debug, Clone, Copy, Default)]
struct NetworkResourceCounts {
    sockets: usize,
    connections: usize,
}

impl ActiveProcess {
    #[cfg(test)]
    fn new(
        kernel_pid: u32,
        kernel_handle: KernelProcessHandle,
        runtime: GuestRuntimeKind,
        execution: ActiveExecution,
    ) -> Self {
        Self::new_with_host_cwd(
            kernel_pid,
            kernel_handle,
            runtime,
            execution,
            PathBuf::from("/"),
        )
    }

    fn new_with_host_cwd(
        kernel_pid: u32,
        kernel_handle: KernelProcessHandle,
        runtime: GuestRuntimeKind,
        execution: ActiveExecution,
        host_cwd: PathBuf,
    ) -> Self {
        Self {
            kernel_pid,
            kernel_handle,
            runtime,
            execution,
            host_cwd,
            child_processes: BTreeMap::new(),
            next_child_process_id: 0,
            tcp_listeners: BTreeMap::new(),
            next_tcp_listener_id: 0,
            tcp_sockets: BTreeMap::new(),
            next_tcp_socket_id: 0,
            unix_listeners: BTreeMap::new(),
            next_unix_listener_id: 0,
            unix_sockets: BTreeMap::new(),
            next_unix_socket_id: 0,
            udp_sockets: BTreeMap::new(),
            next_udp_socket_id: 0,
        }
    }

    fn allocate_child_process_id(&mut self) -> String {
        self.next_child_process_id += 1;
        format!("child-{}", self.next_child_process_id)
    }

    fn allocate_tcp_listener_id(&mut self) -> String {
        self.next_tcp_listener_id += 1;
        format!("listener-{}", self.next_tcp_listener_id)
    }

    fn allocate_tcp_socket_id(&mut self) -> String {
        self.next_tcp_socket_id += 1;
        format!("socket-{}", self.next_tcp_socket_id)
    }

    fn allocate_unix_listener_id(&mut self) -> String {
        self.next_unix_listener_id += 1;
        format!("unix-listener-{}", self.next_unix_listener_id)
    }

    fn allocate_unix_socket_id(&mut self) -> String {
        self.next_unix_socket_id += 1;
        format!("unix-socket-{}", self.next_unix_socket_id)
    }

    fn allocate_udp_socket_id(&mut self) -> String {
        self.next_udp_socket_id += 1;
        format!("udp-socket-{}", self.next_udp_socket_id)
    }

    fn network_resource_counts(&self) -> NetworkResourceCounts {
        let mut counts = NetworkResourceCounts {
            sockets: self.tcp_listeners.len()
                + self.tcp_sockets.len()
                + self.unix_listeners.len()
                + self.unix_sockets.len()
                + self.udp_sockets.len(),
            connections: self.tcp_sockets.len() + self.unix_sockets.len(),
        };

        for child in self.child_processes.values() {
            let child_counts = child.network_resource_counts();
            counts.sockets += child_counts.sockets;
            counts.connections += child_counts.connections;
        }

        counts
    }
}

#[derive(Debug)]
enum JavascriptTcpListenerEvent {
    Connection(PendingTcpSocket),
    Error {
        code: Option<String>,
        message: String,
    },
}

#[derive(Debug)]
struct PendingTcpSocket {
    stream: TcpStream,
    guest_local_addr: SocketAddr,
    guest_remote_addr: SocketAddr,
}

#[derive(Debug)]
enum JavascriptTcpSocketEvent {
    Data(Vec<u8>),
    End,
    Close {
        had_error: bool,
    },
    Error {
        code: Option<String>,
        message: String,
    },
}

#[derive(Debug)]
struct ActiveTcpSocket {
    stream: Arc<Mutex<TcpStream>>,
    events: Receiver<JavascriptTcpSocketEvent>,
    event_sender: Sender<JavascriptTcpSocketEvent>,
    guest_local_addr: SocketAddr,
    guest_remote_addr: SocketAddr,
    listener_id: Option<String>,
    saw_local_shutdown: Arc<AtomicBool>,
    saw_remote_end: Arc<AtomicBool>,
    close_notified: Arc<AtomicBool>,
}

#[derive(Debug, Clone, Copy)]
struct ResolvedTcpConnectAddr {
    actual_addr: SocketAddr,
    guest_remote_addr: SocketAddr,
}

impl ActiveTcpSocket {
    fn connect<B>(
        bridge: &SharedBridge<B>,
        vm_id: &str,
        dns: &VmDnsConfig,
        host: &str,
        port: u16,
        context: &JavascriptSocketPathContext,
    ) -> Result<Self, SidecarError>
    where
        B: NativeSidecarBridge + Send + 'static,
        BridgeError<B>: fmt::Debug + Send + Sync + 'static,
    {
        let resolved = resolve_tcp_connect_addr(bridge, vm_id, dns, host, port, context)?;
        let stream = TcpStream::connect_timeout(&resolved.actual_addr, Duration::from_secs(30))
            .map_err(sidecar_net_error)?;
        let guest_local_addr = stream.local_addr().map_err(sidecar_net_error)?;
        Self::from_stream(stream, None, guest_local_addr, resolved.guest_remote_addr)
    }

    fn from_stream(
        stream: TcpStream,
        listener_id: Option<String>,
        guest_local_addr: SocketAddr,
        guest_remote_addr: SocketAddr,
    ) -> Result<Self, SidecarError> {
        let read_stream = stream.try_clone().map_err(sidecar_net_error)?;
        let stream = Arc::new(Mutex::new(stream));
        let (sender, events) = mpsc::channel();
        let saw_local_shutdown = Arc::new(AtomicBool::new(false));
        let saw_remote_end = Arc::new(AtomicBool::new(false));
        let close_notified = Arc::new(AtomicBool::new(false));
        spawn_tcp_socket_reader(
            read_stream,
            sender.clone(),
            Arc::clone(&saw_local_shutdown),
            Arc::clone(&saw_remote_end),
            Arc::clone(&close_notified),
        );

        Ok(Self {
            stream,
            events,
            event_sender: sender,
            guest_local_addr,
            guest_remote_addr,
            listener_id,
            saw_local_shutdown,
            saw_remote_end,
            close_notified,
        })
    }

    fn poll(&mut self, wait: Duration) -> Result<Option<JavascriptTcpSocketEvent>, SidecarError> {
        match self.events.recv_timeout(wait) {
            Ok(event) => Ok(Some(event)),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Ok(None),
        }
    }

    fn write_all(&self, contents: &[u8]) -> Result<usize, SidecarError> {
        let mut stream = self
            .stream
            .lock()
            .map_err(|_| SidecarError::InvalidState(String::from("TCP socket lock poisoned")))?;
        stream.write_all(contents).map_err(sidecar_net_error)?;
        Ok(contents.len())
    }

    fn shutdown_write(&self) -> Result<(), SidecarError> {
        let stream = self
            .stream
            .lock()
            .map_err(|_| SidecarError::InvalidState(String::from("TCP socket lock poisoned")))?;
        self.saw_local_shutdown.store(true, Ordering::SeqCst);
        stream
            .shutdown(Shutdown::Write)
            .map_err(sidecar_net_error)?;
        if self.saw_remote_end.load(Ordering::SeqCst)
            && !self.close_notified.swap(true, Ordering::SeqCst)
        {
            let _ = self
                .event_sender
                .send(JavascriptTcpSocketEvent::Close { had_error: false });
        }
        Ok(())
    }

    fn close(&self) -> Result<(), SidecarError> {
        let stream = self
            .stream
            .lock()
            .map_err(|_| SidecarError::InvalidState(String::from("TCP socket lock poisoned")))?;
        stream.shutdown(Shutdown::Both).map_err(sidecar_net_error)
    }
}

#[derive(Debug)]
struct ActiveTcpListener {
    listener: TcpListener,
    local_addr: SocketAddr,
    guest_local_addr: SocketAddr,
    backlog: usize,
    active_connection_ids: BTreeSet<String>,
}

#[derive(Debug)]
enum JavascriptUnixListenerEvent {
    Connection(PendingUnixSocket),
    Error {
        code: Option<String>,
        message: String,
    },
}

#[derive(Debug)]
struct PendingUnixSocket {
    stream: UnixStream,
    local_path: Option<String>,
    remote_path: Option<String>,
}

#[derive(Debug)]
struct ActiveUnixSocket {
    stream: Arc<Mutex<UnixStream>>,
    events: Receiver<JavascriptTcpSocketEvent>,
    event_sender: Sender<JavascriptTcpSocketEvent>,
    listener_id: Option<String>,
    saw_local_shutdown: Arc<AtomicBool>,
    saw_remote_end: Arc<AtomicBool>,
    close_notified: Arc<AtomicBool>,
}

impl ActiveUnixSocket {
    fn connect(host_path: &Path, guest_path: &str) -> Result<Self, SidecarError> {
        let stream = UnixStream::connect(host_path).map_err(sidecar_net_error)?;
        Self::from_stream(stream, None, None, Some(guest_path.to_owned()))
    }

    fn from_stream(
        stream: UnixStream,
        listener_id: Option<String>,
        _local_path: Option<String>,
        _remote_path: Option<String>,
    ) -> Result<Self, SidecarError> {
        let read_stream = stream.try_clone().map_err(sidecar_net_error)?;
        let stream = Arc::new(Mutex::new(stream));
        let (sender, events) = mpsc::channel();
        let saw_local_shutdown = Arc::new(AtomicBool::new(false));
        let saw_remote_end = Arc::new(AtomicBool::new(false));
        let close_notified = Arc::new(AtomicBool::new(false));
        spawn_unix_socket_reader(
            read_stream,
            sender.clone(),
            Arc::clone(&saw_local_shutdown),
            Arc::clone(&saw_remote_end),
            Arc::clone(&close_notified),
        );

        Ok(Self {
            stream,
            events,
            event_sender: sender,
            listener_id,
            saw_local_shutdown,
            saw_remote_end,
            close_notified,
        })
    }

    fn poll(&mut self, wait: Duration) -> Result<Option<JavascriptTcpSocketEvent>, SidecarError> {
        match self.events.recv_timeout(wait) {
            Ok(event) => Ok(Some(event)),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Ok(None),
        }
    }

    fn write_all(&self, contents: &[u8]) -> Result<usize, SidecarError> {
        let mut stream = self
            .stream
            .lock()
            .map_err(|_| SidecarError::InvalidState(String::from("Unix socket lock poisoned")))?;
        stream.write_all(contents).map_err(sidecar_net_error)?;
        Ok(contents.len())
    }

    fn shutdown_write(&self) -> Result<(), SidecarError> {
        let stream = self
            .stream
            .lock()
            .map_err(|_| SidecarError::InvalidState(String::from("Unix socket lock poisoned")))?;
        self.saw_local_shutdown.store(true, Ordering::SeqCst);
        stream
            .shutdown(Shutdown::Write)
            .map_err(sidecar_net_error)?;
        if self.saw_remote_end.load(Ordering::SeqCst)
            && !self.close_notified.swap(true, Ordering::SeqCst)
        {
            let _ = self
                .event_sender
                .send(JavascriptTcpSocketEvent::Close { had_error: false });
        }
        Ok(())
    }

    fn close(&self) -> Result<(), SidecarError> {
        let stream = self
            .stream
            .lock()
            .map_err(|_| SidecarError::InvalidState(String::from("Unix socket lock poisoned")))?;
        stream.shutdown(Shutdown::Both).map_err(sidecar_net_error)
    }
}

#[derive(Debug)]
struct ActiveUnixListener {
    listener: UnixListener,
    path: String,
    backlog: usize,
    active_connection_ids: BTreeSet<String>,
}

impl ActiveUnixListener {
    fn bind(
        host_path: &Path,
        guest_path: &str,
        backlog: Option<u32>,
    ) -> Result<Self, SidecarError> {
        if let Some(parent) = host_path.parent() {
            fs::create_dir_all(parent).map_err(sidecar_net_error)?;
        }
        let listener = UnixListener::bind(host_path).map_err(sidecar_net_error)?;
        listener.set_nonblocking(true).map_err(sidecar_net_error)?;
        Ok(Self {
            listener,
            path: guest_path.to_owned(),
            backlog: usize::try_from(backlog.unwrap_or(DEFAULT_JAVASCRIPT_NET_BACKLOG))
                .expect("default backlog fits within usize"),
            active_connection_ids: BTreeSet::new(),
        })
    }

    fn path(&self) -> &str {
        &self.path
    }

    fn poll(
        &mut self,
        wait: Duration,
    ) -> Result<Option<JavascriptUnixListenerEvent>, SidecarError> {
        let deadline = Instant::now() + wait;
        loop {
            match self.listener.accept() {
                Ok((stream, remote_addr)) => {
                    if self.active_connection_ids.len() >= self.backlog {
                        let _ = stream.shutdown(Shutdown::Both);
                        if wait.is_zero() || Instant::now() >= deadline {
                            return Ok(None);
                        }
                        continue;
                    }

                    let local_path = Some(self.path.clone());
                    let remote_path = unix_socket_path(&remote_addr);
                    return Ok(Some(JavascriptUnixListenerEvent::Connection(
                        PendingUnixSocket {
                            stream,
                            local_path,
                            remote_path,
                        },
                    )));
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if wait.is_zero() || Instant::now() >= deadline {
                        return Ok(None);
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => {
                    return Ok(Some(JavascriptUnixListenerEvent::Error {
                        code: io_error_code(&error),
                        message: error.to_string(),
                    }));
                }
            }
        }
    }

    fn close(&self) -> Result<(), SidecarError> {
        Ok(())
    }

    fn active_connection_count(&self) -> usize {
        self.active_connection_ids.len()
    }

    fn register_connection(&mut self, socket_id: &str) {
        self.active_connection_ids.insert(socket_id.to_string());
    }

    fn release_connection(&mut self, socket_id: &str) {
        self.active_connection_ids.remove(socket_id);
    }
}

impl ActiveTcpListener {
    fn bind(guest_host: &str, guest_port: u16, backlog: Option<u32>) -> Result<Self, SidecarError> {
        let bind_addr = resolve_tcp_bind_addr(guest_host, 0)?;
        let listener = TcpListener::bind(bind_addr).map_err(sidecar_net_error)?;
        listener.set_nonblocking(true).map_err(sidecar_net_error)?;
        let local_addr = listener.local_addr().map_err(sidecar_net_error)?;
        Ok(Self {
            listener,
            local_addr,
            guest_local_addr: SocketAddr::new(bind_addr.ip(), guest_port),
            backlog: usize::try_from(backlog.unwrap_or(DEFAULT_JAVASCRIPT_NET_BACKLOG))
                .expect("default backlog fits within usize"),
            active_connection_ids: BTreeSet::new(),
        })
    }

    fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    fn guest_local_addr(&self) -> SocketAddr {
        self.guest_local_addr
    }

    fn poll(&mut self, wait: Duration) -> Result<Option<JavascriptTcpListenerEvent>, SidecarError> {
        let deadline = Instant::now() + wait;
        loop {
            match self.listener.accept() {
                Ok((stream, remote_addr)) => {
                    if self.active_connection_ids.len() >= self.backlog {
                        let _ = stream.shutdown(Shutdown::Both);
                        if wait.is_zero() || Instant::now() >= deadline {
                            return Ok(None);
                        }
                        continue;
                    }
                    return Ok(Some(JavascriptTcpListenerEvent::Connection(
                        PendingTcpSocket {
                            stream,
                            guest_local_addr: self.guest_local_addr,
                            guest_remote_addr: SocketAddr::new(
                                remote_addr.ip(),
                                remote_addr.port(),
                            ),
                        },
                    )));
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if wait.is_zero() || Instant::now() >= deadline {
                        return Ok(None);
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => {
                    return Ok(Some(JavascriptTcpListenerEvent::Error {
                        code: io_error_code(&error),
                        message: error.to_string(),
                    }));
                }
            }
        }
    }

    fn close(&self) -> Result<(), SidecarError> {
        Ok(())
    }

    fn active_connection_count(&self) -> usize {
        self.active_connection_ids.len()
    }

    fn register_connection(&mut self, socket_id: &str) {
        self.active_connection_ids.insert(socket_id.to_string());
    }

    fn release_connection(&mut self, socket_id: &str) {
        self.active_connection_ids.remove(socket_id);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JavascriptUdpFamily {
    Ipv4,
    Ipv6,
}

impl JavascriptUdpFamily {
    fn from_socket_type(value: &str) -> Result<Self, SidecarError> {
        match value {
            "udp4" => Ok(Self::Ipv4),
            "udp6" => Ok(Self::Ipv6),
            other => Err(SidecarError::InvalidState(format!(
                "unsupported dgram socket type {other}"
            ))),
        }
    }

    fn socket_type(self) -> &'static str {
        match self {
            Self::Ipv4 => "udp4",
            Self::Ipv6 => "udp6",
        }
    }

    fn matches_addr(self, addr: &SocketAddr) -> bool {
        match (self, addr) {
            (Self::Ipv4, SocketAddr::V4(_)) | (Self::Ipv6, SocketAddr::V6(_)) => true,
            _ => false,
        }
    }
}

#[derive(Debug)]
enum JavascriptUdpSocketEvent {
    Message {
        data: Vec<u8>,
        remote_addr: SocketAddr,
    },
    Error {
        code: Option<String>,
        message: String,
    },
}

#[derive(Debug)]
struct ActiveUdpSocket {
    family: JavascriptUdpFamily,
    socket: Option<UdpSocket>,
    guest_local_addr: Option<SocketAddr>,
}

impl ActiveUdpSocket {
    fn new(family: JavascriptUdpFamily) -> Self {
        Self {
            family,
            socket: None,
            guest_local_addr: None,
        }
    }

    fn local_addr(&self) -> Option<SocketAddr> {
        self.guest_local_addr
    }

    fn bind(
        &mut self,
        host: Option<&str>,
        port: u16,
        context: &JavascriptSocketPathContext,
    ) -> Result<SocketAddr, SidecarError> {
        if self.socket.is_some() {
            return Err(SidecarError::Execution(String::from(
                "EINVAL: Agent OS dgram socket is already bound",
            )));
        }

        let (guest_host, guest_family) = normalize_udp_bind_host(host, self.family)?;
        let guest_port = allocate_guest_listen_port(
            port,
            guest_family,
            &context.used_udp_guest_ports,
            context.listen_policy,
        )?;
        let bind_addr = resolve_udp_bind_addr(guest_host, 0, self.family)?;
        let socket = UdpSocket::bind(bind_addr).map_err(sidecar_net_error)?;
        socket.set_nonblocking(true).map_err(sidecar_net_error)?;
        let local_addr = SocketAddr::new(bind_addr.ip(), guest_port);
        self.socket = Some(socket);
        self.guest_local_addr = Some(local_addr);
        Ok(local_addr)
    }

    fn ensure_bound_for_send(
        &mut self,
        context: &JavascriptSocketPathContext,
    ) -> Result<SocketAddr, SidecarError> {
        if let Some(local_addr) = self.local_addr() {
            return Ok(local_addr);
        }

        self.bind(None, 0, context)
    }

    fn send_to<B>(
        &mut self,
        bridge: &SharedBridge<B>,
        vm_id: &str,
        dns: &VmDnsConfig,
        host: &str,
        port: u16,
        context: &JavascriptSocketPathContext,
        contents: &[u8],
    ) -> Result<(usize, SocketAddr), SidecarError>
    where
        B: NativeSidecarBridge + Send + 'static,
        BridgeError<B>: fmt::Debug + Send + Sync + 'static,
    {
        let remote_addr = resolve_udp_addr(bridge, vm_id, dns, host, port, self.family, context)?;
        let local_addr = self.ensure_bound_for_send(context)?;
        let socket = self.socket.as_ref().ok_or_else(|| {
            SidecarError::InvalidState(String::from("UDP socket is not initialized"))
        })?;
        let written = socket
            .send_to(contents, remote_addr)
            .map_err(sidecar_net_error)?;
        Ok((written, local_addr))
    }

    fn poll(&self, wait: Duration) -> Result<Option<JavascriptUdpSocketEvent>, SidecarError> {
        let socket = self
            .socket
            .as_ref()
            .ok_or_else(|| SidecarError::InvalidState(String::from("UDP socket is not bound")))?;
        let deadline = Instant::now() + wait;
        let mut buffer = vec![0_u8; 64 * 1024];

        loop {
            match socket.recv_from(&mut buffer) {
                Ok((bytes_read, remote_addr)) => {
                    return Ok(Some(JavascriptUdpSocketEvent::Message {
                        data: buffer[..bytes_read].to_vec(),
                        remote_addr,
                    }));
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if wait.is_zero() || Instant::now() >= deadline {
                        return Ok(None);
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => {
                    return Ok(Some(JavascriptUdpSocketEvent::Error {
                        code: io_error_code(&error),
                        message: error.to_string(),
                    }));
                }
            }
        }
    }

    fn close(&mut self) {
        self.socket.take();
        self.guest_local_addr = None;
    }
}

#[derive(Debug)]
enum ActiveExecution {
    Javascript(JavascriptExecution),
    Python(PythonExecution),
    Wasm(WasmExecution),
}

#[derive(Debug)]
enum ActiveExecutionEvent {
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
    JavascriptSyncRpcRequest(JavascriptSyncRpcRequest),
    PythonVfsRpcRequest(PythonVfsRpcRequest),
    SignalState {
        signal: u32,
        registration: SignalHandlerRegistration,
    },
    Exited(i32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SocketQueryKind {
    TcpListener,
    UdpBound,
}

impl ActiveExecution {
    fn child_pid(&self) -> u32 {
        match self {
            Self::Javascript(execution) => execution.child_pid(),
            Self::Python(execution) => execution.child_pid(),
            Self::Wasm(execution) => execution.child_pid(),
        }
    }

    fn write_stdin(&mut self, chunk: &[u8]) -> Result<(), SidecarError> {
        match self {
            Self::Javascript(execution) => execution
                .write_stdin(chunk)
                .map_err(|error| SidecarError::Execution(error.to_string())),
            Self::Python(execution) => execution
                .write_stdin(chunk)
                .map_err(|error| SidecarError::Execution(error.to_string())),
            Self::Wasm(execution) => execution
                .write_stdin(chunk)
                .map_err(|error| SidecarError::Execution(error.to_string())),
        }
    }

    fn close_stdin(&mut self) -> Result<(), SidecarError> {
        match self {
            Self::Javascript(execution) => execution
                .close_stdin()
                .map_err(|error| SidecarError::Execution(error.to_string())),
            Self::Python(execution) => execution
                .close_stdin()
                .map_err(|error| SidecarError::Execution(error.to_string())),
            Self::Wasm(execution) => execution
                .close_stdin()
                .map_err(|error| SidecarError::Execution(error.to_string())),
        }
    }

    fn respond_python_vfs_rpc_success(
        &mut self,
        id: u64,
        payload: PythonVfsRpcResponsePayload,
    ) -> Result<(), SidecarError> {
        match self {
            Self::Python(execution) => execution
                .respond_vfs_rpc_success(id, payload)
                .map_err(|error| SidecarError::Execution(error.to_string())),
            _ => Err(SidecarError::InvalidState(String::from(
                "only Python executions can service Python VFS RPC responses",
            ))),
        }
    }

    fn respond_python_vfs_rpc_error(
        &mut self,
        id: u64,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<(), SidecarError> {
        match self {
            Self::Python(execution) => execution
                .respond_vfs_rpc_error(id, code, message)
                .map_err(|error| SidecarError::Execution(error.to_string())),
            _ => Err(SidecarError::InvalidState(String::from(
                "only Python executions can service Python VFS RPC responses",
            ))),
        }
    }

    fn respond_javascript_sync_rpc_success(
        &mut self,
        id: u64,
        result: Value,
    ) -> Result<(), SidecarError> {
        match self {
            Self::Javascript(execution) => execution
                .respond_sync_rpc_success(id, result)
                .map_err(|error| SidecarError::Execution(error.to_string())),
            _ => Err(SidecarError::InvalidState(String::from(
                "only JavaScript executions can service JavaScript sync RPC responses",
            ))),
        }
    }

    fn respond_javascript_sync_rpc_error(
        &mut self,
        id: u64,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<(), SidecarError> {
        match self {
            Self::Javascript(execution) => execution
                .respond_sync_rpc_error(id, code, message)
                .map_err(|error| SidecarError::Execution(error.to_string())),
            _ => Err(SidecarError::InvalidState(String::from(
                "only JavaScript executions can service JavaScript sync RPC responses",
            ))),
        }
    }

    fn poll_event(&self, timeout: Duration) -> Result<Option<ActiveExecutionEvent>, SidecarError> {
        match self {
            Self::Javascript(execution) => execution
                .poll_event(timeout)
                .map(|event| {
                    event.map(|event| match event {
                        JavascriptExecutionEvent::Stdout(chunk) => {
                            ActiveExecutionEvent::Stdout(chunk)
                        }
                        JavascriptExecutionEvent::Stderr(chunk) => {
                            ActiveExecutionEvent::Stderr(chunk)
                        }
                        JavascriptExecutionEvent::SyncRpcRequest(request) => {
                            ActiveExecutionEvent::JavascriptSyncRpcRequest(request)
                        }
                        JavascriptExecutionEvent::SignalState {
                            signal,
                            registration,
                        } => ActiveExecutionEvent::SignalState {
                            signal,
                            registration: map_node_signal_registration(registration),
                        },
                        JavascriptExecutionEvent::Exited(code) => {
                            ActiveExecutionEvent::Exited(code)
                        }
                    })
                })
                .map_err(|error| SidecarError::Execution(error.to_string())),
            Self::Python(execution) => execution
                .poll_event(timeout)
                .map(|event| {
                    event.map(|event| match event {
                        PythonExecutionEvent::Stdout(chunk) => ActiveExecutionEvent::Stdout(chunk),
                        PythonExecutionEvent::Stderr(chunk) => ActiveExecutionEvent::Stderr(chunk),
                        PythonExecutionEvent::VfsRpcRequest(request) => {
                            ActiveExecutionEvent::PythonVfsRpcRequest(request)
                        }
                        PythonExecutionEvent::Exited(code) => ActiveExecutionEvent::Exited(code),
                    })
                })
                .map_err(|error| SidecarError::Execution(error.to_string())),
            Self::Wasm(execution) => execution
                .poll_event(timeout)
                .map(|event| {
                    event.map(|event| match event {
                        WasmExecutionEvent::Stdout(chunk) => ActiveExecutionEvent::Stdout(chunk),
                        WasmExecutionEvent::Stderr(chunk) => ActiveExecutionEvent::Stderr(chunk),
                        WasmExecutionEvent::SignalState {
                            signal,
                            registration,
                        } => ActiveExecutionEvent::SignalState {
                            signal,
                            registration: map_wasm_signal_registration(registration),
                        },
                        WasmExecutionEvent::Exited(code) => ActiveExecutionEvent::Exited(code),
                    })
                })
                .map_err(|error| SidecarError::Execution(error.to_string())),
        }
    }
}

pub struct NativeSidecar<B> {
    config: NativeSidecarConfig,
    bridge: SharedBridge<B>,
    mount_plugins: FileSystemPluginRegistry<MountPluginContext<B>>,
    cache_root: PathBuf,
    javascript_engine: JavascriptExecutionEngine,
    python_engine: PythonExecutionEngine,
    wasm_engine: WasmExecutionEngine,
    next_connection_id: usize,
    next_session_id: usize,
    next_vm_id: usize,
    connections: BTreeMap<String, ConnectionState>,
    sessions: BTreeMap<String, SessionState>,
    vms: BTreeMap<String, VmState>,
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
    fn vm_import_cache_base_dir(&self, vm_id: &str, runtime: &str) -> PathBuf {
        self.cache_root
            .join("runtime-import-caches")
            .join(runtime)
            .join(vm_id)
    }

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
            connections: BTreeMap::new(),
            sessions: BTreeMap::new(),
            vms: BTreeMap::new(),
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

    pub fn dispatch(&mut self, request: RequestFrame) -> Result<DispatchResult, SidecarError> {
        if let Err(error) = self.ensure_request_within_frame_limit(&request) {
            return Ok(DispatchResult {
                response: self.reject(&request, error_code(&error), &error.to_string()),
                events: Vec::new(),
            });
        }

        let result = match request.payload.clone() {
            RequestPayload::Authenticate(payload) => {
                self.authenticate_connection(&request, payload)
            }
            RequestPayload::OpenSession(payload) => self.open_session(&request, payload),
            RequestPayload::CreateVm(payload) => self.create_vm(&request, payload),
            RequestPayload::DisposeVm(payload) => self.dispose_vm(&request, payload),
            RequestPayload::BootstrapRootFilesystem(payload) => {
                self.bootstrap_root_filesystem(&request, payload.entries)
            }
            RequestPayload::ConfigureVm(payload) => self.configure_vm(&request, payload),
            RequestPayload::GuestFilesystemCall(payload) => {
                self.guest_filesystem_call(&request, payload)
            }
            RequestPayload::SnapshotRootFilesystem(payload) => {
                self.snapshot_root_filesystem(&request, payload)
            }
            RequestPayload::SnapshotProcesses(_) => self.snapshot_processes(&request),
            RequestPayload::Execute(payload) => self.execute(&request, payload),
            RequestPayload::WriteStdin(payload) => self.write_stdin(&request, payload),
            RequestPayload::CloseStdin(payload) => self.close_stdin(&request, payload),
            RequestPayload::KillProcess(payload) => self.kill_process(&request, payload),
            RequestPayload::FindListener(payload) => self.find_listener(&request, payload),
            RequestPayload::FindBoundUdp(payload) => self.find_bound_udp(&request, payload),
            RequestPayload::GetSignalState(payload) => self.get_signal_state(&request, payload),
            RequestPayload::GetZombieTimerCount(payload) => {
                self.get_zombie_timer_count(&request, payload)
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

    pub fn poll_event(
        &mut self,
        ownership: &OwnershipScope,
        timeout: Duration,
    ) -> Result<Option<EventFrame>, SidecarError> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(event) = self.try_poll_event(ownership)? {
                return Ok(Some(event));
            }

            if Instant::now() >= deadline {
                return Ok(None);
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            thread::sleep(remaining.min(Duration::from_millis(10)));
        }
    }

    pub fn close_session(
        &mut self,
        connection_id: &str,
        session_id: &str,
    ) -> Result<Vec<EventFrame>, SidecarError> {
        self.dispose_session(connection_id, session_id, DisposeReason::Requested)
    }

    pub fn remove_connection(
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
            events.extend(self.dispose_session(
                connection_id,
                &session_id,
                DisposeReason::ConnectionClosed,
            )?);
        }

        self.connections.remove(connection_id);
        Ok(events)
    }

    fn authenticate_connection(
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

    fn open_session(
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

    fn create_vm(
        &mut self,
        request: &RequestFrame,
        payload: crate::protocol::CreateVmRequest,
    ) -> Result<DispatchResult, SidecarError> {
        let (connection_id, session_id) = self.session_scope_for(&request.ownership)?;
        self.require_owned_session(&connection_id, &session_id)?;

        self.next_vm_id += 1;
        let vm_id = format!("vm-{}", self.next_vm_id);
        let cwd = resolve_cwd(payload.metadata.get("cwd"))?;
        let resource_limits = parse_resource_limits(&payload.metadata)?;
        let dns = parse_vm_dns_config(&payload.metadata)?;
        self.bridge
            .set_vm_permissions(&vm_id, &payload.permissions)?;
        let permissions = bridge_permissions(self.bridge.clone(), &vm_id);
        let guest_env = filter_env(&vm_id, &extract_guest_env(&payload.metadata), &permissions);
        let loaded_snapshot = self.bridge.with_mut(|bridge| {
            bridge.load_filesystem_state(LoadFilesystemStateRequest {
                vm_id: vm_id.clone(),
            })
        })?;

        let mut config = KernelVmConfig::new(vm_id.clone());
        config.cwd = String::from("/");
        config.env = guest_env.clone();
        config.permissions = permissions;
        config.resources = resource_limits;
        let root_filesystem =
            build_root_filesystem(&payload.root_filesystem, loaded_snapshot.as_ref())?;
        let mut kernel = KernelVm::new(MountTable::new(root_filesystem), config);
        kernel
            .register_driver(CommandDriver::new(
                EXECUTION_DRIVER_NAME,
                [JAVASCRIPT_COMMAND, PYTHON_COMMAND, WASM_COMMAND],
            ))
            .map_err(kernel_error)?;
        kernel
            .root_filesystem_mut()
            .expect("native sidecar root filesystem should exist")
            .finish_bootstrap();

        self.bridge
            .emit_lifecycle(&vm_id, LifecycleState::Starting)?;
        self.bridge.emit_lifecycle(&vm_id, LifecycleState::Ready)?;
        self.bridge.emit_log(
            &vm_id,
            format!("created VM {vm_id} for session {session_id}"),
        )?;

        self.sessions
            .get_mut(&session_id)
            .expect("owned session should exist")
            .vm_ids
            .insert(vm_id.clone());
        self.javascript_engine.set_import_cache_base_dir(
            vm_id.clone(),
            self.vm_import_cache_base_dir(&vm_id, "javascript"),
        );
        self.python_engine.set_import_cache_base_dir(
            vm_id.clone(),
            self.vm_import_cache_base_dir(&vm_id, "python"),
        );
        self.wasm_engine.set_import_cache_base_dir(
            vm_id.clone(),
            self.vm_import_cache_base_dir(&vm_id, "wasm"),
        );
        self.vms.insert(
            vm_id.clone(),
            VmState {
                connection_id: connection_id.clone(),
                session_id: session_id.clone(),
                metadata: payload.metadata,
                dns,
                guest_env,
                requested_runtime: payload.runtime,
                cwd,
                kernel,
                loaded_snapshot,
                configuration: VmConfiguration::default(),
                command_guest_paths: BTreeMap::new(),
                command_permissions: BTreeMap::new(),
                active_processes: BTreeMap::new(),
                signal_states: BTreeMap::new(),
            },
        );

        let events = vec![
            self.vm_lifecycle_event(
                &connection_id,
                &session_id,
                &vm_id,
                VmLifecycleState::Creating,
            ),
            self.vm_lifecycle_event(&connection_id, &session_id, &vm_id, VmLifecycleState::Ready),
        ];

        Ok(DispatchResult {
            response: self.respond(
                request,
                ResponsePayload::VmCreated(VmCreatedResponse { vm_id }),
            ),
            events,
        })
    }

    fn dispose_vm(
        &mut self,
        request: &RequestFrame,
        payload: DisposeVmRequest,
    ) -> Result<DispatchResult, SidecarError> {
        let (connection_id, session_id, vm_id) = self.vm_scope_for(&request.ownership)?;
        let events =
            self.dispose_vm_internal(&connection_id, &session_id, &vm_id, payload.reason)?;

        Ok(DispatchResult {
            response: self.respond(
                request,
                ResponsePayload::VmDisposed(VmDisposedResponse { vm_id }),
            ),
            events,
        })
    }

    fn bootstrap_root_filesystem(
        &mut self,
        request: &RequestFrame,
        entries: Vec<RootFilesystemEntry>,
    ) -> Result<DispatchResult, SidecarError> {
        let (connection_id, session_id, vm_id) = self.vm_scope_for(&request.ownership)?;
        self.require_owned_vm(&connection_id, &session_id, &vm_id)?;

        let vm = self.vms.get_mut(&vm_id).expect("owned VM should exist");
        let root = vm.kernel.root_filesystem_mut().ok_or_else(|| {
            SidecarError::InvalidState(String::from("VM root filesystem is unavailable"))
        })?;
        for entry in &entries {
            apply_root_filesystem_entry(root, entry)?;
        }

        Ok(DispatchResult {
            response: self.respond(
                request,
                ResponsePayload::RootFilesystemBootstrapped(RootFilesystemBootstrappedResponse {
                    entry_count: entries.len() as u32,
                }),
            ),
            events: Vec::new(),
        })
    }

    fn configure_vm(
        &mut self,
        request: &RequestFrame,
        payload: ConfigureVmRequest,
    ) -> Result<DispatchResult, SidecarError> {
        let (connection_id, session_id, vm_id) = self.vm_scope_for(&request.ownership)?;
        self.require_owned_vm(&connection_id, &session_id, &vm_id)?;

        let mount_plugins = &self.mount_plugins;
        let vm = self.vms.get_mut(&vm_id).expect("owned VM should exist");
        reconcile_mounts(
            mount_plugins,
            vm,
            &payload.mounts,
            MountPluginContext {
                bridge: self.bridge.clone(),
                vm_id: vm_id.clone(),
            },
        )?;
        vm.command_guest_paths = discover_command_guest_paths(&mut vm.kernel);
        let mut execution_commands = vec![
            String::from(JAVASCRIPT_COMMAND),
            String::from(PYTHON_COMMAND),
            String::from(WASM_COMMAND),
        ];
        execution_commands.extend(vm.command_guest_paths.keys().cloned());
        vm.kernel
            .register_driver(CommandDriver::new(
                EXECUTION_DRIVER_NAME,
                execution_commands,
            ))
            .map_err(kernel_error)?;
        vm.command_permissions = payload.command_permissions.clone();
        vm.configuration = VmConfiguration {
            mounts: payload.mounts.clone(),
            software: payload.software.clone(),
            permissions: payload.permissions.clone(),
            instructions: payload.instructions.clone(),
            projected_modules: payload.projected_modules.clone(),
            command_permissions: payload.command_permissions.clone(),
        };
        if !payload.permissions.is_empty() {
            self.bridge
                .set_vm_permissions(&vm_id, &payload.permissions)?;
        }

        Ok(DispatchResult {
            response: self.respond(
                request,
                ResponsePayload::VmConfigured(VmConfiguredResponse {
                    applied_mounts: payload.mounts.len() as u32,
                    applied_software: payload.software.len() as u32,
                }),
            ),
            events: Vec::new(),
        })
    }

    fn guest_filesystem_call(
        &mut self,
        request: &RequestFrame,
        payload: GuestFilesystemCallRequest,
    ) -> Result<DispatchResult, SidecarError> {
        let (connection_id, session_id, vm_id) = self.vm_scope_for(&request.ownership)?;
        self.require_owned_vm(&connection_id, &session_id, &vm_id)?;

        let vm = self.vms.get_mut(&vm_id).expect("owned VM should exist");
        let response = match payload.operation {
            GuestFilesystemOperation::ReadFile => {
                let bytes = vm.kernel.read_file(&payload.path).map_err(kernel_error)?;
                let (content, encoding) = encode_guest_filesystem_content(bytes);
                GuestFilesystemResultResponse {
                    operation: payload.operation,
                    path: payload.path,
                    content: Some(content),
                    encoding: Some(encoding),
                    entries: None,
                    stat: None,
                    exists: None,
                    target: None,
                }
            }
            GuestFilesystemOperation::WriteFile => {
                let bytes = decode_guest_filesystem_content(
                    &payload.path,
                    payload.content.as_deref(),
                    payload.encoding,
                )?;
                vm.kernel
                    .write_file(&payload.path, bytes)
                    .map_err(kernel_error)?;
                GuestFilesystemResultResponse {
                    operation: payload.operation,
                    path: payload.path,
                    content: None,
                    encoding: None,
                    entries: None,
                    stat: None,
                    exists: None,
                    target: None,
                }
            }
            GuestFilesystemOperation::CreateDir => {
                vm.kernel.create_dir(&payload.path).map_err(kernel_error)?;
                GuestFilesystemResultResponse {
                    operation: payload.operation,
                    path: payload.path,
                    content: None,
                    encoding: None,
                    entries: None,
                    stat: None,
                    exists: None,
                    target: None,
                }
            }
            GuestFilesystemOperation::Mkdir => {
                vm.kernel
                    .mkdir(&payload.path, payload.recursive)
                    .map_err(kernel_error)?;
                GuestFilesystemResultResponse {
                    operation: payload.operation,
                    path: payload.path,
                    content: None,
                    encoding: None,
                    entries: None,
                    stat: None,
                    exists: None,
                    target: None,
                }
            }
            GuestFilesystemOperation::Exists => GuestFilesystemResultResponse {
                operation: payload.operation,
                path: payload.path.clone(),
                content: None,
                encoding: None,
                entries: None,
                stat: None,
                exists: Some(vm.kernel.exists(&payload.path).map_err(kernel_error)?),
                target: None,
            },
            GuestFilesystemOperation::Stat => GuestFilesystemResultResponse {
                operation: payload.operation,
                path: payload.path.clone(),
                content: None,
                encoding: None,
                entries: None,
                stat: Some(guest_filesystem_stat(
                    vm.kernel.stat(&payload.path).map_err(kernel_error)?,
                )),
                exists: None,
                target: None,
            },
            GuestFilesystemOperation::Lstat => GuestFilesystemResultResponse {
                operation: payload.operation,
                path: payload.path.clone(),
                content: None,
                encoding: None,
                entries: None,
                stat: Some(guest_filesystem_stat(
                    vm.kernel.lstat(&payload.path).map_err(kernel_error)?,
                )),
                exists: None,
                target: None,
            },
            GuestFilesystemOperation::ReadDir => GuestFilesystemResultResponse {
                operation: payload.operation,
                path: payload.path.clone(),
                content: None,
                encoding: None,
                entries: Some(vm.kernel.read_dir(&payload.path).map_err(kernel_error)?),
                stat: None,
                exists: None,
                target: None,
            },
            GuestFilesystemOperation::RemoveFile => {
                vm.kernel.remove_file(&payload.path).map_err(kernel_error)?;
                GuestFilesystemResultResponse {
                    operation: payload.operation,
                    path: payload.path,
                    content: None,
                    encoding: None,
                    entries: None,
                    stat: None,
                    exists: None,
                    target: None,
                }
            }
            GuestFilesystemOperation::RemoveDir => {
                vm.kernel.remove_dir(&payload.path).map_err(kernel_error)?;
                GuestFilesystemResultResponse {
                    operation: payload.operation,
                    path: payload.path,
                    content: None,
                    encoding: None,
                    entries: None,
                    stat: None,
                    exists: None,
                    target: None,
                }
            }
            GuestFilesystemOperation::Rename => {
                let destination = payload.destination_path.ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "guest filesystem rename requires a destination_path",
                    ))
                })?;
                vm.kernel
                    .rename(&payload.path, &destination)
                    .map_err(kernel_error)?;
                GuestFilesystemResultResponse {
                    operation: payload.operation,
                    path: payload.path,
                    content: None,
                    encoding: None,
                    entries: None,
                    stat: None,
                    exists: None,
                    target: Some(destination),
                }
            }
            GuestFilesystemOperation::Realpath => GuestFilesystemResultResponse {
                operation: payload.operation,
                path: payload.path.clone(),
                content: None,
                encoding: None,
                entries: None,
                stat: None,
                exists: None,
                target: Some(vm.kernel.realpath(&payload.path).map_err(kernel_error)?),
            },
            GuestFilesystemOperation::Symlink => {
                let target = payload.target.ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "guest filesystem symlink requires a target",
                    ))
                })?;
                vm.kernel
                    .symlink(&target, &payload.path)
                    .map_err(kernel_error)?;
                GuestFilesystemResultResponse {
                    operation: payload.operation,
                    path: payload.path,
                    content: None,
                    encoding: None,
                    entries: None,
                    stat: None,
                    exists: None,
                    target: Some(target),
                }
            }
            GuestFilesystemOperation::ReadLink => GuestFilesystemResultResponse {
                operation: payload.operation,
                path: payload.path.clone(),
                content: None,
                encoding: None,
                entries: None,
                stat: None,
                exists: None,
                target: Some(vm.kernel.read_link(&payload.path).map_err(kernel_error)?),
            },
            GuestFilesystemOperation::Link => {
                let destination = payload.destination_path.ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "guest filesystem link requires a destination_path",
                    ))
                })?;
                vm.kernel
                    .link(&payload.path, &destination)
                    .map_err(kernel_error)?;
                GuestFilesystemResultResponse {
                    operation: payload.operation,
                    path: payload.path,
                    content: None,
                    encoding: None,
                    entries: None,
                    stat: None,
                    exists: None,
                    target: Some(destination),
                }
            }
            GuestFilesystemOperation::Chmod => {
                let mode = payload.mode.ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "guest filesystem chmod requires a mode",
                    ))
                })?;
                vm.kernel.chmod(&payload.path, mode).map_err(kernel_error)?;
                GuestFilesystemResultResponse {
                    operation: payload.operation,
                    path: payload.path,
                    content: None,
                    encoding: None,
                    entries: None,
                    stat: None,
                    exists: None,
                    target: None,
                }
            }
            GuestFilesystemOperation::Chown => {
                let uid = payload.uid.ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "guest filesystem chown requires a uid",
                    ))
                })?;
                let gid = payload.gid.ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "guest filesystem chown requires a gid",
                    ))
                })?;
                vm.kernel
                    .chown(&payload.path, uid, gid)
                    .map_err(kernel_error)?;
                GuestFilesystemResultResponse {
                    operation: payload.operation,
                    path: payload.path,
                    content: None,
                    encoding: None,
                    entries: None,
                    stat: None,
                    exists: None,
                    target: None,
                }
            }
            GuestFilesystemOperation::Utimes => {
                let atime_ms = payload.atime_ms.ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "guest filesystem utimes requires atime_ms",
                    ))
                })?;
                let mtime_ms = payload.mtime_ms.ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "guest filesystem utimes requires mtime_ms",
                    ))
                })?;
                vm.kernel
                    .utimes(&payload.path, atime_ms, mtime_ms)
                    .map_err(kernel_error)?;
                GuestFilesystemResultResponse {
                    operation: payload.operation,
                    path: payload.path,
                    content: None,
                    encoding: None,
                    entries: None,
                    stat: None,
                    exists: None,
                    target: None,
                }
            }
            GuestFilesystemOperation::Truncate => {
                let len = payload.len.ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "guest filesystem truncate requires len",
                    ))
                })?;
                vm.kernel
                    .truncate(&payload.path, len)
                    .map_err(kernel_error)?;
                GuestFilesystemResultResponse {
                    operation: payload.operation,
                    path: payload.path,
                    content: None,
                    encoding: None,
                    entries: None,
                    stat: None,
                    exists: None,
                    target: None,
                }
            }
        };

        Ok(DispatchResult {
            response: self.respond(request, ResponsePayload::GuestFilesystemResult(response)),
            events: Vec::new(),
        })
    }

    fn snapshot_root_filesystem(
        &mut self,
        request: &RequestFrame,
        _payload: SnapshotRootFilesystemRequest,
    ) -> Result<DispatchResult, SidecarError> {
        let (connection_id, session_id, vm_id) = self.vm_scope_for(&request.ownership)?;
        self.require_owned_vm(&connection_id, &session_id, &vm_id)?;

        let vm = self.vms.get_mut(&vm_id).expect("owned VM should exist");
        let snapshot = vm.kernel.snapshot_root_filesystem().map_err(kernel_error)?;

        Ok(DispatchResult {
            response: self.respond(
                request,
                ResponsePayload::RootFilesystemSnapshot(RootFilesystemSnapshotResponse {
                    entries: snapshot.entries.iter().map(root_snapshot_entry).collect(),
                }),
            ),
            events: Vec::new(),
        })
    }

    fn snapshot_processes(
        &mut self,
        request: &RequestFrame,
    ) -> Result<DispatchResult, SidecarError> {
        let (connection_id, session_id, vm_id) = self.vm_scope_for(&request.ownership)?;
        self.require_owned_vm(&connection_id, &session_id, &vm_id)?;

        let vm = self.vms.get(&vm_id).expect("owned VM should exist");
        let processes = vm.kernel.list_processes();
        let entries = processes
            .values()
            .map(|process| {
                let process_id = vm
                    .active_processes
                    .iter()
                    .find(|(_, ap)| ap.kernel_pid == process.pid)
                    .map(|(id, _)| id.clone());
                ProcessSnapshotEntry {
                    pid: process.pid,
                    ppid: process.ppid,
                    pgid: process.pgid,
                    sid: process.sid,
                    process_id,
                    driver: process.driver.clone(),
                    command: process.command.clone(),
                    status: if process.exit_code.is_some() {
                        ProcessSnapshotStatus::Exited
                    } else {
                        ProcessSnapshotStatus::Running
                    },
                    exit_code: process.exit_code,
                }
            })
            .collect();

        Ok(DispatchResult {
            response: self.respond(
                request,
                ResponsePayload::ProcessSnapshot(ProcessSnapshotResponse { processes: entries }),
            ),
            events: Vec::new(),
        })
    }

    fn execute(
        &mut self,
        request: &RequestFrame,
        payload: ExecuteRequest,
    ) -> Result<DispatchResult, SidecarError> {
        let (connection_id, session_id, vm_id) = self.vm_scope_for(&request.ownership)?;
        self.require_owned_vm(&connection_id, &session_id, &vm_id)?;

        let vm = self.vms.get_mut(&vm_id).expect("owned VM should exist");
        if vm.active_processes.contains_key(&payload.process_id) {
            return Err(SidecarError::InvalidState(format!(
                "VM {vm_id} already has an active process with id {}",
                payload.process_id
            )));
        }

        let command = match payload.runtime {
            GuestRuntimeKind::JavaScript => JAVASCRIPT_COMMAND,
            GuestRuntimeKind::Python => PYTHON_COMMAND,
            GuestRuntimeKind::WebAssembly => WASM_COMMAND,
        };
        let mut env = vm.guest_env.clone();
        env.extend(payload.env.clone());
        let sandbox_root = normalize_host_path(&vm.cwd);
        let cwd = resolve_execution_cwd(vm, payload.cwd.as_deref())?;
        env.insert(
            String::from(EXECUTION_SANDBOX_ROOT_ENV),
            sandbox_root.to_string_lossy().into_owned(),
        );
        let argv = std::iter::once(payload.entrypoint.clone())
            .chain(payload.args.iter().cloned())
            .collect::<Vec<_>>();
        self.bridge.require_command_access(
            &vm_id,
            CommandAccessRequest {
                vm_id: vm_id.clone(),
                command: command.to_owned(),
                args: argv.clone(),
                cwd: Some(String::from("/")),
                env: env.clone(),
            },
        )?;
        let kernel_handle = vm
            .kernel
            .spawn_process(
                command,
                argv,
                SpawnOptions {
                    requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                    cwd: Some(String::from("/")),
                    ..SpawnOptions::default()
                },
            )
            .map_err(kernel_error)?;

        let execution = match payload.runtime {
            GuestRuntimeKind::JavaScript => {
                let mut execution_env = env.clone();
                let entrypoint = resolve_javascript_execution_entrypoint(
                    vm,
                    &cwd,
                    &payload.entrypoint,
                    &mut execution_env,
                );
                let context =
                    self.javascript_engine
                        .create_context(CreateJavascriptContextRequest {
                            vm_id: vm_id.clone(),
                            bootstrap_module: None,
                            compile_cache_root: Some(self.cache_root.join("node-compile-cache")),
                        });
                let execution = self
                    .javascript_engine
                    .start_execution(StartJavascriptExecutionRequest {
                        vm_id: vm_id.clone(),
                        context_id: context.context_id,
                        argv: std::iter::once(entrypoint)
                            .chain(payload.args.iter().cloned())
                            .collect(),
                        env: execution_env,
                        cwd: cwd.clone(),
                    })
                    .map_err(javascript_error)?;
                ActiveExecution::Javascript(execution)
            }
            GuestRuntimeKind::Python => {
                let python_file_path = python_file_entrypoint(&payload.entrypoint);
                let pyodide_dist_path = self
                    .python_engine
                    .bundled_pyodide_dist_path_for_vm(&vm_id)
                    .map_err(python_error)?;
                env.insert(
                    String::from("AGENT_OS_PYTHON_DISABLE_NODE_PERMISSION"),
                    String::from("1"),
                );
                let context = self
                    .python_engine
                    .create_context(CreatePythonContextRequest {
                        vm_id: vm_id.clone(),
                        pyodide_dist_path,
                    });
                let execution = self
                    .python_engine
                    .start_execution(StartPythonExecutionRequest {
                        vm_id: vm_id.clone(),
                        context_id: context.context_id,
                        code: payload.entrypoint.clone(),
                        file_path: python_file_path,
                        env: env.clone(),
                        cwd: cwd.clone(),
                    })
                    .map_err(python_error)?;
                ActiveExecution::Python(execution)
            }
            GuestRuntimeKind::WebAssembly => {
                apply_wasm_limit_env(&mut env, vm.kernel.resource_limits());
                let wasm_permission_tier = resolve_wasm_permission_tier(
                    vm,
                    None,
                    payload.wasm_permission_tier,
                    &payload.entrypoint,
                );
                let context = self.wasm_engine.create_context(CreateWasmContextRequest {
                    vm_id: vm_id.clone(),
                    module_path: Some(payload.entrypoint.clone()),
                });
                let execution = self
                    .wasm_engine
                    .start_execution(StartWasmExecutionRequest {
                        vm_id: vm_id.clone(),
                        context_id: context.context_id,
                        argv: payload.args.clone(),
                        env,
                        cwd: cwd.clone(),
                        permission_tier: execution_wasm_permission_tier(wasm_permission_tier),
                    })
                    .map_err(wasm_error)?;
                ActiveExecution::Wasm(execution)
            }
        };
        let child_pid = execution.child_pid();

        vm.active_processes.insert(
            payload.process_id.clone(),
            ActiveProcess::new_with_host_cwd(
                kernel_handle.pid(),
                kernel_handle,
                payload.runtime,
                execution,
                cwd,
            ),
        );
        self.bridge.emit_lifecycle(&vm_id, LifecycleState::Busy)?;

        Ok(DispatchResult {
            response: self.respond(
                request,
                ResponsePayload::ProcessStarted(ProcessStartedResponse {
                    process_id: payload.process_id,
                    pid: Some(child_pid),
                }),
            ),
            events: Vec::new(),
        })
    }

    fn write_stdin(
        &mut self,
        request: &RequestFrame,
        payload: WriteStdinRequest,
    ) -> Result<DispatchResult, SidecarError> {
        let (connection_id, session_id, vm_id) = self.vm_scope_for(&request.ownership)?;
        self.require_owned_vm(&connection_id, &session_id, &vm_id)?;

        let vm = self.vms.get_mut(&vm_id).expect("owned VM should exist");
        let process = vm
            .active_processes
            .get_mut(&payload.process_id)
            .ok_or_else(|| {
                SidecarError::InvalidState(format!(
                    "VM {vm_id} has no active process {}",
                    payload.process_id
                ))
            })?;
        process.execution.write_stdin(payload.chunk.as_bytes())?;

        Ok(DispatchResult {
            response: self.respond(
                request,
                ResponsePayload::StdinWritten(StdinWrittenResponse {
                    process_id: payload.process_id,
                    accepted_bytes: payload.chunk.len() as u64,
                }),
            ),
            events: Vec::new(),
        })
    }

    fn close_stdin(
        &mut self,
        request: &RequestFrame,
        payload: CloseStdinRequest,
    ) -> Result<DispatchResult, SidecarError> {
        let (connection_id, session_id, vm_id) = self.vm_scope_for(&request.ownership)?;
        self.require_owned_vm(&connection_id, &session_id, &vm_id)?;

        let vm = self.vms.get_mut(&vm_id).expect("owned VM should exist");
        let process = vm
            .active_processes
            .get_mut(&payload.process_id)
            .ok_or_else(|| {
                SidecarError::InvalidState(format!(
                    "VM {vm_id} has no active process {}",
                    payload.process_id
                ))
            })?;
        process.execution.close_stdin()?;

        Ok(DispatchResult {
            response: self.respond(
                request,
                ResponsePayload::StdinClosed(StdinClosedResponse {
                    process_id: payload.process_id,
                }),
            ),
            events: Vec::new(),
        })
    }

    fn kill_process(
        &mut self,
        request: &RequestFrame,
        payload: KillProcessRequest,
    ) -> Result<DispatchResult, SidecarError> {
        let (connection_id, session_id, vm_id) = self.vm_scope_for(&request.ownership)?;
        self.require_owned_vm(&connection_id, &session_id, &vm_id)?;
        self.kill_process_internal(&vm_id, &payload.process_id, &payload.signal)?;

        Ok(DispatchResult {
            response: self.respond(
                request,
                ResponsePayload::ProcessKilled(ProcessKilledResponse {
                    process_id: payload.process_id,
                }),
            ),
            events: Vec::new(),
        })
    }

    fn find_listener(
        &mut self,
        request: &RequestFrame,
        payload: FindListenerRequest,
    ) -> Result<DispatchResult, SidecarError> {
        let (connection_id, session_id, vm_id) = self.vm_scope_for(&request.ownership)?;
        self.require_owned_vm(&connection_id, &session_id, &vm_id)?;

        let listener =
            find_socket_state_entry(self.vms.get(&vm_id), SocketQueryKind::TcpListener, &payload)?;

        Ok(DispatchResult {
            response: self.respond(
                request,
                ResponsePayload::ListenerSnapshot(ListenerSnapshotResponse { listener }),
            ),
            events: Vec::new(),
        })
    }

    fn find_bound_udp(
        &mut self,
        request: &RequestFrame,
        payload: FindBoundUdpRequest,
    ) -> Result<DispatchResult, SidecarError> {
        let (connection_id, session_id, vm_id) = self.vm_scope_for(&request.ownership)?;
        self.require_owned_vm(&connection_id, &session_id, &vm_id)?;

        let lookup_request = FindListenerRequest {
            host: payload.host,
            port: payload.port,
            path: None,
        };
        let socket = find_socket_state_entry(
            self.vms.get(&vm_id),
            SocketQueryKind::UdpBound,
            &lookup_request,
        )?;

        Ok(DispatchResult {
            response: self.respond(
                request,
                ResponsePayload::BoundUdpSnapshot(BoundUdpSnapshotResponse { socket }),
            ),
            events: Vec::new(),
        })
    }

    fn get_signal_state(
        &mut self,
        request: &RequestFrame,
        payload: GetSignalStateRequest,
    ) -> Result<DispatchResult, SidecarError> {
        let (connection_id, session_id, vm_id) = self.vm_scope_for(&request.ownership)?;
        self.require_owned_vm(&connection_id, &session_id, &vm_id)?;

        let handlers = self
            .vms
            .get(&vm_id)
            .and_then(|vm| vm.signal_states.get(&payload.process_id))
            .cloned()
            .unwrap_or_default();

        Ok(DispatchResult {
            response: self.respond(
                request,
                ResponsePayload::SignalState(SignalStateResponse {
                    process_id: payload.process_id,
                    handlers,
                }),
            ),
            events: Vec::new(),
        })
    }

    fn get_zombie_timer_count(
        &mut self,
        request: &RequestFrame,
        _payload: GetZombieTimerCountRequest,
    ) -> Result<DispatchResult, SidecarError> {
        let (connection_id, session_id, vm_id) = self.vm_scope_for(&request.ownership)?;
        self.require_owned_vm(&connection_id, &session_id, &vm_id)?;

        let count = self
            .vms
            .get(&vm_id)
            .map(|vm| vm.kernel.zombie_timer_count() as u64)
            .unwrap_or_default();

        Ok(DispatchResult {
            response: self.respond(
                request,
                ResponsePayload::ZombieTimerCount(ZombieTimerCountResponse { count }),
            ),
            events: Vec::new(),
        })
    }

    fn dispose_session(
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
            events.extend(self.dispose_vm_internal(
                connection_id,
                session_id,
                &vm_id,
                reason.clone(),
            )?);
        }

        self.sessions.remove(session_id);
        if let Some(connection) = self.connections.get_mut(connection_id) {
            connection.sessions.remove(session_id);
        }
        Ok(events)
    }

    fn dispose_vm_internal(
        &mut self,
        connection_id: &str,
        session_id: &str,
        vm_id: &str,
        _reason: DisposeReason,
    ) -> Result<Vec<EventFrame>, SidecarError> {
        self.require_owned_vm(connection_id, session_id, vm_id)?;

        let mut events = vec![self.vm_lifecycle_event(
            connection_id,
            session_id,
            vm_id,
            VmLifecycleState::Disposing,
        )];
        self.terminate_vm_processes(vm_id, &mut events)?;

        let mut vm = self
            .vms
            .remove(vm_id)
            .expect("owned VM should exist before disposal");
        let snapshot = FilesystemSnapshot {
            format: String::from(ROOT_FILESYSTEM_SNAPSHOT_FORMAT),
            bytes: encode_root_snapshot(
                &vm.kernel.snapshot_root_filesystem().map_err(kernel_error)?,
            )
            .map_err(root_filesystem_error)?,
        };

        self.bridge
            .emit_lifecycle(vm_id, LifecycleState::Terminated)?;
        vm.kernel.dispose().map_err(kernel_error)?;
        self.bridge.with_mut(|bridge| {
            bridge.flush_filesystem_state(FlushFilesystemStateRequest {
                vm_id: vm_id.to_owned(),
                snapshot,
            })
        })?;
        self.bridge.clear_vm_permissions(vm_id)?;
        self.javascript_engine.dispose_vm(vm_id);
        self.python_engine.dispose_vm(vm_id);
        self.wasm_engine.dispose_vm(vm_id);

        if let Some(session) = self.sessions.get_mut(session_id) {
            session.vm_ids.remove(vm_id);
        }

        events.push(self.vm_lifecycle_event(
            connection_id,
            session_id,
            vm_id,
            VmLifecycleState::Disposed,
        ));
        Ok(events)
    }

    fn terminate_vm_processes(
        &mut self,
        vm_id: &str,
        events: &mut Vec<EventFrame>,
    ) -> Result<(), SidecarError> {
        let process_ids = self
            .vms
            .get(vm_id)
            .map(|vm| vm.active_processes.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        if process_ids.is_empty() {
            return Ok(());
        }

        for process_id in process_ids {
            if self
                .vms
                .get(vm_id)
                .is_some_and(|vm| vm.active_processes.contains_key(&process_id))
            {
                self.kill_process_internal(vm_id, &process_id, "SIGTERM")?;
            }
        }
        self.wait_for_vm_processes_to_exit(vm_id, DISPOSE_VM_SIGTERM_GRACE, events)?;

        if !self.vm_has_active_processes(vm_id) {
            return Ok(());
        }

        let remaining = self
            .vms
            .get(vm_id)
            .map(|vm| vm.active_processes.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        for process_id in remaining {
            if self
                .vms
                .get(vm_id)
                .is_some_and(|vm| vm.active_processes.contains_key(&process_id))
            {
                self.kill_process_internal(vm_id, &process_id, "SIGKILL")?;
            }
        }
        self.wait_for_vm_processes_to_exit(vm_id, DISPOSE_VM_SIGKILL_GRACE, events)?;

        if self.vm_has_active_processes(vm_id) {
            return Err(SidecarError::Execution(format!(
                "failed to terminate active guest executions for VM {vm_id}"
            )));
        }

        Ok(())
    }

    fn wait_for_vm_processes_to_exit(
        &mut self,
        vm_id: &str,
        timeout: Duration,
        events: &mut Vec<EventFrame>,
    ) -> Result<(), SidecarError> {
        let ownership = self.vm_ownership(vm_id)?;
        let deadline = Instant::now() + timeout;

        while self.vm_has_active_processes(vm_id) && Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if let Some(event) =
                self.poll_event(&ownership, remaining.min(Duration::from_millis(10)))?
            {
                events.push(event);
            }
        }

        Ok(())
    }

    fn kill_process_internal(
        &self,
        vm_id: &str,
        process_id: &str,
        signal: &str,
    ) -> Result<(), SidecarError> {
        let signal_name = signal.to_owned();
        let signal = parse_signal(signal)?;
        let vm = self
            .vms
            .get(vm_id)
            .ok_or_else(|| SidecarError::InvalidState(format!("unknown sidecar VM {vm_id}")))?;
        let process = vm.active_processes.get(process_id).ok_or_else(|| {
            SidecarError::InvalidState(format!("VM {vm_id} has no active process {process_id}"))
        })?;

        signal_runtime_process(process.execution.child_pid(), signal)?;
        emit_security_audit_event(
            &self.bridge,
            vm_id,
            "security.process.kill",
            audit_fields([
                (String::from("source"), String::from("control_plane")),
                (String::from("source_pid"), String::from("0")),
                (String::from("target_pid"), process.kernel_pid.to_string()),
                (String::from("process_id"), process_id.to_owned()),
                (String::from("signal"), signal_name),
                (
                    String::from("host_pid"),
                    process.execution.child_pid().to_string(),
                ),
            ]),
        );
        Ok(())
    }

    fn try_poll_event(
        &mut self,
        ownership: &OwnershipScope,
    ) -> Result<Option<EventFrame>, SidecarError> {
        let vm_ids = self.vm_ids_for_scope(ownership)?;
        for vm_id in vm_ids {
            let process_ids = self
                .vms
                .get(&vm_id)
                .map(|vm| vm.active_processes.keys().cloned().collect::<Vec<_>>())
                .unwrap_or_default();

            for process_id in process_ids {
                let event = {
                    let vm = self.vms.get_mut(&vm_id).expect("VM should still exist");
                    let process = vm
                        .active_processes
                        .get_mut(&process_id)
                        .expect("process should still exist");
                    process.execution.poll_event(Duration::ZERO)?
                };

                let Some(event) = event else {
                    continue;
                };

                return self.handle_execution_event(&vm_id, &process_id, event);
            }
        }

        Ok(None)
    }

    fn handle_execution_event(
        &mut self,
        vm_id: &str,
        process_id: &str,
        event: ActiveExecutionEvent,
    ) -> Result<Option<EventFrame>, SidecarError> {
        let (connection_id, session_id) = {
            let vm = self.vms.get(vm_id).expect("VM should exist");
            (vm.connection_id.clone(), vm.session_id.clone())
        };
        let ownership = OwnershipScope::vm(&connection_id, &session_id, vm_id);

        match event {
            ActiveExecutionEvent::Stdout(chunk) => Ok(Some(EventFrame::new(
                ownership,
                EventPayload::ProcessOutput(ProcessOutputEvent {
                    process_id: process_id.to_owned(),
                    channel: StreamChannel::Stdout,
                    chunk: String::from_utf8_lossy(&chunk).into_owned(),
                }),
            ))),
            ActiveExecutionEvent::Stderr(chunk) => Ok(Some(EventFrame::new(
                ownership,
                EventPayload::ProcessOutput(ProcessOutputEvent {
                    process_id: process_id.to_owned(),
                    channel: StreamChannel::Stderr,
                    chunk: String::from_utf8_lossy(&chunk).into_owned(),
                }),
            ))),
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
                let vm = self.vms.get_mut(vm_id).expect("VM should exist");
                vm.signal_states
                    .entry(process_id.to_owned())
                    .or_default()
                    .insert(signal, registration);
                Ok(None)
            }
            ActiveExecutionEvent::Exited(exit_code) => {
                let became_idle = {
                    let vm = self.vms.get_mut(vm_id).expect("VM should exist");
                    let mut process = vm
                        .active_processes
                        .remove(process_id)
                        .expect("process should still exist");
                    terminate_child_process_tree(&mut vm.kernel, &mut process);
                    process.kernel_handle.finish(exit_code);
                    let _ = vm.kernel.wait_and_reap(process.kernel_pid);
                    vm.active_processes.is_empty()
                };

                if became_idle {
                    self.bridge.emit_lifecycle(vm_id, LifecycleState::Ready)?;
                }

                Ok(Some(EventFrame::new(
                    ownership,
                    EventPayload::ProcessExited(ProcessExitedEvent {
                        process_id: process_id.to_owned(),
                        exit_code,
                    }),
                )))
            }
        }
    }

    fn handle_python_vfs_rpc_request(
        &mut self,
        vm_id: &str,
        process_id: &str,
        request: PythonVfsRpcRequest,
    ) -> Result<(), SidecarError> {
        let response = match normalize_python_vfs_rpc_path(&request.path) {
            Ok(path) => {
                let vm = self.vms.get_mut(vm_id).expect("VM should exist");
                match request.method {
                    PythonVfsRpcMethod::Read => vm
                        .kernel
                        .read_file(&path)
                        .map(|content| PythonVfsRpcResponsePayload::Read {
                            content_base64: base64::engine::general_purpose::STANDARD
                                .encode(content),
                        })
                        .map_err(kernel_error),
                    PythonVfsRpcMethod::Write => {
                        let content_base64 =
                            request.content_base64.as_deref().ok_or_else(|| {
                                SidecarError::InvalidState(format!(
                                    "python VFS fsWrite for {} requires contentBase64",
                                    path
                                ))
                            })?;
                        let bytes = base64::engine::general_purpose::STANDARD
                            .decode(content_base64)
                            .map_err(|error| {
                                SidecarError::InvalidState(format!(
                                    "invalid base64 python VFS content for {}: {error}",
                                    path
                                ))
                            })?;
                        vm.kernel
                            .write_file(&path, bytes)
                            .map(|()| PythonVfsRpcResponsePayload::Empty)
                            .map_err(kernel_error)
                    }
                    PythonVfsRpcMethod::Stat => vm
                        .kernel
                        .stat(&path)
                        .map(|stat| PythonVfsRpcResponsePayload::Stat {
                            stat: PythonVfsRpcStat {
                                mode: stat.mode,
                                size: stat.size,
                                is_directory: stat.is_directory,
                                is_symbolic_link: stat.is_symbolic_link,
                            },
                        })
                        .map_err(kernel_error),
                    PythonVfsRpcMethod::ReadDir => vm
                        .kernel
                        .read_dir(&path)
                        .map(|entries| PythonVfsRpcResponsePayload::ReadDir { entries })
                        .map_err(kernel_error),
                    PythonVfsRpcMethod::Mkdir => vm
                        .kernel
                        .mkdir(&path, request.recursive)
                        .map(|()| PythonVfsRpcResponsePayload::Empty)
                        .map_err(kernel_error),
                }
            }
            Err(error) => Err(error),
        };

        let vm = self.vms.get_mut(vm_id).expect("VM should exist");
        let process = vm
            .active_processes
            .get_mut(process_id)
            .expect("process should still exist");

        match response {
            Ok(payload) => process
                .execution
                .respond_python_vfs_rpc_success(request.id, payload),
            Err(error) => process.execution.respond_python_vfs_rpc_error(
                request.id,
                "ERR_AGENT_OS_PYTHON_VFS_RPC",
                error.to_string(),
            ),
        }
    }

    fn resolve_javascript_child_process_execution(
        &self,
        vm: &VmState,
        parent_host_cwd: &Path,
        request: &JavascriptChildProcessSpawnRequest,
    ) -> Result<ResolvedChildProcessExecution, SidecarError> {
        let guest_cwd = normalize_path(request.options.cwd.as_deref().unwrap_or("/root"));
        let host_cwd = request
            .options
            .cwd
            .as_deref()
            .map(|_| {
                host_path_for_child_guest_cwd(
                    vm,
                    &request.options.internal_bootstrap_env,
                    parent_host_cwd,
                    &guest_cwd,
                )
            })
            .unwrap_or_else(|| parent_host_cwd.to_path_buf());
        let mut env = vm.guest_env.clone();
        env.extend(request.options.env.clone());

        let (command, process_args) = if request.options.shell {
            if vm.command_guest_paths.contains_key("sh") {
                (
                    String::from("sh"),
                    vec![String::from("-c"), request.command.clone()],
                )
            } else {
                let tokens = tokenize_shell_free_command(&request.command);
                let Some((command, args)) = tokens.split_first() else {
                    return Err(SidecarError::InvalidState(String::from(
                        "child_process shell command must not be empty",
                    )));
                };
                (command.clone(), args.to_vec())
            }
        } else {
            (request.command.clone(), request.args.clone())
        };

        if matches!(command.as_str(), "node" | "npm" | "npx") {
            let Some(entrypoint_specifier) = process_args.first() else {
                return Err(SidecarError::InvalidState(format!(
                    "{command} child_process spawn requires an entrypoint"
                )));
            };

            let entrypoint = if is_path_like_specifier(entrypoint_specifier) {
                let guest_entrypoint = if entrypoint_specifier.starts_with('/') {
                    normalize_path(entrypoint_specifier)
                } else if entrypoint_specifier.starts_with("file:") {
                    normalize_path(entrypoint_specifier.trim_start_matches("file:"))
                } else {
                    normalize_path(&format!("{guest_cwd}/{entrypoint_specifier}"))
                };
                let host_entrypoint = if entrypoint_specifier.starts_with("./")
                    || entrypoint_specifier.starts_with("../")
                {
                    host_cwd.join(entrypoint_specifier)
                } else {
                    host_path_for_child_guest_path(
                        vm,
                        &request.options.internal_bootstrap_env,
                        &guest_entrypoint,
                    )
                };
                env.insert(String::from("AGENT_OS_GUEST_ENTRYPOINT"), guest_entrypoint);
                host_entrypoint.to_string_lossy().into_owned()
            } else {
                entrypoint_specifier.clone()
            };

            return Ok(ResolvedChildProcessExecution {
                command,
                process_args: process_args.clone(),
                runtime: GuestRuntimeKind::JavaScript,
                entrypoint,
                execution_args: process_args.iter().skip(1).cloned().collect(),
                env,
                guest_cwd,
                host_cwd,
                wasm_permission_tier: None,
            });
        }

        if command == PYTHON_COMMAND {
            return Err(SidecarError::InvalidState(String::from(
                "nested python child_process execution is not supported yet",
            )));
        }

        if is_path_like_specifier(&command) {
            let guest_entrypoint = if command.starts_with('/') {
                normalize_path(&command)
            } else if command.starts_with("file:") {
                normalize_path(command.trim_start_matches("file:"))
            } else {
                normalize_path(&format!("{guest_cwd}/{command}"))
            };
            let host_entrypoint = if command.starts_with("./") || command.starts_with("../") {
                host_cwd.join(&command)
            } else {
                host_path_for_child_guest_path(
                    vm,
                    &request.options.internal_bootstrap_env,
                    &guest_entrypoint,
                )
            };

            if is_javascript_entrypoint_path(&host_entrypoint) {
                env.insert(String::from("AGENT_OS_GUEST_ENTRYPOINT"), guest_entrypoint);
                return Ok(ResolvedChildProcessExecution {
                    command,
                    process_args: process_args.clone(),
                    runtime: GuestRuntimeKind::JavaScript,
                    entrypoint: host_entrypoint.to_string_lossy().into_owned(),
                    execution_args: process_args,
                    env,
                    guest_cwd,
                    host_cwd,
                    wasm_permission_tier: None,
                });
            }
        }

        let guest_entrypoint = vm
            .command_guest_paths
            .get(&command)
            .ok_or_else(|| SidecarError::InvalidState(format!("command not found: {command}")))?;
        let host_entrypoint = host_path_for_guest_path(vm, guest_entrypoint);
        let wasm_permission_tier = vm.command_permissions.get(&command).copied();

        Ok(ResolvedChildProcessExecution {
            command,
            process_args: process_args.clone(),
            runtime: GuestRuntimeKind::WebAssembly,
            entrypoint: host_entrypoint.to_string_lossy().into_owned(),
            execution_args: process_args,
            env,
            guest_cwd,
            host_cwd,
            wasm_permission_tier,
        })
    }

    fn spawn_javascript_child_process(
        &mut self,
        vm_id: &str,
        process_id: &str,
        request: JavascriptChildProcessSpawnRequest,
    ) -> Result<Value, SidecarError> {
        let resolved = {
            let vm = self.vms.get(vm_id).expect("VM should exist");
            let parent = vm
                .active_processes
                .get(process_id)
                .expect("process should still exist");
            self.resolve_javascript_child_process_execution(vm, &parent.host_cwd, &request)?
        };

        let (parent_kernel_pid, child_process_id) = {
            let vm = self.vms.get_mut(vm_id).expect("VM should exist");
            let process = vm
                .active_processes
                .get_mut(process_id)
                .expect("process should still exist");
            (process.kernel_pid, process.allocate_child_process_id())
        };

        let vm = self.vms.get_mut(vm_id).expect("VM should exist");
        let kernel_handle = vm
            .kernel
            .spawn_process(
                &resolved.command,
                resolved.process_args.clone(),
                SpawnOptions {
                    requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                    parent_pid: Some(parent_kernel_pid),
                    env: resolved.env.clone(),
                    cwd: Some(resolved.guest_cwd.clone()),
                },
            )
            .map_err(kernel_error)?;
        let kernel_pid = kernel_handle.pid();

        let mut execution_env = resolved.env.clone();

        let execution = match resolved.runtime {
            GuestRuntimeKind::JavaScript => {
                execution_env.extend(sanitize_javascript_child_process_internal_bootstrap_env(
                    &request.options.internal_bootstrap_env,
                ));
                execution_env.insert(
                    String::from("AGENT_OS_VIRTUAL_PROCESS_PID"),
                    kernel_pid.to_string(),
                );
                execution_env.insert(
                    String::from("AGENT_OS_VIRTUAL_PROCESS_PPID"),
                    parent_kernel_pid.to_string(),
                );
                let context =
                    self.javascript_engine
                        .create_context(CreateJavascriptContextRequest {
                            vm_id: vm_id.to_owned(),
                            bootstrap_module: None,
                            compile_cache_root: Some(self.cache_root.join("node-compile-cache")),
                        });
                let execution = self
                    .javascript_engine
                    .start_execution(StartJavascriptExecutionRequest {
                        vm_id: vm_id.to_owned(),
                        context_id: context.context_id,
                        argv: std::iter::once(resolved.entrypoint.clone())
                            .chain(resolved.execution_args.clone())
                            .collect(),
                        env: execution_env,
                        cwd: resolved.host_cwd.clone(),
                    })
                    .map_err(javascript_error)?;
                ActiveExecution::Javascript(execution)
            }
            GuestRuntimeKind::WebAssembly => {
                apply_wasm_limit_env(&mut execution_env, vm.kernel.resource_limits());
                let context = self.wasm_engine.create_context(CreateWasmContextRequest {
                    vm_id: vm_id.to_owned(),
                    module_path: Some(resolved.entrypoint.clone()),
                });
                let execution = self
                    .wasm_engine
                    .start_execution(StartWasmExecutionRequest {
                        vm_id: vm_id.to_owned(),
                        context_id: context.context_id,
                        argv: resolved.execution_args.clone(),
                        env: execution_env,
                        cwd: resolved.host_cwd.clone(),
                        permission_tier: execution_wasm_permission_tier(
                            resolved
                                .wasm_permission_tier
                                .unwrap_or(WasmPermissionTier::Full),
                        ),
                    })
                    .map_err(wasm_error)?;
                ActiveExecution::Wasm(execution)
            }
            GuestRuntimeKind::Python => unreachable!("python child_process execution is rejected"),
        };

        vm.active_processes
            .get_mut(process_id)
            .expect("process should still exist")
            .child_processes
            .insert(
                child_process_id.clone(),
                ActiveProcess::new_with_host_cwd(
                    kernel_pid,
                    kernel_handle,
                    resolved.runtime,
                    execution,
                    resolved.host_cwd.clone(),
                ),
            );

        Ok(json!({
            "childId": child_process_id,
            "pid": kernel_pid,
            "command": resolved.command,
            "args": resolved.process_args,
        }))
    }

    fn poll_javascript_child_process(
        &mut self,
        vm_id: &str,
        process_id: &str,
        child_process_id: &str,
        wait_ms: u64,
    ) -> Result<Value, SidecarError> {
        loop {
            let event = {
                let vm = self.vms.get_mut(vm_id).expect("VM should exist");
                let child = vm
                    .active_processes
                    .get_mut(process_id)
                    .expect("process should still exist")
                    .child_processes
                    .get_mut(child_process_id)
                    .ok_or_else(|| {
                        SidecarError::InvalidState(format!(
                            "unknown child process {child_process_id}"
                        ))
                    })?;
                child
                    .execution
                    .poll_event(Duration::from_millis(wait_ms))
                    .map_err(|error| SidecarError::Execution(error.to_string()))?
            };

            let Some(event) = event else {
                return Ok(Value::Null);
            };

            match event {
                ActiveExecutionEvent::Stdout(chunk) => {
                    return Ok(json!({
                        "type": "stdout",
                        "data": javascript_sync_rpc_bytes_value(&chunk),
                    }));
                }
                ActiveExecutionEvent::Stderr(chunk) => {
                    return Ok(json!({
                        "type": "stderr",
                        "data": javascript_sync_rpc_bytes_value(&chunk),
                    }));
                }
                ActiveExecutionEvent::Exited(exit_code) => {
                    let vm = self.vms.get_mut(vm_id).expect("VM should exist");
                    let parent_runtime_pid = vm
                        .active_processes
                        .get(process_id)
                        .expect("process should still exist")
                        .execution
                        .child_pid();
                    let child = vm
                        .active_processes
                        .get_mut(process_id)
                        .expect("process should still exist")
                        .child_processes
                        .remove(child_process_id)
                        .expect("child process should still exist");
                    child.kernel_handle.finish(exit_code);
                    let _ = vm.kernel.wait_and_reap(child.kernel_pid);
                    signal_runtime_process(parent_runtime_pid, libc::SIGCHLD)?;
                    return Ok(json!({
                        "type": "exit",
                        "exitCode": exit_code,
                    }));
                }
                ActiveExecutionEvent::JavascriptSyncRpcRequest(request) => {
                    let response = {
                        let vm = self.vms.get_mut(vm_id).expect("VM should exist");
                        if request.method.starts_with("child_process.") {
                            Err(SidecarError::InvalidState(String::from(
                                "nested child_process calls from a child process are not supported yet",
                            )))
                        } else {
                            let resource_limits = vm.kernel.resource_limits().clone();
                            let network_counts = vm_network_resource_counts(vm);
                            let socket_paths = build_javascript_socket_path_context(vm)?;
                            let child = vm
                                .active_processes
                                .get_mut(process_id)
                                .expect("process should still exist")
                                .child_processes
                                .get_mut(child_process_id)
                                .expect("child process should still exist");
                            service_javascript_sync_rpc(
                                &self.bridge,
                                vm_id,
                                &vm.dns,
                                &socket_paths,
                                &mut vm.kernel,
                                child,
                                &request,
                                &resource_limits,
                                network_counts,
                            )
                        }
                    };

                    let vm = self.vms.get_mut(vm_id).expect("VM should exist");
                    let child = vm
                        .active_processes
                        .get_mut(process_id)
                        .expect("process should still exist")
                        .child_processes
                        .get_mut(child_process_id)
                        .expect("child process should still exist");
                    match response {
                        Ok(result) => child
                            .execution
                            .respond_javascript_sync_rpc_success(request.id, result)
                            .or_else(ignore_stale_javascript_sync_rpc_response)?,
                        Err(error) => child
                            .execution
                            .respond_javascript_sync_rpc_error(
                                request.id,
                                "ERR_AGENT_OS_NODE_SYNC_RPC",
                                error.to_string(),
                            )
                            .or_else(ignore_stale_javascript_sync_rpc_response)?,
                    }
                }
                ActiveExecutionEvent::PythonVfsRpcRequest(_) => {
                    return Err(SidecarError::InvalidState(String::from(
                        "nested Python child_process execution is not supported yet",
                    )));
                }
                ActiveExecutionEvent::SignalState { .. } => {}
            }
        }
    }

    fn write_javascript_child_process_stdin(
        &mut self,
        vm_id: &str,
        process_id: &str,
        child_process_id: &str,
        chunk: &[u8],
    ) -> Result<(), SidecarError> {
        let vm = self.vms.get_mut(vm_id).expect("VM should exist");
        let child = vm
            .active_processes
            .get_mut(process_id)
            .expect("process should still exist")
            .child_processes
            .get_mut(child_process_id)
            .ok_or_else(|| {
                SidecarError::InvalidState(format!("unknown child process {child_process_id}"))
            })?;
        child.execution.write_stdin(chunk)
    }

    fn close_javascript_child_process_stdin(
        &mut self,
        vm_id: &str,
        process_id: &str,
        child_process_id: &str,
    ) -> Result<(), SidecarError> {
        let vm = self.vms.get_mut(vm_id).expect("VM should exist");
        let child = vm
            .active_processes
            .get_mut(process_id)
            .expect("process should still exist")
            .child_processes
            .get_mut(child_process_id)
            .ok_or_else(|| {
                SidecarError::InvalidState(format!("unknown child process {child_process_id}"))
            })?;
        child.execution.close_stdin()
    }

    fn kill_javascript_child_process(
        &mut self,
        vm_id: &str,
        process_id: &str,
        child_process_id: &str,
        signal: &str,
    ) -> Result<(), SidecarError> {
        let signal_name = signal.to_owned();
        let signal = parse_signal(signal)?;
        let vm = self.vms.get_mut(vm_id).expect("VM should exist");
        let process = vm
            .active_processes
            .get_mut(process_id)
            .expect("process should still exist");
        let source_pid = process.kernel_pid;
        let child = process
            .child_processes
            .get_mut(child_process_id)
            .ok_or_else(|| {
                SidecarError::InvalidState(format!("unknown child process {child_process_id}"))
            })?;
        vm.kernel
            .kill_process(EXECUTION_DRIVER_NAME, child.kernel_pid, signal)
            .map_err(kernel_error)?;
        emit_security_audit_event(
            &self.bridge,
            vm_id,
            "security.process.kill",
            audit_fields([
                (String::from("source"), String::from("guest_child_process")),
                (String::from("source_pid"), source_pid.to_string()),
                (String::from("target_pid"), child.kernel_pid.to_string()),
                (String::from("process_id"), process_id.to_owned()),
                (
                    String::from("child_process_id"),
                    child_process_id.to_owned(),
                ),
                (String::from("signal"), signal_name),
            ]),
        );
        Ok(())
    }

    fn handle_javascript_sync_rpc_request(
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

    fn vm_ids_for_scope(&self, ownership: &OwnershipScope) -> Result<Vec<String>, SidecarError> {
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

    fn vm_ownership(&self, vm_id: &str) -> Result<OwnershipScope, SidecarError> {
        let vm = self
            .vms
            .get(vm_id)
            .ok_or_else(|| SidecarError::InvalidState(format!("unknown sidecar VM {vm_id}")))?;
        Ok(OwnershipScope::vm(&vm.connection_id, &vm.session_id, vm_id))
    }

    fn vm_has_active_processes(&self, vm_id: &str) -> bool {
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

    fn require_owned_session(
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

    fn require_owned_vm(
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

    fn session_scope_for(
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

    fn vm_scope_for(
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
        request_id: u64,
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

    fn respond(&self, request: &RequestFrame, payload: ResponsePayload) -> ResponseFrame {
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

    fn vm_lifecycle_event(
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

fn audit_fields<I, K, V>(fields: I) -> BTreeMap<String, String>
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

fn emit_structured_event<B>(
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

fn emit_security_audit_event<B>(
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

fn filesystem_operation_label(operation: FsOperation) -> &'static str {
    match operation {
        FsOperation::Read => "read",
        FsOperation::Write => "write",
        FsOperation::Mkdir => "mkdir",
        FsOperation::CreateDir => "createDir",
        FsOperation::ReadDir => "readdir",
        FsOperation::Stat => "stat",
        FsOperation::Remove => "rm",
        FsOperation::Rename => "rename",
        FsOperation::Exists => "exists",
        FsOperation::Symlink => "symlink",
        FsOperation::ReadLink => "readlink",
        FsOperation::Link => "link",
        FsOperation::Chmod => "chmod",
        FsOperation::Chown => "chown",
        FsOperation::Utimes => "utimes",
        FsOperation::Truncate => "truncate",
        FsOperation::MountSensitive => "mount",
    }
}

fn map_wasm_signal_registration(
    registration: agent_os_execution::wasm::WasmSignalHandlerRegistration,
) -> SignalHandlerRegistration {
    SignalHandlerRegistration {
        action: match registration.action {
            agent_os_execution::wasm::WasmSignalDispositionAction::Default => {
                crate::protocol::SignalDispositionAction::Default
            }
            agent_os_execution::wasm::WasmSignalDispositionAction::Ignore => {
                crate::protocol::SignalDispositionAction::Ignore
            }
            agent_os_execution::wasm::WasmSignalDispositionAction::User => {
                crate::protocol::SignalDispositionAction::User
            }
        },
        mask: registration.mask,
        flags: registration.flags,
    }
}

fn map_node_signal_registration(
    registration: NodeSignalHandlerRegistration,
) -> SignalHandlerRegistration {
    SignalHandlerRegistration {
        action: match registration.action {
            NodeSignalDispositionAction::Default => SignalDispositionAction::Default,
            NodeSignalDispositionAction::Ignore => SignalDispositionAction::Ignore,
            NodeSignalDispositionAction::User => SignalDispositionAction::User,
        },
        mask: registration.mask,
        flags: registration.flags,
    }
}

fn bridge_permissions<B>(bridge: SharedBridge<B>, vm_id: &str) -> Permissions
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let vm_id = vm_id.to_owned();

    let filesystem_bridge = bridge.clone();
    let filesystem_vm_id = vm_id.clone();
    let network_bridge = bridge.clone();
    let network_vm_id = vm_id.clone();
    let command_bridge = bridge.clone();
    let command_vm_id = vm_id.clone();
    let environment_bridge = bridge;

    Permissions {
        filesystem: Some(Arc::new(move |request: &FsAccessRequest| {
            let access = match request.op {
                FsOperation::Read => FilesystemAccess::Read,
                FsOperation::Write => FilesystemAccess::Write,
                FsOperation::Mkdir | FsOperation::CreateDir => FilesystemAccess::CreateDir,
                FsOperation::ReadDir => FilesystemAccess::ReadDir,
                FsOperation::Stat | FsOperation::Exists => FilesystemAccess::Stat,
                FsOperation::Remove => FilesystemAccess::Remove,
                FsOperation::Rename => FilesystemAccess::Rename,
                FsOperation::Symlink => FilesystemAccess::Symlink,
                FsOperation::ReadLink => FilesystemAccess::Read,
                FsOperation::Link => FilesystemAccess::Write,
                FsOperation::Chmod => FilesystemAccess::Write,
                FsOperation::Chown => FilesystemAccess::Write,
                FsOperation::Utimes => FilesystemAccess::Write,
                FsOperation::Truncate => FilesystemAccess::Write,
                FsOperation::MountSensitive => FilesystemAccess::Write,
            };
            let policy = if request.op == FsOperation::MountSensitive {
                "fs.mount_sensitive"
            } else {
                filesystem_permission_capability(access)
            };
            let decision = if request.op == FsOperation::MountSensitive {
                filesystem_bridge
                    .static_permission_decision(&filesystem_vm_id, policy, "fs")
                    .unwrap_or_else(PermissionDecision::allow)
            } else {
                filesystem_bridge.filesystem_decision(&filesystem_vm_id, &request.path, access)
            };

            if !decision.allow {
                emit_security_audit_event(
                    &filesystem_bridge,
                    &filesystem_vm_id,
                    "security.permission.denied",
                    audit_fields([
                        (
                            String::from("operation"),
                            filesystem_operation_label(request.op).to_owned(),
                        ),
                        (String::from("path"), request.path.clone()),
                        (String::from("policy"), String::from(policy)),
                        (
                            String::from("reason"),
                            decision
                                .reason
                                .clone()
                                .unwrap_or_else(|| String::from("permission denied")),
                        ),
                    ]),
                );
            }

            decision
        })),
        network: Some(Arc::new(move |request: &NetworkAccessRequest| {
            network_bridge.network_decision(&network_vm_id, request)
        })),
        child_process: Some(Arc::new(move |request: &CommandAccessRequest| {
            command_bridge.command_decision(&command_vm_id, request)
        })),
        environment: Some(Arc::new(move |request: &EnvAccessRequest| {
            environment_bridge.environment_decision(&vm_id, request)
        })),
    }
}

fn build_mount_plugin_registry<B>()
-> Result<FileSystemPluginRegistry<MountPluginContext<B>>, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let mut registry = FileSystemPluginRegistry::new();
    registry.register(MemoryMountPlugin).map_err(plugin_error)?;
    registry
        .register(HostDirMountPlugin)
        .map_err(plugin_error)?;
    registry
        .register(SandboxAgentMountPlugin)
        .map_err(plugin_error)?;
    registry.register(S3MountPlugin).map_err(plugin_error)?;
    registry
        .register(GoogleDriveMountPlugin)
        .map_err(plugin_error)?;
    registry
        .register(JsBridgeMountPlugin)
        .map_err(plugin_error)?;
    Ok(registry)
}

fn reconcile_mounts<B>(
    mount_plugins: &FileSystemPluginRegistry<MountPluginContext<B>>,
    vm: &mut VmState,
    mounts: &[crate::protocol::MountDescriptor],
    context: MountPluginContext<B>,
) -> Result<(), SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    for existing in &vm.configuration.mounts {
        match vm.kernel.unmount_filesystem(&existing.guest_path) {
            Ok(()) => emit_security_audit_event(
                &context.bridge,
                &context.vm_id,
                "security.mount.unmounted",
                audit_fields([
                    (String::from("guest_path"), existing.guest_path.clone()),
                    (String::from("plugin_id"), existing.plugin.id.clone()),
                    (String::from("read_only"), existing.read_only.to_string()),
                ]),
            ),
            Err(error) if error.code() == "EINVAL" => {}
            Err(error) => return Err(kernel_error(error)),
        }
    }

    for mount in mounts {
        let filesystem = mount_plugins
            .open(
                &mount.plugin.id,
                OpenFileSystemPluginRequest {
                    vm_id: &context.vm_id,
                    guest_path: &mount.guest_path,
                    read_only: mount.read_only,
                    config: &mount.plugin.config,
                    context: &context,
                },
            )
            .map_err(plugin_error)?;

        vm.kernel
            .mount_boxed_filesystem(
                &mount.guest_path,
                filesystem,
                MountOptions::new(mount.plugin.id.clone()).read_only(mount.read_only),
            )
            .map_err(kernel_error)?;
        emit_security_audit_event(
            &context.bridge,
            &context.vm_id,
            "security.mount.mounted",
            audit_fields([
                (String::from("guest_path"), mount.guest_path.clone()),
                (String::from("plugin_id"), mount.plugin.id.clone()),
                (String::from("read_only"), mount.read_only.to_string()),
            ]),
        );
    }

    Ok(())
}

fn resolve_cwd(value: Option<&String>) -> Result<PathBuf, SidecarError> {
    match value {
        Some(path) => {
            let cwd = PathBuf::from(path);
            let resolved = if cwd.is_absolute() {
                cwd
            } else {
                std::env::current_dir()
                    .map_err(|error| {
                        SidecarError::Io(format!("failed to resolve current directory: {error}"))
                    })?
                    .join(cwd)
            };
            Ok(resolved)
        }
        None => std::env::current_dir().map_err(|error| {
            SidecarError::Io(format!("failed to resolve current directory: {error}"))
        }),
    }
}

fn resolve_execution_cwd(vm: &VmState, value: Option<&str>) -> Result<PathBuf, SidecarError> {
    let sandbox_root = normalize_host_path(&vm.cwd);
    let candidate = match value {
        Some(path) => {
            let path = PathBuf::from(path);
            if path.is_absolute() {
                path
            } else {
                sandbox_root.join(path)
            }
        }
        None => sandbox_root.clone(),
    };
    let normalized = normalize_host_path(&candidate);

    if !path_is_within_root(&normalized, &sandbox_root) {
        return Err(SidecarError::InvalidState(format!(
            "execute cwd {} escapes VM sandbox root {}",
            normalized.display(),
            sandbox_root.display()
        )));
    }

    Ok(normalized)
}

fn extract_guest_env(metadata: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    metadata
        .iter()
        .filter_map(|(key, value)| {
            key.strip_prefix("env.")
                .map(|env_key| (env_key.to_owned(), value.clone()))
        })
        .collect()
}

fn apply_wasm_limit_env(env: &mut BTreeMap<String, String>, limits: &ResourceLimits) {
    if let Some(limit) = limits.max_wasm_fuel {
        env.insert(String::from(WASM_MAX_FUEL_ENV), limit.to_string());
    }
    if let Some(limit) = limits.max_wasm_memory_bytes {
        env.insert(String::from(WASM_MAX_MEMORY_BYTES_ENV), limit.to_string());
    }
    if let Some(limit) = limits.max_wasm_stack_bytes {
        env.insert(String::from(WASM_MAX_STACK_BYTES_ENV), limit.to_string());
    }
}

fn parse_resource_limits(
    metadata: &BTreeMap<String, String>,
) -> Result<ResourceLimits, SidecarError> {
    let mut limits = ResourceLimits::default();
    if metadata.contains_key("resource.max_processes") {
        limits.max_processes = parse_resource_limit(metadata, "resource.max_processes")?;
    }
    if metadata.contains_key("resource.max_open_fds") {
        limits.max_open_fds = parse_resource_limit(metadata, "resource.max_open_fds")?;
    }
    if metadata.contains_key("resource.max_pipes") {
        limits.max_pipes = parse_resource_limit(metadata, "resource.max_pipes")?;
    }
    if metadata.contains_key("resource.max_ptys") {
        limits.max_ptys = parse_resource_limit(metadata, "resource.max_ptys")?;
    }
    if metadata.contains_key("resource.max_sockets") {
        limits.max_sockets = parse_resource_limit(metadata, "resource.max_sockets")?;
    }
    if metadata.contains_key("resource.max_connections") {
        limits.max_connections = parse_resource_limit(metadata, "resource.max_connections")?;
    }
    if metadata.contains_key("resource.max_filesystem_bytes") {
        limits.max_filesystem_bytes =
            parse_resource_limit_u64(metadata, "resource.max_filesystem_bytes")?;
    }
    if metadata.contains_key("resource.max_inode_count") {
        limits.max_inode_count = parse_resource_limit(metadata, "resource.max_inode_count")?;
    }
    if metadata.contains_key("resource.max_blocking_read_ms") {
        limits.max_blocking_read_ms =
            parse_resource_limit_u64(metadata, "resource.max_blocking_read_ms")?;
    }
    if metadata.contains_key("resource.max_pread_bytes") {
        limits.max_pread_bytes = parse_resource_limit(metadata, "resource.max_pread_bytes")?;
    }
    if metadata.contains_key("resource.max_fd_write_bytes") {
        limits.max_fd_write_bytes = parse_resource_limit(metadata, "resource.max_fd_write_bytes")?;
    }
    if metadata.contains_key("resource.max_process_argv_bytes") {
        limits.max_process_argv_bytes =
            parse_resource_limit(metadata, "resource.max_process_argv_bytes")?;
    }
    if metadata.contains_key("resource.max_process_env_bytes") {
        limits.max_process_env_bytes =
            parse_resource_limit(metadata, "resource.max_process_env_bytes")?;
    }
    if metadata.contains_key("resource.max_readdir_entries") {
        limits.max_readdir_entries =
            parse_resource_limit(metadata, "resource.max_readdir_entries")?;
    }
    if metadata.contains_key("resource.max_wasm_fuel") {
        limits.max_wasm_fuel = parse_resource_limit_u64(metadata, "resource.max_wasm_fuel")?;
    }
    if metadata.contains_key("resource.max_wasm_memory_bytes") {
        limits.max_wasm_memory_bytes =
            parse_resource_limit_u64(metadata, "resource.max_wasm_memory_bytes")?;
    }
    if metadata.contains_key("resource.max_wasm_stack_bytes") {
        limits.max_wasm_stack_bytes =
            parse_resource_limit(metadata, "resource.max_wasm_stack_bytes")?;
    }
    Ok(limits)
}

fn parse_resource_limit(
    metadata: &BTreeMap<String, String>,
    key: &str,
) -> Result<Option<usize>, SidecarError> {
    let Some(value) = metadata.get(key) else {
        return Ok(None);
    };

    let parsed = value.parse::<usize>().map_err(|error| {
        SidecarError::InvalidState(format!("invalid resource limit {key}={value}: {error}"))
    })?;
    Ok(Some(parsed))
}

fn parse_resource_limit_u64(
    metadata: &BTreeMap<String, String>,
    key: &str,
) -> Result<Option<u64>, SidecarError> {
    let Some(value) = metadata.get(key) else {
        return Ok(None);
    };

    let parsed = value.parse::<u64>().map_err(|error| {
        SidecarError::InvalidState(format!("invalid resource limit {key}={value}: {error}"))
    })?;
    Ok(Some(parsed))
}

fn parse_vm_dns_config(metadata: &BTreeMap<String, String>) -> Result<VmDnsConfig, SidecarError> {
    let mut config = VmDnsConfig::default();

    if let Some(value) = metadata.get(VM_DNS_SERVERS_METADATA_KEY) {
        config.name_servers = value
            .split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .map(parse_vm_dns_nameserver)
            .collect::<Result<Vec<_>, _>>()?;
    }

    for (key, value) in metadata {
        let Some(hostname) = key.strip_prefix(VM_DNS_OVERRIDE_METADATA_PREFIX) else {
            continue;
        };
        let normalized_hostname = normalize_dns_hostname(hostname)?;
        let addresses = value
            .split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .map(|entry| {
                entry.parse::<IpAddr>().map_err(|error| {
                    SidecarError::InvalidState(format!(
                        "invalid DNS override {key}={value}: {error}"
                    ))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        if addresses.is_empty() {
            return Err(SidecarError::InvalidState(format!(
                "DNS override {key} must contain at least one IP address"
            )));
        }
        config.overrides.insert(normalized_hostname, addresses);
    }

    Ok(config)
}

fn parse_vm_listen_policy(
    metadata: &BTreeMap<String, String>,
) -> Result<VmListenPolicy, SidecarError> {
    let mut policy = VmListenPolicy::default();

    if let Some(value) = metadata.get(VM_LISTEN_PORT_MIN_METADATA_KEY) {
        policy.port_min = parse_listen_port_metadata(VM_LISTEN_PORT_MIN_METADATA_KEY, value)?;
    }
    if let Some(value) = metadata.get(VM_LISTEN_PORT_MAX_METADATA_KEY) {
        policy.port_max = parse_listen_port_metadata(VM_LISTEN_PORT_MAX_METADATA_KEY, value)?;
    }
    if policy.port_min > policy.port_max {
        return Err(SidecarError::InvalidState(format!(
            "invalid listen port range {}={} exceeds {}={}",
            VM_LISTEN_PORT_MIN_METADATA_KEY,
            policy.port_min,
            VM_LISTEN_PORT_MAX_METADATA_KEY,
            policy.port_max
        )));
    }
    if let Some(value) = metadata.get(VM_LISTEN_ALLOW_PRIVILEGED_METADATA_KEY) {
        policy.allow_privileged = value.parse::<bool>().map_err(|error| {
            SidecarError::InvalidState(format!(
                "invalid {}={value}: {error}",
                VM_LISTEN_ALLOW_PRIVILEGED_METADATA_KEY
            ))
        })?;
    }

    Ok(policy)
}

fn parse_listen_port_metadata(key: &str, value: &str) -> Result<u16, SidecarError> {
    let parsed = value
        .parse::<u16>()
        .map_err(|error| SidecarError::InvalidState(format!("invalid {key}={value}: {error}")))?;
    if parsed == 0 {
        return Err(SidecarError::InvalidState(format!(
            "{key} must be between 1 and 65535"
        )));
    }
    Ok(parsed)
}

fn parse_loopback_exempt_ports(
    env: &BTreeMap<String, String>,
) -> Result<BTreeSet<u16>, SidecarError> {
    let Some(value) = env.get(LOOPBACK_EXEMPT_PORTS_ENV) else {
        return Ok(BTreeSet::new());
    };

    let parsed = serde_json::from_str::<Vec<Value>>(value).map_err(|error| {
        SidecarError::InvalidState(format!(
            "invalid {LOOPBACK_EXEMPT_PORTS_ENV}={value}: {error}"
        ))
    })?;

    let mut ports = BTreeSet::new();
    for entry in parsed {
        let port = match entry {
            Value::String(raw) => raw.parse::<u16>().map_err(|error| {
                SidecarError::InvalidState(format!(
                    "invalid {LOOPBACK_EXEMPT_PORTS_ENV} entry {raw:?}: {error}"
                ))
            })?,
            Value::Number(raw) => raw
                .as_u64()
                .and_then(|port| u16::try_from(port).ok())
                .ok_or_else(|| {
                    SidecarError::InvalidState(format!(
                        "invalid {LOOPBACK_EXEMPT_PORTS_ENV} entry {raw}"
                    ))
                })?,
            other => {
                return Err(SidecarError::InvalidState(format!(
                    "invalid {LOOPBACK_EXEMPT_PORTS_ENV} entry {other:?}"
                )));
            }
        };
        ports.insert(port);
    }

    Ok(ports)
}

fn parse_vm_dns_nameserver(value: &str) -> Result<SocketAddr, SidecarError> {
    if let Ok(address) = value.parse::<SocketAddr>() {
        return Ok(address);
    }
    if let Ok(ip) = value.parse::<IpAddr>() {
        return Ok(SocketAddr::new(ip, 53));
    }
    Err(SidecarError::InvalidState(format!(
        "invalid {} entry {value}; expected IP or IP:port",
        VM_DNS_SERVERS_METADATA_KEY
    )))
}

fn normalize_dns_hostname(hostname: &str) -> Result<String, SidecarError> {
    let normalized = hostname.trim().trim_end_matches('.').to_ascii_lowercase();
    if normalized.is_empty() {
        return Err(SidecarError::InvalidState(String::from(
            "DNS hostname must not be empty",
        )));
    }
    Ok(normalized)
}

fn vm_dns_resolver_config(dns: &VmDnsConfig) -> Option<ResolverConfig> {
    if dns.name_servers.is_empty() {
        return None;
    }

    let name_servers = dns
        .name_servers
        .iter()
        .map(|server| {
            let mut config = NameServerConfig::udp_and_tcp(server.ip());
            for connection in &mut config.connections {
                connection.port = server.port();
                connection.bind_addr = Some(SocketAddr::new(
                    if server.is_ipv6() {
                        IpAddr::V6(Ipv6Addr::UNSPECIFIED)
                    } else {
                        IpAddr::V4(Ipv4Addr::UNSPECIFIED)
                    },
                    0,
                ));
            }
            config
        })
        .collect();
    Some(ResolverConfig::from_parts(None, vec![], name_servers))
}

fn resolve_dns_with_sidecar_resolver(
    dns: &VmDnsConfig,
    hostname: &str,
) -> Result<Vec<IpAddr>, SidecarError> {
    let runtime = tokio::runtime::Runtime::new().map_err(|error| {
        SidecarError::Execution(format!("failed to create DNS runtime: {error}"))
    })?;

    runtime.block_on(async {
        let builder = if let Some(config) = vm_dns_resolver_config(dns) {
            TokioResolver::builder_with_config(config, TokioRuntimeProvider::default())
        } else {
            TokioResolver::builder_tokio().map_err(|error| {
                SidecarError::Execution(format!(
                    "failed to initialize DNS resolver from system configuration: {error}"
                ))
            })?
        };

        let resolver = builder.build().map_err(|error| {
            SidecarError::Execution(format!("failed to build DNS resolver: {error}"))
        })?;
        let lookup = resolver.lookup_ip(hostname).await.map_err(|error| {
            SidecarError::Execution(format!("failed to resolve DNS address {hostname}: {error}"))
        })?;

        let mut addresses = Vec::new();
        let mut seen = BTreeSet::new();
        for ip in lookup.iter() {
            if seen.insert(ip) {
                addresses.push(ip);
            }
        }

        if addresses.is_empty() {
            return Err(SidecarError::Execution(format!(
                "failed to resolve DNS address {hostname}"
            )));
        }

        Ok(addresses)
    })
}

fn emit_dns_resolution_event<B>(
    bridge: &SharedBridge<B>,
    vm_id: &str,
    hostname: &str,
    source: DnsResolutionSource,
    addresses: &[IpAddr],
    dns: &VmDnsConfig,
) where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let _ = emit_structured_event(
        bridge,
        vm_id,
        "network.dns.resolved",
        audit_fields([
            ("hostname", hostname.to_owned()),
            ("source", source.as_str().to_owned()),
            (
                "addresses",
                addresses
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(","),
            ),
            ("address_count", addresses.len().to_string()),
            ("resolver_count", dns.name_servers.len().to_string()),
            (
                "resolvers",
                dns.name_servers
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(","),
            ),
        ]),
    );
}

fn emit_dns_resolution_failure_event<B>(
    bridge: &SharedBridge<B>,
    vm_id: &str,
    hostname: &str,
    dns: &VmDnsConfig,
    error: &SidecarError,
) where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let _ = emit_structured_event(
        bridge,
        vm_id,
        "network.dns.resolve_failed",
        audit_fields([
            ("hostname", hostname.to_owned()),
            ("reason", error.to_string()),
            ("resolver_count", dns.name_servers.len().to_string()),
            (
                "resolvers",
                dns.name_servers
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(","),
            ),
        ]),
    );
}

fn build_root_filesystem(
    descriptor: &RootFilesystemDescriptor,
    loaded_snapshot: Option<&FilesystemSnapshot>,
) -> Result<RootFileSystem, SidecarError> {
    let restored_snapshot = match loaded_snapshot {
        Some(snapshot) if snapshot.format == ROOT_FILESYSTEM_SNAPSHOT_FORMAT => {
            Some(decode_root_snapshot(&snapshot.bytes).map_err(root_filesystem_error)?)
        }
        _ => None,
    };
    let has_restored_snapshot = restored_snapshot.is_some();

    let lowers = if let Some(snapshot) = restored_snapshot {
        vec![snapshot]
    } else {
        descriptor
            .lowers
            .iter()
            .map(convert_root_lower_descriptor)
            .collect::<Result<Vec<_>, _>>()?
    };

    RootFileSystem::from_descriptor(KernelRootFilesystemDescriptor {
        mode: match descriptor.mode {
            RootFilesystemMode::Ephemeral => KernelRootFilesystemMode::Ephemeral,
            RootFilesystemMode::ReadOnly => KernelRootFilesystemMode::ReadOnly,
        },
        disable_default_base_layer: has_restored_snapshot || descriptor.disable_default_base_layer,
        lowers,
        bootstrap_entries: descriptor
            .bootstrap_entries
            .iter()
            .map(convert_root_filesystem_entry)
            .collect::<Result<Vec<_>, _>>()?,
    })
    .map_err(root_filesystem_error)
}

fn convert_root_lower_descriptor(
    lower: &RootFilesystemLowerDescriptor,
) -> Result<RootFilesystemSnapshot, SidecarError> {
    match lower {
        RootFilesystemLowerDescriptor::Snapshot { entries } => Ok(RootFilesystemSnapshot {
            entries: entries
                .iter()
                .map(convert_root_filesystem_entry)
                .collect::<Result<Vec<_>, _>>()?,
        }),
    }
}

fn convert_root_filesystem_entry(
    entry: &RootFilesystemEntry,
) -> Result<KernelFilesystemEntry, SidecarError> {
    let mode = entry.mode.unwrap_or_else(|| match entry.kind {
        RootFilesystemEntryKind::File => {
            if entry.executable {
                0o755
            } else {
                0o644
            }
        }
        RootFilesystemEntryKind::Directory => 0o755,
        RootFilesystemEntryKind::Symlink => 0o777,
    });

    let content = match entry.content.as_ref() {
        Some(content) => match entry.encoding {
            Some(RootFilesystemEntryEncoding::Base64) => Some(
                base64::engine::general_purpose::STANDARD
                    .decode(content)
                    .map_err(|error| {
                        SidecarError::InvalidState(format!(
                            "invalid base64 root filesystem content for {}: {error}",
                            entry.path
                        ))
                    })?,
            ),
            Some(RootFilesystemEntryEncoding::Utf8) | None => Some(content.as_bytes().to_vec()),
        },
        None => None,
    };

    Ok(KernelFilesystemEntry {
        path: normalize_path(&entry.path),
        kind: match entry.kind {
            RootFilesystemEntryKind::File => KernelFilesystemEntryKind::File,
            RootFilesystemEntryKind::Directory => KernelFilesystemEntryKind::Directory,
            RootFilesystemEntryKind::Symlink => KernelFilesystemEntryKind::Symlink,
        },
        mode,
        uid: entry.uid.unwrap_or(0),
        gid: entry.gid.unwrap_or(0),
        content,
        target: entry.target.clone(),
    })
}

fn root_snapshot_entry(entry: &KernelFilesystemEntry) -> RootFilesystemEntry {
    let (content, encoding) = match entry.content.as_ref() {
        Some(bytes) => {
            let (content, encoding) = encode_guest_filesystem_content(bytes.clone());
            (Some(content), Some(encoding))
        }
        None => (None, None),
    };

    RootFilesystemEntry {
        path: entry.path.clone(),
        kind: match entry.kind {
            KernelFilesystemEntryKind::File => RootFilesystemEntryKind::File,
            KernelFilesystemEntryKind::Directory => RootFilesystemEntryKind::Directory,
            KernelFilesystemEntryKind::Symlink => RootFilesystemEntryKind::Symlink,
        },
        mode: Some(entry.mode),
        uid: Some(entry.uid),
        gid: Some(entry.gid),
        content,
        encoding,
        target: entry.target.clone(),
        executable: matches!(entry.kind, KernelFilesystemEntryKind::File)
            && (entry.mode & 0o111) != 0,
    }
}

fn guest_filesystem_stat(stat: VirtualStat) -> GuestFilesystemStat {
    GuestFilesystemStat {
        mode: stat.mode,
        size: stat.size,
        blocks: stat.blocks,
        dev: stat.dev,
        rdev: stat.rdev,
        is_directory: stat.is_directory,
        is_symbolic_link: stat.is_symbolic_link,
        atime_ms: stat.atime_ms,
        mtime_ms: stat.mtime_ms,
        ctime_ms: stat.ctime_ms,
        birthtime_ms: stat.birthtime_ms,
        ino: stat.ino,
        nlink: stat.nlink,
        uid: stat.uid,
        gid: stat.gid,
    }
}

fn encode_guest_filesystem_content(content: Vec<u8>) -> (String, RootFilesystemEntryEncoding) {
    match String::from_utf8(content) {
        Ok(text) => (text, RootFilesystemEntryEncoding::Utf8),
        Err(error) => (
            base64::engine::general_purpose::STANDARD.encode(error.into_bytes()),
            RootFilesystemEntryEncoding::Base64,
        ),
    }
}

fn decode_guest_filesystem_content(
    path: &str,
    content: Option<&str>,
    encoding: Option<RootFilesystemEntryEncoding>,
) -> Result<Vec<u8>, SidecarError> {
    let content = content.ok_or_else(|| {
        SidecarError::InvalidState(format!(
            "guest filesystem write_file for {path} requires content",
        ))
    })?;

    match encoding.unwrap_or(RootFilesystemEntryEncoding::Utf8) {
        RootFilesystemEntryEncoding::Utf8 => Ok(content.as_bytes().to_vec()),
        RootFilesystemEntryEncoding::Base64 => base64::engine::general_purpose::STANDARD
            .decode(content)
            .map_err(|error| {
                SidecarError::InvalidState(format!(
                    "invalid base64 guest filesystem content for {path}: {error}",
                ))
            }),
    }
}

fn apply_root_filesystem_entry<F>(
    filesystem: &mut F,
    entry: &RootFilesystemEntry,
) -> Result<(), SidecarError>
where
    F: VirtualFileSystem,
{
    let kernel_entry = convert_root_filesystem_entry(entry)?;
    ensure_parent_directories(filesystem, &kernel_entry.path)?;

    match kernel_entry.kind {
        KernelFilesystemEntryKind::Directory => filesystem
            .mkdir(&kernel_entry.path, true)
            .map_err(vfs_error)?,
        KernelFilesystemEntryKind::File => filesystem
            .write_file(&kernel_entry.path, kernel_entry.content.unwrap_or_default())
            .map_err(vfs_error)?,
        KernelFilesystemEntryKind::Symlink => filesystem
            .symlink(
                kernel_entry.target.as_deref().ok_or_else(|| {
                    SidecarError::InvalidState(format!(
                        "root filesystem bootstrap for symlink {} requires a target",
                        entry.path
                    ))
                })?,
                &kernel_entry.path,
            )
            .map_err(vfs_error)?,
    }

    if !matches!(kernel_entry.kind, KernelFilesystemEntryKind::Symlink) {
        filesystem
            .chmod(&kernel_entry.path, kernel_entry.mode)
            .map_err(vfs_error)?;
        filesystem
            .chown(&kernel_entry.path, kernel_entry.uid, kernel_entry.gid)
            .map_err(vfs_error)?;
    }

    Ok(())
}

fn ensure_parent_directories<F>(filesystem: &mut F, path: &str) -> Result<(), SidecarError>
where
    F: VirtualFileSystem,
{
    let parent = dirname(path);
    if parent != "/" && !filesystem.exists(&parent) {
        filesystem.mkdir(&parent, true).map_err(vfs_error)?;
    }
    Ok(())
}

#[derive(Debug)]
struct ProcNetEntry {
    local_host: String,
    local_port: u16,
    state: String,
    inode: u64,
}

fn find_socket_state_entry(
    vm: Option<&VmState>,
    kind: SocketQueryKind,
    request: &FindListenerRequest,
) -> Result<Option<SocketStateEntry>, SidecarError> {
    let vm = vm.ok_or_else(|| SidecarError::InvalidState(String::from("unknown sidecar VM")))?;

    for (process_id, process) in &vm.active_processes {
        if let Some(path) = request.path.as_deref() {
            if matches!(kind, SocketQueryKind::TcpListener) {
                for listener in process.unix_listeners.values() {
                    if listener.path() != path {
                        continue;
                    }
                    return Ok(Some(SocketStateEntry {
                        process_id: process_id.to_owned(),
                        host: None,
                        port: None,
                        path: Some(path.to_owned()),
                    }));
                }
            }
        }

        if request.path.is_none() {
            match kind {
                SocketQueryKind::TcpListener => {
                    for listener in process.tcp_listeners.values() {
                        let local_addr = listener.guest_local_addr();
                        let local_host = local_addr.ip().to_string();
                        if !socket_host_matches(request.host.as_deref(), &local_host) {
                            continue;
                        }
                        if let Some(port) = request.port {
                            if local_addr.port() != port {
                                continue;
                            }
                        }
                        return Ok(Some(SocketStateEntry {
                            process_id: process_id.to_owned(),
                            host: Some(local_host),
                            port: Some(local_addr.port()),
                            path: None,
                        }));
                    }
                }
                SocketQueryKind::UdpBound => {
                    for socket in process.udp_sockets.values() {
                        let Some(local_addr) = socket.local_addr() else {
                            continue;
                        };
                        let local_host = local_addr.ip().to_string();
                        if !socket_host_matches(request.host.as_deref(), &local_host) {
                            continue;
                        }
                        if let Some(port) = request.port {
                            if local_addr.port() != port {
                                continue;
                            }
                        }
                        return Ok(Some(SocketStateEntry {
                            process_id: process_id.to_owned(),
                            host: Some(local_host),
                            port: Some(local_addr.port()),
                            path: None,
                        }));
                    }
                }
            }
        }

        let child_pid = process.execution.child_pid();
        let inodes = socket_inodes_for_pid(child_pid)?;
        if inodes.is_empty() {
            continue;
        }

        if let Some(path) = request.path.as_deref() {
            if let Some(listener) = find_unix_socket_for_pid(child_pid, &inodes, path, process_id)?
            {
                return Ok(Some(listener));
            }
            continue;
        }

        let table_paths = match kind {
            SocketQueryKind::TcpListener => [
                format!("/proc/{child_pid}/net/tcp"),
                format!("/proc/{child_pid}/net/tcp6"),
            ],
            SocketQueryKind::UdpBound => [
                format!("/proc/{child_pid}/net/udp"),
                format!("/proc/{child_pid}/net/udp6"),
            ],
        };
        for table_path in table_paths {
            if let Some(entry) = find_inet_socket_for_pid(
                &table_path,
                &inodes,
                kind,
                request.host.as_deref(),
                request.port,
                process_id,
            )? {
                return Ok(Some(entry));
            }
        }
    }

    Ok(None)
}

fn socket_inodes_for_pid(pid: u32) -> Result<BTreeSet<u64>, SidecarError> {
    let fd_dir = PathBuf::from(format!("/proc/{pid}/fd"));
    let entries = match fs::read_dir(&fd_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(error) => {
            return Err(SidecarError::Io(format!(
                "failed to read socket descriptors for process {pid}: {error}"
            )));
        }
    };

    let mut inodes = BTreeSet::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            SidecarError::Io(format!(
                "failed to inspect fd entry for process {pid}: {error}"
            ))
        })?;
        let target = match fs::read_link(entry.path()) {
            Ok(target) => target,
            Err(_) => continue,
        };
        if let Some(inode) = parse_socket_inode(&target) {
            inodes.insert(inode);
        }
    }

    Ok(inodes)
}

fn parse_socket_inode(target: &Path) -> Option<u64> {
    let value = target.to_string_lossy();
    let trimmed = value.strip_prefix("socket:[")?.strip_suffix(']')?;
    trimmed.parse().ok()
}

fn unix_socket_path(addr: &UnixSocketAddr) -> Option<String> {
    addr.as_pathname()
        .map(|path| path.to_string_lossy().into_owned())
}

fn find_unix_socket_for_pid(
    pid: u32,
    inodes: &BTreeSet<u64>,
    path: &str,
    process_id: &str,
) -> Result<Option<SocketStateEntry>, SidecarError> {
    let table_path = format!("/proc/{pid}/net/unix");
    let contents = match fs::read_to_string(&table_path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(SidecarError::Io(format!(
                "failed to inspect unix sockets for process {pid}: {error}"
            )));
        }
    };

    for line in contents.lines().skip(1) {
        let columns = line.split_whitespace().collect::<Vec<_>>();
        if columns.len() < 8 {
            continue;
        }
        let Ok(inode) = columns[6].parse::<u64>() else {
            continue;
        };
        if !inodes.contains(&inode) || columns[7] != path {
            continue;
        }
        return Ok(Some(SocketStateEntry {
            process_id: process_id.to_owned(),
            host: None,
            port: None,
            path: Some(path.to_owned()),
        }));
    }

    Ok(None)
}

fn find_inet_socket_for_pid(
    table_path: &str,
    inodes: &BTreeSet<u64>,
    kind: SocketQueryKind,
    requested_host: Option<&str>,
    requested_port: Option<u16>,
    process_id: &str,
) -> Result<Option<SocketStateEntry>, SidecarError> {
    for entry in parse_proc_net_entries(table_path)? {
        if !inodes.contains(&entry.inode) {
            continue;
        }
        if matches!(kind, SocketQueryKind::TcpListener) && entry.state != "0A" {
            continue;
        }
        if !socket_host_matches(requested_host, &entry.local_host) {
            continue;
        }
        if let Some(port) = requested_port {
            if entry.local_port != port {
                continue;
            }
        }
        return Ok(Some(SocketStateEntry {
            process_id: process_id.to_owned(),
            host: Some(entry.local_host),
            port: Some(entry.local_port),
            path: None,
        }));
    }

    Ok(None)
}

fn is_unspecified_socket_host(host: &str) -> bool {
    host == "0.0.0.0" || host == "::"
}

fn is_loopback_socket_host(host: &str) -> bool {
    host == "127.0.0.1" || host == "::1" || host.eq_ignore_ascii_case("localhost")
}

fn vm_network_resource_counts(vm: &VmState) -> NetworkResourceCounts {
    let mut counts = NetworkResourceCounts::default();
    for process in vm.active_processes.values() {
        let process_counts = process.network_resource_counts();
        counts.sockets += process_counts.sockets;
        counts.connections += process_counts.connections;
    }
    counts
}

fn collect_javascript_socket_port_state(
    process: &ActiveProcess,
    tcp_guest_to_host: &mut BTreeMap<(JavascriptSocketFamily, u16), u16>,
    udp_guest_to_host: &mut BTreeMap<(JavascriptSocketFamily, u16), u16>,
    udp_host_to_guest: &mut BTreeMap<(JavascriptSocketFamily, u16), u16>,
    used_tcp_ports: &mut BTreeMap<JavascriptSocketFamily, BTreeSet<u16>>,
    used_udp_ports: &mut BTreeMap<JavascriptSocketFamily, BTreeSet<u16>>,
) {
    for listener in process.tcp_listeners.values() {
        let guest_addr = listener.guest_local_addr();
        let family = JavascriptSocketFamily::from_ip(guest_addr.ip());
        used_tcp_ports
            .entry(family)
            .or_default()
            .insert(guest_addr.port());
        if is_loopback_ip(guest_addr.ip()) {
            tcp_guest_to_host.insert((family, guest_addr.port()), listener.local_addr().port());
        }
    }

    for socket in process.udp_sockets.values() {
        let Some(guest_addr) = socket.local_addr() else {
            continue;
        };
        let family = JavascriptSocketFamily::from_ip(guest_addr.ip());
        used_udp_ports
            .entry(family)
            .or_default()
            .insert(guest_addr.port());
        if let Some(host_addr) = socket
            .socket
            .as_ref()
            .and_then(|socket| socket.local_addr().ok())
        {
            if is_loopback_ip(guest_addr.ip()) {
                udp_guest_to_host.insert((family, guest_addr.port()), host_addr.port());
                udp_host_to_guest.insert((family, host_addr.port()), guest_addr.port());
            }
        }
    }

    for child in process.child_processes.values() {
        collect_javascript_socket_port_state(
            child,
            tcp_guest_to_host,
            udp_guest_to_host,
            udp_host_to_guest,
            used_tcp_ports,
            used_udp_ports,
        );
    }
}

fn build_javascript_socket_path_context(
    vm: &VmState,
) -> Result<JavascriptSocketPathContext, SidecarError> {
    let internal_env = extract_guest_env(&vm.metadata);
    let mut tcp_loopback_guest_to_host_ports = BTreeMap::new();
    let mut udp_loopback_guest_to_host_ports = BTreeMap::new();
    let mut udp_loopback_host_to_guest_ports = BTreeMap::new();
    let mut used_tcp_guest_ports = BTreeMap::new();
    let mut used_udp_guest_ports = BTreeMap::new();
    for process in vm.active_processes.values() {
        collect_javascript_socket_port_state(
            process,
            &mut tcp_loopback_guest_to_host_ports,
            &mut udp_loopback_guest_to_host_ports,
            &mut udp_loopback_host_to_guest_ports,
            &mut used_tcp_guest_ports,
            &mut used_udp_guest_ports,
        );
    }
    Ok(JavascriptSocketPathContext {
        sandbox_root: vm.cwd.clone(),
        mounts: vm.configuration.mounts.clone(),
        listen_policy: parse_vm_listen_policy(&vm.metadata)?,
        loopback_exempt_ports: parse_loopback_exempt_ports(&internal_env)?,
        tcp_loopback_guest_to_host_ports,
        udp_loopback_guest_to_host_ports,
        udp_loopback_host_to_guest_ports,
        used_tcp_guest_ports,
        used_udp_guest_ports,
    })
}

fn check_network_resource_limit(
    limit: Option<usize>,
    current: usize,
    additional: usize,
    label: &str,
) -> Result<(), SidecarError> {
    if let Some(limit) = limit {
        if current.saturating_add(additional) > limit {
            return Err(SidecarError::Execution(format!(
                "EAGAIN: maximum {label} count reached"
            )));
        }
    }
    Ok(())
}

fn normalize_tcp_listen_host(
    host: Option<&str>,
) -> Result<(JavascriptSocketFamily, &'static str), SidecarError> {
    match host.unwrap_or("127.0.0.1") {
        "127.0.0.1" | "localhost" => Ok((JavascriptSocketFamily::Ipv4, "127.0.0.1")),
        "::1" => Ok((JavascriptSocketFamily::Ipv6, "::1")),
        "0.0.0.0" | "::" => Err(SidecarError::Execution(String::from(
            "EACCES: TCP listeners must bind to loopback, not unspecified addresses",
        ))),
        other => Err(SidecarError::Execution(format!(
            "EACCES: TCP listeners must bind to loopback, got {other}"
        ))),
    }
}

fn normalize_udp_bind_host(
    host: Option<&str>,
    family: JavascriptUdpFamily,
) -> Result<(&'static str, JavascriptSocketFamily), SidecarError> {
    match (family, host) {
        (JavascriptUdpFamily::Ipv4, None)
        | (JavascriptUdpFamily::Ipv4, Some("127.0.0.1"))
        | (JavascriptUdpFamily::Ipv4, Some("localhost")) => {
            Ok(("127.0.0.1", JavascriptSocketFamily::Ipv4))
        }
        (JavascriptUdpFamily::Ipv6, None)
        | (JavascriptUdpFamily::Ipv6, Some("::1"))
        | (JavascriptUdpFamily::Ipv6, Some("localhost")) => {
            Ok(("::1", JavascriptSocketFamily::Ipv6))
        }
        (_, Some("0.0.0.0")) | (_, Some("::")) => Err(SidecarError::Execution(String::from(
            "EACCES: UDP sockets must bind to loopback, not unspecified addresses",
        ))),
        (JavascriptUdpFamily::Ipv4, Some(other)) => Err(SidecarError::Execution(format!(
            "EACCES: udp4 sockets must bind to 127.0.0.1, got {other}"
        ))),
        (JavascriptUdpFamily::Ipv6, Some(other)) => Err(SidecarError::Execution(format!(
            "EACCES: udp6 sockets must bind to ::1, got {other}"
        ))),
    }
}

fn allocate_guest_listen_port(
    requested_port: u16,
    family: JavascriptSocketFamily,
    used_ports: &BTreeMap<JavascriptSocketFamily, BTreeSet<u16>>,
    policy: VmListenPolicy,
) -> Result<u16, SidecarError> {
    let is_allowed = |port: u16| {
        port >= policy.port_min
            && port <= policy.port_max
            && (policy.allow_privileged || port >= 1024)
    };
    let used = used_ports.get(&family);

    if requested_port != 0 {
        if !is_allowed(requested_port) {
            let reason = if requested_port < 1024 && !policy.allow_privileged {
                format!(
                    "EACCES: privileged listen port {requested_port} requires {}=true",
                    VM_LISTEN_ALLOW_PRIVILEGED_METADATA_KEY
                )
            } else {
                format!(
                    "EACCES: listen port {requested_port} is outside the allowed range {}-{}",
                    policy.port_min, policy.port_max
                )
            };
            return Err(SidecarError::Execution(reason));
        }
        if used.is_some_and(|ports| ports.contains(&requested_port)) {
            return Err(sidecar_net_error(std::io::Error::from_raw_os_error(
                libc::EADDRINUSE,
            )));
        }
        return Ok(requested_port);
    }

    let allocation_start = policy
        .port_min
        .max(if policy.allow_privileged { 1 } else { 1024 });
    for candidate in allocation_start..=policy.port_max {
        if used.is_some_and(|ports| ports.contains(&candidate)) {
            continue;
        }
        return Ok(candidate);
    }

    Err(sidecar_net_error(std::io::Error::from_raw_os_error(
        libc::EADDRINUSE,
    )))
}

fn socket_host_matches(requested: Option<&str>, actual: &str) -> bool {
    match requested {
        None => true,
        Some(requested) if requested == actual => true,
        Some(requested)
            if is_unspecified_socket_host(requested) && is_unspecified_socket_host(actual) =>
        {
            true
        }
        Some(requested) if requested.eq_ignore_ascii_case("localhost") => {
            is_loopback_socket_host(actual)
        }
        _ => false,
    }
}

fn parse_proc_net_entries(table_path: &str) -> Result<Vec<ProcNetEntry>, SidecarError> {
    let contents = match fs::read_to_string(table_path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(SidecarError::Io(format!(
                "failed to inspect socket table {table_path}: {error}"
            )));
        }
    };

    let mut entries = Vec::new();
    for line in contents.lines().skip(1) {
        let columns = line.split_whitespace().collect::<Vec<_>>();
        if columns.len() < 10 {
            continue;
        }
        let Some((host, port)) = parse_proc_ip_port(columns[1]) else {
            continue;
        };
        let Ok(inode) = columns[9].parse::<u64>() else {
            continue;
        };
        entries.push(ProcNetEntry {
            local_host: host,
            local_port: port,
            state: columns[3].to_owned(),
            inode,
        });
    }

    Ok(entries)
}

fn parse_proc_ip_port(value: &str) -> Option<(String, u16)> {
    let (raw_ip, raw_port) = value.split_once(':')?;
    let port = u16::from_str_radix(raw_port, 16).ok()?;
    let host = match raw_ip.len() {
        8 => {
            let raw = u32::from_str_radix(raw_ip, 16).ok()?;
            Ipv4Addr::from(raw.to_le_bytes()).to_string()
        }
        32 => {
            let mut bytes = [0_u8; 16];
            for (index, chunk) in raw_ip.as_bytes().chunks(8).enumerate() {
                let word = u32::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok()?;
                bytes[index * 4..(index + 1) * 4].copy_from_slice(&word.to_le_bytes());
            }
            Ipv6Addr::from(bytes).to_string()
        }
        _ => return None,
    };
    Some((host, port))
}

fn root_filesystem_error(error: impl std::fmt::Display) -> SidecarError {
    SidecarError::InvalidState(format!("root filesystem: {error}"))
}

fn normalize_path(path: &str) -> String {
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

fn normalize_python_vfs_rpc_path(path: &str) -> Result<String, SidecarError> {
    if !path.starts_with('/') {
        return Err(SidecarError::InvalidState(format!(
            "python VFS RPC path {path} must be absolute within {PYTHON_VFS_RPC_GUEST_ROOT}"
        )));
    }

    let normalized = normalize_path(path);
    if normalized == PYTHON_VFS_RPC_GUEST_ROOT
        || normalized.starts_with(&format!("{PYTHON_VFS_RPC_GUEST_ROOT}/"))
    {
        Ok(normalized)
    } else {
        Err(SidecarError::InvalidState(format!(
            "python VFS RPC path {normalized} escapes guest workspace root {PYTHON_VFS_RPC_GUEST_ROOT}"
        )))
    }
}

fn normalize_host_path(path: &Path) -> PathBuf {
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

fn path_is_within_root(path: &Path, root: &Path) -> bool {
    path == root || path.starts_with(root)
}

fn dirname(path: &str) -> String {
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

fn python_file_entrypoint(entrypoint: &str) -> Option<PathBuf> {
    let path = Path::new(entrypoint);
    (path.extension().and_then(|extension| extension.to_str()) == Some("py"))
        .then(|| path.to_path_buf())
}

fn discover_command_guest_paths(kernel: &mut SidecarKernel) -> BTreeMap<String, String> {
    let mut command_guest_paths = BTreeMap::new();
    let Ok(command_roots) = kernel.read_dir("/__agentos/commands") else {
        return command_guest_paths;
    };

    let mut ordered_roots = command_roots
        .into_iter()
        .filter(|entry| !entry.is_empty() && entry.chars().all(|ch| ch.is_ascii_digit()))
        .collect::<Vec<_>>();
    ordered_roots.sort();

    for root in ordered_roots {
        let guest_root = format!("/__agentos/commands/{root}");
        let Ok(entries) = kernel.read_dir(&guest_root) else {
            continue;
        };

        for entry in entries {
            if entry.starts_with('.') || command_guest_paths.contains_key(&entry) {
                continue;
            }
            command_guest_paths.insert(entry.clone(), format!("{guest_root}/{entry}"));
        }
    }

    command_guest_paths
}

fn is_path_like_specifier(specifier: &str) -> bool {
    specifier.starts_with('/')
        || specifier.starts_with("./")
        || specifier.starts_with("../")
        || specifier.starts_with("file:")
}

fn is_javascript_entrypoint_path(path: &Path) -> bool {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase())
    {
        Some(extension) if matches!(extension.as_str(), "js" | "mjs" | "cjs") => true,
        _ => fs::read(path)
            .ok()
            .and_then(|contents| {
                contents
                    .split(|byte| *byte == b'\n')
                    .next()
                    .map(|line| String::from_utf8_lossy(line).contains("node"))
            })
            .unwrap_or(false),
    }
}

fn execution_wasm_permission_tier(tier: WasmPermissionTier) -> ExecutionWasmPermissionTier {
    match tier {
        WasmPermissionTier::Full => ExecutionWasmPermissionTier::Full,
        WasmPermissionTier::ReadWrite => ExecutionWasmPermissionTier::ReadWrite,
        WasmPermissionTier::ReadOnly => ExecutionWasmPermissionTier::ReadOnly,
        WasmPermissionTier::Isolated => ExecutionWasmPermissionTier::Isolated,
    }
}

fn resolve_wasm_permission_tier(
    vm: &VmState,
    command_name: Option<&str>,
    explicit_tier: Option<WasmPermissionTier>,
    entrypoint: &str,
) -> WasmPermissionTier {
    explicit_tier
        .or_else(|| command_name.and_then(|command| vm.command_permissions.get(command).copied()))
        .or_else(|| {
            Path::new(entrypoint)
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|command| vm.command_permissions.get(command).copied())
        })
        .unwrap_or(WasmPermissionTier::Full)
}

fn tokenize_shell_free_command(command: &str) -> Vec<String> {
    command
        .split_whitespace()
        .filter(|segment| !segment.is_empty())
        .map(str::to_owned)
        .collect()
}

fn host_mount_path_for_guest_path(vm: &VmState, guest_path: &str) -> Option<PathBuf> {
    let normalized = normalize_path(guest_path);

    let mut mounts = vm
        .configuration
        .mounts
        .iter()
        .filter_map(|mount| {
            (mount.plugin.id == "host_dir")
                .then(|| {
                    mount
                        .plugin
                        .config
                        .get("hostPath")
                        .and_then(Value::as_str)
                        .map(|host_path| (mount.guest_path.as_str(), host_path))
                })
                .flatten()
        })
        .collect::<Vec<_>>();
    mounts.sort_by(|left, right| right.0.len().cmp(&left.0.len()));

    for (guest_root, host_root) in mounts {
        if normalized != guest_root && !normalized.starts_with(&format!("{guest_root}/")) {
            continue;
        }

        let suffix = normalized
            .strip_prefix(guest_root)
            .unwrap_or_default()
            .trim_start_matches('/');
        let mut path = PathBuf::from(host_root);
        if !suffix.is_empty() {
            path.push(suffix);
        }
        return Some(path);
    }

    None
}

fn host_path_for_guest_path(vm: &VmState, guest_path: &str) -> PathBuf {
    if let Some(host_path) = host_mount_path_for_guest_path(vm, guest_path) {
        return host_path;
    }

    let normalized = normalize_path(guest_path);
    let mut path = normalize_host_path(&vm.cwd);
    let suffix = normalized.trim_start_matches('/');
    if !suffix.is_empty() {
        path.push(suffix);
    }
    path
}

fn guest_path_for_host_execution_path(host_cwd: &Path, host_path: &Path) -> Option<String> {
    let normalized_cwd = normalize_host_path(host_cwd);
    let normalized_path = normalize_host_path(host_path);
    let relative = normalized_path.strip_prefix(&normalized_cwd).ok()?;
    let mut guest_path = PathBuf::from("/root");
    if !relative.as_os_str().is_empty() {
        guest_path.push(relative);
    }
    Some(normalize_path(&guest_path.to_string_lossy()))
}

fn resolve_javascript_execution_entrypoint(
    vm: &VmState,
    host_cwd: &Path,
    entrypoint: &str,
    env: &mut BTreeMap<String, String>,
) -> String {
    if !is_path_like_specifier(entrypoint) {
        return entrypoint.to_owned();
    }

    if entrypoint.starts_with("./") || entrypoint.starts_with("../") {
        return host_cwd.join(entrypoint).to_string_lossy().into_owned();
    }

    let host_entrypoint = normalize_host_path(Path::new(entrypoint));
    if host_entrypoint.is_absolute() && host_entrypoint.exists() {
        if let Some(guest_entrypoint) =
            guest_path_for_host_execution_path(host_cwd, &host_entrypoint)
        {
            env.insert(String::from("AGENT_OS_GUEST_ENTRYPOINT"), guest_entrypoint);
        }
        return host_entrypoint.to_string_lossy().into_owned();
    }

    let guest_entrypoint = if entrypoint.starts_with("file:") {
        normalize_path(entrypoint.trim_start_matches("file:"))
    } else {
        normalize_path(entrypoint)
    };
    env.insert(
        String::from("AGENT_OS_GUEST_ENTRYPOINT"),
        guest_entrypoint.clone(),
    );
    host_path_for_guest_path(vm, &guest_entrypoint)
        .to_string_lossy()
        .into_owned()
}

fn host_mount_path_for_guest_path_from_mounts(
    mounts: &[crate::protocol::MountDescriptor],
    guest_path: &str,
) -> Option<PathBuf> {
    let normalized = normalize_path(guest_path);

    let mut host_mounts = mounts
        .iter()
        .filter_map(|mount| {
            (mount.plugin.id == "host_dir")
                .then(|| {
                    mount
                        .plugin
                        .config
                        .get("hostPath")
                        .and_then(Value::as_str)
                        .map(|host_path| (mount.guest_path.as_str(), host_path))
                })
                .flatten()
        })
        .collect::<Vec<_>>();
    host_mounts.sort_by(|left, right| right.0.len().cmp(&left.0.len()));

    for (guest_root, host_root) in host_mounts {
        if normalized != guest_root && !normalized.starts_with(&format!("{guest_root}/")) {
            continue;
        }

        let suffix = normalized
            .strip_prefix(guest_root)
            .unwrap_or_default()
            .trim_start_matches('/');
        let mut path = PathBuf::from(host_root);
        if !suffix.is_empty() {
            path.push(suffix);
        }
        return Some(path);
    }

    None
}

fn host_path_for_guest_path_from_mappings(
    mappings: &[JavascriptGuestPathMapping],
    guest_path: &str,
) -> Option<PathBuf> {
    let normalized = normalize_path(guest_path);

    mappings
        .iter()
        .filter_map(|mapping| {
            let guest_root = normalize_path(&mapping.guest_path);
            if normalized != guest_root && !normalized.starts_with(&format!("{guest_root}/")) {
                return None;
            }

            let suffix = normalized
                .strip_prefix(&guest_root)
                .unwrap_or_default()
                .trim_start_matches('/');
            let mut host_path = PathBuf::from(&mapping.host_path);
            if !suffix.is_empty() {
                host_path.push(suffix);
            }

            Some((guest_root.len(), mapping.host_path.len(), host_path))
        })
        .max_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)))
        .map(|(_, _, host_path)| host_path)
}

fn parse_javascript_guest_path_mappings(
    env: &BTreeMap<String, String>,
) -> Vec<JavascriptGuestPathMapping> {
    env.get("AGENT_OS_GUEST_PATH_MAPPINGS")
        .and_then(|value| serde_json::from_str::<Vec<JavascriptGuestPathMapping>>(value).ok())
        .unwrap_or_default()
}

fn host_path_for_child_guest_path(
    vm: &VmState,
    internal_bootstrap_env: &BTreeMap<String, String>,
    guest_path: &str,
) -> PathBuf {
    let mappings = parse_javascript_guest_path_mappings(internal_bootstrap_env);
    host_path_for_guest_path_from_mappings(&mappings, guest_path)
        .unwrap_or_else(|| host_path_for_guest_path(vm, guest_path))
}

fn host_path_for_child_guest_cwd(
    vm: &VmState,
    internal_bootstrap_env: &BTreeMap<String, String>,
    parent_host_cwd: &Path,
    guest_cwd: &str,
) -> PathBuf {
    let normalized = normalize_path(guest_cwd);
    let mappings = parse_javascript_guest_path_mappings(internal_bootstrap_env);
    if let Some(path) = host_path_for_guest_path_from_mappings(&mappings, &normalized) {
        return path;
    }

    if normalized == "/root" {
        return normalize_host_path(parent_host_cwd);
    }
    if let Some(suffix) = normalized.strip_prefix("/root/") {
        let mut path = normalize_host_path(parent_host_cwd);
        if !suffix.is_empty() {
            path.push(suffix);
        }
        return path;
    }

    host_path_for_guest_path(vm, &normalized)
}

fn resolve_guest_socket_host_path(
    context: &JavascriptSocketPathContext,
    guest_path: &str,
) -> PathBuf {
    if let Some(path) = host_mount_path_for_guest_path_from_mounts(&context.mounts, guest_path) {
        return path;
    }

    let normalized = normalize_path(guest_path);
    let mut host_path = context.sandbox_root.clone();
    let suffix = normalized.trim_start_matches('/');
    if !suffix.is_empty() {
        host_path.push(suffix);
    }
    host_path
}

fn ensure_kernel_parent_directories(
    kernel: &mut SidecarKernel,
    path: &str,
) -> Result<(), SidecarError> {
    let parent = dirname(path);
    if parent != "/" && !kernel.exists(&parent).map_err(kernel_error)? {
        kernel.mkdir(&parent, true).map_err(kernel_error)?;
    }
    Ok(())
}

#[derive(Debug, Deserialize, Default)]
struct JavascriptChildProcessSpawnOptions {
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(rename = "internalBootstrapEnv", default)]
    internal_bootstrap_env: BTreeMap<String, String>,
    #[serde(default)]
    shell: bool,
}

#[derive(Debug, Deserialize)]
struct JavascriptGuestPathMapping {
    #[serde(rename = "guestPath")]
    guest_path: String,
    #[serde(rename = "hostPath")]
    host_path: String,
}

#[derive(Debug, Deserialize)]
struct JavascriptChildProcessSpawnRequest {
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    options: JavascriptChildProcessSpawnOptions,
}

#[derive(Debug)]
struct ResolvedChildProcessExecution {
    command: String,
    process_args: Vec<String>,
    runtime: GuestRuntimeKind,
    entrypoint: String,
    execution_args: Vec<String>,
    env: BTreeMap<String, String>,
    guest_cwd: String,
    host_cwd: PathBuf,
    wasm_permission_tier: Option<WasmPermissionTier>,
}

fn sanitize_javascript_child_process_internal_bootstrap_env(
    env: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    const ALLOWED_KEYS: &[&str] = &[
        "AGENT_OS_ALLOWED_NODE_BUILTINS",
        "AGENT_OS_GUEST_PATH_MAPPINGS",
        "AGENT_OS_LOOPBACK_EXEMPT_PORTS",
        "AGENT_OS_VIRTUAL_PROCESS_EXEC_PATH",
        "AGENT_OS_VIRTUAL_PROCESS_UID",
        "AGENT_OS_VIRTUAL_PROCESS_GID",
        "AGENT_OS_VIRTUAL_PROCESS_VERSION",
    ];

    env.iter()
        .filter(|(key, _)| {
            ALLOWED_KEYS.contains(&key.as_str()) || key.starts_with("AGENT_OS_VIRTUAL_OS_")
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

#[derive(Debug, Deserialize)]
struct JavascriptNetConnectRequest {
    #[serde(default)]
    host: Option<String>,
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct JavascriptNetListenRequest {
    #[serde(default)]
    host: Option<String>,
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    backlog: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct JavascriptDgramCreateSocketRequest {
    #[serde(rename = "type")]
    socket_type: String,
}

#[derive(Debug, Deserialize)]
struct JavascriptDgramBindRequest {
    #[serde(default)]
    address: Option<String>,
    #[serde(default)]
    port: u16,
}

#[derive(Debug, Deserialize)]
struct JavascriptDgramSendRequest {
    #[serde(default)]
    address: Option<String>,
    port: u16,
}

#[derive(Debug, Deserialize)]
struct JavascriptDnsLookupRequest {
    hostname: String,
    #[serde(default)]
    family: Option<u8>,
}

#[derive(Debug, Deserialize)]
struct JavascriptDnsResolveRequest {
    hostname: String,
    #[serde(default)]
    rrtype: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct VmDnsConfig {
    name_servers: Vec<SocketAddr>,
    overrides: BTreeMap<String, Vec<IpAddr>>,
}

#[derive(Debug, Clone, Copy)]
enum DnsResolutionSource {
    Literal,
    Override,
    Resolver,
}

impl DnsResolutionSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Literal => "literal",
            Self::Override => "override",
            Self::Resolver => "resolver",
        }
    }
}

fn resolve_tcp_bind_addr(host: &str, port: u16) -> Result<SocketAddr, SidecarError> {
    (host, port)
        .to_socket_addrs()
        .map_err(sidecar_net_error)?
        .next()
        .ok_or_else(|| {
            SidecarError::Execution(format!("failed to resolve TCP bind address {host}:{port}"))
        })
}

fn format_dns_resource(hostname: &str) -> String {
    format!("dns://{hostname}")
}

fn format_tcp_resource(host: &str, port: u16) -> String {
    format!("tcp://{host}:{port}")
}

fn is_loopback_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_loopback(),
        IpAddr::V6(ip) => {
            ip.is_loopback()
                || ip
                    .to_ipv4_mapped()
                    .is_some_and(|mapped| mapped.is_loopback())
        }
    }
}

fn loopback_cidr(ip: IpAddr) -> &'static str {
    match ip {
        IpAddr::V4(ip) if ip.is_loopback() => "127.0.0.0/8",
        IpAddr::V6(ip)
            if ip
                .to_ipv4_mapped()
                .is_some_and(|mapped| mapped.is_loopback()) =>
        {
            "127.0.0.0/8"
        }
        IpAddr::V6(_) => "::1/128",
        IpAddr::V4(_) => "127.0.0.0/8",
    }
}

fn restricted_non_loopback_ip_range(ip: IpAddr) -> Option<(&'static str, &'static str)> {
    match ip {
        IpAddr::V4(ip) => {
            let [first, second, ..] = ip.octets();
            match (first, second) {
                (10, _) => Some(("10.0.0.0/8", "private")),
                (172, 16..=31) => Some(("172.16.0.0/12", "private")),
                (192, 168) => Some(("192.168.0.0/16", "private")),
                (169, 254) => Some(("169.254.0.0/16", "link-local")),
                _ => None,
            }
        }
        IpAddr::V6(ip) => {
            if let Some(mapped) = ip.to_ipv4_mapped() {
                return restricted_non_loopback_ip_range(IpAddr::V4(mapped));
            }

            let segments = ip.segments();
            if (segments[0] & 0xfe00) == 0xfc00 {
                return Some(("fc00::/7", "unique-local"));
            }
            if (segments[0] & 0xffc0) == 0xfe80 {
                return Some(("fe80::/10", "link-local"));
            }
            None
        }
    }
}

fn blocked_dns_resolution_error(
    resource: &str,
    ip: IpAddr,
    cidr: &str,
    label: &str,
) -> SidecarError {
    SidecarError::Execution(format!(
        "EACCES: blocked outbound network access to {resource}: {ip} is within restricted {label} range {cidr}"
    ))
}

fn blocked_loopback_connect_error(resource: &str, ip: IpAddr, port: u16) -> SidecarError {
    SidecarError::Execution(format!(
        "EACCES: blocked outbound network access to {resource}: {ip} is loopback ({}) and port {port} is not owned by this VM and is not listed in {LOOPBACK_EXEMPT_PORTS_ENV}",
        loopback_cidr(ip)
    ))
}

fn filter_dns_safe_ip_addrs(
    addresses: Vec<IpAddr>,
    hostname: &str,
) -> Result<Vec<IpAddr>, SidecarError> {
    let resource = format_dns_resource(hostname);
    let mut allowed = Vec::new();
    let mut blocked = None;

    for ip in addresses {
        if let Some((cidr, label)) = restricted_non_loopback_ip_range(ip) {
            blocked.get_or_insert((ip, cidr, label));
            continue;
        }
        allowed.push(ip);
    }

    if allowed.is_empty() {
        let (ip, cidr, label) = blocked.expect("blocked DNS results should capture a reason");
        return Err(blocked_dns_resolution_error(&resource, ip, cidr, label));
    }

    Ok(allowed)
}

fn loopback_connect_allowed(context: &JavascriptSocketPathContext, port: u16) -> bool {
    context.loopback_port_allowed(port)
}

fn filter_tcp_connect_ip_addrs(
    addresses: Vec<IpAddr>,
    host: &str,
    port: u16,
    context: &JavascriptSocketPathContext,
) -> Result<Vec<IpAddr>, SidecarError> {
    let resource = format_tcp_resource(host, port);
    let mut allowed = Vec::new();
    let mut blocked = None;

    for ip in addresses {
        if let Some((cidr, label)) = restricted_non_loopback_ip_range(ip) {
            blocked.get_or_insert_with(|| blocked_dns_resolution_error(&resource, ip, cidr, label));
            continue;
        }
        if is_loopback_ip(ip) && !loopback_connect_allowed(context, port) {
            blocked.get_or_insert_with(|| blocked_loopback_connect_error(&resource, ip, port));
            continue;
        }
        allowed.push(ip);
    }

    if allowed.is_empty() {
        return Err(blocked.expect("blocked TCP connect results should capture a reason"));
    }

    Ok(allowed)
}

fn resolve_tcp_connect_addr<B>(
    bridge: &SharedBridge<B>,
    vm_id: &str,
    dns: &VmDnsConfig,
    host: &str,
    port: u16,
    context: &JavascriptSocketPathContext,
) -> Result<ResolvedTcpConnectAddr, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let allowed = filter_tcp_connect_ip_addrs(
        resolve_dns_ip_addrs(bridge, vm_id, dns, host)?,
        host,
        port,
        context,
    )?;
    let ip = allowed
        .iter()
        .copied()
        .find(|candidate| {
            let family = JavascriptSocketFamily::from_ip(*candidate);
            context.translate_tcp_loopback_port(family, port).is_some()
        })
        .or_else(|| allowed.first().copied())
        .ok_or_else(|| {
            SidecarError::Execution(format!("failed to resolve TCP address {host}:{port}"))
        })?;
    let family = JavascriptSocketFamily::from_ip(ip);
    let actual_port = if is_loopback_ip(ip) {
        context
            .translate_tcp_loopback_port(family, port)
            .unwrap_or(port)
    } else {
        port
    };
    Ok(ResolvedTcpConnectAddr {
        actual_addr: SocketAddr::new(ip, actual_port),
        guest_remote_addr: SocketAddr::new(ip, port),
    })
}

fn resolve_dns_ip_addrs<B>(
    bridge: &SharedBridge<B>,
    vm_id: &str,
    dns: &VmDnsConfig,
    hostname: &str,
) -> Result<Vec<IpAddr>, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    if let Ok(ip_addr) = hostname.parse::<IpAddr>() {
        let addresses = vec![ip_addr];
        emit_dns_resolution_event(
            bridge,
            vm_id,
            hostname,
            DnsResolutionSource::Literal,
            &addresses,
            dns,
        );
        return Ok(addresses);
    }

    let normalized_hostname = normalize_dns_hostname(hostname)?;
    if let Some(addresses) = dns.overrides.get(&normalized_hostname) {
        emit_dns_resolution_event(
            bridge,
            vm_id,
            hostname,
            DnsResolutionSource::Override,
            addresses,
            dns,
        );
        return Ok(addresses.clone());
    }

    let addresses = match resolve_dns_with_sidecar_resolver(dns, &normalized_hostname) {
        Ok(addresses) => addresses,
        Err(error) => {
            emit_dns_resolution_failure_event(bridge, vm_id, hostname, dns, &error);
            return Err(error);
        }
    };
    emit_dns_resolution_event(
        bridge,
        vm_id,
        hostname,
        DnsResolutionSource::Resolver,
        &addresses,
        dns,
    );
    Ok(addresses)
}

fn filter_dns_ip_addrs(
    addresses: Vec<IpAddr>,
    family: Option<u8>,
) -> Result<Vec<IpAddr>, SidecarError> {
    let filtered: Vec<_> = match family.unwrap_or(0) {
        0 => addresses,
        4 => addresses
            .into_iter()
            .filter(|ip| matches!(ip, IpAddr::V4(_)))
            .collect(),
        6 => addresses
            .into_iter()
            .filter(|ip| matches!(ip, IpAddr::V6(_)))
            .collect(),
        other => {
            return Err(SidecarError::InvalidState(format!(
                "unsupported dns family {other}"
            )));
        }
    };

    if filtered.is_empty() {
        return Err(SidecarError::Execution(String::from(
            "failed to resolve DNS address for requested family",
        )));
    }

    Ok(filtered)
}

fn resolve_udp_bind_addr(
    host: &str,
    port: u16,
    family: JavascriptUdpFamily,
) -> Result<SocketAddr, SidecarError> {
    (host, port)
        .to_socket_addrs()
        .map_err(sidecar_net_error)?
        .find(|addr| family.matches_addr(addr))
        .ok_or_else(|| {
            SidecarError::Execution(format!(
                "failed to resolve {} UDP bind address {host}:{port}",
                family.socket_type()
            ))
        })
}

fn resolve_udp_addr<B>(
    bridge: &SharedBridge<B>,
    vm_id: &str,
    dns: &VmDnsConfig,
    host: &str,
    port: u16,
    family: JavascriptUdpFamily,
    context: &JavascriptSocketPathContext,
) -> Result<SocketAddr, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    resolve_dns_ip_addrs(bridge, vm_id, dns, host)?
        .into_iter()
        .map(|ip| {
            let family_key = JavascriptSocketFamily::from_ip(ip);
            let actual_port = if is_loopback_ip(ip) {
                context
                    .translate_udp_loopback_port(family_key, port)
                    .unwrap_or(port)
            } else {
                port
            };
            SocketAddr::new(ip, actual_port)
        })
        .find(|addr| family.matches_addr(addr))
        .ok_or_else(|| {
            SidecarError::Execution(format!(
                "failed to resolve {} UDP address {host}:{port}",
                family.socket_type()
            ))
        })
}

fn socket_addr_family(addr: &SocketAddr) -> &'static str {
    match addr {
        SocketAddr::V4(_) => "IPv4",
        SocketAddr::V6(_) => "IPv6",
    }
}

fn io_error_code(error: &std::io::Error) -> Option<String> {
    match error.raw_os_error() {
        Some(libc::EADDRINUSE) => Some(String::from("EADDRINUSE")),
        Some(libc::EADDRNOTAVAIL) => Some(String::from("EADDRNOTAVAIL")),
        Some(libc::ECONNREFUSED) => Some(String::from("ECONNREFUSED")),
        Some(libc::ECONNRESET) => Some(String::from("ECONNRESET")),
        Some(libc::EINVAL) => Some(String::from("EINVAL")),
        Some(libc::EPIPE) => Some(String::from("EPIPE")),
        Some(libc::ETIMEDOUT) => Some(String::from("ETIMEDOUT")),
        Some(libc::EHOSTUNREACH) => Some(String::from("EHOSTUNREACH")),
        Some(libc::ENETUNREACH) => Some(String::from("ENETUNREACH")),
        _ => None,
    }
}

fn sidecar_net_error(error: std::io::Error) -> SidecarError {
    let message = match io_error_code(&error) {
        Some(code) => format!("{code}: {error}"),
        None => error.to_string(),
    };
    SidecarError::Execution(message)
}

fn spawn_tcp_socket_reader(
    stream: TcpStream,
    sender: Sender<JavascriptTcpSocketEvent>,
    saw_local_shutdown: Arc<AtomicBool>,
    saw_remote_end: Arc<AtomicBool>,
    close_notified: Arc<AtomicBool>,
) {
    thread::spawn(move || {
        let mut stream = stream;
        let mut buffer = vec![0_u8; 64 * 1024];
        loop {
            match stream.read(&mut buffer) {
                Ok(0) => {
                    saw_remote_end.store(true, Ordering::SeqCst);
                    let _ = sender.send(JavascriptTcpSocketEvent::End);
                    if saw_local_shutdown.load(Ordering::SeqCst)
                        && !close_notified.swap(true, Ordering::SeqCst)
                    {
                        let _ = sender.send(JavascriptTcpSocketEvent::Close { had_error: false });
                    }
                    break;
                }
                Ok(bytes_read) => {
                    if sender
                        .send(JavascriptTcpSocketEvent::Data(
                            buffer[..bytes_read].to_vec(),
                        ))
                        .is_err()
                    {
                        break;
                    }
                }
                Err(error) => {
                    let code = io_error_code(&error);
                    let _ = sender.send(JavascriptTcpSocketEvent::Error {
                        code,
                        message: error.to_string(),
                    });
                    if !close_notified.swap(true, Ordering::SeqCst) {
                        let _ = sender.send(JavascriptTcpSocketEvent::Close { had_error: true });
                    }
                    break;
                }
            }
        }
    });
}

fn spawn_unix_socket_reader(
    stream: UnixStream,
    sender: Sender<JavascriptTcpSocketEvent>,
    saw_local_shutdown: Arc<AtomicBool>,
    saw_remote_end: Arc<AtomicBool>,
    close_notified: Arc<AtomicBool>,
) {
    thread::spawn(move || {
        let mut stream = stream;
        let mut buffer = vec![0_u8; 64 * 1024];
        loop {
            match stream.read(&mut buffer) {
                Ok(0) => {
                    saw_remote_end.store(true, Ordering::SeqCst);
                    let _ = sender.send(JavascriptTcpSocketEvent::End);
                    if saw_local_shutdown.load(Ordering::SeqCst)
                        && !close_notified.swap(true, Ordering::SeqCst)
                    {
                        let _ = sender.send(JavascriptTcpSocketEvent::Close { had_error: false });
                    }
                    break;
                }
                Ok(bytes_read) => {
                    if sender
                        .send(JavascriptTcpSocketEvent::Data(
                            buffer[..bytes_read].to_vec(),
                        ))
                        .is_err()
                    {
                        break;
                    }
                }
                Err(error) => {
                    let code = io_error_code(&error);
                    let _ = sender.send(JavascriptTcpSocketEvent::Error {
                        code,
                        message: error.to_string(),
                    });
                    if !close_notified.swap(true, Ordering::SeqCst) {
                        let _ = sender.send(JavascriptTcpSocketEvent::Close { had_error: true });
                    }
                    break;
                }
            }
        }
    });
}

fn terminate_child_process_tree(kernel: &mut SidecarKernel, process: &mut ActiveProcess) {
    let listener_ids = process.tcp_listeners.keys().cloned().collect::<Vec<_>>();
    for listener_id in listener_ids {
        if let Some(listener) = process.tcp_listeners.remove(&listener_id) {
            let _ = listener.close();
        }
    }

    let sockets = process.tcp_sockets.keys().cloned().collect::<Vec<_>>();
    for socket_id in sockets {
        if let Some(socket) = process.tcp_sockets.remove(&socket_id) {
            let _ = socket.close();
        }
    }

    let unix_listener_ids = process.unix_listeners.keys().cloned().collect::<Vec<_>>();
    for listener_id in unix_listener_ids {
        if let Some(listener) = process.unix_listeners.remove(&listener_id) {
            let _ = listener.close();
        }
    }

    let unix_sockets = process.unix_sockets.keys().cloned().collect::<Vec<_>>();
    for socket_id in unix_sockets {
        if let Some(socket) = process.unix_sockets.remove(&socket_id) {
            let _ = socket.close();
        }
    }

    let udp_socket_ids = process.udp_sockets.keys().cloned().collect::<Vec<_>>();
    for socket_id in udp_socket_ids {
        if let Some(mut socket) = process.udp_sockets.remove(&socket_id) {
            socket.close();
        }
    }

    let child_ids = process.child_processes.keys().cloned().collect::<Vec<_>>();
    for child_id in child_ids {
        let Some(mut child) = process.child_processes.remove(&child_id) else {
            continue;
        };
        terminate_child_process_tree(kernel, &mut child);
        let _ = kernel.kill_process(EXECUTION_DRIVER_NAME, child.kernel_pid, SIGTERM);
        let _ = signal_runtime_process(child.execution.child_pid(), SIGTERM);
        child.kernel_handle.finish(0);
        let _ = kernel.wait_and_reap(child.kernel_pid);
    }
}

fn javascript_sync_rpc_arg_str<'a>(
    args: &'a [Value],
    index: usize,
    label: &str,
) -> Result<&'a str, SidecarError> {
    args.get(index)
        .and_then(Value::as_str)
        .ok_or_else(|| SidecarError::InvalidState(format!("{label} must be a string argument")))
}

fn javascript_sync_rpc_encoding(args: &[Value]) -> Option<String> {
    args.get(1).and_then(|value| {
        value.as_str().map(str::to_owned).or_else(|| {
            value
                .get("encoding")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
    })
}

fn javascript_sync_rpc_option_bool(args: &[Value], index: usize, key: &str) -> Option<bool> {
    args.get(index)
        .and_then(|value| value.get(key))
        .and_then(Value::as_bool)
}

fn javascript_sync_rpc_option_u32(
    args: &[Value],
    index: usize,
    key: &str,
) -> Result<Option<u32>, SidecarError> {
    let Some(value) = args.get(index).and_then(|value| {
        if value.is_object() {
            value.get(key)
        } else if key == "mode" {
            Some(value)
        } else {
            None
        }
    }) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }

    let numeric = value
        .as_u64()
        .or_else(|| {
            value
                .as_f64()
                .filter(|number| number.is_finite() && *number >= 0.0)
                .map(|number| number as u64)
        })
        .ok_or_else(|| SidecarError::InvalidState(format!("{key} must be numeric")))?;

    u32::try_from(numeric)
        .map(Some)
        .map_err(|_| SidecarError::InvalidState(format!("{key} must fit within u32")))
}

fn javascript_sync_rpc_arg_u32(
    args: &[Value],
    index: usize,
    label: &str,
) -> Result<u32, SidecarError> {
    let value = javascript_sync_rpc_arg_u64(args, index, label)?;
    u32::try_from(value)
        .map_err(|_| SidecarError::InvalidState(format!("{label} must fit within u32")))
}

fn javascript_sync_rpc_arg_u32_optional(
    args: &[Value],
    index: usize,
    label: &str,
) -> Result<Option<u32>, SidecarError> {
    javascript_sync_rpc_arg_u64_optional(args, index, label)?
        .map(|value| {
            u32::try_from(value)
                .map_err(|_| SidecarError::InvalidState(format!("{label} must fit within u32")))
        })
        .transpose()
}

fn javascript_sync_rpc_arg_u64(
    args: &[Value],
    index: usize,
    label: &str,
) -> Result<u64, SidecarError> {
    let Some(value) = args.get(index) else {
        return Err(SidecarError::InvalidState(format!("{label} is required")));
    };

    value
        .as_u64()
        .or_else(|| {
            value
                .as_f64()
                .filter(|number| number.is_finite() && *number >= 0.0)
                .map(|number| number as u64)
        })
        .ok_or_else(|| SidecarError::InvalidState(format!("{label} must be a numeric argument")))
}

fn javascript_sync_rpc_arg_u64_optional(
    args: &[Value],
    index: usize,
    label: &str,
) -> Result<Option<u64>, SidecarError> {
    let Some(value) = args.get(index) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    javascript_sync_rpc_arg_u64(args, index, label).map(Some)
}

fn javascript_sync_rpc_stat_value(stat: VirtualStat) -> Value {
    json!({
        "mode": stat.mode,
        "size": stat.size,
        "blocks": stat.blocks,
        "dev": stat.dev,
        "rdev": stat.rdev,
        "isDirectory": stat.is_directory,
        "isSymbolicLink": stat.is_symbolic_link,
        "atimeMs": stat.atime_ms,
        "mtimeMs": stat.mtime_ms,
        "ctimeMs": stat.ctime_ms,
        "birthtimeMs": stat.birthtime_ms,
        "ino": stat.ino,
        "nlink": stat.nlink,
        "uid": stat.uid,
        "gid": stat.gid,
    })
}

fn javascript_sync_rpc_readdir_value(entries: Vec<String>) -> Value {
    json!(
        entries
            .into_iter()
            .filter(|entry| entry != "." && entry != "..")
            .collect::<Vec<_>>()
    )
}

fn javascript_sync_rpc_bytes_arg(
    args: &[Value],
    index: usize,
    label: &str,
) -> Result<Vec<u8>, SidecarError> {
    let Some(value) = args.get(index) else {
        return Err(SidecarError::InvalidState(format!("{label} is required")));
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
        return Err(SidecarError::InvalidState(format!(
            "{label} must be a string or encoded bytes payload"
        )));
    };

    base64::engine::general_purpose::STANDARD
        .decode(base64_value)
        .map_err(|error| {
            SidecarError::InvalidState(format!("{label} contains invalid base64: {error}"))
        })
}

fn javascript_sync_rpc_bytes_value(bytes: &[u8]) -> Value {
    json!({
        "__agentOsType": "bytes",
        "base64": base64::engine::general_purpose::STANDARD.encode(bytes),
    })
}

fn service_javascript_sync_rpc<B>(
    bridge: &SharedBridge<B>,
    vm_id: &str,
    dns: &VmDnsConfig,
    socket_paths: &JavascriptSocketPathContext,
    kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
    request: &JavascriptSyncRpcRequest,
    resource_limits: &ResourceLimits,
    network_counts: NetworkResourceCounts,
) -> Result<Value, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    match request.method.as_str() {
        "dns.lookup" | "dns.resolve" | "dns.resolve4" | "dns.resolve6" => {
            service_javascript_dns_sync_rpc(bridge, vm_id, dns, request)
        }
        "net.connect"
        | "net.listen"
        | "net.poll"
        | "net.server_poll"
        | "net.server_connections"
        | "net.write"
        | "net.shutdown"
        | "net.destroy"
        | "net.server_close" => service_javascript_net_sync_rpc(
            bridge,
            vm_id,
            dns,
            socket_paths,
            kernel,
            process,
            request,
            resource_limits,
            network_counts,
        ),
        "dgram.createSocket" | "dgram.bind" | "dgram.send" | "dgram.poll" | "dgram.close" => {
            service_javascript_dgram_sync_rpc(
                bridge,
                vm_id,
                dns,
                socket_paths,
                process,
                request,
                resource_limits,
                network_counts,
            )
        }
        "process.umask" => {
            let new_mask = javascript_sync_rpc_arg_u32_optional(&request.args, 0, "process umask")?;
            kernel
                .umask(EXECUTION_DRIVER_NAME, process.kernel_pid, new_mask)
                .map(|mask| json!(mask))
                .map_err(kernel_error)
        }
        _ => service_javascript_fs_sync_rpc(kernel, process.kernel_pid, request),
    }
}

fn service_javascript_dns_sync_rpc<B>(
    bridge: &SharedBridge<B>,
    vm_id: &str,
    dns: &VmDnsConfig,
    request: &JavascriptSyncRpcRequest,
) -> Result<Value, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    match request.method.as_str() {
        "dns.lookup" => {
            let payload = request
                .args
                .first()
                .cloned()
                .ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "dns.lookup requires a request payload",
                    ))
                })
                .and_then(|value| {
                    serde_json::from_value::<JavascriptDnsLookupRequest>(value).map_err(|error| {
                        SidecarError::InvalidState(format!("invalid dns.lookup payload: {error}"))
                    })
                })?;
            bridge.require_network_access(
                vm_id,
                NetworkOperation::Dns,
                format_dns_resource(&payload.hostname),
            )?;
            let addresses = filter_dns_ip_addrs(
                resolve_dns_ip_addrs(bridge, vm_id, dns, &payload.hostname)?,
                payload.family,
            )?;
            let addresses = filter_dns_safe_ip_addrs(addresses, &payload.hostname)?;
            Ok(Value::Array(
                addresses
                    .into_iter()
                    .map(|ip| {
                        json!({
                            "address": ip.to_string(),
                            "family": if ip.is_ipv6() { 6 } else { 4 },
                        })
                    })
                    .collect(),
            ))
        }
        "dns.resolve" | "dns.resolve4" | "dns.resolve6" => {
            let payload = request
                .args
                .first()
                .cloned()
                .ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "dns.resolve requires a request payload",
                    ))
                })
                .and_then(|value| {
                    serde_json::from_value::<JavascriptDnsResolveRequest>(value).map_err(|error| {
                        SidecarError::InvalidState(format!("invalid dns.resolve payload: {error}"))
                    })
                })?;
            let family = match request.method.as_str() {
                "dns.resolve4" => Some(4),
                "dns.resolve6" => Some(6),
                _ => match payload
                    .rrtype
                    .as_deref()
                    .unwrap_or("A")
                    .to_ascii_uppercase()
                    .as_str()
                {
                    "A" => Some(4),
                    "AAAA" => Some(6),
                    other => {
                        return Err(SidecarError::InvalidState(format!(
                            "unsupported dns rrtype {other}"
                        )));
                    }
                },
            };
            bridge.require_network_access(
                vm_id,
                NetworkOperation::Dns,
                format_dns_resource(&payload.hostname),
            )?;
            let addresses = filter_dns_ip_addrs(
                resolve_dns_ip_addrs(bridge, vm_id, dns, &payload.hostname)?,
                family,
            )?;
            let addresses = filter_dns_safe_ip_addrs(addresses, &payload.hostname)?;
            Ok(Value::Array(
                addresses
                    .into_iter()
                    .map(|ip| Value::String(ip.to_string()))
                    .collect(),
            ))
        }
        other => Err(SidecarError::InvalidState(format!(
            "unsupported JavaScript dns sync RPC method {other}"
        ))),
    }
}

fn service_javascript_dgram_sync_rpc<B>(
    bridge: &SharedBridge<B>,
    vm_id: &str,
    dns: &VmDnsConfig,
    socket_paths: &JavascriptSocketPathContext,
    process: &mut ActiveProcess,
    request: &JavascriptSyncRpcRequest,
    resource_limits: &ResourceLimits,
    network_counts: NetworkResourceCounts,
) -> Result<Value, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    match request.method.as_str() {
        "dgram.createSocket" => {
            check_network_resource_limit(
                resource_limits.max_sockets,
                network_counts.sockets,
                1,
                "socket",
            )?;
            let payload = request
                .args
                .first()
                .cloned()
                .ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "dgram.createSocket requires a request payload",
                    ))
                })
                .and_then(|value| {
                    serde_json::from_value::<JavascriptDgramCreateSocketRequest>(value).map_err(
                        |error| {
                            SidecarError::InvalidState(format!(
                                "invalid dgram.createSocket payload: {error}"
                            ))
                        },
                    )
                })?;
            let family = JavascriptUdpFamily::from_socket_type(&payload.socket_type)?;
            let socket_id = process.allocate_udp_socket_id();
            process
                .udp_sockets
                .insert(socket_id.clone(), ActiveUdpSocket::new(family));
            Ok(json!({
                "socketId": socket_id,
                "type": family.socket_type(),
            }))
        }
        "dgram.bind" => {
            let socket_id = javascript_sync_rpc_arg_str(&request.args, 0, "dgram.bind socket id")?;
            let payload = request
                .args
                .get(1)
                .cloned()
                .ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "dgram.bind requires a request payload",
                    ))
                })
                .and_then(|value| {
                    serde_json::from_value::<JavascriptDgramBindRequest>(value).map_err(|error| {
                        SidecarError::InvalidState(format!("invalid dgram.bind payload: {error}"))
                    })
                })?;
            let socket = process.udp_sockets.get_mut(socket_id).ok_or_else(|| {
                SidecarError::InvalidState(format!("unknown UDP socket {socket_id}"))
            })?;
            let local_addr = socket.bind(payload.address.as_deref(), payload.port, socket_paths)?;
            Ok(json!({
                "localAddress": local_addr.ip().to_string(),
                "localPort": local_addr.port(),
                "family": socket_addr_family(&local_addr),
            }))
        }
        "dgram.send" => {
            let socket_id = javascript_sync_rpc_arg_str(&request.args, 0, "dgram.send socket id")?;
            let chunk = javascript_sync_rpc_bytes_arg(&request.args, 1, "dgram.send payload")?;
            let payload = request
                .args
                .get(2)
                .cloned()
                .ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "dgram.send requires a request payload",
                    ))
                })
                .and_then(|value| {
                    serde_json::from_value::<JavascriptDgramSendRequest>(value).map_err(|error| {
                        SidecarError::InvalidState(format!("invalid dgram.send payload: {error}"))
                    })
                })?;
            let socket = process.udp_sockets.get_mut(socket_id).ok_or_else(|| {
                SidecarError::InvalidState(format!("unknown UDP socket {socket_id}"))
            })?;
            let (written, local_addr) = socket.send_to(
                bridge,
                vm_id,
                dns,
                payload.address.as_deref().unwrap_or("localhost"),
                payload.port,
                socket_paths,
                &chunk,
            )?;
            Ok(json!({
                "bytes": written,
                "localAddress": local_addr.ip().to_string(),
                "localPort": local_addr.port(),
                "family": socket_addr_family(&local_addr),
            }))
        }
        "dgram.poll" => {
            let socket_id = javascript_sync_rpc_arg_str(&request.args, 0, "dgram.poll socket id")?;
            let wait_ms =
                javascript_sync_rpc_arg_u64_optional(&request.args, 1, "dgram.poll wait ms")?
                    .unwrap_or_default();
            let event = {
                let socket = process.udp_sockets.get(socket_id).ok_or_else(|| {
                    SidecarError::InvalidState(format!("unknown UDP socket {socket_id}"))
                })?;
                socket.poll(Duration::from_millis(wait_ms))?
            };

            match event {
                Some(JavascriptUdpSocketEvent::Message { data, remote_addr }) => {
                    let family = JavascriptSocketFamily::from_ip(remote_addr.ip());
                    let guest_remote_port = if is_loopback_ip(remote_addr.ip()) {
                        socket_paths
                            .guest_udp_port_for_host_port(family, remote_addr.port())
                            .unwrap_or(remote_addr.port())
                    } else {
                        remote_addr.port()
                    };
                    Ok(json!({
                    "type": "message",
                    "data": javascript_sync_rpc_bytes_value(&data),
                    "remoteAddress": remote_addr.ip().to_string(),
                    "remotePort": guest_remote_port,
                    "remoteFamily": socket_addr_family(&remote_addr),
                    }))
                }
                Some(JavascriptUdpSocketEvent::Error { code, message }) => Ok(json!({
                    "type": "error",
                    "code": code,
                    "message": message,
                })),
                None => Ok(Value::Null),
            }
        }
        "dgram.close" => {
            let socket_id = javascript_sync_rpc_arg_str(&request.args, 0, "dgram.close socket id")?;
            let mut socket = process.udp_sockets.remove(socket_id).ok_or_else(|| {
                SidecarError::InvalidState(format!("unknown UDP socket {socket_id}"))
            })?;
            socket.close();
            Ok(Value::Null)
        }
        other => Err(SidecarError::InvalidState(format!(
            "unsupported JavaScript dgram sync RPC method {other}"
        ))),
    }
}

fn service_javascript_net_sync_rpc<B>(
    bridge: &SharedBridge<B>,
    vm_id: &str,
    dns: &VmDnsConfig,
    socket_paths: &JavascriptSocketPathContext,
    kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
    request: &JavascriptSyncRpcRequest,
    resource_limits: &ResourceLimits,
    network_counts: NetworkResourceCounts,
) -> Result<Value, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    match request.method.as_str() {
        "net.connect" => {
            check_network_resource_limit(
                resource_limits.max_sockets,
                network_counts.sockets,
                1,
                "socket",
            )?;
            check_network_resource_limit(
                resource_limits.max_connections,
                network_counts.connections,
                1,
                "connection",
            )?;
            let payload = request
                .args
                .first()
                .cloned()
                .ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "net.connect requires a request payload",
                    ))
                })
                .and_then(|value| {
                    serde_json::from_value::<JavascriptNetConnectRequest>(value).map_err(|error| {
                        SidecarError::InvalidState(format!("invalid net.connect payload: {error}"))
                    })
                })?;
            if let Some(path) = payload.path.as_deref() {
                let guest_path = normalize_path(path);
                let host_path = resolve_guest_socket_host_path(socket_paths, &guest_path);
                let socket = ActiveUnixSocket::connect(&host_path, &guest_path)?;
                let socket_id = process.allocate_unix_socket_id();
                process.unix_sockets.insert(socket_id.clone(), socket);
                Ok(json!({
                    "socketId": socket_id,
                    "remotePath": guest_path,
                }))
            } else {
                let port = payload.port.ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "net.connect requires either a path or port",
                    ))
                })?;
                let host = payload.host.as_deref().unwrap_or("localhost");
                bridge.require_network_access(
                    vm_id,
                    NetworkOperation::Http,
                    format_tcp_resource(host, port),
                )?;
                let socket =
                    ActiveTcpSocket::connect(bridge, vm_id, dns, host, port, socket_paths)?;
                let socket_id = process.allocate_tcp_socket_id();
                let local_addr = socket.guest_local_addr;
                let remote_addr = socket.guest_remote_addr;
                process.tcp_sockets.insert(socket_id.clone(), socket);
                Ok(json!({
                    "socketId": socket_id,
                    "localAddress": local_addr.ip().to_string(),
                    "localPort": local_addr.port(),
                    "remoteAddress": remote_addr.ip().to_string(),
                    "remotePort": remote_addr.port(),
                    "remoteFamily": socket_addr_family(&remote_addr),
                }))
            }
        }
        "net.listen" => {
            check_network_resource_limit(
                resource_limits.max_sockets,
                network_counts.sockets,
                1,
                "socket",
            )?;
            let payload = request
                .args
                .first()
                .cloned()
                .ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "net.listen requires a request payload",
                    ))
                })
                .and_then(|value| {
                    serde_json::from_value::<JavascriptNetListenRequest>(value).map_err(|error| {
                        SidecarError::InvalidState(format!("invalid net.listen payload: {error}"))
                    })
                })?;
            if let Some(path) = payload.path.as_deref() {
                let guest_path = normalize_path(path);
                if kernel.exists(&guest_path).map_err(kernel_error)? {
                    return Err(sidecar_net_error(std::io::Error::from_raw_os_error(
                        libc::EADDRINUSE,
                    )));
                }

                let host_path = resolve_guest_socket_host_path(socket_paths, &guest_path);
                let on_host_mount =
                    host_mount_path_for_guest_path_from_mounts(&socket_paths.mounts, &guest_path)
                        .is_some();
                let listener = ActiveUnixListener::bind(&host_path, &guest_path, payload.backlog)?;
                if !on_host_mount {
                    ensure_kernel_parent_directories(kernel, &guest_path)?;
                    kernel
                        .write_file(&guest_path, Vec::new())
                        .map_err(kernel_error)?;
                }
                let listener_id = process.allocate_unix_listener_id();
                process.unix_listeners.insert(listener_id.clone(), listener);
                Ok(json!({
                    "serverId": listener_id,
                    "path": guest_path,
                }))
            } else {
                let (family, host) = normalize_tcp_listen_host(payload.host.as_deref())?;
                let requested_port = payload.port.unwrap_or(0);
                bridge.require_network_access(
                    vm_id,
                    NetworkOperation::Listen,
                    format_tcp_resource(host, requested_port),
                )?;
                let port = allocate_guest_listen_port(
                    requested_port,
                    family,
                    &socket_paths.used_tcp_guest_ports,
                    socket_paths.listen_policy,
                )?;
                let listener = ActiveTcpListener::bind(host, port, payload.backlog)?;
                let listener_id = process.allocate_tcp_listener_id();
                let local_addr = listener.guest_local_addr();
                process.tcp_listeners.insert(listener_id.clone(), listener);
                Ok(json!({
                    "serverId": listener_id,
                    "localAddress": local_addr.ip().to_string(),
                    "localPort": local_addr.port(),
                    "family": socket_addr_family(&local_addr),
                }))
            }
        }
        "net.poll" => {
            let socket_id = javascript_sync_rpc_arg_str(&request.args, 0, "net.poll socket id")?;
            let wait_ms =
                javascript_sync_rpc_arg_u64_optional(&request.args, 1, "net.poll wait ms")?
                    .unwrap_or_default();
            let event = if let Some(socket) = process.tcp_sockets.get_mut(socket_id) {
                socket.poll(Duration::from_millis(wait_ms))?
            } else if let Some(socket) = process.unix_sockets.get_mut(socket_id) {
                socket.poll(Duration::from_millis(wait_ms))?
            } else {
                return Err(SidecarError::InvalidState(format!(
                    "unknown net socket {socket_id}"
                )));
            };

            match event {
                Some(JavascriptTcpSocketEvent::Data(chunk)) => Ok(json!({
                    "type": "data",
                    "data": javascript_sync_rpc_bytes_value(&chunk),
                })),
                Some(JavascriptTcpSocketEvent::End) => Ok(json!({
                    "type": "end",
                })),
                Some(JavascriptTcpSocketEvent::Error { code, message }) => Ok(json!({
                    "type": "error",
                    "code": code,
                    "message": message,
                })),
                Some(JavascriptTcpSocketEvent::Close { had_error }) => {
                    if let Some(socket) = process.tcp_sockets.remove(socket_id) {
                        if let Some(listener_id) = socket.listener_id.as_deref() {
                            if let Some(listener) = process.tcp_listeners.get_mut(listener_id) {
                                listener.release_connection(socket_id);
                            }
                        }
                    } else if let Some(socket) = process.unix_sockets.remove(socket_id) {
                        if let Some(listener_id) = socket.listener_id.as_deref() {
                            if let Some(listener) = process.unix_listeners.get_mut(listener_id) {
                                listener.release_connection(socket_id);
                            }
                        }
                    }
                    Ok(json!({
                        "type": "close",
                        "hadError": had_error,
                    }))
                }
                None => Ok(Value::Null),
            }
        }
        "net.server_poll" => {
            let listener_id =
                javascript_sync_rpc_arg_str(&request.args, 0, "net.server_poll listener id")?;
            let wait_ms =
                javascript_sync_rpc_arg_u64_optional(&request.args, 1, "net.server_poll wait ms")?
                    .unwrap_or_default();
            let tcp_event = if let Some(listener) = process.tcp_listeners.get_mut(listener_id) {
                Some(listener.poll(Duration::from_millis(wait_ms))?)
            } else {
                None
            };

            if let Some(event) = tcp_event {
                return match event {
                    Some(JavascriptTcpListenerEvent::Connection(pending)) => {
                        if let Err(error) = check_network_resource_limit(
                            resource_limits.max_sockets,
                            network_counts.sockets,
                            1,
                            "socket",
                        )
                        .and_then(|()| {
                            check_network_resource_limit(
                                resource_limits.max_connections,
                                network_counts.connections,
                                1,
                                "connection",
                            )
                        }) {
                            let _ = pending.stream.shutdown(Shutdown::Both);
                            return Ok(json!({
                                "type": "error",
                                "code": "EAGAIN",
                                "message": error.to_string(),
                            }));
                        }
                        let socket = ActiveTcpSocket::from_stream(
                            pending.stream,
                            Some(listener_id.to_string()),
                            pending.guest_local_addr,
                            pending.guest_remote_addr,
                        )?;
                        let socket_id = process.allocate_tcp_socket_id();
                        if let Some(listener) = process.tcp_listeners.get_mut(listener_id) {
                            listener.register_connection(&socket_id);
                        }
                        process.tcp_sockets.insert(socket_id.clone(), socket);
                        Ok(json!({
                            "type": "connection",
                            "socketId": socket_id,
                            "localAddress": pending.guest_local_addr.ip().to_string(),
                            "localPort": pending.guest_local_addr.port(),
                            "remoteAddress": pending.guest_remote_addr.ip().to_string(),
                            "remotePort": pending.guest_remote_addr.port(),
                            "remoteFamily": socket_addr_family(&pending.guest_remote_addr),
                        }))
                    }
                    Some(JavascriptTcpListenerEvent::Error { code, message }) => Ok(json!({
                        "type": "error",
                        "code": code,
                        "message": message,
                    })),
                    None => Ok(Value::Null),
                };
            }

            let event = {
                let listener = process.unix_listeners.get_mut(listener_id).ok_or_else(|| {
                    SidecarError::InvalidState(format!("unknown net listener {listener_id}"))
                })?;
                listener.poll(Duration::from_millis(wait_ms))?
            };

            match event {
                Some(JavascriptUnixListenerEvent::Connection(pending)) => {
                    if let Err(error) = check_network_resource_limit(
                        resource_limits.max_sockets,
                        network_counts.sockets,
                        1,
                        "socket",
                    )
                    .and_then(|()| {
                        check_network_resource_limit(
                            resource_limits.max_connections,
                            network_counts.connections,
                            1,
                            "connection",
                        )
                    }) {
                        let _ = pending.stream.shutdown(Shutdown::Both);
                        return Ok(json!({
                            "type": "error",
                            "code": "EAGAIN",
                            "message": error.to_string(),
                        }));
                    }
                    let socket = ActiveUnixSocket::from_stream(
                        pending.stream,
                        Some(listener_id.to_string()),
                        pending.local_path.clone(),
                        pending.remote_path.clone(),
                    )?;
                    let socket_id = process.allocate_unix_socket_id();
                    if let Some(listener) = process.unix_listeners.get_mut(listener_id) {
                        listener.register_connection(&socket_id);
                    }
                    process.unix_sockets.insert(socket_id.clone(), socket);
                    Ok(json!({
                        "type": "connection",
                        "socketId": socket_id,
                        "localPath": pending.local_path,
                        "remotePath": pending.remote_path,
                    }))
                }
                Some(JavascriptUnixListenerEvent::Error { code, message }) => Ok(json!({
                    "type": "error",
                    "code": code,
                    "message": message,
                })),
                None => Ok(Value::Null),
            }
        }
        "net.server_connections" => {
            let listener_id = javascript_sync_rpc_arg_str(
                &request.args,
                0,
                "net.server_connections listener id",
            )?;
            if let Some(listener) = process.tcp_listeners.get(listener_id) {
                Ok(json!(listener.active_connection_count()))
            } else {
                let listener = process.unix_listeners.get(listener_id).ok_or_else(|| {
                    SidecarError::InvalidState(format!("unknown net listener {listener_id}"))
                })?;
                Ok(json!(listener.active_connection_count()))
            }
        }
        "net.write" => {
            let socket_id = javascript_sync_rpc_arg_str(&request.args, 0, "net.write socket id")?;
            let chunk = javascript_sync_rpc_bytes_arg(&request.args, 1, "net.write chunk")?;
            if let Some(socket) = process.tcp_sockets.get(socket_id) {
                socket.write_all(&chunk).map(|written| json!(written))
            } else {
                let socket = process.unix_sockets.get(socket_id).ok_or_else(|| {
                    SidecarError::InvalidState(format!("unknown net socket {socket_id}"))
                })?;
                socket.write_all(&chunk).map(|written| json!(written))
            }
        }
        "net.shutdown" => {
            let socket_id =
                javascript_sync_rpc_arg_str(&request.args, 0, "net.shutdown socket id")?;
            if let Some(socket) = process.tcp_sockets.get(socket_id) {
                socket.shutdown_write()?;
            } else {
                let socket = process.unix_sockets.get(socket_id).ok_or_else(|| {
                    SidecarError::InvalidState(format!("unknown net socket {socket_id}"))
                })?;
                socket.shutdown_write()?;
            }
            Ok(Value::Null)
        }
        "net.destroy" => {
            let socket_id = javascript_sync_rpc_arg_str(&request.args, 0, "net.destroy socket id")?;
            if let Some(socket) = process.tcp_sockets.remove(socket_id) {
                if let Some(listener_id) = socket.listener_id.as_deref() {
                    if let Some(listener) = process.tcp_listeners.get_mut(listener_id) {
                        listener.release_connection(socket_id);
                    }
                }
                let _ = socket.close();
                Ok(Value::Null)
            } else {
                let socket = process.unix_sockets.remove(socket_id).ok_or_else(|| {
                    SidecarError::InvalidState(format!("unknown net socket {socket_id}"))
                })?;
                if let Some(listener_id) = socket.listener_id.as_deref() {
                    if let Some(listener) = process.unix_listeners.get_mut(listener_id) {
                        listener.release_connection(socket_id);
                    }
                }
                let _ = socket.close();
                Ok(Value::Null)
            }
        }
        "net.server_close" => {
            let listener_id =
                javascript_sync_rpc_arg_str(&request.args, 0, "net.server_close listener id")?;
            if let Some(listener) = process.tcp_listeners.remove(listener_id) {
                listener.close()?;
                Ok(Value::Null)
            } else {
                let listener = process.unix_listeners.remove(listener_id).ok_or_else(|| {
                    SidecarError::InvalidState(format!("unknown net listener {listener_id}"))
                })?;
                listener.close()?;
                Ok(Value::Null)
            }
        }
        _ => Err(SidecarError::InvalidState(format!(
            "unsupported JavaScript net sync RPC method {}",
            request.method
        ))),
    }
}

fn service_javascript_fs_sync_rpc(
    kernel: &mut SidecarKernel,
    kernel_pid: u32,
    request: &JavascriptSyncRpcRequest,
) -> Result<Value, SidecarError> {
    match request.method.as_str() {
        "fs.open" | "fs.openSync" => {
            let path = javascript_sync_rpc_arg_str(&request.args, 0, "filesystem open path")?;
            let flags = javascript_sync_rpc_arg_u32(&request.args, 1, "filesystem open flags")?;
            let mode =
                javascript_sync_rpc_arg_u32_optional(&request.args, 2, "filesystem open mode")?;
            kernel
                .fd_open(EXECUTION_DRIVER_NAME, kernel_pid, path, flags, mode)
                .map(|fd| json!(fd))
                .map_err(kernel_error)
        }
        "fs.read" | "fs.readSync" => {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "filesystem read fd")?;
            let length = usize::try_from(javascript_sync_rpc_arg_u64(
                &request.args,
                1,
                "filesystem read length",
            )?)
            .map_err(|_| {
                SidecarError::InvalidState(
                    "filesystem read length must fit within usize".to_string(),
                )
            })?;
            let position =
                javascript_sync_rpc_arg_u64_optional(&request.args, 2, "filesystem read position")?;
            let bytes = match position {
                Some(offset) => {
                    kernel.fd_pread(EXECUTION_DRIVER_NAME, kernel_pid, fd, length, offset)
                }
                None => kernel.fd_read(EXECUTION_DRIVER_NAME, kernel_pid, fd, length),
            };
            bytes
                .map(|payload| javascript_sync_rpc_bytes_value(&payload))
                .map_err(kernel_error)
        }
        "fs.write" | "fs.writeSync" => {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "filesystem write fd")?;
            let contents =
                javascript_sync_rpc_bytes_arg(&request.args, 1, "filesystem write contents")?;
            let position = javascript_sync_rpc_arg_u64_optional(
                &request.args,
                2,
                "filesystem write position",
            )?;
            let written = match position {
                Some(offset) => {
                    kernel.fd_pwrite(EXECUTION_DRIVER_NAME, kernel_pid, fd, &contents, offset)
                }
                None => kernel.fd_write(EXECUTION_DRIVER_NAME, kernel_pid, fd, &contents),
            };
            written.map(|count| json!(count)).map_err(kernel_error)
        }
        "fs.close" | "fs.closeSync" => {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "filesystem close fd")?;
            kernel
                .fd_close(EXECUTION_DRIVER_NAME, kernel_pid, fd)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "fs.fstat" | "fs.fstatSync" => {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "filesystem fstat fd")?;
            kernel
                .fd_stat(EXECUTION_DRIVER_NAME, kernel_pid, fd)
                .map_err(kernel_error)?;
            kernel
                .dev_fd_stat(EXECUTION_DRIVER_NAME, kernel_pid, fd)
                .map(javascript_sync_rpc_stat_value)
                .map_err(kernel_error)
        }
        "fs.readFileSync" | "fs.promises.readFile" => {
            let path = javascript_sync_rpc_arg_str(&request.args, 0, "filesystem readFile path")?;
            let encoding = javascript_sync_rpc_encoding(&request.args);
            kernel
                .read_file_for_process(EXECUTION_DRIVER_NAME, kernel_pid, path)
                .map(|content| match encoding.as_deref() {
                    Some("utf8") | Some("utf-8") => {
                        Value::String(String::from_utf8_lossy(&content).into_owned())
                    }
                    _ => javascript_sync_rpc_bytes_value(&content),
                })
                .map_err(kernel_error)
        }
        "fs.writeFileSync" | "fs.promises.writeFile" => {
            let path = javascript_sync_rpc_arg_str(&request.args, 0, "filesystem writeFile path")?;
            let contents =
                javascript_sync_rpc_bytes_arg(&request.args, 1, "filesystem writeFile contents")?;
            kernel
                .write_file_for_process(
                    EXECUTION_DRIVER_NAME,
                    kernel_pid,
                    path,
                    contents,
                    javascript_sync_rpc_option_u32(&request.args, 2, "mode")?,
                )
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "fs.statSync" | "fs.promises.stat" => {
            let path = javascript_sync_rpc_arg_str(&request.args, 0, "filesystem stat path")?;
            kernel
                .stat_for_process(EXECUTION_DRIVER_NAME, kernel_pid, path)
                .map(javascript_sync_rpc_stat_value)
                .map_err(kernel_error)
        }
        "fs.lstatSync" | "fs.promises.lstat" => {
            let path = javascript_sync_rpc_arg_str(&request.args, 0, "filesystem lstat path")?;
            kernel
                .lstat_for_process(EXECUTION_DRIVER_NAME, kernel_pid, path)
                .map(javascript_sync_rpc_stat_value)
                .map_err(kernel_error)
        }
        "fs.readdirSync" | "fs.promises.readdir" => {
            let path = javascript_sync_rpc_arg_str(&request.args, 0, "filesystem readdir path")?;
            kernel
                .read_dir_for_process(EXECUTION_DRIVER_NAME, kernel_pid, path)
                .map(javascript_sync_rpc_readdir_value)
                .map_err(kernel_error)
        }
        "fs.mkdirSync" | "fs.promises.mkdir" => {
            let path = javascript_sync_rpc_arg_str(&request.args, 0, "filesystem mkdir path")?;
            let recursive =
                javascript_sync_rpc_option_bool(&request.args, 1, "recursive").unwrap_or(false);
            kernel
                .mkdir_for_process(
                    EXECUTION_DRIVER_NAME,
                    kernel_pid,
                    path,
                    recursive,
                    javascript_sync_rpc_option_u32(&request.args, 1, "mode")?,
                )
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "fs.accessSync" | "fs.promises.access" => {
            let path = javascript_sync_rpc_arg_str(&request.args, 0, "filesystem access path")?;
            kernel
                .stat_for_process(EXECUTION_DRIVER_NAME, kernel_pid, path)
                .map(|_| Value::Null)
                .map_err(kernel_error)
        }
        "fs.copyFileSync" | "fs.promises.copyFile" => {
            let source =
                javascript_sync_rpc_arg_str(&request.args, 0, "filesystem copyFile source")?;
            let destination =
                javascript_sync_rpc_arg_str(&request.args, 1, "filesystem copyFile destination")?;
            let contents = kernel
                .read_file_for_process(EXECUTION_DRIVER_NAME, kernel_pid, source)
                .map_err(kernel_error)?;
            kernel
                .write_file_for_process(
                    EXECUTION_DRIVER_NAME,
                    kernel_pid,
                    destination,
                    contents,
                    None,
                )
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "fs.existsSync" => {
            let path = javascript_sync_rpc_arg_str(&request.args, 0, "filesystem exists path")?;
            kernel
                .exists_for_process(EXECUTION_DRIVER_NAME, kernel_pid, path)
                .map(Value::Bool)
                .map_err(kernel_error)
        }
        "fs.readlinkSync" => {
            let path = javascript_sync_rpc_arg_str(&request.args, 0, "filesystem readlink path")?;
            kernel
                .read_link_for_process(EXECUTION_DRIVER_NAME, kernel_pid, path)
                .map(Value::String)
                .map_err(kernel_error)
        }
        "fs.symlinkSync" => {
            let target =
                javascript_sync_rpc_arg_str(&request.args, 0, "filesystem symlink target")?;
            let link_path =
                javascript_sync_rpc_arg_str(&request.args, 1, "filesystem symlink path")?;
            kernel
                .symlink(target, link_path)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "fs.linkSync" => {
            let source = javascript_sync_rpc_arg_str(&request.args, 0, "filesystem link source")?;
            let destination =
                javascript_sync_rpc_arg_str(&request.args, 1, "filesystem link path")?;
            kernel
                .link(source, destination)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "fs.renameSync" | "fs.promises.rename" => {
            let source = javascript_sync_rpc_arg_str(&request.args, 0, "filesystem rename source")?;
            let destination =
                javascript_sync_rpc_arg_str(&request.args, 1, "filesystem rename destination")?;
            kernel
                .rename(source, destination)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "fs.rmdirSync" | "fs.promises.rmdir" => {
            let path = javascript_sync_rpc_arg_str(&request.args, 0, "filesystem rmdir path")?;
            kernel
                .remove_dir(path)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "fs.unlinkSync" | "fs.promises.unlink" => {
            let path = javascript_sync_rpc_arg_str(&request.args, 0, "filesystem unlink path")?;
            kernel
                .remove_file(path)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "fs.chmodSync" | "fs.promises.chmod" => {
            let path = javascript_sync_rpc_arg_str(&request.args, 0, "filesystem chmod path")?;
            let mode = javascript_sync_rpc_arg_u32(&request.args, 1, "filesystem chmod mode")?;
            kernel
                .chmod(path, mode)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "fs.chownSync" | "fs.promises.chown" => {
            let path = javascript_sync_rpc_arg_str(&request.args, 0, "filesystem chown path")?;
            let uid = javascript_sync_rpc_arg_u32(&request.args, 1, "filesystem chown uid")?;
            let gid = javascript_sync_rpc_arg_u32(&request.args, 2, "filesystem chown gid")?;
            kernel
                .chown(path, uid, gid)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "fs.utimesSync" | "fs.promises.utimes" => {
            let path = javascript_sync_rpc_arg_str(&request.args, 0, "filesystem utimes path")?;
            let atime_ms =
                javascript_sync_rpc_arg_u64(&request.args, 1, "filesystem utimes atime")?;
            let mtime_ms =
                javascript_sync_rpc_arg_u64(&request.args, 2, "filesystem utimes mtime")?;
            kernel
                .utimes(path, atime_ms, mtime_ms)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        _ => Err(SidecarError::InvalidState(format!(
            "unsupported JavaScript sync RPC method {}",
            request.method
        ))),
    }
}

fn kernel_error(error: KernelError) -> SidecarError {
    SidecarError::Kernel(error.to_string())
}

fn plugin_error(error: PluginError) -> SidecarError {
    SidecarError::Plugin(error.to_string())
}

fn javascript_error(error: JavascriptExecutionError) -> SidecarError {
    SidecarError::Execution(error.to_string())
}

fn wasm_error(error: WasmExecutionError) -> SidecarError {
    SidecarError::Execution(error.to_string())
}

fn python_error(error: PythonExecutionError) -> SidecarError {
    SidecarError::Execution(error.to_string())
}

fn vfs_error(error: VfsError) -> SidecarError {
    SidecarError::Kernel(error.to_string())
}

fn parse_signal(signal: &str) -> Result<i32, SidecarError> {
    let trimmed = signal.trim();
    if trimmed.is_empty() {
        return Err(SidecarError::InvalidState(String::from(
            "kill_process requires a non-empty signal",
        )));
    }

    if let Ok(value) = trimmed.parse::<i32>() {
        return match value {
            0 | libc::SIGINT | SIGKILL | SIGTERM | libc::SIGCONT | libc::SIGSTOP => Ok(value),
            _ => Err(SidecarError::InvalidState(format!(
                "unsupported kill_process signal {signal}"
            ))),
        };
    }

    let upper = trimmed.to_ascii_uppercase();
    let normalized = upper.strip_prefix("SIG").unwrap_or(&upper);

    signal_number_from_name(normalized).ok_or_else(|| {
        SidecarError::InvalidState(format!("unsupported kill_process signal {signal}"))
    })
}

fn signal_number_from_name(signal: &str) -> Option<i32> {
    match signal {
        "INT" => Some(libc::SIGINT),
        "KILL" => Some(SIGKILL),
        "TERM" => Some(SIGTERM),
        "CONT" => Some(libc::SIGCONT),
        "STOP" => Some(libc::SIGSTOP),
        _ => None,
    }
}

fn runtime_child_is_alive(child_pid: u32) -> Result<bool, SidecarError> {
    let wait_flags = WaitPidFlag::WNOHANG
        | WaitPidFlag::WNOWAIT
        | WaitPidFlag::WEXITED
        | WaitPidFlag::WUNTRACED
        | WaitPidFlag::WCONTINUED;
    match wait_on_child(WaitId::Pid(Pid::from_raw(child_pid as i32)), wait_flags) {
        Ok(WaitStatus::StillAlive)
        | Ok(WaitStatus::Stopped(_, _))
        | Ok(WaitStatus::Continued(_)) => Ok(true),
        Ok(WaitStatus::Exited(_, _)) | Ok(WaitStatus::Signaled(_, _, _)) => Ok(false),
        #[cfg(any(target_os = "linux", target_os = "android"))]
        Ok(WaitStatus::PtraceEvent(_, _, _) | WaitStatus::PtraceSyscall(_)) => Ok(true),
        Err(nix::errno::Errno::ECHILD) => Ok(false),
        Err(error) => Err(SidecarError::Execution(format!(
            "failed to inspect guest runtime process {child_pid}: {error}"
        ))),
    }
}

fn signal_runtime_process(child_pid: u32, signal: i32) -> Result<(), SidecarError> {
    if !runtime_child_is_alive(child_pid)? {
        return Ok(());
    }

    if signal == 0 {
        return Ok(());
    }

    let parsed = Signal::try_from(signal).map_err(|_| {
        SidecarError::InvalidState(format!("unsupported kill_process signal {signal}"))
    })?;
    let result = send_signal(Pid::from_raw(child_pid as i32), Some(parsed));

    match result {
        Ok(()) => Ok(()),
        Err(nix::errno::Errno::ESRCH) => Ok(()),
        Err(error) => Err(SidecarError::Execution(format!(
            "failed to signal guest runtime process {child_pid}: {error}"
        ))),
    }
}

fn error_code(error: &SidecarError) -> &'static str {
    match error {
        SidecarError::InvalidState(_) => "invalid_state",
        SidecarError::Unauthorized(_) => "unauthorized",
        SidecarError::Unsupported(_) => "unsupported",
        SidecarError::FrameTooLarge(_) => "frame_too_large",
        SidecarError::Kernel(_) => "kernel_error",
        SidecarError::Plugin(_) => "plugin_error",
        SidecarError::Execution(_) => "execution_error",
        SidecarError::Bridge(_) => "bridge_error",
        SidecarError::Io(_) => "io_error",
    }
}

fn guest_errno_code(message: &str) -> Option<&str> {
    let (code, _) = message.split_once(':')?;
    if code.len() < 2 || !code.starts_with('E') {
        return None;
    }
    code[1..]
        .bytes()
        .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
        .then_some(code)
}

fn javascript_sync_rpc_error_code(error: &SidecarError) -> String {
    match error {
        SidecarError::Execution(message) => guest_errno_code(message)
            .unwrap_or("ERR_AGENT_OS_NODE_SYNC_RPC")
            .to_owned(),
        _ => String::from("ERR_AGENT_OS_NODE_SYNC_RPC"),
    }
}

fn ignore_stale_javascript_sync_rpc_response(error: SidecarError) -> Result<(), SidecarError> {
    match error {
        SidecarError::Execution(message)
            if message.ends_with("is no longer pending")
                && message.starts_with("sync RPC request ") =>
        {
            Ok(())
        }
        other => Err(other),
    }
}

#[cfg(test)]
mod tests {
    mod bridge_support {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../bridge/tests/support.rs"
        ));
    }

    use super::*;
    use crate::protocol::{
        AuthenticateRequest, BootstrapRootFilesystemRequest, ConfigureVmRequest, CreateVmRequest,
        DisposeReason, GetZombieTimerCountRequest, GuestRuntimeKind, MountDescriptor,
        MountPluginDescriptor, OpenSessionRequest, OwnershipScope, PermissionDescriptor,
        PermissionMode, RequestFrame, RequestPayload, ResponsePayload, RootFilesystemEntry,
        RootFilesystemEntryKind, SidecarPlacement,
    };
    use crate::s3_plugin::test_support::MockS3Server;
    use crate::sandbox_agent_plugin::test_support::MockSandboxAgentServer;
    use agent_os_kernel::command_registry::CommandDriver;
    use agent_os_kernel::kernel::SpawnOptions;
    use agent_os_kernel::mount_table::MountEntry;
    use bridge_support::RecordingBridge;
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::fs;
    use std::io::{Read, Write};
    use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    const TEST_AUTH_TOKEN: &str = "sidecar-test-token";
    const TLS_TEST_KEY_PEM: &str = "-----BEGIN PRIVATE KEY-----\n\
MIIEvgIBADANBgkqhkiG9w0BAQEFAASCBKgwggSkAgEAAoIBAQClvETzHfSyd1Y+\n\
sjCfGkuyGxFMzwQlYjUrE0iwdMF774LYHFdpvtEo3sLOW6/b1xfXS/55jq+aggxS\n\
v+vgtjrhGf/y33XzdrjxcVBRWIsgAtxMHsNKO4EQ/uA1g6zlbaSIu+ZWX3bkDuTi\n\
K45VW69M0XSVyv8XFGYOcf8LTI87gTtXHuT92iej77IM2lHqLXCzQVr+NQ9yvXld\n\
9yHlA2ZfYqhkSTLdDablqfgirrQIzZzLypSGQwZUU06nCtZ+dg6SNV4TGL4NqekD\n\
jXR3BvmZu5l4sGAsNfFVjLx6hxsLt8uqn65sCAwBDdfucR+39+pHA+esj6NAWAFO\n\
J9CB94sfAgMBAAECggEABQTA772x+a98aJSbvU2eCiwgp3tDTGB/bKj+U/2NGFQl\n\
2aZuDTEugzbPnlEPb7BBNA9EiujDr4GNnvnZyimqecOASRn0J+Wp7wG35Waxe8wq\n\
YJGz5y0LGPkmz+gHVcEusMdDz8y/PGOpEaIxAquukLxs89Y8SDYhawGPsAdm9O3F\n\
4a+aosyQwS26mkZ/1WZOTsOVd4A1/1pxBvsANURj+pq7ed/1WqgrZBN/BG1TX5Xm\n\
DZeYy01kTCMWtcAb4f8PxGpbkSGMvBb+Mj5XtZByvfQeC+Cs5ECXhmJtVaYVUHhT\n\
vI0oTMGvit9ffoYNds0qTeZpEeineaDH3sD16D037QKBgQDX5b65KfIVH0/WvcbJ\n\
Gx2Wh7knXdDBky40wdq4buKK+ImzPPRxOsQ+xEMgEaZs8gb7LBapbB0cZ+YsKBOt\n\
4FY86XQU5V5ju2ntldIIIaugIGgvGS0jdRMH3ux6iEjPZE6Fm7/s8bjIgqB7keWh\n\
1rcZwDrwMzqwAUoBTJX58OY/fQKBgQDEhT5U7TqgEFVSspYh8c8yVRV9udiphPH3\n\
3XIbo9iV3xzNFdwtNHC+2eLM+4J3WKjhB0UvzrlIegSqKPIsy+0nD1uzaU+O72gg\n\
7+NKSh0RT61UDolk+P4s/2+5tnZqSNYO7Sd/svE/rkwIEtDEI5tb1nqq75h/HDEW\n\
k56GHAxvywKBgGmGmTdmIjZizKJYti4b+9VU15I/T8ceCmqtChw1zrNAkgWy2IPz\n\
xnIreefV2LPNhM4GGbmL55q3yhBxMlU9nsk9DokcJ4u10ivXnAZvdrTYwjOrKZ34\n\
HmotcwbdUEFWdO7nVuMYr0oKVyivAj+ddHe4ttYrJBddOe/yoCe/sLr9AoGBAKHL\n\
IVpCRXXqfJStOzWPI4rIyfzMuTg3oA71XjCrYHFjUw715GPDPN+j+znQB8XCVKeP\n\
mMKXa6vj6Vs+gsOm0QTLfC/lj/6Z1Bzp4zMSeYP7GTSPE0bySDE7y/wV4L/4X2PC\n\
lDZqWHyZPzeWZhJVTl754dxBjkd4KmHv/x9ikEqpAoGBAJNA0u0fKhdWDz32+a2F\n\
+plJ18kQvGuwKFWIIVHBDc0wCxLKWKr5wgkhdcAEpy4mgosiZ09DzV/OpQBBHVWZ\n\
v/Cn/DwZyoiXIi5onf7AqWIhw+aem+oMbugbSIYqDwYkwnN79tsza0KC1ScphIuf\n\
vKoOAdY4xOcG9BEZZoKVOa8R\n\
-----END PRIVATE KEY-----\n";
    const TLS_TEST_CERT_PEM: &str = "-----BEGIN CERTIFICATE-----\n\
MIIDCTCCAfGgAwIBAgIUJqRgTEIlpbfqbQnyo9hxLyIn3qYwDQYJKoZIhvcNAQEL\n\
BQAwFDESMBAGA1UEAwwJbG9jYWxob3N0MB4XDTI2MDQwNTA3MTAwOVoXDTI2MDQw\n\
NjA3MTAwOVowFDESMBAGA1UEAwwJbG9jYWxob3N0MIIBIjANBgkqhkiG9w0BAQEF\n\
AAOCAQ8AMIIBCgKCAQEApbxE8x30sndWPrIwnxpLshsRTM8EJWI1KxNIsHTBe++C\n\
2BxXab7RKN7Czluv29cX10v+eY6vmoIMUr/r4LY64Rn/8t9183a48XFQUViLIALc\n\
TB7DSjuBEP7gNYOs5W2kiLvmVl925A7k4iuOVVuvTNF0lcr/FxRmDnH/C0yPO4E7\n\
Vx7k/dono++yDNpR6i1ws0Fa/jUPcr15Xfch5QNmX2KoZEky3Q2m5an4Iq60CM2c\n\
y8qUhkMGVFNOpwrWfnYOkjVeExi+DanpA410dwb5mbuZeLBgLDXxVYy8eocbC7fL\n\
qp+ubAgMAQ3X7nEft/fqRwPnrI+jQFgBTifQgfeLHwIDAQABo1MwUTAdBgNVHQ4E\n\
FgQUwViZyKE6S2vgTAkexnZFccSwoPMwHwYDVR0jBBgwFoAUwViZyKE6S2vgTAke\n\
xnZFccSwoPMwDwYDVR0TAQH/BAUwAwEB/zANBgkqhkiG9w0BAQsFAAOCAQEAadmK\n\
3Ugrvep6glHAfgPP54um9cjJZQZDPn5I7yvgDr/Zp/u/UMW/OUKSfL1VNHlbAVLc\n\
Yzq2RVTrJKObiTSoy99OzYkEdgfuEBBP7XBEQlqoOGYNRR+IZXBBiQ+m9CtajNwQ\n\
G6mr9//zZtV1y2UUBgtxVpry5iOekpkr8iXyDLnGpS2gKL5dwXCzWCKVCO3qVotn\n\
r6FBg4DCBMkwO6xOVN2yInPd6CPy/JAUPW50zWPnn4DKfeAAU0C+E75HN65jozdi\n\
12yT4K772P8oSecGPInZhqJgOv1q0BDG8gccOxX1PA4sE00Enqlbvxz7sku9y4zp\n\
ykAheWCsAteSEWVc0w==\n\
-----END CERTIFICATE-----\n";

    fn request(
        request_id: u64,
        ownership: OwnershipScope,
        payload: RequestPayload,
    ) -> RequestFrame {
        RequestFrame::new(request_id, ownership, payload)
    }

    fn create_test_sidecar() -> NativeSidecar<RecordingBridge> {
        NativeSidecar::with_config(
            RecordingBridge::default(),
            NativeSidecarConfig {
                sidecar_id: String::from("sidecar-test"),
                compile_cache_root: Some(std::env::temp_dir().join("agent-os-sidecar-test-cache")),
                expected_auth_token: Some(String::from(TEST_AUTH_TOKEN)),
                ..NativeSidecarConfig::default()
            },
        )
        .expect("create sidecar")
    }

    fn unexpected_response_error(expected: &str, other: ResponsePayload) -> SidecarError {
        SidecarError::InvalidState(format!("expected {expected} response, got {other:?}"))
    }

    fn authenticated_connection_id(auth: DispatchResult) -> Result<String, SidecarError> {
        match auth.response.payload {
            ResponsePayload::Authenticated(response) => {
                assert_eq!(
                    auth.response.ownership,
                    OwnershipScope::connection(&response.connection_id)
                );
                Ok(response.connection_id)
            }
            other => Err(unexpected_response_error("authenticated", other)),
        }
    }

    fn opened_session_id(session: DispatchResult) -> Result<String, SidecarError> {
        match session.response.payload {
            ResponsePayload::SessionOpened(response) => Ok(response.session_id),
            other => Err(unexpected_response_error("session_opened", other)),
        }
    }

    fn created_vm_id(response: DispatchResult) -> Result<String, SidecarError> {
        match response.response.payload {
            ResponsePayload::VmCreated(response) => Ok(response.vm_id),
            other => Err(unexpected_response_error("vm_created", other)),
        }
    }

    fn authenticate_and_open_session(
        sidecar: &mut NativeSidecar<RecordingBridge>,
    ) -> Result<(String, String), SidecarError> {
        let auth = sidecar
            .dispatch(request(
                1,
                OwnershipScope::connection("conn-1"),
                RequestPayload::Authenticate(AuthenticateRequest {
                    client_name: String::from("service-tests"),
                    auth_token: String::from(TEST_AUTH_TOKEN),
                }),
            ))
            .expect("authenticate");
        let connection_id = authenticated_connection_id(auth)?;

        let session = sidecar
            .dispatch(request(
                2,
                OwnershipScope::connection(&connection_id),
                RequestPayload::OpenSession(OpenSessionRequest {
                    placement: SidecarPlacement::Shared { pool: None },
                    metadata: BTreeMap::new(),
                }),
            ))
            .expect("open session");
        let session_id = opened_session_id(session)?;
        Ok((connection_id, session_id))
    }

    fn create_vm(
        sidecar: &mut NativeSidecar<RecordingBridge>,
        connection_id: &str,
        session_id: &str,
        permissions: Vec<PermissionDescriptor>,
    ) -> Result<String, SidecarError> {
        create_vm_with_metadata(
            sidecar,
            connection_id,
            session_id,
            permissions,
            BTreeMap::new(),
        )
    }

    fn create_vm_with_metadata(
        sidecar: &mut NativeSidecar<RecordingBridge>,
        connection_id: &str,
        session_id: &str,
        permissions: Vec<PermissionDescriptor>,
        metadata: BTreeMap<String, String>,
    ) -> Result<String, SidecarError> {
        let response = sidecar
            .dispatch(request(
                3,
                OwnershipScope::session(connection_id, session_id),
                RequestPayload::CreateVm(CreateVmRequest {
                    runtime: GuestRuntimeKind::JavaScript,
                    metadata,
                    root_filesystem: Default::default(),
                    permissions,
                }),
            ))
            .expect("create vm");

        created_vm_id(response)
    }

    fn temp_dir(prefix: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be monotonic enough for temp paths")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("{prefix}-{suffix}"));
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    fn write_fixture(path: &Path, contents: &str) {
        fs::write(path, contents).expect("write fixture");
    }

    fn assert_node_available() {
        let output = Command::new("node")
            .arg("--version")
            .output()
            .expect("spawn node --version");
        assert!(
            output.status.success(),
            "node must be available for python dispatch tests"
        );
    }

    fn run_javascript_entry(
        sidecar: &mut NativeSidecar<RecordingBridge>,
        vm_id: &str,
        cwd: &Path,
        process_id: &str,
        allowed_node_builtins: &str,
    ) -> (String, String, Option<i32>) {
        let context = sidecar
            .javascript_engine
            .create_context(CreateJavascriptContextRequest {
                vm_id: vm_id.to_owned(),
                bootstrap_module: None,
                compile_cache_root: None,
            });
        let execution = sidecar
            .javascript_engine
            .start_execution(StartJavascriptExecutionRequest {
                vm_id: vm_id.to_owned(),
                context_id: context.context_id,
                argv: vec![String::from("./entry.mjs")],
                env: BTreeMap::from([(
                    String::from("AGENT_OS_ALLOWED_NODE_BUILTINS"),
                    allowed_node_builtins.to_owned(),
                )]),
                cwd: cwd.to_path_buf(),
            })
            .expect("start fake javascript execution");

        let kernel_handle = {
            let vm = sidecar.vms.get_mut(vm_id).expect("javascript vm");
            vm.kernel
                .spawn_process(
                    JAVASCRIPT_COMMAND,
                    vec![String::from("./entry.mjs")],
                    SpawnOptions {
                        requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                        cwd: Some(String::from("/")),
                        ..SpawnOptions::default()
                    },
                )
                .expect("spawn kernel javascript process")
        };

        {
            let vm = sidecar.vms.get_mut(vm_id).expect("javascript vm");
            vm.active_processes.insert(
                process_id.to_owned(),
                ActiveProcess::new(
                    kernel_handle.pid(),
                    kernel_handle,
                    GuestRuntimeKind::JavaScript,
                    ActiveExecution::Javascript(execution),
                ),
            );
        }

        let mut stdout = String::new();
        let mut stderr = String::new();
        let mut exit_code = None;
        for _ in 0..64 {
            let next_event = {
                let vm = sidecar.vms.get(vm_id).expect("javascript vm");
                vm.active_processes
                    .get(process_id)
                    .map(|process| {
                        process
                            .execution
                            .poll_event(Duration::from_secs(5))
                            .expect("poll javascript event")
                    })
                    .flatten()
            };
            let Some(event) = next_event else {
                if exit_code.is_some() {
                    break;
                }
                panic!("javascript process {process_id} disappeared before exit");
            };

            match &event {
                ActiveExecutionEvent::Stdout(chunk) => {
                    stdout.push_str(&String::from_utf8_lossy(chunk));
                }
                ActiveExecutionEvent::Stderr(chunk) => {
                    stderr.push_str(&String::from_utf8_lossy(chunk));
                }
                ActiveExecutionEvent::Exited(code) => {
                    exit_code = Some(*code);
                }
                _ => {}
            }

            sidecar
                .handle_execution_event(vm_id, process_id, event)
                .expect("handle javascript event");
        }

        (stdout, stderr, exit_code)
    }

    fn start_fake_javascript_process(
        sidecar: &mut NativeSidecar<RecordingBridge>,
        vm_id: &str,
        cwd: &Path,
        process_id: &str,
        allowed_node_builtins: &str,
    ) {
        let context = sidecar
            .javascript_engine
            .create_context(CreateJavascriptContextRequest {
                vm_id: vm_id.to_owned(),
                bootstrap_module: None,
                compile_cache_root: None,
            });
        let execution = sidecar
            .javascript_engine
            .start_execution(StartJavascriptExecutionRequest {
                vm_id: vm_id.to_owned(),
                context_id: context.context_id,
                argv: vec![String::from("./entry.mjs")],
                env: BTreeMap::from([(
                    String::from("AGENT_OS_ALLOWED_NODE_BUILTINS"),
                    allowed_node_builtins.to_owned(),
                )]),
                cwd: cwd.to_path_buf(),
            })
            .expect("start fake javascript execution");

        let kernel_handle = {
            let vm = sidecar.vms.get_mut(vm_id).expect("javascript vm");
            vm.kernel
                .spawn_process(
                    JAVASCRIPT_COMMAND,
                    vec![String::from("./entry.mjs")],
                    SpawnOptions {
                        requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                        cwd: Some(String::from("/")),
                        ..SpawnOptions::default()
                    },
                )
                .expect("spawn kernel javascript process")
        };

        let vm = sidecar.vms.get_mut(vm_id).expect("javascript vm");
        vm.active_processes.insert(
            process_id.to_owned(),
            ActiveProcess::new_with_host_cwd(
                kernel_handle.pid(),
                kernel_handle,
                GuestRuntimeKind::JavaScript,
                ActiveExecution::Javascript(execution),
                cwd.to_path_buf(),
            ),
        );
    }

    fn call_javascript_sync_rpc(
        sidecar: &mut NativeSidecar<RecordingBridge>,
        vm_id: &str,
        process_id: &str,
        request: JavascriptSyncRpcRequest,
    ) -> Result<Value, SidecarError> {
        let bridge = sidecar.bridge.clone();
        let (dns, socket_paths, counts, limits) = {
            let vm = sidecar.vms.get(vm_id).expect("javascript vm");
            (
                vm.dns.clone(),
                build_javascript_socket_path_context(vm).expect("build socket path context"),
                vm.active_processes
                    .get(process_id)
                    .expect("javascript process")
                    .network_resource_counts(),
                ResourceLimits::default(),
            )
        };

        let vm = sidecar.vms.get_mut(vm_id).expect("javascript vm");
        let process = vm
            .active_processes
            .get_mut(process_id)
            .expect("javascript process");
        service_javascript_sync_rpc(
            &bridge,
            vm_id,
            &dns,
            &socket_paths,
            &mut vm.kernel,
            process,
            &request,
            &limits,
            counts,
        )
    }

    #[test]
    fn dispose_vm_removes_per_vm_javascript_import_cache_directory() {
        let mut sidecar = create_test_sidecar();
        let (connection_id, session_id) =
            authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
        let vm_a =
            create_vm(&mut sidecar, &connection_id, &session_id, Vec::new()).expect("create vm a");
        let vm_b =
            create_vm(&mut sidecar, &connection_id, &session_id, Vec::new()).expect("create vm b");

        let cache_path_a = sidecar
            .javascript_engine
            .materialize_import_cache_for_vm(&vm_a)
            .expect("materialize vm a import cache")
            .to_path_buf();
        let cache_path_b = sidecar
            .javascript_engine
            .materialize_import_cache_for_vm(&vm_b)
            .expect("materialize vm b import cache")
            .to_path_buf();
        let cache_root_a = cache_path_a
            .parent()
            .expect("vm a cache parent")
            .to_path_buf();
        let cache_root_b = cache_path_b
            .parent()
            .expect("vm b cache parent")
            .to_path_buf();

        assert_ne!(cache_root_a, cache_root_b);
        assert!(cache_root_a.exists(), "vm a cache root should exist");
        assert!(cache_root_b.exists(), "vm b cache root should exist");

        sidecar
            .dispose_vm_internal(&connection_id, &session_id, &vm_a, DisposeReason::Requested)
            .expect("dispose vm a");

        assert!(
            !cache_root_a.exists(),
            "vm a cache root should be removed on dispose"
        );
        assert!(
            cache_root_b.exists(),
            "vm b cache root should remain until that VM is disposed"
        );
        assert!(
            sidecar
                .javascript_engine
                .import_cache_path_for_vm(&vm_a)
                .is_none(),
            "vm a cache entry should be removed from the engine"
        );
        assert_eq!(
            sidecar.javascript_engine.import_cache_path_for_vm(&vm_b),
            Some(cache_path_b.as_path())
        );

        sidecar
            .dispose_vm_internal(&connection_id, &session_id, &vm_b, DisposeReason::Requested)
            .expect("dispose vm b");
        assert!(
            !cache_root_b.exists(),
            "vm b cache root should be removed on dispose"
        );
    }

    #[test]
    fn get_zombie_timer_count_reports_kernel_state_before_and_after_waitpid() {
        let mut sidecar = create_test_sidecar();
        let (connection_id, session_id) =
            authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
        let vm_id =
            create_vm(&mut sidecar, &connection_id, &session_id, Vec::new()).expect("create vm");

        let zombie_pid = {
            let vm = sidecar.vms.get_mut(&vm_id).expect("configured vm");
            vm.kernel
                .register_driver(CommandDriver::new("test-driver", ["test-zombie"]))
                .expect("register test driver");
            let process = vm
                .kernel
                .spawn_process(
                    "test-zombie",
                    Vec::new(),
                    SpawnOptions {
                        requester_driver: Some(String::from("test-driver")),
                        ..SpawnOptions::default()
                    },
                )
                .expect("spawn test process");
            process.finish(17);
            assert_eq!(vm.kernel.zombie_timer_count(), 1);
            process.pid()
        };

        let zombie_count = sidecar
            .dispatch(request(
                4,
                OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                RequestPayload::GetZombieTimerCount(GetZombieTimerCountRequest::default()),
            ))
            .expect("query zombie count");
        match zombie_count.response.payload {
            ResponsePayload::ZombieTimerCount(response) => assert_eq!(response.count, 1),
            other => panic!("unexpected zombie count response: {other:?}"),
        }

        {
            let vm = sidecar.vms.get_mut(&vm_id).expect("configured vm");
            let waited = vm.kernel.waitpid(zombie_pid).expect("waitpid");
            assert_eq!(waited.pid, zombie_pid);
            assert_eq!(waited.status, 17);
            assert_eq!(vm.kernel.zombie_timer_count(), 0);
        }

        let reaped_count = sidecar
            .dispatch(request(
                5,
                OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                RequestPayload::GetZombieTimerCount(GetZombieTimerCountRequest::default()),
            ))
            .expect("query reaped zombie count");
        match reaped_count.response.payload {
            ResponsePayload::ZombieTimerCount(response) => assert_eq!(response.count, 0),
            other => panic!("unexpected zombie count response: {other:?}"),
        }
    }

    #[test]
    fn parse_signal_only_accepts_whitelisted_guest_signals() {
        assert_eq!(parse_signal("SIGINT").expect("parse SIGINT"), libc::SIGINT);
        assert_eq!(parse_signal("kill").expect("parse SIGKILL"), SIGKILL);
        assert_eq!(parse_signal("15").expect("parse numeric SIGTERM"), SIGTERM);
        assert_eq!(
            parse_signal("SIGCONT").expect("parse SIGCONT"),
            libc::SIGCONT
        );
        assert_eq!(
            parse_signal("SIGSTOP").expect("parse SIGSTOP"),
            libc::SIGSTOP
        );
        assert_eq!(parse_signal("0").expect("parse signal 0"), 0);
        assert!(parse_signal("SIGUSR1").is_err());
    }

    #[test]
    fn runtime_child_liveness_only_tracks_owned_children() {
        assert!(
            !runtime_child_is_alive(std::process::id()).expect("current pid is not a child"),
            "current process should not be treated as a guest runtime child"
        );

        let mut child = Command::new("sh")
            .arg("-c")
            .arg("sleep 10")
            .spawn()
            .expect("spawn child process");
        let child_pid = child.id();

        assert!(
            runtime_child_is_alive(child_pid).expect("inspect running child"),
            "running child should be considered alive"
        );

        signal_runtime_process(child_pid, SIGTERM).expect("signal running child");
        child.wait().expect("wait for signaled child");

        assert!(
            !runtime_child_is_alive(child_pid).expect("inspect reaped child"),
            "reaped child should no longer be considered alive"
        );
        signal_runtime_process(child_pid, SIGTERM).expect("ignore reaped child");
    }

    #[test]
    fn authenticated_connection_id_returns_error_for_unexpected_response() {
        let error = authenticated_connection_id(DispatchResult {
            response: ResponseFrame::new(
                1,
                OwnershipScope::connection("conn-1"),
                ResponsePayload::SessionOpened(SessionOpenedResponse {
                    session_id: String::from("session-1"),
                    owner_connection_id: String::from("conn-1"),
                }),
            ),
            events: Vec::new(),
        })
        .expect_err("unexpected auth payload should return an error");

        match error {
            SidecarError::InvalidState(message) => {
                assert!(message.contains("expected authenticated response"));
                assert!(message.contains("SessionOpened"));
            }
            other => panic!("expected invalid_state error, got {other:?}"),
        }
    }

    #[test]
    fn opened_session_id_returns_error_for_unexpected_response() {
        let error = opened_session_id(DispatchResult {
            response: ResponseFrame::new(
                2,
                OwnershipScope::connection("conn-1"),
                ResponsePayload::VmCreated(VmCreatedResponse {
                    vm_id: String::from("vm-1"),
                }),
            ),
            events: Vec::new(),
        })
        .expect_err("unexpected session payload should return an error");

        match error {
            SidecarError::InvalidState(message) => {
                assert!(message.contains("expected session_opened response"));
                assert!(message.contains("VmCreated"));
            }
            other => panic!("expected invalid_state error, got {other:?}"),
        }
    }

    #[test]
    fn created_vm_id_returns_error_for_unexpected_response() {
        let error = created_vm_id(DispatchResult {
            response: ResponseFrame::new(
                3,
                OwnershipScope::session("conn-1", "session-1"),
                ResponsePayload::Rejected(RejectedResponse {
                    code: String::from("invalid_state"),
                    message: String::from("not owned"),
                }),
            ),
            events: Vec::new(),
        })
        .expect_err("unexpected vm payload should return an error");

        match error {
            SidecarError::InvalidState(message) => {
                assert!(message.contains("expected vm_created response"));
                assert!(message.contains("Rejected"));
            }
            other => panic!("expected invalid_state error, got {other:?}"),
        }
    }

    #[test]
    fn configure_vm_instantiates_memory_mounts_through_the_plugin_registry() {
        let mut sidecar = create_test_sidecar();
        let (connection_id, session_id) =
            authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
        let vm_id =
            create_vm(&mut sidecar, &connection_id, &session_id, Vec::new()).expect("create vm");

        sidecar
            .dispatch(request(
                4,
                OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                RequestPayload::BootstrapRootFilesystem(BootstrapRootFilesystemRequest {
                    entries: vec![
                        RootFilesystemEntry {
                            path: String::from("/workspace"),
                            kind: RootFilesystemEntryKind::Directory,
                            ..Default::default()
                        },
                        RootFilesystemEntry {
                            path: String::from("/workspace/root-only.txt"),
                            kind: RootFilesystemEntryKind::File,
                            content: Some(String::from("root bootstrap file")),
                            ..Default::default()
                        },
                    ],
                }),
            ))
            .expect("bootstrap root workspace");

        sidecar
            .dispatch(request(
                5,
                OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                RequestPayload::ConfigureVm(ConfigureVmRequest {
                    mounts: vec![MountDescriptor {
                        guest_path: String::from("/workspace"),
                        read_only: false,
                        plugin: MountPluginDescriptor {
                            id: String::from("memory"),
                            config: json!({}),
                        },
                    }],
                    software: Vec::new(),
                    permissions: Vec::new(),
                    instructions: Vec::new(),
                    projected_modules: Vec::new(),
                    command_permissions: BTreeMap::new(),
                }),
            ))
            .expect("configure mounts");

        let vm = sidecar.vms.get_mut(&vm_id).expect("configured vm");
        let hidden = vm
            .kernel
            .filesystem_mut()
            .read_file("/workspace/root-only.txt")
            .expect_err("mounted filesystem should hide root-backed file");
        assert_eq!(hidden.code(), "ENOENT");

        vm.kernel
            .filesystem_mut()
            .write_file("/workspace/from-mount.txt", b"native mount".to_vec())
            .expect("write mounted file");
        assert_eq!(
            vm.kernel
                .filesystem_mut()
                .read_file("/workspace/from-mount.txt")
                .expect("read mounted file"),
            b"native mount".to_vec()
        );
        assert_eq!(
            vm.kernel.mounted_filesystems(),
            vec![
                MountEntry {
                    path: String::from("/workspace"),
                    plugin_id: String::from("memory"),
                    read_only: false,
                },
                MountEntry {
                    path: String::from("/"),
                    plugin_id: String::from("root"),
                    read_only: false,
                },
            ]
        );
    }

    #[test]
    fn configure_vm_applies_read_only_mount_wrappers() {
        let mut sidecar = create_test_sidecar();
        let (connection_id, session_id) =
            authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
        let vm_id =
            create_vm(&mut sidecar, &connection_id, &session_id, Vec::new()).expect("create vm");

        sidecar
            .dispatch(request(
                4,
                OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                RequestPayload::ConfigureVm(ConfigureVmRequest {
                    mounts: vec![MountDescriptor {
                        guest_path: String::from("/readonly"),
                        read_only: true,
                        plugin: MountPluginDescriptor {
                            id: String::from("memory"),
                            config: json!({}),
                        },
                    }],
                    software: Vec::new(),
                    permissions: Vec::new(),
                    instructions: Vec::new(),
                    projected_modules: Vec::new(),
                    command_permissions: BTreeMap::new(),
                }),
            ))
            .expect("configure readonly mount");

        let vm = sidecar.vms.get_mut(&vm_id).expect("configured vm");
        let error = vm
            .kernel
            .filesystem_mut()
            .write_file("/readonly/blocked.txt", b"nope".to_vec())
            .expect_err("readonly mount should reject writes");
        assert_eq!(error.code(), "EROFS");
    }

    #[test]
    fn configure_vm_instantiates_host_dir_mounts_through_the_plugin_registry() {
        let host_dir = temp_dir("agent-os-sidecar-host-dir");
        fs::write(host_dir.join("hello.txt"), "hello from host").expect("seed host dir");

        let mut sidecar = create_test_sidecar();
        let (connection_id, session_id) =
            authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
        let vm_id =
            create_vm(&mut sidecar, &connection_id, &session_id, Vec::new()).expect("create vm");

        sidecar
            .dispatch(request(
                4,
                OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                RequestPayload::BootstrapRootFilesystem(BootstrapRootFilesystemRequest {
                    entries: vec![
                        RootFilesystemEntry {
                            path: String::from("/workspace"),
                            kind: RootFilesystemEntryKind::Directory,
                            ..Default::default()
                        },
                        RootFilesystemEntry {
                            path: String::from("/workspace/root-only.txt"),
                            kind: RootFilesystemEntryKind::File,
                            content: Some(String::from("root bootstrap file")),
                            ..Default::default()
                        },
                    ],
                }),
            ))
            .expect("bootstrap root workspace");

        sidecar
            .dispatch(request(
                5,
                OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                RequestPayload::ConfigureVm(ConfigureVmRequest {
                    mounts: vec![MountDescriptor {
                        guest_path: String::from("/workspace"),
                        read_only: false,
                        plugin: MountPluginDescriptor {
                            id: String::from("host_dir"),
                            config: json!({
                                "hostPath": host_dir,
                                "readOnly": false,
                            }),
                        },
                    }],
                    software: Vec::new(),
                    permissions: Vec::new(),
                    instructions: Vec::new(),
                    projected_modules: Vec::new(),
                    command_permissions: BTreeMap::new(),
                }),
            ))
            .expect("configure host_dir mount");

        let vm = sidecar.vms.get_mut(&vm_id).expect("configured vm");
        let hidden = vm
            .kernel
            .filesystem_mut()
            .read_file("/workspace/root-only.txt")
            .expect_err("mounted host dir should hide root-backed file");
        assert_eq!(hidden.code(), "ENOENT");
        assert_eq!(
            vm.kernel
                .filesystem_mut()
                .read_file("/workspace/hello.txt")
                .expect("read mounted host file"),
            b"hello from host".to_vec()
        );

        vm.kernel
            .filesystem_mut()
            .write_file("/workspace/from-vm.txt", b"native host dir".to_vec())
            .expect("write host dir file");
        assert_eq!(
            fs::read_to_string(host_dir.join("from-vm.txt")).expect("read host output"),
            "native host dir"
        );

        fs::remove_dir_all(host_dir).expect("remove temp dir");
    }

    #[test]
    fn configure_vm_js_bridge_mount_preserves_hard_link_identity() {
        let mut sidecar = create_test_sidecar();
        sidecar
            .bridge
            .inspect(|bridge| {
                bridge.seed_directory(
                    "/workspace",
                    vec![agent_os_bridge::DirectoryEntry {
                        name: String::from("original.txt"),
                        kind: FileKind::File,
                    }],
                );
                bridge.seed_file("/workspace/original.txt", b"hello world".to_vec());
            })
            .expect("seed js bridge filesystem");

        let (connection_id, session_id) =
            authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
        let vm_id =
            create_vm(&mut sidecar, &connection_id, &session_id, Vec::new()).expect("create vm");

        sidecar
            .dispatch(request(
                4,
                OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                RequestPayload::ConfigureVm(ConfigureVmRequest {
                    mounts: vec![MountDescriptor {
                        guest_path: String::from("/workspace"),
                        read_only: false,
                        plugin: MountPluginDescriptor {
                            id: String::from("js_bridge"),
                            config: json!({}),
                        },
                    }],
                    software: Vec::new(),
                    permissions: Vec::new(),
                    instructions: Vec::new(),
                    projected_modules: Vec::new(),
                    command_permissions: BTreeMap::new(),
                }),
            ))
            .expect("configure js_bridge mount");

        let vm = sidecar.vms.get_mut(&vm_id).expect("configured vm");
        vm.kernel
            .filesystem_mut()
            .link("/workspace/original.txt", "/workspace/linked.txt")
            .expect("create js bridge hard link");

        let original = vm
            .kernel
            .filesystem_mut()
            .stat("/workspace/original.txt")
            .expect("stat original");
        let linked = vm
            .kernel
            .filesystem_mut()
            .stat("/workspace/linked.txt")
            .expect("stat linked");
        assert_eq!(original.ino, linked.ino);
        assert_eq!(original.nlink, 2);
        assert_eq!(linked.nlink, 2);

        vm.kernel
            .filesystem_mut()
            .write_file("/workspace/linked.txt", b"updated".to_vec())
            .expect("write through hard link");
        assert_eq!(
            vm.kernel
                .filesystem_mut()
                .read_file("/workspace/original.txt")
                .expect("read original through shared inode"),
            b"updated".to_vec()
        );

        vm.kernel
            .filesystem_mut()
            .remove_file("/workspace/original.txt")
            .expect("remove original hard link");
        assert!(
            !vm.kernel
                .filesystem()
                .exists("/workspace/original.txt")
                .expect("check removed original")
        );
        assert_eq!(
            vm.kernel
                .filesystem_mut()
                .read_file("/workspace/linked.txt")
                .expect("read surviving hard link"),
            b"updated".to_vec()
        );
        assert_eq!(
            vm.kernel
                .filesystem_mut()
                .stat("/workspace/linked.txt")
                .expect("stat surviving hard link")
                .nlink,
            1
        );
    }

    #[test]
    fn configure_vm_js_bridge_mount_preserves_metadata_updates() {
        let mut sidecar = create_test_sidecar();
        sidecar
            .bridge
            .inspect(|bridge| {
                bridge.seed_directory(
                    "/workspace",
                    vec![agent_os_bridge::DirectoryEntry {
                        name: String::from("original.txt"),
                        kind: FileKind::File,
                    }],
                );
                bridge.seed_file("/workspace/original.txt", b"hello world".to_vec());
            })
            .expect("seed js bridge filesystem");

        let (connection_id, session_id) =
            authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
        let vm_id =
            create_vm(&mut sidecar, &connection_id, &session_id, Vec::new()).expect("create vm");

        sidecar
            .dispatch(request(
                4,
                OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                RequestPayload::ConfigureVm(ConfigureVmRequest {
                    mounts: vec![MountDescriptor {
                        guest_path: String::from("/workspace"),
                        read_only: false,
                        plugin: MountPluginDescriptor {
                            id: String::from("js_bridge"),
                            config: json!({}),
                        },
                    }],
                    software: Vec::new(),
                    permissions: Vec::new(),
                    instructions: Vec::new(),
                    projected_modules: Vec::new(),
                    command_permissions: BTreeMap::new(),
                }),
            ))
            .expect("configure js_bridge mount");

        let vm = sidecar.vms.get_mut(&vm_id).expect("configured vm");
        vm.kernel
            .filesystem_mut()
            .link("/workspace/original.txt", "/workspace/linked.txt")
            .expect("create js bridge hard link");

        vm.kernel
            .filesystem_mut()
            .chown("/workspace/original.txt", 2000, 3000)
            .expect("update js bridge ownership");
        vm.kernel
            .filesystem_mut()
            .utimes(
                "/workspace/linked.txt",
                1_700_000_000_000,
                1_710_000_000_000,
            )
            .expect("update js bridge timestamps");

        let original = vm
            .kernel
            .filesystem_mut()
            .stat("/workspace/original.txt")
            .expect("stat original");
        let linked = vm
            .kernel
            .filesystem_mut()
            .stat("/workspace/linked.txt")
            .expect("stat linked");

        assert_eq!(original.uid, 2000);
        assert_eq!(original.gid, 3000);
        assert_eq!(linked.uid, 2000);
        assert_eq!(linked.gid, 3000);
        assert_eq!(original.atime_ms, 1_700_000_000_000);
        assert_eq!(original.mtime_ms, 1_710_000_000_000);
        assert_eq!(linked.atime_ms, 1_700_000_000_000);
        assert_eq!(linked.mtime_ms, 1_710_000_000_000);
    }

    #[test]
    fn configure_vm_instantiates_sandbox_agent_mounts_through_the_plugin_registry() {
        let server = MockSandboxAgentServer::start("agent-os-sidecar-sandbox", None);
        fs::write(server.root().join("hello.txt"), "hello from sandbox")
            .expect("seed sandbox file");

        let mut sidecar = create_test_sidecar();
        let (connection_id, session_id) =
            authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
        let vm_id =
            create_vm(&mut sidecar, &connection_id, &session_id, Vec::new()).expect("create vm");

        sidecar
            .dispatch(request(
                4,
                OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                RequestPayload::BootstrapRootFilesystem(BootstrapRootFilesystemRequest {
                    entries: vec![
                        RootFilesystemEntry {
                            path: String::from("/sandbox"),
                            kind: RootFilesystemEntryKind::Directory,
                            ..Default::default()
                        },
                        RootFilesystemEntry {
                            path: String::from("/sandbox/root-only.txt"),
                            kind: RootFilesystemEntryKind::File,
                            content: Some(String::from("root bootstrap file")),
                            ..Default::default()
                        },
                    ],
                }),
            ))
            .expect("bootstrap root sandbox dir");

        sidecar
            .dispatch(request(
                5,
                OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                RequestPayload::ConfigureVm(ConfigureVmRequest {
                    mounts: vec![MountDescriptor {
                        guest_path: String::from("/sandbox"),
                        read_only: false,
                        plugin: MountPluginDescriptor {
                            id: String::from("sandbox_agent"),
                            config: json!({
                                "baseUrl": server.base_url(),
                            }),
                        },
                    }],
                    software: Vec::new(),
                    permissions: Vec::new(),
                    instructions: Vec::new(),
                    projected_modules: Vec::new(),
                    command_permissions: BTreeMap::new(),
                }),
            ))
            .expect("configure sandbox_agent mount");

        let vm = sidecar.vms.get_mut(&vm_id).expect("configured vm");
        let hidden = vm
            .kernel
            .filesystem_mut()
            .read_file("/sandbox/root-only.txt")
            .expect_err("mounted sandbox should hide root-backed file");
        assert_eq!(hidden.code(), "ENOENT");
        assert_eq!(
            vm.kernel
                .filesystem_mut()
                .read_file("/sandbox/hello.txt")
                .expect("read mounted sandbox file"),
            b"hello from sandbox".to_vec()
        );

        vm.kernel
            .filesystem_mut()
            .write_file("/sandbox/from-vm.txt", b"native sandbox mount".to_vec())
            .expect("write sandbox file");
        assert_eq!(
            fs::read_to_string(server.root().join("from-vm.txt")).expect("read sandbox output"),
            "native sandbox mount"
        );
    }

    #[test]
    fn configure_vm_instantiates_s3_mounts_through_the_plugin_registry() {
        let server = MockS3Server::start();

        let mut sidecar = create_test_sidecar();
        let (connection_id, session_id) =
            authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
        let vm_id =
            create_vm(&mut sidecar, &connection_id, &session_id, Vec::new()).expect("create vm");

        sidecar
            .dispatch(request(
                4,
                OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                RequestPayload::BootstrapRootFilesystem(BootstrapRootFilesystemRequest {
                    entries: vec![
                        RootFilesystemEntry {
                            path: String::from("/data"),
                            kind: RootFilesystemEntryKind::Directory,
                            ..Default::default()
                        },
                        RootFilesystemEntry {
                            path: String::from("/data/root-only.txt"),
                            kind: RootFilesystemEntryKind::File,
                            content: Some(String::from("root bootstrap file")),
                            ..Default::default()
                        },
                    ],
                }),
            ))
            .expect("bootstrap root s3 dir");

        sidecar
            .dispatch(request(
                5,
                OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                RequestPayload::ConfigureVm(ConfigureVmRequest {
                    mounts: vec![MountDescriptor {
                        guest_path: String::from("/data"),
                        read_only: false,
                        plugin: MountPluginDescriptor {
                            id: String::from("s3"),
                            config: json!({
                                "bucket": "test-bucket",
                                "prefix": "service-test",
                                "region": "us-east-1",
                                "endpoint": server.base_url(),
                                "allowLoopbackEndpoint": true,
                                "credentials": {
                                    "accessKeyId": "minioadmin",
                                    "secretAccessKey": "minioadmin",
                                },
                                "chunkSize": 8,
                                "inlineThreshold": 4,
                            }),
                        },
                    }],
                    software: Vec::new(),
                    permissions: Vec::new(),
                    instructions: Vec::new(),
                    projected_modules: Vec::new(),
                    command_permissions: BTreeMap::new(),
                }),
            ))
            .expect("configure s3 mount");

        let vm = sidecar.vms.get_mut(&vm_id).expect("configured vm");
        let hidden = vm
            .kernel
            .filesystem_mut()
            .read_file("/data/root-only.txt")
            .expect_err("mounted s3 fs should hide root-backed file");
        assert_eq!(hidden.code(), "ENOENT");

        vm.kernel
            .filesystem_mut()
            .write_file("/data/from-vm.txt", b"native s3 mount".to_vec())
            .expect("write s3-backed file");
        assert_eq!(
            vm.kernel
                .filesystem_mut()
                .read_file("/data/from-vm.txt")
                .expect("read s3-backed file"),
            b"native s3 mount".to_vec()
        );
        drop(sidecar);

        let requests = server.requests();
        assert!(
            requests.iter().any(|request| request.method == "PUT"),
            "expected the native plugin to persist data back to S3"
        );
        assert!(
            requests
                .iter()
                .any(|request| request.path.contains("filesystem-manifest.json")),
            "expected the native plugin to store a manifest object"
        );
    }

    #[test]
    fn bridge_permissions_map_symlink_operations_to_symlink_access() {
        let bridge = SharedBridge::new(RecordingBridge::default());
        let permissions = bridge_permissions(bridge.clone(), "vm-symlink");
        let check = permissions
            .filesystem
            .as_ref()
            .expect("filesystem permission callback");

        let decision = check(&FsAccessRequest {
            vm_id: String::from("ignored-by-bridge"),
            op: FsOperation::Symlink,
            path: String::from("/workspace/link.txt"),
        });
        assert!(decision.allow);

        let recorded = bridge
            .inspect(|bridge| bridge.filesystem_permission_requests.clone())
            .expect("inspect bridge");
        assert_eq!(
            recorded,
            vec![FilesystemPermissionRequest {
                vm_id: String::from("vm-symlink"),
                path: String::from("/workspace/link.txt"),
                access: FilesystemAccess::Symlink,
            }]
        );
    }

    #[test]
    fn parse_resource_limits_reads_filesystem_limits() {
        let metadata = BTreeMap::from([
            (String::from("resource.max_sockets"), String::from("8")),
            (String::from("resource.max_connections"), String::from("4")),
            (
                String::from("resource.max_filesystem_bytes"),
                String::from("4096"),
            ),
            (
                String::from("resource.max_inode_count"),
                String::from("128"),
            ),
            (
                String::from("resource.max_blocking_read_ms"),
                String::from("250"),
            ),
            (
                String::from("resource.max_pread_bytes"),
                String::from("8192"),
            ),
            (
                String::from("resource.max_fd_write_bytes"),
                String::from("4096"),
            ),
            (
                String::from("resource.max_process_argv_bytes"),
                String::from("2048"),
            ),
            (
                String::from("resource.max_process_env_bytes"),
                String::from("1024"),
            ),
            (
                String::from("resource.max_readdir_entries"),
                String::from("32"),
            ),
            (String::from("resource.max_wasm_fuel"), String::from("5000")),
            (
                String::from("resource.max_wasm_memory_bytes"),
                String::from("131072"),
            ),
            (
                String::from("resource.max_wasm_stack_bytes"),
                String::from("262144"),
            ),
        ]);

        let limits = parse_resource_limits(&metadata).expect("parse resource limits");
        assert_eq!(limits.max_sockets, Some(8));
        assert_eq!(limits.max_connections, Some(4));
        assert_eq!(limits.max_filesystem_bytes, Some(4096));
        assert_eq!(limits.max_inode_count, Some(128));
        assert_eq!(limits.max_blocking_read_ms, Some(250));
        assert_eq!(limits.max_pread_bytes, Some(8192));
        assert_eq!(limits.max_fd_write_bytes, Some(4096));
        assert_eq!(limits.max_process_argv_bytes, Some(2048));
        assert_eq!(limits.max_process_env_bytes, Some(1024));
        assert_eq!(limits.max_readdir_entries, Some(32));
        assert_eq!(limits.max_wasm_fuel, Some(5000));
        assert_eq!(limits.max_wasm_memory_bytes, Some(131072));
        assert_eq!(limits.max_wasm_stack_bytes, Some(262144));
    }

    #[test]
    fn create_vm_applies_filesystem_permission_descriptors_to_kernel_access() {
        let mut sidecar = create_test_sidecar();
        let (connection_id, session_id) =
            authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
        let vm_id = create_vm(
            &mut sidecar,
            &connection_id,
            &session_id,
            vec![
                PermissionDescriptor {
                    capability: String::from("fs"),
                    mode: PermissionMode::Allow,
                },
                PermissionDescriptor {
                    capability: String::from("fs.read"),
                    mode: PermissionMode::Deny,
                },
            ],
        )
        .expect("create vm");

        let vm = sidecar.vms.get_mut(&vm_id).expect("configured vm");
        vm.kernel
            .filesystem_mut()
            .write_file("/blocked.txt", b"nope".to_vec())
            .expect("write should be allowed");

        let read_error = vm
            .kernel
            .filesystem_mut()
            .read_file("/blocked.txt")
            .expect_err("read should be denied");
        assert_eq!(read_error.code(), "EACCES");
    }

    #[test]
    fn configure_vm_mounts_require_fs_write_permission() {
        let mut sidecar = create_test_sidecar();
        let (connection_id, session_id) =
            authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
        let vm_id =
            create_vm(&mut sidecar, &connection_id, &session_id, Vec::new()).expect("create vm");
        sidecar
            .bridge
            .set_vm_permissions(
                &vm_id,
                &[PermissionDescriptor {
                    capability: String::from("fs.write"),
                    mode: PermissionMode::Deny,
                }],
            )
            .expect("set vm permissions");

        let result = sidecar
            .dispatch(request(
                4,
                OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                RequestPayload::ConfigureVm(ConfigureVmRequest {
                    mounts: vec![MountDescriptor {
                        guest_path: String::from("/workspace"),
                        read_only: false,
                        plugin: MountPluginDescriptor {
                            id: String::from("memory"),
                            config: json!({}),
                        },
                    }],
                    software: Vec::new(),
                    permissions: Vec::new(),
                    instructions: Vec::new(),
                    projected_modules: Vec::new(),
                    command_permissions: BTreeMap::new(),
                }),
            ))
            .expect("dispatch configure vm");

        match result.response.payload {
            ResponsePayload::Rejected(rejected) => {
                assert_eq!(rejected.code, "kernel_error");
                assert!(
                    rejected.message.contains("EACCES"),
                    "unexpected error: {}",
                    rejected.message
                );
            }
            other => panic!("expected rejected response, got {other:?}"),
        }
    }

    #[test]
    fn configure_vm_sensitive_mounts_require_fs_mount_sensitive_permission() {
        let mut sidecar = create_test_sidecar();
        let (connection_id, session_id) =
            authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
        let vm_id =
            create_vm(&mut sidecar, &connection_id, &session_id, Vec::new()).expect("create vm");
        sidecar
            .bridge
            .set_vm_permissions(
                &vm_id,
                &[
                    PermissionDescriptor {
                        capability: String::from("fs.write"),
                        mode: PermissionMode::Allow,
                    },
                    PermissionDescriptor {
                        capability: String::from("fs.mount_sensitive"),
                        mode: PermissionMode::Deny,
                    },
                ],
            )
            .expect("set vm permissions");

        let result = sidecar
            .dispatch(request(
                4,
                OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                RequestPayload::ConfigureVm(ConfigureVmRequest {
                    mounts: vec![MountDescriptor {
                        guest_path: String::from("/etc"),
                        read_only: false,
                        plugin: MountPluginDescriptor {
                            id: String::from("memory"),
                            config: json!({}),
                        },
                    }],
                    software: Vec::new(),
                    permissions: Vec::new(),
                    instructions: Vec::new(),
                    projected_modules: Vec::new(),
                    command_permissions: BTreeMap::new(),
                }),
            ))
            .expect("dispatch configure vm");

        match result.response.payload {
            ResponsePayload::Rejected(rejected) => {
                assert_eq!(rejected.code, "kernel_error");
                assert!(
                    rejected.message.contains("EACCES"),
                    "unexpected error: {}",
                    rejected.message
                );
                assert!(
                    rejected.message.contains("fs.mount_sensitive"),
                    "unexpected error: {}",
                    rejected.message
                );
            }
            other => panic!("expected rejected response, got {other:?}"),
        }
    }

    #[test]
    fn scoped_host_filesystem_unscoped_target_requires_exact_guest_root_prefix() {
        let filesystem = ScopedHostFilesystem::new(
            HostFilesystem::new(SharedBridge::new(RecordingBridge::default()), "vm-1"),
            "/data",
        );

        assert_eq!(
            filesystem.unscoped_target(String::from("/database")),
            "/database"
        );
        assert_eq!(
            filesystem.unscoped_target(String::from("/data/nested.txt")),
            "/nested.txt"
        );
        assert_eq!(filesystem.unscoped_target(String::from("/data")), "/");
    }

    #[test]
    fn scoped_host_filesystem_realpath_preserves_paths_outside_guest_root() {
        let bridge = SharedBridge::new(RecordingBridge::default());
        bridge
            .inspect(|bridge| {
                agent_os_bridge::FilesystemBridge::symlink(
                    bridge,
                    SymlinkRequest {
                        vm_id: String::from("vm-1"),
                        target_path: String::from("/database"),
                        link_path: String::from("/data/alias"),
                    },
                )
                .expect("seed alias symlink");
            })
            .expect("inspect bridge");

        let filesystem = ScopedHostFilesystem::new(HostFilesystem::new(bridge, "vm-1"), "/data");

        assert_eq!(
            filesystem.realpath("/alias").expect("resolve alias"),
            "/database"
        );
    }

    #[test]
    fn host_filesystem_realpath_fails_closed_on_circular_symlinks() {
        let bridge = SharedBridge::new(RecordingBridge::default());
        bridge
            .inspect(|bridge| {
                agent_os_bridge::FilesystemBridge::symlink(
                    bridge,
                    SymlinkRequest {
                        vm_id: String::from("vm-1"),
                        target_path: String::from("/loop-b.txt"),
                        link_path: String::from("/loop-a.txt"),
                    },
                )
                .expect("seed loop-a symlink");
                agent_os_bridge::FilesystemBridge::symlink(
                    bridge,
                    SymlinkRequest {
                        vm_id: String::from("vm-1"),
                        target_path: String::from("/loop-a.txt"),
                        link_path: String::from("/loop-b.txt"),
                    },
                )
                .expect("seed loop-b symlink");
            })
            .expect("inspect bridge");

        let filesystem = HostFilesystem::new(bridge, "vm-1");
        let error = filesystem
            .realpath("/loop-a.txt")
            .expect_err("circular symlink chain should fail closed");
        assert_eq!(error.code(), "ELOOP");
    }

    #[test]
    fn configure_vm_host_dir_plugin_fails_closed_for_escape_symlinks() {
        let host_dir = temp_dir("agent-os-sidecar-host-dir-escape");
        std::os::unix::fs::symlink("/etc", host_dir.join("escape")).expect("seed escape symlink");

        let mut sidecar = create_test_sidecar();
        let (connection_id, session_id) =
            authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
        let vm_id =
            create_vm(&mut sidecar, &connection_id, &session_id, Vec::new()).expect("create vm");

        sidecar
            .dispatch(request(
                4,
                OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                RequestPayload::ConfigureVm(ConfigureVmRequest {
                    mounts: vec![MountDescriptor {
                        guest_path: String::from("/workspace"),
                        read_only: false,
                        plugin: MountPluginDescriptor {
                            id: String::from("host_dir"),
                            config: json!({
                                "hostPath": host_dir,
                                "readOnly": false,
                            }),
                        },
                    }],
                    software: Vec::new(),
                    permissions: Vec::new(),
                    instructions: Vec::new(),
                    projected_modules: Vec::new(),
                    command_permissions: BTreeMap::new(),
                }),
            ))
            .expect("configure host_dir mount");

        let vm = sidecar.vms.get_mut(&vm_id).expect("configured vm");
        let error = vm
            .kernel
            .filesystem_mut()
            .read_file("/workspace/escape/hostname")
            .expect_err("escape symlink should fail closed");
        assert_eq!(error.code(), "EACCES");

        fs::remove_dir_all(host_dir).expect("remove temp dir");
    }

    #[test]
    fn execute_starts_python_runtime_instead_of_rejecting_it() {
        assert_node_available();

        let cache_root = temp_dir("agent-os-sidecar-python-cache");

        let mut sidecar = NativeSidecar::with_config(
            RecordingBridge::default(),
            NativeSidecarConfig {
                sidecar_id: String::from("sidecar-python-test"),
                compile_cache_root: Some(cache_root),
                expected_auth_token: Some(String::from(TEST_AUTH_TOKEN)),
                ..NativeSidecarConfig::default()
            },
        )
        .expect("create sidecar");
        let (connection_id, session_id) =
            authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
        let vm_id =
            create_vm(&mut sidecar, &connection_id, &session_id, Vec::new()).expect("create vm");

        let result = sidecar
            .dispatch(request(
                4,
                OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                RequestPayload::Execute(crate::protocol::ExecuteRequest {
                    process_id: String::from("proc-python"),
                    runtime: GuestRuntimeKind::Python,
                    entrypoint: String::from("print('hello from python')"),
                    args: Vec::new(),
                    env: BTreeMap::new(),
                    cwd: None,
                    wasm_permission_tier: None,
                }),
            ))
            .expect("dispatch python execute");

        match result.response.payload {
            ResponsePayload::ProcessStarted(response) => {
                assert_eq!(response.process_id, "proc-python");
                assert!(
                    response.pid.is_some(),
                    "python runtime should expose a child pid"
                );
            }
            other => panic!("unexpected execute response: {other:?}"),
        }

        let vm = sidecar.vms.get(&vm_id).expect("python vm");
        let process = vm
            .active_processes
            .get("proc-python")
            .expect("python process should be tracked");
        assert_eq!(process.runtime, GuestRuntimeKind::Python);
        match &process.execution {
            ActiveExecution::Python(_) => {}
            other => panic!("unexpected active execution variant: {other:?}"),
        }
    }

    #[test]
    fn python_vfs_rpc_requests_proxy_into_the_vm_kernel_filesystem() {
        assert_node_available();

        let mut sidecar = create_test_sidecar();
        let (connection_id, session_id) =
            authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
        let vm_id =
            create_vm(&mut sidecar, &connection_id, &session_id, Vec::new()).expect("create vm");
        let cwd = temp_dir("agent-os-sidecar-python-vfs-rpc-cwd");
        let pyodide_dir = temp_dir("agent-os-sidecar-python-vfs-rpc-pyodide");
        write_fixture(
            &pyodide_dir.join("pyodide.mjs"),
            r#"
export async function loadPyodide() {
  return {
    setStdin(_stdin) {},
    async runPythonAsync(_code) {
      await new Promise(() => {});
    },
  };
}
"#,
        );
        write_fixture(
            &pyodide_dir.join("pyodide-lock.json"),
            "{\"packages\":[]}\n",
        );

        let context = sidecar
            .python_engine
            .create_context(CreatePythonContextRequest {
                vm_id: vm_id.clone(),
                pyodide_dist_path: pyodide_dir,
            });
        let execution = sidecar
            .python_engine
            .start_execution(StartPythonExecutionRequest {
                vm_id: vm_id.clone(),
                context_id: context.context_id,
                code: String::from("print('hold-open')"),
                file_path: None,
                env: BTreeMap::new(),
                cwd: cwd.clone(),
            })
            .expect("start fake python execution");

        let kernel_handle = {
            let vm = sidecar.vms.get_mut(&vm_id).expect("python vm");
            vm.kernel
                .spawn_process(
                    PYTHON_COMMAND,
                    vec![String::from("print('hold-open')")],
                    SpawnOptions {
                        requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                        cwd: Some(String::from("/")),
                        ..SpawnOptions::default()
                    },
                )
                .expect("spawn kernel python process")
        };

        {
            let vm = sidecar.vms.get_mut(&vm_id).expect("python vm");
            vm.active_processes.insert(
                String::from("proc-python-vfs"),
                ActiveProcess::new(
                    kernel_handle.pid(),
                    kernel_handle,
                    GuestRuntimeKind::Python,
                    ActiveExecution::Python(execution),
                ),
            );
        }

        sidecar
            .handle_python_vfs_rpc_request(
                &vm_id,
                "proc-python-vfs",
                PythonVfsRpcRequest {
                    id: 1,
                    method: PythonVfsRpcMethod::Mkdir,
                    path: String::from("/workspace"),
                    content_base64: None,
                    recursive: false,
                },
            )
            .expect("handle python mkdir rpc");
        sidecar
            .handle_python_vfs_rpc_request(
                &vm_id,
                "proc-python-vfs",
                PythonVfsRpcRequest {
                    id: 2,
                    method: PythonVfsRpcMethod::Write,
                    path: String::from("/workspace/note.txt"),
                    content_base64: Some(String::from("aGVsbG8gZnJvbSBzaWRlY2FyIHJwYw==")),
                    recursive: false,
                },
            )
            .expect("handle python write rpc");

        let content = {
            let vm = sidecar.vms.get_mut(&vm_id).expect("python vm");
            String::from_utf8(
                vm.kernel
                    .read_file("/workspace/note.txt")
                    .expect("read bridged file from kernel"),
            )
            .expect("utf8 file contents")
        };
        assert_eq!(content, "hello from sidecar rpc");

        let process = {
            let vm = sidecar.vms.get_mut(&vm_id).expect("python vm");
            vm.active_processes
                .remove("proc-python-vfs")
                .expect("remove fake python process")
        };
        let _ = signal_runtime_process(process.execution.child_pid(), SIGTERM);
    }

    #[test]
    fn javascript_sync_rpc_requests_proxy_into_the_vm_kernel_filesystem() {
        assert_node_available();

        let mut sidecar = create_test_sidecar();
        let (connection_id, session_id) =
            authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
        let vm_id =
            create_vm(&mut sidecar, &connection_id, &session_id, Vec::new()).expect("create vm");
        let cwd = temp_dir("agent-os-sidecar-js-sync-rpc-cwd");
        write_fixture(
            &cwd.join("entry.mjs"),
            r#"
import fs from "node:fs";

fs.writeFileSync("/rpc/note.txt", "hello from sidecar rpc");
fs.mkdirSync("/rpc/subdir", { recursive: true });
fs.symlinkSync("/rpc/note.txt", "/rpc/link.txt");
const linkTarget = fs.readlinkSync("/rpc/link.txt");
const existsBefore = fs.existsSync("/rpc/note.txt");
const lstat = fs.lstatSync("/rpc/link.txt");
fs.linkSync("/rpc/note.txt", "/rpc/hard.txt");
fs.renameSync("/rpc/hard.txt", "/rpc/renamed.txt");
const contents = fs.readFileSync("/rpc/renamed.txt", "utf8");
fs.unlinkSync("/rpc/renamed.txt");
fs.rmdirSync("/rpc/subdir");
console.log(JSON.stringify({ existsBefore, linkTarget, linkIsSymlink: lstat.isSymbolicLink(), contents }));
await new Promise(() => {});
"#,
        );

        let context = sidecar
            .javascript_engine
            .create_context(CreateJavascriptContextRequest {
                vm_id: vm_id.clone(),
                bootstrap_module: None,
                compile_cache_root: None,
            });
        let execution = sidecar
            .javascript_engine
            .start_execution(StartJavascriptExecutionRequest {
                vm_id: vm_id.clone(),
                context_id: context.context_id,
                argv: vec![String::from("./entry.mjs")],
                env: BTreeMap::from([(
                    String::from("AGENT_OS_NODE_SYNC_RPC_ENABLE"),
                    String::from("1"),
                )]),
                cwd: cwd.clone(),
            })
            .expect("start fake javascript execution");

        let kernel_handle = {
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            vm.kernel
                .spawn_process(
                    JAVASCRIPT_COMMAND,
                    vec![String::from("./entry.mjs")],
                    SpawnOptions {
                        requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                        cwd: Some(String::from("/")),
                        ..SpawnOptions::default()
                    },
                )
                .expect("spawn kernel javascript process")
        };

        {
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            vm.active_processes.insert(
                String::from("proc-js-sync"),
                ActiveProcess::new(
                    kernel_handle.pid(),
                    kernel_handle,
                    GuestRuntimeKind::JavaScript,
                    ActiveExecution::Javascript(execution),
                ),
            );
        }

        let mut saw_stdout = false;
        for _ in 0..16 {
            let event = {
                let vm = sidecar.vms.get(&vm_id).expect("javascript vm");
                let process = vm
                    .active_processes
                    .get("proc-js-sync")
                    .expect("javascript process should be tracked");
                process
                    .execution
                    .poll_event(Duration::from_secs(5))
                    .expect("poll javascript sync rpc event")
                    .expect("javascript sync rpc event")
            };

            if let ActiveExecutionEvent::Stdout(chunk) = &event {
                let stdout = String::from_utf8(chunk.clone()).expect("stdout utf8");
                if stdout.contains("\"contents\":\"hello from sidecar rpc\"")
                    && stdout.contains("\"existsBefore\":true")
                    && stdout.contains("\"linkTarget\":\"/rpc/note.txt\"")
                    && stdout.contains("\"linkIsSymlink\":true")
                {
                    saw_stdout = true;
                    break;
                }
            }

            sidecar
                .handle_execution_event(&vm_id, "proc-js-sync", event)
                .expect("handle javascript sync rpc event");
        }

        let content = {
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            String::from_utf8(
                vm.kernel
                    .read_file("/rpc/note.txt")
                    .expect("read bridged file from kernel"),
            )
            .expect("utf8 file contents")
        };
        assert_eq!(content, "hello from sidecar rpc");
        let link_target = {
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            vm.kernel
                .read_link("/rpc/link.txt")
                .expect("read bridged symlink")
        };
        assert_eq!(link_target, "/rpc/note.txt");
        {
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            assert!(
                !vm.kernel
                    .exists("/rpc/renamed.txt")
                    .expect("renamed file should be gone"),
                "expected renamed file to be removed",
            );
            assert!(
                !vm.kernel
                    .exists("/rpc/subdir")
                    .expect("subdir should be gone"),
                "expected subdir to be removed",
            );
        }
        assert!(saw_stdout, "expected guest stdout after sync fs round-trip");

        let process = {
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            vm.active_processes
                .remove("proc-js-sync")
                .expect("remove fake javascript process")
        };
        let _ = signal_runtime_process(process.execution.child_pid(), SIGTERM);
    }

    #[test]
    fn python_vfs_rpc_paths_are_scoped_to_workspace_root() {
        assert_eq!(
            normalize_python_vfs_rpc_path("/workspace/./note.txt")
                .expect("normalize workspace path"),
            String::from("/workspace/note.txt")
        );
        assert!(
            normalize_python_vfs_rpc_path("/workspace/../etc/passwd").is_err(),
            "workspace escape should be rejected",
        );
        assert!(
            normalize_python_vfs_rpc_path("/etc/passwd").is_err(),
            "non-workspace paths should be rejected",
        );
        assert!(
            normalize_python_vfs_rpc_path("workspace/note.txt").is_err(),
            "relative paths should be rejected",
        );
    }

    #[test]
    fn javascript_fs_sync_rpc_resolves_proc_self_against_the_kernel_process() {
        let mut config = KernelVmConfig::new("vm-js-procfs-rpc");
        config.permissions = Permissions::allow_all();
        let mut kernel = SidecarKernel::new(MountTable::new(MemoryFileSystem::new()), config);
        kernel
            .register_driver(CommandDriver::new(
                EXECUTION_DRIVER_NAME,
                [JAVASCRIPT_COMMAND],
            ))
            .expect("register execution driver");

        let process = kernel
            .spawn_process(
                JAVASCRIPT_COMMAND,
                Vec::new(),
                SpawnOptions {
                    requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                    ..SpawnOptions::default()
                },
            )
            .expect("spawn javascript kernel process");

        let link = service_javascript_fs_sync_rpc(
            &mut kernel,
            process.pid(),
            &JavascriptSyncRpcRequest {
                id: 1,
                method: String::from("fs.readlinkSync"),
                args: vec![json!("/proc/self")],
            },
        )
        .expect("resolve /proc/self");
        assert_eq!(link, Value::String(format!("/proc/{}", process.pid())));

        let entries = service_javascript_fs_sync_rpc(
            &mut kernel,
            process.pid(),
            &JavascriptSyncRpcRequest {
                id: 2,
                method: String::from("fs.readdirSync"),
                args: vec![json!("/proc/self/fd")],
            },
        )
        .expect("read /proc/self/fd");
        let entry_names = entries
            .as_array()
            .expect("readdir should return an array")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        assert!(entry_names.contains(&"0"));
        assert!(entry_names.contains(&"1"));
        assert!(entry_names.contains(&"2"));

        process.finish(0);
        kernel
            .waitpid(process.pid())
            .expect("wait javascript process");
    }

    #[test]
    fn javascript_fd_and_stream_rpc_requests_proxy_into_the_vm_kernel_filesystem() {
        assert_node_available();

        let mut sidecar = create_test_sidecar();
        let (connection_id, session_id) =
            authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
        let vm_id =
            create_vm(&mut sidecar, &connection_id, &session_id, Vec::new()).expect("create vm");
        {
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            vm.kernel
                .write_file("/rpc/input.txt", b"abcdefg")
                .expect("seed input file");
        }
        let cwd = temp_dir("agent-os-sidecar-js-fd-rpc-cwd");
        write_fixture(
            &cwd.join("entry.mjs"),
            r#"
import fs from "node:fs";
import { once } from "node:events";

const inFd = fs.openSync("/rpc/input.txt", "r");
const buffer = Buffer.alloc(5);
const bytesRead = fs.readSync(inFd, buffer, 0, buffer.length, 1);
const stat = fs.fstatSync(inFd);
fs.closeSync(inFd);

const defaultUmask = process.umask();
const previousUmask = process.umask(0o027);
const outFd = fs.openSync("/rpc/output.txt", "w", 0o666);
const written = fs.writeSync(outFd, Buffer.from("kernel"), 0, 6, 0);
fs.closeSync(outFd);
fs.mkdirSync("/rpc/private", { mode: 0o777 });
const outputStat = fs.statSync("/rpc/output.txt");
const privateDirStat = fs.statSync("/rpc/private");

const asyncSummary = await new Promise((resolve, reject) => {
  fs.open("/rpc/input.txt", "r", (openError, asyncFd) => {
    if (openError) {
      reject(openError);
      return;
    }

    const target = Buffer.alloc(5);
    fs.read(asyncFd, target, 0, 5, 0, (readError, asyncBytesRead) => {
      if (readError) {
        reject(readError);
        return;
      }

      fs.fstat(asyncFd, (statError, asyncStat) => {
        if (statError) {
          reject(statError);
          return;
        }

        fs.close(asyncFd, (closeError) => {
          if (closeError) {
            reject(closeError);
            return;
          }

          resolve({
            asyncBytesRead,
            asyncText: target.toString("utf8"),
            asyncSize: asyncStat.size,
          });
        });
      });
    });
  });
});

const reader = fs.createReadStream("/rpc/input.txt", {
  encoding: "utf8",
  start: 0,
  end: 4,
  highWaterMark: 3,
});
const streamChunks = [];
reader.on("data", (chunk) => streamChunks.push(chunk));
await once(reader, "close");

const writer = fs.createWriteStream("/rpc/stream.txt", { start: 0 });
writer.write("ab");
writer.end("cd");
await once(writer, "close");

let watchCode = "";
let watchFileCode = "";
try {
  fs.watch("/rpc/input.txt");
} catch (error) {
  watchCode = error.code;
}
try {
  fs.watchFile("/rpc/input.txt", () => {});
} catch (error) {
  watchFileCode = error.code;
}

console.log(
  JSON.stringify({
    text: buffer.toString("utf8"),
    bytesRead,
    size: stat.size,
    blocks: stat.blocks,
    dev: stat.dev,
    rdev: stat.rdev,
    written,
    defaultUmask,
    previousUmask,
    outputMode: outputStat.mode & 0o777,
    privateDirMode: privateDirStat.mode & 0o777,
    asyncSummary,
    streamChunks,
    watchCode,
    watchFileCode,
  }),
);
"#,
        );

        let context = sidecar
            .javascript_engine
            .create_context(CreateJavascriptContextRequest {
                vm_id: vm_id.clone(),
                bootstrap_module: None,
                compile_cache_root: None,
            });
        let execution = sidecar
            .javascript_engine
            .start_execution(StartJavascriptExecutionRequest {
                vm_id: vm_id.clone(),
                context_id: context.context_id,
                argv: vec![String::from("./entry.mjs")],
                env: BTreeMap::from([(
                    String::from("AGENT_OS_ALLOWED_NODE_BUILTINS"),
                    String::from(
                        "[\"assert\",\"buffer\",\"child_process\",\"console\",\"crypto\",\"events\",\"fs\",\"path\",\"querystring\",\"stream\",\"string_decoder\",\"timers\",\"url\",\"util\",\"zlib\"]",
                    ),
                )]),
                cwd: cwd.clone(),
            })
            .expect("start fake javascript execution");

        let kernel_handle = {
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            vm.kernel
                .spawn_process(
                    JAVASCRIPT_COMMAND,
                    vec![String::from("./entry.mjs")],
                    SpawnOptions {
                        requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                        cwd: Some(String::from("/")),
                        ..SpawnOptions::default()
                    },
                )
                .expect("spawn kernel javascript process")
        };

        {
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            vm.active_processes.insert(
                String::from("proc-js-fd"),
                ActiveProcess::new(
                    kernel_handle.pid(),
                    kernel_handle,
                    GuestRuntimeKind::JavaScript,
                    ActiveExecution::Javascript(execution),
                ),
            );
        }

        let mut stdout = String::new();
        let mut stderr = String::new();
        let mut exit_code = None;
        for _ in 0..64 {
            let next_event = {
                let vm = sidecar.vms.get(&vm_id).expect("javascript vm");
                vm.active_processes
                    .get("proc-js-fd")
                    .map(|process| {
                        process
                            .execution
                            .poll_event(Duration::from_secs(5))
                            .expect("poll javascript fd rpc event")
                    })
                    .flatten()
            };
            let Some(event) = next_event else {
                if exit_code.is_some() {
                    break;
                }
                panic!("javascript fd process disappeared before exit");
            };

            match &event {
                ActiveExecutionEvent::Stdout(chunk) => {
                    stdout.push_str(&String::from_utf8_lossy(chunk));
                }
                ActiveExecutionEvent::Stderr(chunk) => {
                    stderr.push_str(&String::from_utf8_lossy(chunk));
                }
                ActiveExecutionEvent::Exited(code) => {
                    exit_code = Some(*code);
                }
                _ => {}
            }

            sidecar
                .handle_execution_event(&vm_id, "proc-js-fd", event)
                .expect("handle javascript fd rpc event");
        }

        assert_eq!(exit_code, Some(0), "stdout: {stdout}\nstderr: {stderr}");
        assert!(stdout.contains("\"text\":\"bcdef\""), "stdout: {stdout}");
        assert!(stdout.contains("\"bytesRead\":5"), "stdout: {stdout}");
        assert!(stdout.contains("\"size\":7"), "stdout: {stdout}");
        assert!(stdout.contains("\"blocks\":1"), "stdout: {stdout}");
        assert!(stdout.contains("\"dev\":1"), "stdout: {stdout}");
        assert!(stdout.contains("\"rdev\":0"), "stdout: {stdout}");
        assert!(stdout.contains("\"written\":6"), "stdout: {stdout}");
        assert!(stdout.contains("\"defaultUmask\":18"), "stdout: {stdout}");
        assert!(stdout.contains("\"previousUmask\":18"), "stdout: {stdout}");
        assert!(stdout.contains("\"outputMode\":416"), "stdout: {stdout}");
        assert!(
            stdout.contains("\"privateDirMode\":488"),
            "stdout: {stdout}"
        );
        assert!(
            stdout.contains("\"asyncText\":\"abcde\""),
            "stdout: {stdout}"
        );
        assert!(stdout.contains("\"asyncSize\":7"), "stdout: {stdout}");
        assert!(
            stdout.contains("\"streamChunks\":[\"abc\",\"de\"]"),
            "stdout: {stdout}"
        );
        assert!(
            stdout.contains("\"watchCode\":\"ERR_AGENT_OS_FS_WATCH_UNAVAILABLE\""),
            "stdout: {stdout}"
        );
        assert!(
            stdout.contains("\"watchFileCode\":\"ERR_AGENT_OS_FS_WATCH_UNAVAILABLE\""),
            "stdout: {stdout}"
        );
        {
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            let output = String::from_utf8(
                vm.kernel
                    .read_file("/rpc/output.txt")
                    .expect("read fd output file"),
            )
            .expect("utf8 output contents");
            assert_eq!(output, "kernel");

            let stream = String::from_utf8(
                vm.kernel
                    .read_file("/rpc/stream.txt")
                    .expect("read stream output file"),
            )
            .expect("utf8 stream contents");
            assert_eq!(stream, "abcd");
        }
    }

    #[test]
    fn javascript_fs_promises_rpc_requests_proxy_into_the_vm_kernel_filesystem() {
        assert_node_available();

        let mut sidecar = create_test_sidecar();
        let (connection_id, session_id) =
            authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
        let vm_id =
            create_vm(&mut sidecar, &connection_id, &session_id, Vec::new()).expect("create vm");
        let cwd = temp_dir("agent-os-sidecar-js-promises-rpc-cwd");
        write_fixture(
            &cwd.join("entry.mjs"),
            r#"
import fs from "node:fs/promises";

await fs.writeFile("/rpc/note.txt", "hello from sidecar promises rpc");
const contents = await fs.readFile("/rpc/note.txt", "utf8");
console.log(contents);
await new Promise(() => {});
"#,
        );

        let context = sidecar
            .javascript_engine
            .create_context(CreateJavascriptContextRequest {
                vm_id: vm_id.clone(),
                bootstrap_module: None,
                compile_cache_root: None,
            });
        let execution = sidecar
            .javascript_engine
            .start_execution(StartJavascriptExecutionRequest {
                vm_id: vm_id.clone(),
                context_id: context.context_id,
                argv: vec![String::from("./entry.mjs")],
                env: BTreeMap::from([
                    (
                        String::from("AGENT_OS_ALLOWED_NODE_BUILTINS"),
                        String::from(
                            "[\"assert\",\"buffer\",\"console\",\"child_process\",\"crypto\",\"events\",\"fs\",\"path\",\"querystring\",\"stream\",\"string_decoder\",\"timers\",\"url\",\"util\",\"zlib\"]",
                        ),
                    ),
                    (
                        String::from("AGENT_OS_GUEST_PATH_MAPPINGS"),
                        json!([
                            {
                                "guestPath": "/root",
                                "hostPath": cwd,
                            }
                        ])
                        .to_string(),
                    ),
                ]),
                cwd: cwd.clone(),
            })
            .expect("start fake javascript execution");

        let kernel_handle = {
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            vm.kernel
                .spawn_process(
                    JAVASCRIPT_COMMAND,
                    vec![String::from("./entry.mjs")],
                    SpawnOptions {
                        requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                        cwd: Some(String::from("/")),
                        ..SpawnOptions::default()
                    },
                )
                .expect("spawn kernel javascript process")
        };

        {
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            vm.active_processes.insert(
                String::from("proc-js-promises"),
                ActiveProcess::new(
                    kernel_handle.pid(),
                    kernel_handle,
                    GuestRuntimeKind::JavaScript,
                    ActiveExecution::Javascript(execution),
                ),
            );
        }

        let mut saw_stdout = false;
        for _ in 0..4 {
            let event = {
                let vm = sidecar.vms.get(&vm_id).expect("javascript vm");
                let process = vm
                    .active_processes
                    .get("proc-js-promises")
                    .expect("javascript process should be tracked");
                process
                    .execution
                    .poll_event(Duration::from_secs(5))
                    .expect("poll javascript promises rpc event")
                    .expect("javascript promises rpc event")
            };

            if let ActiveExecutionEvent::Stdout(chunk) = &event {
                let stdout = String::from_utf8(chunk.clone()).expect("stdout utf8");
                if stdout.contains("hello from sidecar promises rpc") {
                    saw_stdout = true;
                    break;
                }
            }

            sidecar
                .handle_execution_event(&vm_id, "proc-js-promises", event)
                .expect("handle javascript promises rpc event");
        }

        let content = {
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            String::from_utf8(
                vm.kernel
                    .read_file("/rpc/note.txt")
                    .expect("read bridged file from kernel"),
            )
            .expect("utf8 file contents")
        };
        assert_eq!(content, "hello from sidecar promises rpc");
        assert!(
            saw_stdout,
            "expected guest stdout after fs.promises round-trip"
        );

        let process = {
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            vm.active_processes
                .remove("proc-js-promises")
                .expect("remove fake javascript process")
        };
        let _ = signal_runtime_process(process.execution.child_pid(), SIGTERM);
    }

    #[test]
    fn javascript_net_rpc_connects_to_host_tcp_server() {
        assert_node_available();

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind tcp listener");
        let port = listener.local_addr().expect("listener address").port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept tcp client");
            let mut received = Vec::new();
            stream
                .read_to_end(&mut received)
                .expect("read client payload");
            assert_eq!(String::from_utf8(received).expect("client utf8"), "ping");
            stream.write_all(b"pong").expect("write server payload");
        });

        let mut sidecar = create_test_sidecar();
        let (connection_id, session_id) =
            authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
        let vm_id = create_vm_with_metadata(
            &mut sidecar,
            &connection_id,
            &session_id,
            Vec::new(),
            BTreeMap::from([(
                format!("env.{LOOPBACK_EXEMPT_PORTS_ENV}"),
                serde_json::to_string(&vec![port.to_string()]).expect("serialize exempt ports"),
            )]),
        )
        .expect("create vm");
        let cwd = temp_dir("agent-os-sidecar-js-net-rpc-cwd");
        write_fixture(
            &cwd.join("entry.mjs"),
            &format!(
                r#"
import net from "node:net";

const socket = net.createConnection({{ host: "127.0.0.1", port: {port} }});
let data = "";
socket.setEncoding("utf8");
socket.on("connect", () => {{
  socket.end("ping");
}});
socket.on("data", (chunk) => {{
  data += chunk;
}});
socket.on("error", (error) => {{
  console.error(error.stack ?? error.message);
  process.exit(1);
}});
socket.on("close", (hadError) => {{
  console.log(JSON.stringify({{
    data,
    hadError,
    remoteAddress: socket.remoteAddress,
    remotePort: socket.remotePort,
    localPort: socket.localPort,
  }}));
  process.exit(hadError ? 1 : 0);
}});
"#,
            ),
        );

        let (stdout, stderr, exit_code) = run_javascript_entry(
            &mut sidecar,
            &vm_id,
            &cwd,
            "proc-js-net",
            "[\"assert\",\"buffer\",\"console\",\"crypto\",\"events\",\"fs\",\"net\",\"path\",\"querystring\",\"stream\",\"string_decoder\",\"timers\",\"url\",\"util\",\"zlib\"]",
        );

        server.join().expect("join tcp server");
        assert_eq!(exit_code, Some(0), "stderr: {stderr}");
        assert!(stdout.contains("\"data\":\"pong\""), "stdout: {stdout}");
        assert!(stdout.contains("\"hadError\":false"), "stdout: {stdout}");
        assert!(
            stdout.contains(&format!("\"remotePort\":{port}")),
            "stdout: {stdout}"
        );
    }

    #[test]
    fn javascript_dgram_rpc_sends_and_receives_host_udp_packets() {
        assert_node_available();

        let listener = UdpSocket::bind("127.0.0.1:0").expect("bind udp listener");
        let port = listener.local_addr().expect("listener address").port();
        let server = thread::spawn(move || {
            let mut buffer = [0_u8; 64 * 1024];
            let (bytes_read, remote_addr) = listener.recv_from(&mut buffer).expect("recv packet");
            assert_eq!(
                String::from_utf8(buffer[..bytes_read].to_vec()).expect("udp payload utf8"),
                "ping"
            );
            listener
                .send_to(b"pong", remote_addr)
                .expect("send udp response");
        });

        let mut sidecar = create_test_sidecar();
        let (connection_id, session_id) =
            authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
        let vm_id =
            create_vm(&mut sidecar, &connection_id, &session_id, Vec::new()).expect("create vm");
        let cwd = temp_dir("agent-os-sidecar-js-dgram-rpc-cwd");
        write_fixture(
            &cwd.join("entry.mjs"),
            &format!(
                r#"
import dgram from "node:dgram";

const socket = dgram.createSocket("udp4");
const summary = await new Promise((resolve) => {{
socket.on("error", (error) => {{
  console.error(error.stack ?? error.message);
  process.exit(1);
}});
socket.on("message", (message, rinfo) => {{
  const address = socket.address();
  socket.close(() => {{
    resolve({{
      address,
      message: message.toString("utf8"),
      rinfo,
    }});
  }});
}});
socket.bind(0, "127.0.0.1", () => {{
  socket.send("ping", {port}, "127.0.0.1");
}});
}});

console.log(JSON.stringify(summary));
"#,
            ),
        );

        let context = sidecar
            .javascript_engine
            .create_context(CreateJavascriptContextRequest {
                vm_id: vm_id.clone(),
                bootstrap_module: None,
                compile_cache_root: None,
            });
        let execution = sidecar
            .javascript_engine
            .start_execution(StartJavascriptExecutionRequest {
                vm_id: vm_id.clone(),
                context_id: context.context_id,
                argv: vec![String::from("./entry.mjs")],
                env: BTreeMap::from([(
                    String::from("AGENT_OS_ALLOWED_NODE_BUILTINS"),
                    String::from(
                        "[\"assert\",\"buffer\",\"console\",\"crypto\",\"dgram\",\"events\",\"fs\",\"path\",\"querystring\",\"stream\",\"string_decoder\",\"timers\",\"url\",\"util\",\"zlib\"]",
                    ),
                )]),
                cwd: cwd.clone(),
            })
            .expect("start fake javascript execution");

        let kernel_handle = {
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            vm.kernel
                .spawn_process(
                    JAVASCRIPT_COMMAND,
                    vec![String::from("./entry.mjs")],
                    SpawnOptions {
                        requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                        cwd: Some(String::from("/")),
                        ..SpawnOptions::default()
                    },
                )
                .expect("spawn kernel javascript process")
        };

        {
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            vm.active_processes.insert(
                String::from("proc-js-dgram"),
                ActiveProcess::new(
                    kernel_handle.pid(),
                    kernel_handle,
                    GuestRuntimeKind::JavaScript,
                    ActiveExecution::Javascript(execution),
                ),
            );
        }

        let mut stdout = String::new();
        let mut stderr = String::new();
        let mut exit_code = None;
        for _ in 0..64 {
            let next_event = {
                let vm = sidecar.vms.get(&vm_id).expect("javascript vm");
                vm.active_processes
                    .get("proc-js-dgram")
                    .map(|process| {
                        process
                            .execution
                            .poll_event(Duration::from_secs(5))
                            .expect("poll javascript dgram rpc event")
                    })
                    .flatten()
            };
            let Some(event) = next_event else {
                if exit_code.is_some() {
                    break;
                }
                panic!("javascript dgram process disappeared before exit");
            };

            match &event {
                ActiveExecutionEvent::Stdout(chunk) => {
                    stdout.push_str(&String::from_utf8_lossy(chunk));
                }
                ActiveExecutionEvent::Stderr(chunk) => {
                    stderr.push_str(&String::from_utf8_lossy(chunk));
                }
                ActiveExecutionEvent::Exited(code) => {
                    exit_code = Some(*code);
                }
                _ => {}
            }

            sidecar
                .handle_execution_event(&vm_id, "proc-js-dgram", event)
                .expect("handle javascript dgram rpc event");
        }

        server.join().expect("join udp server");
        assert_eq!(exit_code, Some(0), "stderr: {stderr}");
        assert!(stdout.contains("\"message\":\"pong\""), "stdout: {stdout}");
        assert!(
            stdout.contains("\"address\":{\"address\":\"127.0.0.1\""),
            "stdout: {stdout}"
        );
        assert!(
            stdout.contains(&format!("\"port\":{port}")),
            "stdout: {stdout}"
        );
    }

    #[test]
    fn javascript_dns_rpc_resolves_localhost() {
        assert_node_available();

        let mut sidecar = create_test_sidecar();
        let (connection_id, session_id) =
            authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
        let vm_id =
            create_vm(&mut sidecar, &connection_id, &session_id, Vec::new()).expect("create vm");
        let cwd = temp_dir("agent-os-sidecar-js-dns-rpc-cwd");
        write_fixture(
            &cwd.join("entry.mjs"),
            r#"
import dns from "node:dns";

const lookup = await dns.promises.lookup("localhost", { all: true });
const resolve4 = await dns.promises.resolve4("localhost");

console.log(JSON.stringify({ lookup, resolve4 }));
"#,
        );

        let context = sidecar
            .javascript_engine
            .create_context(CreateJavascriptContextRequest {
                vm_id: vm_id.clone(),
                bootstrap_module: None,
                compile_cache_root: None,
            });
        let execution = sidecar
            .javascript_engine
            .start_execution(StartJavascriptExecutionRequest {
                vm_id: vm_id.clone(),
                context_id: context.context_id,
                argv: vec![String::from("./entry.mjs")],
                env: BTreeMap::from([(
                    String::from("AGENT_OS_ALLOWED_NODE_BUILTINS"),
                    String::from(
                        "[\"assert\",\"buffer\",\"console\",\"crypto\",\"dns\",\"events\",\"fs\",\"path\",\"querystring\",\"stream\",\"string_decoder\",\"timers\",\"url\",\"util\",\"zlib\"]",
                    ),
                )]),
                cwd: cwd.clone(),
            })
            .expect("start fake javascript execution");

        let kernel_handle = {
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            vm.kernel
                .spawn_process(
                    JAVASCRIPT_COMMAND,
                    vec![String::from("./entry.mjs")],
                    SpawnOptions {
                        requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                        cwd: Some(String::from("/")),
                        ..SpawnOptions::default()
                    },
                )
                .expect("spawn kernel javascript process")
        };

        {
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            vm.active_processes.insert(
                String::from("proc-js-dns"),
                ActiveProcess::new(
                    kernel_handle.pid(),
                    kernel_handle,
                    GuestRuntimeKind::JavaScript,
                    ActiveExecution::Javascript(execution),
                ),
            );
        }

        let mut stdout = String::new();
        let mut stderr = String::new();
        let mut exit_code = None;
        for _ in 0..64 {
            let next_event = {
                let vm = sidecar.vms.get(&vm_id).expect("javascript vm");
                vm.active_processes
                    .get("proc-js-dns")
                    .map(|process| {
                        process
                            .execution
                            .poll_event(Duration::from_secs(5))
                            .expect("poll javascript dns rpc event")
                    })
                    .flatten()
            };
            let Some(event) = next_event else {
                if exit_code.is_some() {
                    break;
                }
                panic!("javascript dns process disappeared before exit");
            };

            match &event {
                ActiveExecutionEvent::Stdout(chunk) => {
                    stdout.push_str(&String::from_utf8_lossy(chunk));
                }
                ActiveExecutionEvent::Stderr(chunk) => {
                    stderr.push_str(&String::from_utf8_lossy(chunk));
                }
                ActiveExecutionEvent::Exited(code) => {
                    exit_code = Some(*code);
                }
                _ => {}
            }

            sidecar
                .handle_execution_event(&vm_id, "proc-js-dns", event)
                .expect("handle javascript dns rpc event");
        }

        assert_eq!(exit_code, Some(0), "stderr: {stderr}");
        let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse dns JSON");
        assert!(
            parsed["lookup"]
                .as_array()
                .is_some_and(|entries| !entries.is_empty()),
            "stdout: {stdout}"
        );
        assert!(
            parsed["resolve4"]
                .as_array()
                .is_some_and(|entries| entries.iter().any(|entry| entry == "127.0.0.1")),
            "stdout: {stdout}"
        );
    }

    #[test]
    fn javascript_network_ssrf_protection_blocks_private_dns_and_unowned_loopback_targets() {
        assert_node_available();

        let loopback_listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback listener");
        let loopback_port = loopback_listener
            .local_addr()
            .expect("loopback listener address")
            .port();

        let mut sidecar = create_test_sidecar();
        let (connection_id, session_id) =
            authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
        let vm_id = create_vm_with_metadata(
            &mut sidecar,
            &connection_id,
            &session_id,
            Vec::new(),
            BTreeMap::from([(
                String::from("network.dns.override.metadata.test"),
                String::from("169.254.169.254"),
            )]),
        )
        .expect("create vm");
        let cwd = temp_dir("agent-os-sidecar-js-ssrf-protection-cwd");
        write_fixture(
            &cwd.join("entry.mjs"),
            &format!(
                r#"
import dns from "node:dns";
import net from "node:net";

const dnsLookup = await (async () => {{
  try {{
    await dns.promises.lookup("metadata.test", {{ family: 4 }});
    return {{ unexpected: true }};
  }} catch (error) {{
    return {{ code: error.code ?? null, message: error.message }};
  }}
}})();

const privateConnect = await new Promise((resolve) => {{
  const socket = net.createConnection({{ host: "metadata.test", port: 80 }});
  socket.on("connect", () => {{
    socket.destroy();
    resolve({{ unexpected: true }});
  }});
  socket.on("error", (error) => {{
    resolve({{ code: error.code ?? null, message: error.message }});
  }});
}});

const loopbackConnect = await new Promise((resolve) => {{
  const socket = net.createConnection({{ host: "127.0.0.1", port: {loopback_port} }});
  socket.on("connect", () => {{
    socket.destroy();
    resolve({{ unexpected: true }});
  }});
  socket.on("error", (error) => {{
    resolve({{ code: error.code ?? null, message: error.message }});
  }});
}});

console.log(JSON.stringify({{ dnsLookup, privateConnect, loopbackConnect }}));
process.exit(0);
"#,
            ),
        );

        let context = sidecar
            .javascript_engine
            .create_context(CreateJavascriptContextRequest {
                vm_id: vm_id.clone(),
                bootstrap_module: None,
                compile_cache_root: None,
            });
        let execution = sidecar
            .javascript_engine
            .start_execution(StartJavascriptExecutionRequest {
                vm_id: vm_id.clone(),
                context_id: context.context_id,
                argv: vec![String::from("./entry.mjs")],
                env: BTreeMap::from([(
                    String::from("AGENT_OS_ALLOWED_NODE_BUILTINS"),
                    String::from(
                        "[\"assert\",\"buffer\",\"console\",\"crypto\",\"dns\",\"events\",\"fs\",\"net\",\"path\",\"querystring\",\"stream\",\"string_decoder\",\"timers\",\"url\",\"util\",\"zlib\"]",
                    ),
                )]),
                cwd: cwd.clone(),
            })
            .expect("start fake javascript execution");

        let kernel_handle = {
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            vm.kernel
                .spawn_process(
                    JAVASCRIPT_COMMAND,
                    vec![String::from("./entry.mjs")],
                    SpawnOptions {
                        requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                        cwd: Some(String::from("/")),
                        ..SpawnOptions::default()
                    },
                )
                .expect("spawn kernel javascript process")
        };

        {
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            vm.active_processes.insert(
                String::from("proc-js-ssrf-protection"),
                ActiveProcess::new(
                    kernel_handle.pid(),
                    kernel_handle,
                    GuestRuntimeKind::JavaScript,
                    ActiveExecution::Javascript(execution),
                ),
            );
        }

        let mut stdout = String::new();
        let mut stderr = String::new();
        let mut exit_code = None;
        for _ in 0..64 {
            let next_event = {
                let vm = sidecar.vms.get(&vm_id).expect("javascript vm");
                vm.active_processes
                    .get("proc-js-ssrf-protection")
                    .map(|process| {
                        process
                            .execution
                            .poll_event(Duration::from_secs(5))
                            .expect("poll javascript ssrf event")
                    })
                    .flatten()
            };
            let Some(event) = next_event else {
                if exit_code.is_some() {
                    break;
                }
                panic!("javascript ssrf process disappeared before exit");
            };

            match &event {
                ActiveExecutionEvent::Stdout(chunk) => {
                    stdout.push_str(&String::from_utf8_lossy(chunk));
                }
                ActiveExecutionEvent::Stderr(chunk) => {
                    stderr.push_str(&String::from_utf8_lossy(chunk));
                }
                ActiveExecutionEvent::Exited(code) => {
                    exit_code = Some(*code);
                }
                _ => {}
            }

            sidecar
                .handle_execution_event(&vm_id, "proc-js-ssrf-protection", event)
                .expect("handle javascript ssrf event");
        }

        assert_eq!(exit_code, Some(0), "stderr: {stderr}");
        let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse ssrf JSON");
        assert_eq!(
            parsed["dnsLookup"]["code"],
            Value::String(String::from("EACCES"))
        );
        assert!(
            parsed["dnsLookup"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("169.254.0.0/16")),
            "stdout: {stdout}"
        );
        assert_eq!(
            parsed["privateConnect"]["code"],
            Value::String(String::from("EACCES"))
        );
        assert!(
            parsed["privateConnect"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("169.254.0.0/16")),
            "stdout: {stdout}"
        );
        assert_eq!(
            parsed["loopbackConnect"]["code"],
            Value::String(String::from("EACCES"))
        );
        assert!(
            parsed["loopbackConnect"]["message"]
                .as_str()
                .is_some_and(|message| message.contains(LOOPBACK_EXEMPT_PORTS_ENV)),
            "stdout: {stdout}"
        );

        drop(loopback_listener);
    }

    #[test]
    fn javascript_dns_rpc_honors_vm_dns_overrides_and_net_connect_uses_sidecar_dns() {
        assert_node_available();

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind tcp listener");
        let port = listener.local_addr().expect("listener address").port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept tcp client");
            let mut received = Vec::new();
            stream
                .read_to_end(&mut received)
                .expect("read client payload");
            assert_eq!(String::from_utf8(received).expect("client utf8"), "ping");
            stream.write_all(b"pong").expect("write server payload");
        });

        let mut sidecar = create_test_sidecar();
        let (connection_id, session_id) =
            authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
        let vm_id = create_vm_with_metadata(
            &mut sidecar,
            &connection_id,
            &session_id,
            Vec::new(),
            BTreeMap::from([
                (
                    format!("env.{LOOPBACK_EXEMPT_PORTS_ENV}"),
                    serde_json::to_string(&vec![port.to_string()]).expect("serialize exempt ports"),
                ),
                (
                    String::from("network.dns.override.example.test"),
                    String::from("127.0.0.1"),
                ),
                (
                    String::from(VM_DNS_SERVERS_METADATA_KEY),
                    String::from("203.0.113.53:5353"),
                ),
            ]),
        )
        .expect("create vm");
        let cwd = temp_dir("agent-os-sidecar-js-dns-override-rpc-cwd");
        write_fixture(
            &cwd.join("entry.mjs"),
            &format!(
                r#"
import dns from "node:dns";
import net from "node:net";

const lookup = await dns.promises.lookup("example.test", {{ family: 4 }});
const resolved = await dns.promises.resolve4("example.test");
const socketSummary = await new Promise((resolve, reject) => {{
  const socket = net.createConnection({{ host: "example.test", port: {port} }});
  let data = "";
  socket.setEncoding("utf8");
  socket.on("connect", () => {{
    socket.end("ping");
  }});
  socket.on("data", (chunk) => {{
    data += chunk;
  }});
  socket.on("error", reject);
  socket.on("close", (hadError) => {{
    resolve({{
      data,
      hadError,
      remoteAddress: socket.remoteAddress,
      remotePort: socket.remotePort,
    }});
  }});
}});

console.log(JSON.stringify({{ lookup, resolved, socketSummary }}));
"#,
            ),
        );

        let context = sidecar
            .javascript_engine
            .create_context(CreateJavascriptContextRequest {
                vm_id: vm_id.clone(),
                bootstrap_module: None,
                compile_cache_root: None,
            });
        let execution = sidecar
            .javascript_engine
            .start_execution(StartJavascriptExecutionRequest {
                vm_id: vm_id.clone(),
                context_id: context.context_id,
                argv: vec![String::from("./entry.mjs")],
                env: BTreeMap::from([(
                    String::from("AGENT_OS_ALLOWED_NODE_BUILTINS"),
                    String::from(
                        "[\"assert\",\"buffer\",\"console\",\"crypto\",\"dns\",\"events\",\"fs\",\"net\",\"path\",\"querystring\",\"stream\",\"string_decoder\",\"timers\",\"url\",\"util\",\"zlib\"]",
                    ),
                )]),
                cwd: cwd.clone(),
            })
            .expect("start fake javascript execution");

        let kernel_handle = {
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            vm.kernel
                .spawn_process(
                    JAVASCRIPT_COMMAND,
                    vec![String::from("./entry.mjs")],
                    SpawnOptions {
                        requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                        cwd: Some(String::from("/")),
                        ..SpawnOptions::default()
                    },
                )
                .expect("spawn kernel javascript process")
        };

        {
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            vm.active_processes.insert(
                String::from("proc-js-dns-override"),
                ActiveProcess::new(
                    kernel_handle.pid(),
                    kernel_handle,
                    GuestRuntimeKind::JavaScript,
                    ActiveExecution::Javascript(execution),
                ),
            );
        }

        let mut stdout = String::new();
        let mut stderr = String::new();
        let mut exit_code = None;
        for _ in 0..64 {
            let next_event = {
                let vm = sidecar.vms.get(&vm_id).expect("javascript vm");
                vm.active_processes
                    .get("proc-js-dns-override")
                    .map(|process| {
                        process
                            .execution
                            .poll_event(Duration::from_secs(5))
                            .expect("poll javascript dns override rpc event")
                    })
                    .flatten()
            };
            let Some(event) = next_event else {
                if exit_code.is_some() {
                    break;
                }
                panic!("javascript dns override process disappeared before exit");
            };

            match &event {
                ActiveExecutionEvent::Stdout(chunk) => {
                    stdout.push_str(&String::from_utf8_lossy(chunk));
                }
                ActiveExecutionEvent::Stderr(chunk) => {
                    stderr.push_str(&String::from_utf8_lossy(chunk));
                }
                ActiveExecutionEvent::Exited(code) => {
                    exit_code = Some(*code);
                }
                _ => {}
            }

            sidecar
                .handle_execution_event(&vm_id, "proc-js-dns-override", event)
                .expect("handle javascript dns override rpc event");
        }

        server.join().expect("join tcp server");
        assert_eq!(exit_code, Some(0), "stderr: {stderr}");
        let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse dns JSON");
        assert_eq!(parsed["lookup"]["address"], Value::from("127.0.0.1"));
        assert_eq!(parsed["lookup"]["family"], Value::from(4));
        assert_eq!(parsed["resolved"][0], Value::from("127.0.0.1"));
        assert_eq!(parsed["socketSummary"]["data"], Value::from("pong"));
        assert_eq!(parsed["socketSummary"]["hadError"], Value::from(false));
        assert_eq!(
            parsed["socketSummary"]["remoteAddress"],
            Value::from("127.0.0.1")
        );
        assert_eq!(
            parsed["socketSummary"]["remotePort"],
            Value::from(u64::from(port))
        );

        let events = sidecar
            .with_bridge_mut(|bridge| bridge.structured_events.clone())
            .expect("collect structured events");
        let dns_events = events
            .iter()
            .filter(|event| event.name == "network.dns.resolved")
            .filter(|event| {
                event.fields.get("hostname").map(String::as_str) == Some("example.test")
            })
            .collect::<Vec<_>>();
        assert!(
            dns_events.len() >= 3,
            "expected dns events for lookup, resolve4, and net.connect: {dns_events:?}"
        );
        for event in dns_events {
            assert_eq!(event.fields["source"], "override");
            assert_eq!(event.fields["addresses"], "127.0.0.1");
            assert_eq!(event.fields["resolver_count"], "1");
            assert_eq!(event.fields["resolvers"], "203.0.113.53:5353");
        }
    }

    #[test]
    fn javascript_network_permission_callbacks_fire_for_dns_lookup_connect_and_listen() {
        assert_node_available();

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind tcp listener");
        let port = listener.local_addr().expect("listener address").port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept tcp client");
            let mut received = Vec::new();
            stream
                .read_to_end(&mut received)
                .expect("read client payload");
            assert_eq!(String::from_utf8(received).expect("client utf8"), "ping");
        });

        let mut sidecar = create_test_sidecar();
        let (connection_id, session_id) =
            authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
        let vm_id = create_vm_with_metadata(
            &mut sidecar,
            &connection_id,
            &session_id,
            Vec::new(),
            BTreeMap::from([
                (
                    format!("env.{LOOPBACK_EXEMPT_PORTS_ENV}"),
                    serde_json::to_string(&vec![port.to_string()]).expect("serialize exempt ports"),
                ),
                (
                    String::from("network.dns.override.example.test"),
                    String::from("127.0.0.1"),
                ),
            ]),
        )
        .expect("create vm");
        sidecar
            .bridge
            .clear_vm_permissions(&vm_id)
            .expect("clear static vm permissions");
        let cwd = temp_dir("agent-os-sidecar-js-network-permission-callbacks");
        write_fixture(
            &cwd.join("entry.mjs"),
            &format!(
                r#"
import dns from "node:dns";
import net from "node:net";

const lookup = await dns.promises.lookup("example.test", {{ family: 4 }});
const listenAddress = await new Promise((resolve, reject) => {{
  const server = net.createServer();
  server.on("error", reject);
  server.listen(0, "127.0.0.1", () => {{
    const address = server.address();
    server.close((error) => {{
      if (error) {{
        reject(error);
        return;
      }}
      resolve(address);
    }});
  }});
}});
const connectResult = await new Promise((resolve, reject) => {{
  const socket = net.createConnection({{ host: "127.0.0.1", port: {port} }});
  socket.on("error", reject);
  socket.on("connect", () => {{
    socket.end("ping");
  }});
  socket.on("close", (hadError) => {{
    resolve({{ hadError }});
  }});
}});

console.log(JSON.stringify({{ lookup, listenAddress, connectResult }}));
process.exit(0);
"#,
            ),
        );

        let (stdout, stderr, exit_code) = run_javascript_entry(
            &mut sidecar,
            &vm_id,
            &cwd,
            "proc-js-network-permission-callbacks",
            "[\"assert\",\"buffer\",\"console\",\"crypto\",\"dns\",\"events\",\"fs\",\"net\",\"path\",\"querystring\",\"stream\",\"string_decoder\",\"timers\",\"url\",\"util\",\"zlib\"]",
        );

        server.join().expect("join tcp server");
        assert_eq!(exit_code, Some(0), "stderr: {stderr}");
        let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse callback JSON");
        assert_eq!(
            parsed["lookup"]["address"],
            Value::String(String::from("127.0.0.1"))
        );
        assert_eq!(parsed["connectResult"]["hadError"], Value::Bool(false));
        assert!(
            parsed["listenAddress"]["port"]
                .as_u64()
                .is_some_and(|value| value > 0),
            "stdout: {stdout}"
        );

        let expected = [
            format!("net:{vm_id}:{}", format_dns_resource("example.test")),
            format!("net:{vm_id}:{}", format_tcp_resource("127.0.0.1", 0)),
            format!("net:{vm_id}:{}", format_tcp_resource("127.0.0.1", port)),
        ];
        let checks = sidecar
            .with_bridge_mut(|bridge| {
                bridge
                    .permission_checks
                    .iter()
                    .filter(|entry| entry.starts_with("net:"))
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .expect("read permission checks");
        for check in expected {
            assert!(
                checks.iter().any(|entry| entry == &check),
                "missing permission check {check:?} in {checks:?}"
            );
        }
    }

    #[test]
    fn javascript_network_permission_denials_surface_eacces_to_guest_code() {
        assert_node_available();

        let mut sidecar = create_test_sidecar();
        let (connection_id, session_id) =
            authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
        let vm_id = create_vm_with_metadata(
            &mut sidecar,
            &connection_id,
            &session_id,
            vec![
                PermissionDescriptor {
                    capability: String::from("fs"),
                    mode: PermissionMode::Allow,
                },
                PermissionDescriptor {
                    capability: String::from("env"),
                    mode: PermissionMode::Allow,
                },
                PermissionDescriptor {
                    capability: String::from("child_process"),
                    mode: PermissionMode::Allow,
                },
                PermissionDescriptor {
                    capability: String::from("network"),
                    mode: PermissionMode::Allow,
                },
                PermissionDescriptor {
                    capability: String::from("network.dns"),
                    mode: PermissionMode::Deny,
                },
                PermissionDescriptor {
                    capability: String::from("network.http"),
                    mode: PermissionMode::Deny,
                },
                PermissionDescriptor {
                    capability: String::from("network.listen"),
                    mode: PermissionMode::Deny,
                },
            ],
            BTreeMap::from([(
                String::from("network.dns.override.example.test"),
                String::from("127.0.0.1"),
            )]),
        )
        .expect("create vm");
        let cwd = temp_dir("agent-os-sidecar-js-network-permission-denials");
        write_fixture(
            &cwd.join("entry.mjs"),
            r#"
import dns from "node:dns";
import net from "node:net";

let dnsResult = null;
try {
  dnsResult = { unexpected: await dns.promises.lookup("example.test", { family: 4 }) };
} catch (error) {
  dnsResult = { code: error.code ?? null, message: error.message };
}
const listenResult = (() => {
  const server = net.createServer();
  try {
    server.listen(0, "127.0.0.1");
    return { unexpected: true };
  } catch (error) {
    return { code: error.code ?? null, message: error.message };
  }
})();
const connectResult = await new Promise((resolve) => {
  const socket = net.createConnection({ host: "127.0.0.1", port: 43111 });
  socket.on("connect", () => resolve({ unexpected: true }));
  socket.on("error", (error) => {
    resolve({ code: error.code ?? null, message: error.message });
  });
});

console.log(JSON.stringify({ dnsResult, listenResult, connectResult }));
process.exit(0);
"#,
        );

        let (stdout, stderr, exit_code) = run_javascript_entry(
            &mut sidecar,
            &vm_id,
            &cwd,
            "proc-js-network-permission-denials",
            "[\"assert\",\"buffer\",\"console\",\"crypto\",\"dns\",\"events\",\"fs\",\"net\",\"path\",\"querystring\",\"stream\",\"string_decoder\",\"timers\",\"url\",\"util\",\"zlib\"]",
        );

        assert_eq!(exit_code, Some(0), "stderr: {stderr}");
        let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse denial JSON");
        for field in ["dnsResult", "listenResult", "connectResult"] {
            assert_eq!(parsed[field]["code"], Value::String(String::from("EACCES")));
            assert!(
                parsed[field]["message"]
                    .as_str()
                    .is_some_and(|message| message.contains("blocked by network.")),
                "missing policy detail for {field}: {stdout}"
            );
        }
    }

    #[test]
    fn javascript_tls_rpc_connects_and_serves_over_guest_net() {
        assert_node_available();

        let mut sidecar = create_test_sidecar();
        let (connection_id, session_id) =
            authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
        let vm_id =
            create_vm(&mut sidecar, &connection_id, &session_id, Vec::new()).expect("create vm");
        let cwd = temp_dir("agent-os-sidecar-js-tls-rpc-cwd");
        let entry = format!(
            r#"
import tls from "node:tls";

const key = {key:?};
const cert = {cert:?};

const summary = await new Promise((resolve, reject) => {{
  const server = tls.createServer({{ key, cert }}, (socket) => {{
    let received = "";
    socket.setEncoding("utf8");
    socket.on("data", (chunk) => {{
      received += chunk;
      socket.end(`pong:${{chunk}}`);
    }});
    socket.on("error", reject);
    socket.on("close", () => {{
      server.close(() => {{
        resolve({{
          authorized: client.authorized,
          encrypted: client.encrypted,
          hadError: closeState.hadError,
          localPort: client.localPort,
          received,
          remoteAddress: client.remoteAddress,
          response,
          serverPort: port,
          serverSecure: secureConnectionSeen,
        }});
      }});
    }});
  }});
  let response = "";
  let port = null;
  let secureConnectionSeen = false;
  let closeState = {{ hadError: false }};
  let client = null;

  server.on("secureConnection", () => {{
    secureConnectionSeen = true;
  }});
  server.on("error", reject);
  server.listen(0, "127.0.0.1", () => {{
    port = server.address().port;
    client = tls.connect({{
      host: "127.0.0.1",
      port,
      rejectUnauthorized: false,
    }}, () => {{
      client.write("ping");
    }});
    client.setEncoding("utf8");
    client.on("data", (chunk) => {{
      response += chunk;
    }});
    client.on("error", reject);
    client.on("close", (hadError) => {{
      closeState = {{ hadError }};
    }});
  }});
}});

console.log(JSON.stringify(summary));
"#,
            key = TLS_TEST_KEY_PEM,
            cert = TLS_TEST_CERT_PEM,
        );
        write_fixture(&cwd.join("entry.mjs"), &entry);

        let context = sidecar
            .javascript_engine
            .create_context(CreateJavascriptContextRequest {
                vm_id: vm_id.clone(),
                bootstrap_module: None,
                compile_cache_root: None,
            });
        let execution = sidecar
            .javascript_engine
            .start_execution(StartJavascriptExecutionRequest {
                vm_id: vm_id.clone(),
                context_id: context.context_id,
                argv: vec![String::from("./entry.mjs")],
                env: BTreeMap::from([(
                    String::from("AGENT_OS_ALLOWED_NODE_BUILTINS"),
                    String::from(
                        "[\"assert\",\"buffer\",\"console\",\"crypto\",\"events\",\"fs\",\"net\",\"path\",\"querystring\",\"stream\",\"string_decoder\",\"timers\",\"tls\",\"url\",\"util\",\"zlib\"]",
                    ),
                )]),
                cwd: cwd.clone(),
            })
            .expect("start fake javascript execution");

        let kernel_handle = {
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            vm.kernel
                .spawn_process(
                    JAVASCRIPT_COMMAND,
                    vec![String::from("./entry.mjs")],
                    SpawnOptions {
                        requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                        cwd: Some(String::from("/")),
                        ..SpawnOptions::default()
                    },
                )
                .expect("spawn kernel javascript process")
        };

        {
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            vm.active_processes.insert(
                String::from("proc-js-tls"),
                ActiveProcess::new(
                    kernel_handle.pid(),
                    kernel_handle,
                    GuestRuntimeKind::JavaScript,
                    ActiveExecution::Javascript(execution),
                ),
            );
        }

        let mut stdout = String::new();
        let mut stderr = String::new();
        let mut exit_code = None;
        for _ in 0..192 {
            let next_event = {
                let vm = sidecar.vms.get(&vm_id).expect("javascript vm");
                vm.active_processes
                    .get("proc-js-tls")
                    .map(|process| {
                        process
                            .execution
                            .poll_event(Duration::from_secs(5))
                            .expect("poll javascript tls rpc event")
                    })
                    .flatten()
            };
            let Some(event) = next_event else {
                if exit_code.is_some() {
                    break;
                }
                continue;
            };

            match &event {
                ActiveExecutionEvent::Stdout(chunk) => {
                    stdout.push_str(&String::from_utf8_lossy(chunk));
                }
                ActiveExecutionEvent::Stderr(chunk) => {
                    stderr.push_str(&String::from_utf8_lossy(chunk));
                }
                ActiveExecutionEvent::Exited(code) => {
                    exit_code = Some(*code);
                }
                _ => {}
            }

            sidecar
                .handle_execution_event(&vm_id, "proc-js-tls", event)
                .expect("handle javascript tls rpc event");
        }

        assert_eq!(exit_code, Some(0), "stderr: {stderr}");
        let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse tls JSON");
        assert_eq!(parsed["response"], Value::String(String::from("pong:ping")));
        assert_eq!(parsed["received"], Value::String(String::from("ping")));
        assert_eq!(parsed["serverSecure"], Value::Bool(true));
        assert_eq!(parsed["encrypted"], Value::Bool(true));
        assert_eq!(parsed["hadError"], Value::Bool(false));
        assert_eq!(
            parsed["remoteAddress"],
            Value::String(String::from("127.0.0.1"))
        );
        assert!(
            parsed["serverPort"].as_u64().is_some_and(|port| port > 0),
            "stdout: {stdout}"
        );
    }

    #[test]
    fn javascript_http_rpc_requests_gets_and_serves_over_guest_net() {
        assert_node_available();

        let mut sidecar = create_test_sidecar();
        let (connection_id, session_id) =
            authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
        let vm_id =
            create_vm(&mut sidecar, &connection_id, &session_id, Vec::new()).expect("create vm");
        let cwd = temp_dir("agent-os-sidecar-js-http-rpc-cwd");
        write_fixture(
            &cwd.join("entry.mjs"),
            r#"
import http from "node:http";

const summary = await new Promise((resolve, reject) => {
  const requests = [];
  let requestResponse = "";
  let getResponse = "";

  const server = http.createServer((req, res) => {
    let body = "";
    req.setEncoding("utf8");
    req.on("data", (chunk) => {
      body += chunk;
    });
    req.on("end", () => {
      requests.push({
        method: req.method,
        url: req.url,
        body,
      });
      res.end(`pong:${req.method}:${body || req.url}`);
    });
  });

  let port = null;
  server.on("error", reject);
  server.listen(0, "127.0.0.1", () => {
    port = server.address().port;
    const req = http.request(
      {
        host: "127.0.0.1",
        method: "POST",
        path: "/submit",
        port,
      },
      (res) => {
        res.setEncoding("utf8");
        res.on("data", (chunk) => {
          requestResponse += chunk;
        });
        res.on("end", () => {
          http
            .get(`http://127.0.0.1:${port}/health`, (getRes) => {
              getRes.setEncoding("utf8");
              getRes.on("data", (chunk) => {
                getResponse += chunk;
              });
              getRes.on("end", () => {
                server.close(() => {
                  resolve({
                    getResponse,
                    port,
                    requestResponse,
                    requests,
                  });
                });
              });
            })
            .on("error", reject);
        });
      },
    );
    req.on("error", reject);
    req.end("ping");
  });
});

console.log(JSON.stringify(summary));
"#,
        );

        let context = sidecar
            .javascript_engine
            .create_context(CreateJavascriptContextRequest {
                vm_id: vm_id.clone(),
                bootstrap_module: None,
                compile_cache_root: None,
            });
        let execution = sidecar
            .javascript_engine
            .start_execution(StartJavascriptExecutionRequest {
                vm_id: vm_id.clone(),
                context_id: context.context_id,
                argv: vec![String::from("./entry.mjs")],
                env: BTreeMap::from([(
                    String::from("AGENT_OS_ALLOWED_NODE_BUILTINS"),
                    String::from(
                        "[\"assert\",\"buffer\",\"console\",\"crypto\",\"events\",\"fs\",\"http\",\"path\",\"querystring\",\"stream\",\"string_decoder\",\"timers\",\"url\",\"util\",\"zlib\"]",
                    ),
                )]),
                cwd: cwd.clone(),
            })
            .expect("start fake javascript execution");

        let kernel_handle = {
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            vm.kernel
                .spawn_process(
                    JAVASCRIPT_COMMAND,
                    vec![String::from("./entry.mjs")],
                    SpawnOptions {
                        requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                        cwd: Some(String::from("/")),
                        ..SpawnOptions::default()
                    },
                )
                .expect("spawn kernel javascript process")
        };

        {
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            vm.active_processes.insert(
                String::from("proc-js-http"),
                ActiveProcess::new(
                    kernel_handle.pid(),
                    kernel_handle,
                    GuestRuntimeKind::JavaScript,
                    ActiveExecution::Javascript(execution),
                ),
            );
        }

        let mut stdout = String::new();
        let mut stderr = String::new();
        let mut exit_code = None;
        for _ in 0..192 {
            let next_event = {
                let vm = sidecar.vms.get(&vm_id).expect("javascript vm");
                vm.active_processes
                    .get("proc-js-http")
                    .map(|process| {
                        process
                            .execution
                            .poll_event(Duration::from_secs(5))
                            .expect("poll javascript http rpc event")
                    })
                    .flatten()
            };
            let Some(event) = next_event else {
                if exit_code.is_some() {
                    break;
                }
                continue;
            };

            match &event {
                ActiveExecutionEvent::Stdout(chunk) => {
                    stdout.push_str(&String::from_utf8_lossy(chunk));
                }
                ActiveExecutionEvent::Stderr(chunk) => {
                    stderr.push_str(&String::from_utf8_lossy(chunk));
                }
                ActiveExecutionEvent::Exited(code) => {
                    exit_code = Some(*code);
                }
                _ => {}
            }

            sidecar
                .handle_execution_event(&vm_id, "proc-js-http", event)
                .expect("handle javascript http rpc event");
        }

        assert_eq!(exit_code, Some(0), "stderr: {stderr}");
        let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse http JSON");
        assert_eq!(
            parsed["requestResponse"],
            Value::String(String::from("pong:POST:ping"))
        );
        assert_eq!(
            parsed["getResponse"],
            Value::String(String::from("pong:GET:/health"))
        );
        assert_eq!(
            parsed["requests"][0]["url"],
            Value::String(String::from("/submit"))
        );
        assert_eq!(
            parsed["requests"][1]["url"],
            Value::String(String::from("/health"))
        );
        assert!(
            parsed["port"].as_u64().is_some_and(|port| port > 0),
            "stdout: {stdout}"
        );
    }

    #[test]
    fn javascript_https_rpc_requests_and_serves_over_guest_tls() {
        assert_node_available();

        let mut sidecar = create_test_sidecar();
        let (connection_id, session_id) =
            authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
        let vm_id =
            create_vm(&mut sidecar, &connection_id, &session_id, Vec::new()).expect("create vm");
        let cwd = temp_dir("agent-os-sidecar-js-https-rpc-cwd");
        let entry = format!(
            r#"
import https from "node:https";

const key = {key:?};
const cert = {cert:?};

const summary = await new Promise((resolve, reject) => {{
  let received = "";
  let response = "";
  const server = https.createServer({{ key, cert }}, (req, res) => {{
    req.setEncoding("utf8");
    req.on("data", (chunk) => {{
      received += chunk;
    }});
    req.on("end", () => {{
      res.end(`pong:${{req.method}}:${{received}}`);
    }});
  }});

  let port = null;
  server.on("error", reject);
  server.listen(0, "127.0.0.1", () => {{
    port = server.address().port;
    const req = https.request({{
      host: "127.0.0.1",
      method: "POST",
      path: "/secure",
      port,
      rejectUnauthorized: false,
    }}, (res) => {{
      res.setEncoding("utf8");
      res.on("data", (chunk) => {{
        response += chunk;
      }});
      res.on("end", () => {{
        server.close(() => {{
          resolve({{
            port,
            received,
            response,
          }});
        }});
      }});
    }});
    req.on("error", reject);
    req.end("ping");
  }});
}});

console.log(JSON.stringify(summary));
"#,
            key = TLS_TEST_KEY_PEM,
            cert = TLS_TEST_CERT_PEM,
        );
        write_fixture(&cwd.join("entry.mjs"), &entry);

        let context = sidecar
            .javascript_engine
            .create_context(CreateJavascriptContextRequest {
                vm_id: vm_id.clone(),
                bootstrap_module: None,
                compile_cache_root: None,
            });
        let execution = sidecar
            .javascript_engine
            .start_execution(StartJavascriptExecutionRequest {
                vm_id: vm_id.clone(),
                context_id: context.context_id,
                argv: vec![String::from("./entry.mjs")],
                env: BTreeMap::from([(
                    String::from("AGENT_OS_ALLOWED_NODE_BUILTINS"),
                    String::from(
                        "[\"assert\",\"buffer\",\"console\",\"crypto\",\"events\",\"fs\",\"https\",\"path\",\"querystring\",\"stream\",\"string_decoder\",\"timers\",\"url\",\"util\",\"zlib\"]",
                    ),
                )]),
                cwd: cwd.clone(),
            })
            .expect("start fake javascript execution");

        let kernel_handle = {
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            vm.kernel
                .spawn_process(
                    JAVASCRIPT_COMMAND,
                    vec![String::from("./entry.mjs")],
                    SpawnOptions {
                        requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                        cwd: Some(String::from("/")),
                        ..SpawnOptions::default()
                    },
                )
                .expect("spawn kernel javascript process")
        };

        {
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            vm.active_processes.insert(
                String::from("proc-js-https"),
                ActiveProcess::new(
                    kernel_handle.pid(),
                    kernel_handle,
                    GuestRuntimeKind::JavaScript,
                    ActiveExecution::Javascript(execution),
                ),
            );
        }

        let mut stdout = String::new();
        let mut stderr = String::new();
        let mut exit_code = None;
        for _ in 0..192 {
            let next_event = {
                let vm = sidecar.vms.get(&vm_id).expect("javascript vm");
                vm.active_processes
                    .get("proc-js-https")
                    .map(|process| {
                        process
                            .execution
                            .poll_event(Duration::from_secs(5))
                            .expect("poll javascript https rpc event")
                    })
                    .flatten()
            };
            let Some(event) = next_event else {
                if exit_code.is_some() {
                    break;
                }
                continue;
            };

            match &event {
                ActiveExecutionEvent::Stdout(chunk) => {
                    stdout.push_str(&String::from_utf8_lossy(chunk));
                }
                ActiveExecutionEvent::Stderr(chunk) => {
                    stderr.push_str(&String::from_utf8_lossy(chunk));
                }
                ActiveExecutionEvent::Exited(code) => {
                    exit_code = Some(*code);
                }
                _ => {}
            }

            sidecar
                .handle_execution_event(&vm_id, "proc-js-https", event)
                .expect("handle javascript https rpc event");
        }

        assert_eq!(exit_code, Some(0), "stderr: {stderr}");
        let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse https JSON");
        assert_eq!(parsed["received"], Value::String(String::from("ping")));
        assert_eq!(
            parsed["response"],
            Value::String(String::from("pong:POST:ping"))
        );
        assert!(
            parsed["port"].as_u64().is_some_and(|port| port > 0),
            "stdout: {stdout}"
        );
    }

    #[test]
    fn javascript_net_rpc_listens_accepts_connections_and_reports_listener_state() {
        assert_node_available();

        let mut sidecar = create_test_sidecar();
        let (connection_id, session_id) =
            authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
        let vm_id =
            create_vm(&mut sidecar, &connection_id, &session_id, Vec::new()).expect("create vm");
        let cwd = temp_dir("agent-os-sidecar-js-net-server-cwd");
        write_fixture(&cwd.join("entry.mjs"), "setInterval(() => {}, 1000);");
        start_fake_javascript_process(&mut sidecar, &vm_id, &cwd, "proc-js-server", "[\"net\"]");

        let listen = call_javascript_sync_rpc(
            &mut sidecar,
            &vm_id,
            "proc-js-server",
            JavascriptSyncRpcRequest {
                id: 1,
                method: String::from("net.listen"),
                args: vec![json!({
                    "host": "127.0.0.1",
                    "port": 0,
                    "backlog": 2,
                })],
            },
        )
        .expect("listen through sidecar net RPC");
        let server_id = listen["serverId"].as_str().expect("server id").to_string();
        let guest_port = listen["localPort"]
            .as_u64()
            .and_then(|value| u16::try_from(value).ok())
            .expect("guest listener port");
        let host_port = {
            let vm = sidecar.vms.get(&vm_id).expect("javascript vm");
            vm.active_processes
                .get("proc-js-server")
                .and_then(|process| process.tcp_listeners.get(&server_id))
                .expect("sidecar tcp listener")
                .local_addr()
                .port()
        };

        let response = sidecar
            .dispatch(request(
                1,
                OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                RequestPayload::FindListener(FindListenerRequest {
                    host: Some(String::from("127.0.0.1")),
                    port: Some(guest_port),
                    path: None,
                }),
            ))
            .expect("query sidecar listener");
        match response.response.payload {
            ResponsePayload::ListenerSnapshot(snapshot) => {
                let listener = snapshot.listener.expect("listener snapshot");
                assert_eq!(listener.process_id, "proc-js-server");
                assert_eq!(listener.host.as_deref(), Some("127.0.0.1"));
                assert_eq!(listener.port, Some(guest_port));
            }
            other => panic!("unexpected find_listener response payload: {other:?}"),
        }

        let client = thread::spawn(move || {
            let mut stream =
                TcpStream::connect(("127.0.0.1", host_port)).expect("connect to sidecar listener");
            stream.write_all(b"ping").expect("write client payload");
            stream
                .shutdown(Shutdown::Write)
                .expect("shutdown client write half");
            let mut received = Vec::new();
            stream
                .read_to_end(&mut received)
                .expect("read server response");
            assert_eq!(
                String::from_utf8(received).expect("server response utf8"),
                "pong:ping"
            );
        });

        let accepted = call_javascript_sync_rpc(
            &mut sidecar,
            &vm_id,
            "proc-js-server",
            JavascriptSyncRpcRequest {
                id: 2,
                method: String::from("net.server_poll"),
                args: vec![json!(server_id), json!(250)],
            },
        )
        .expect("accept connection");
        assert_eq!(accepted["type"], Value::from("connection"));
        assert_eq!(accepted["localAddress"], Value::from("127.0.0.1"));
        assert_eq!(accepted["localPort"], Value::from(guest_port));
        let socket_id = accepted["socketId"]
            .as_str()
            .expect("socket id")
            .to_string();

        let data = call_javascript_sync_rpc(
            &mut sidecar,
            &vm_id,
            "proc-js-server",
            JavascriptSyncRpcRequest {
                id: 3,
                method: String::from("net.poll"),
                args: vec![json!(socket_id.clone()), json!(250)],
            },
        )
        .expect("poll socket data");
        assert_eq!(data["type"], Value::from("data"));

        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data["data"]["base64"].as_str().expect("base64 payload"))
            .expect("decode payload");
        assert_eq!(bytes, b"ping");

        let written = call_javascript_sync_rpc(
            &mut sidecar,
            &vm_id,
            "proc-js-server",
            JavascriptSyncRpcRequest {
                id: 4,
                method: String::from("net.write"),
                args: vec![json!(socket_id.clone()), json!("pong:ping")],
            },
        )
        .expect("write response");
        assert_eq!(written, Value::from(9));

        call_javascript_sync_rpc(
            &mut sidecar,
            &vm_id,
            "proc-js-server",
            JavascriptSyncRpcRequest {
                id: 5,
                method: String::from("net.shutdown"),
                args: vec![json!(socket_id)],
            },
        )
        .expect("shutdown write half");
        client.join().expect("join tcp client");
    }

    #[test]
    fn javascript_net_rpc_reports_connection_counts_and_enforces_backlog() {
        assert_node_available();

        let mut sidecar = create_test_sidecar();
        let (connection_id, session_id) =
            authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
        let vm_id =
            create_vm(&mut sidecar, &connection_id, &session_id, Vec::new()).expect("create vm");
        let cwd = temp_dir("agent-os-sidecar-js-net-backlog-cwd");
        write_fixture(&cwd.join("entry.mjs"), "setInterval(() => {}, 1000);");

        let context = sidecar
            .javascript_engine
            .create_context(CreateJavascriptContextRequest {
                vm_id: vm_id.clone(),
                bootstrap_module: None,
                compile_cache_root: None,
            });
        let execution = sidecar
            .javascript_engine
            .start_execution(StartJavascriptExecutionRequest {
                vm_id: vm_id.clone(),
                context_id: context.context_id,
                argv: vec![String::from("./entry.mjs")],
                env: BTreeMap::from([(
                    String::from("AGENT_OS_ALLOWED_NODE_BUILTINS"),
                    String::from(
                        "[\"assert\",\"buffer\",\"console\",\"crypto\",\"events\",\"fs\",\"net\",\"path\",\"querystring\",\"stream\",\"string_decoder\",\"timers\",\"url\",\"util\",\"zlib\"]",
                    ),
                )]),
                cwd: cwd.clone(),
            })
            .expect("start fake javascript execution");

        let kernel_handle = {
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            vm.kernel
                .spawn_process(
                    JAVASCRIPT_COMMAND,
                    vec![String::from("./entry.mjs")],
                    SpawnOptions {
                        requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                        cwd: Some(String::from("/")),
                        ..SpawnOptions::default()
                    },
                )
                .expect("spawn kernel javascript process")
        };

        {
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            vm.active_processes.insert(
                String::from("proc-js-backlog"),
                ActiveProcess::new(
                    kernel_handle.pid(),
                    kernel_handle,
                    GuestRuntimeKind::JavaScript,
                    ActiveExecution::Javascript(execution),
                ),
            );
        }

        let bridge = sidecar.bridge.clone();
        let dns = sidecar.vms.get(&vm_id).expect("javascript vm").dns.clone();
        let limits = ResourceLimits::default();
        let socket_paths = {
            let vm = sidecar.vms.get(&vm_id).expect("javascript vm");
            build_javascript_socket_path_context(vm).expect("build socket path context")
        };

        let listen = {
            let counts = sidecar
                .vms
                .get(&vm_id)
                .and_then(|vm| vm.active_processes.get("proc-js-backlog"))
                .expect("backlog process")
                .network_resource_counts();
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            let process = vm
                .active_processes
                .get_mut("proc-js-backlog")
                .expect("backlog process");
            service_javascript_net_sync_rpc(
                &bridge,
                &vm_id,
                &dns,
                &socket_paths,
                &mut vm.kernel,
                process,
                &JavascriptSyncRpcRequest {
                    id: 1,
                    method: String::from("net.listen"),
                    args: vec![json!({
                        "host": "127.0.0.1",
                        "port": 0,
                        "backlog": 1,
                    })],
                },
                &limits,
                counts,
            )
            .expect("listen through sidecar net RPC")
        };
        let server_id = listen["serverId"].as_str().expect("server id").to_string();
        let _port = listen["localPort"]
            .as_u64()
            .and_then(|value| u16::try_from(value).ok())
            .expect("listener port");
        let host_port = {
            let vm = sidecar.vms.get(&vm_id).expect("javascript vm");
            vm.active_processes
                .get("proc-js-backlog")
                .and_then(|process| process.tcp_listeners.get(&server_id))
                .expect("host backlog listener")
                .local_addr()
                .port()
        };

        let first_client = thread::spawn(move || {
            let mut stream =
                TcpStream::connect(("127.0.0.1", host_port)).expect("connect first backlog client");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("set first client timeout");
            let mut received = Vec::new();
            stream
                .read_to_end(&mut received)
                .expect("read first backlog client EOF");
            assert!(
                received.is_empty(),
                "first backlog client should not receive data"
            );
        });

        let first_connection = {
            let counts = sidecar
                .vms
                .get(&vm_id)
                .and_then(|vm| vm.active_processes.get("proc-js-backlog"))
                .expect("backlog process")
                .network_resource_counts();
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            let process = vm
                .active_processes
                .get_mut("proc-js-backlog")
                .expect("backlog process");
            service_javascript_net_sync_rpc(
                &bridge,
                &vm_id,
                &dns,
                &socket_paths,
                &mut vm.kernel,
                process,
                &JavascriptSyncRpcRequest {
                    id: 2,
                    method: String::from("net.server_poll"),
                    args: vec![json!(server_id), json!(250)],
                },
                &limits,
                counts,
            )
            .expect("accept first backlog connection")
        };
        let first_socket_id = first_connection["socketId"]
            .as_str()
            .expect("first socket id")
            .to_string();

        let connection_count = {
            let counts = sidecar
                .vms
                .get(&vm_id)
                .and_then(|vm| vm.active_processes.get("proc-js-backlog"))
                .expect("backlog process")
                .network_resource_counts();
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            let process = vm
                .active_processes
                .get_mut("proc-js-backlog")
                .expect("backlog process");
            service_javascript_net_sync_rpc(
                &bridge,
                &vm_id,
                &dns,
                &socket_paths,
                &mut vm.kernel,
                process,
                &JavascriptSyncRpcRequest {
                    id: 3,
                    method: String::from("net.server_connections"),
                    args: vec![json!(server_id)],
                },
                &limits,
                counts,
            )
            .expect("query server connections")
        };
        assert_eq!(connection_count, json!(1));

        let second_client = thread::spawn(move || {
            let address = SocketAddr::from(([127, 0, 0, 1], host_port));
            let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(2))
                .expect("connect second backlog client");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("set second client timeout");
            stream
                .write_all(b"blocked")
                .expect("write second backlog client payload");
            let mut buffer = [0_u8; 16];
            match stream.read(&mut buffer) {
                Ok(0) => {}
                Ok(bytes_read) => panic!(
                    "unexpected second backlog payload: {}",
                    String::from_utf8_lossy(&buffer[..bytes_read])
                ),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::ConnectionAborted
                            | std::io::ErrorKind::ConnectionReset
                            | std::io::ErrorKind::NotConnected
                            | std::io::ErrorKind::TimedOut
                            | std::io::ErrorKind::WouldBlock
                    ) => {}
                Err(error) => panic!("unexpected second backlog read error: {error}"),
            }
        });

        let second_poll = {
            let counts = sidecar
                .vms
                .get(&vm_id)
                .and_then(|vm| vm.active_processes.get("proc-js-backlog"))
                .expect("backlog process")
                .network_resource_counts();
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            let process = vm
                .active_processes
                .get_mut("proc-js-backlog")
                .expect("backlog process");
            service_javascript_net_sync_rpc(
                &bridge,
                &vm_id,
                &dns,
                &socket_paths,
                &mut vm.kernel,
                process,
                &JavascriptSyncRpcRequest {
                    id: 4,
                    method: String::from("net.server_poll"),
                    args: vec![json!(server_id), json!(250)],
                },
                &limits,
                counts,
            )
            .expect("poll second backlog connection")
        };
        assert_eq!(second_poll, Value::Null);
        second_client.join().expect("join second backlog client");

        let connection_count = {
            let counts = sidecar
                .vms
                .get(&vm_id)
                .and_then(|vm| vm.active_processes.get("proc-js-backlog"))
                .expect("backlog process")
                .network_resource_counts();
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            let process = vm
                .active_processes
                .get_mut("proc-js-backlog")
                .expect("backlog process");
            service_javascript_net_sync_rpc(
                &bridge,
                &vm_id,
                &dns,
                &socket_paths,
                &mut vm.kernel,
                process,
                &JavascriptSyncRpcRequest {
                    id: 5,
                    method: String::from("net.server_connections"),
                    args: vec![json!(server_id)],
                },
                &limits,
                counts,
            )
            .expect("query server connections after backlog rejection")
        };
        assert_eq!(connection_count, json!(1));

        {
            let counts = sidecar
                .vms
                .get(&vm_id)
                .and_then(|vm| vm.active_processes.get("proc-js-backlog"))
                .expect("backlog process")
                .network_resource_counts();
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            let process = vm
                .active_processes
                .get_mut("proc-js-backlog")
                .expect("backlog process");
            service_javascript_net_sync_rpc(
                &bridge,
                &vm_id,
                &dns,
                &socket_paths,
                &mut vm.kernel,
                process,
                &JavascriptSyncRpcRequest {
                    id: 6,
                    method: String::from("net.destroy"),
                    args: vec![json!(first_socket_id)],
                },
                &limits,
                counts,
            )
            .expect("destroy first backlog socket");
        }
        first_client.join().expect("join first backlog client");

        {
            let counts = sidecar
                .vms
                .get(&vm_id)
                .and_then(|vm| vm.active_processes.get("proc-js-backlog"))
                .expect("backlog process")
                .network_resource_counts();
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            let process = vm
                .active_processes
                .get_mut("proc-js-backlog")
                .expect("backlog process");
            service_javascript_net_sync_rpc(
                &bridge,
                &vm_id,
                &dns,
                &socket_paths,
                &mut vm.kernel,
                process,
                &JavascriptSyncRpcRequest {
                    id: 7,
                    method: String::from("net.server_close"),
                    args: vec![json!(server_id)],
                },
                &limits,
                counts,
            )
            .expect("close backlog listener");
        }

        sidecar
            .dispose_vm_internal(
                &connection_id,
                &session_id,
                &vm_id,
                DisposeReason::Requested,
            )
            .expect("dispose backlog vm");
    }

    #[test]
    fn javascript_network_bind_policy_restricts_hosts_and_ports() {
        assert_node_available();

        let mut sidecar = create_test_sidecar();
        let (connection_id, session_id) =
            authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
        let vm_id = create_vm_with_metadata(
            &mut sidecar,
            &connection_id,
            &session_id,
            Vec::new(),
            BTreeMap::from([
                (
                    String::from(VM_LISTEN_PORT_MIN_METADATA_KEY),
                    String::from("49152"),
                ),
                (
                    String::from(VM_LISTEN_PORT_MAX_METADATA_KEY),
                    String::from("49160"),
                ),
            ]),
        )
        .expect("create vm");
        let cwd = temp_dir("agent-os-sidecar-js-bind-policy-cwd");
        write_fixture(&cwd.join("entry.mjs"), "setInterval(() => {}, 1000);");
        start_fake_javascript_process(
            &mut sidecar,
            &vm_id,
            &cwd,
            "proc-js-bind-policy",
            "[\"dgram\",\"net\"]",
        );

        let unspecified = call_javascript_sync_rpc(
            &mut sidecar,
            &vm_id,
            "proc-js-bind-policy",
            JavascriptSyncRpcRequest {
                id: 1,
                method: String::from("net.listen"),
                args: vec![json!({
                    "host": "0.0.0.0",
                    "port": 49152,
                })],
            },
        )
        .expect_err("deny unspecified TCP listen host");
        assert!(
            unspecified
                .to_string()
                .contains("must bind to loopback, not unspecified"),
            "{unspecified}"
        );

        let privileged = call_javascript_sync_rpc(
            &mut sidecar,
            &vm_id,
            "proc-js-bind-policy",
            JavascriptSyncRpcRequest {
                id: 2,
                method: String::from("net.listen"),
                args: vec![json!({
                    "host": "127.0.0.1",
                    "port": 80,
                })],
            },
        )
        .expect_err("deny privileged port");
        assert!(
            privileged
                .to_string()
                .contains("privileged listen port 80 requires"),
            "{privileged}"
        );

        let out_of_range = call_javascript_sync_rpc(
            &mut sidecar,
            &vm_id,
            "proc-js-bind-policy",
            JavascriptSyncRpcRequest {
                id: 3,
                method: String::from("net.listen"),
                args: vec![json!({
                    "host": "127.0.0.1",
                    "port": 40000,
                })],
            },
        )
        .expect_err("deny out-of-range port");
        assert!(
            out_of_range
                .to_string()
                .contains("outside the allowed range 49152-49160"),
            "{out_of_range}"
        );

        let udp_socket = call_javascript_sync_rpc(
            &mut sidecar,
            &vm_id,
            "proc-js-bind-policy",
            JavascriptSyncRpcRequest {
                id: 4,
                method: String::from("dgram.createSocket"),
                args: vec![json!({ "type": "udp4" })],
            },
        )
        .expect("create udp socket");
        let udp_socket_id = udp_socket["socketId"]
            .as_str()
            .expect("udp socket id")
            .to_string();

        let udp_unspecified = call_javascript_sync_rpc(
            &mut sidecar,
            &vm_id,
            "proc-js-bind-policy",
            JavascriptSyncRpcRequest {
                id: 5,
                method: String::from("dgram.bind"),
                args: vec![
                    json!(udp_socket_id),
                    json!({
                        "address": "0.0.0.0",
                        "port": 49153,
                    }),
                ],
            },
        )
        .expect_err("deny unspecified UDP bind host");
        assert!(
            udp_unspecified
                .to_string()
                .contains("must bind to loopback, not unspecified"),
            "{udp_unspecified}"
        );

        let success = call_javascript_sync_rpc(
            &mut sidecar,
            &vm_id,
            "proc-js-bind-policy",
            JavascriptSyncRpcRequest {
                id: 6,
                method: String::from("net.listen"),
                args: vec![json!({
                    "host": "127.0.0.1",
                    "port": 49155,
                })],
            },
        )
        .expect("allow loopback listener inside configured range");
        assert_eq!(success["localAddress"], Value::from("127.0.0.1"));
        assert_eq!(success["localPort"], Value::from(49155));
    }

    #[test]
    fn javascript_network_bind_policy_can_allow_privileged_guest_ports() {
        assert_node_available();

        let mut sidecar = create_test_sidecar();
        let (connection_id, session_id) =
            authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
        let vm_id = create_vm_with_metadata(
            &mut sidecar,
            &connection_id,
            &session_id,
            Vec::new(),
            BTreeMap::from([
                (
                    String::from(VM_LISTEN_PORT_MIN_METADATA_KEY),
                    String::from("1"),
                ),
                (
                    String::from(VM_LISTEN_PORT_MAX_METADATA_KEY),
                    String::from("128"),
                ),
                (
                    String::from(VM_LISTEN_ALLOW_PRIVILEGED_METADATA_KEY),
                    String::from("true"),
                ),
            ]),
        )
        .expect("create vm");
        let cwd = temp_dir("agent-os-sidecar-js-privileged-listen-cwd");
        write_fixture(&cwd.join("entry.mjs"), "setInterval(() => {}, 1000);");
        start_fake_javascript_process(
            &mut sidecar,
            &vm_id,
            &cwd,
            "proc-js-privileged",
            "[\"net\"]",
        );

        let listen = call_javascript_sync_rpc(
            &mut sidecar,
            &vm_id,
            "proc-js-privileged",
            JavascriptSyncRpcRequest {
                id: 1,
                method: String::from("net.listen"),
                args: vec![json!({
                    "host": "127.0.0.1",
                    "port": 80,
                })],
            },
        )
        .expect("allow privileged guest port");
        assert_eq!(listen["localAddress"], Value::from("127.0.0.1"));
        assert_eq!(listen["localPort"], Value::from(80));
    }

    #[test]
    fn javascript_network_listeners_are_isolated_per_vm_even_with_same_guest_port() {
        assert_node_available();

        let mut sidecar = create_test_sidecar();
        let (connection_id, session_id) =
            authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
        let vm_a =
            create_vm(&mut sidecar, &connection_id, &session_id, Vec::new()).expect("create vm a");
        let vm_b =
            create_vm(&mut sidecar, &connection_id, &session_id, Vec::new()).expect("create vm b");
        let cwd_a = temp_dir("agent-os-sidecar-js-net-isolation-a");
        let cwd_b = temp_dir("agent-os-sidecar-js-net-isolation-b");
        write_fixture(&cwd_a.join("entry.mjs"), "setInterval(() => {}, 1000);");
        write_fixture(&cwd_b.join("entry.mjs"), "setInterval(() => {}, 1000);");
        start_fake_javascript_process(&mut sidecar, &vm_a, &cwd_a, "proc-a", "[\"net\"]");
        start_fake_javascript_process(&mut sidecar, &vm_b, &cwd_b, "proc-b", "[\"net\"]");

        let listen_a = call_javascript_sync_rpc(
            &mut sidecar,
            &vm_a,
            "proc-a",
            JavascriptSyncRpcRequest {
                id: 1,
                method: String::from("net.listen"),
                args: vec![json!({
                    "host": "127.0.0.1",
                    "port": 43111,
                })],
            },
        )
        .expect("listen on vm a");
        let listen_b = call_javascript_sync_rpc(
            &mut sidecar,
            &vm_b,
            "proc-b",
            JavascriptSyncRpcRequest {
                id: 1,
                method: String::from("net.listen"),
                args: vec![json!({
                    "host": "127.0.0.1",
                    "port": 43111,
                })],
            },
        )
        .expect("listen on vm b");
        assert_eq!(listen_a["localPort"], Value::from(43111));
        assert_eq!(listen_b["localPort"], Value::from(43111));

        let connect_a = call_javascript_sync_rpc(
            &mut sidecar,
            &vm_a,
            "proc-a",
            JavascriptSyncRpcRequest {
                id: 2,
                method: String::from("net.connect"),
                args: vec![json!({
                    "host": "127.0.0.1",
                    "port": 43111,
                })],
            },
        )
        .expect("connect within vm a");
        let connect_b = call_javascript_sync_rpc(
            &mut sidecar,
            &vm_b,
            "proc-b",
            JavascriptSyncRpcRequest {
                id: 2,
                method: String::from("net.connect"),
                args: vec![json!({
                    "host": "127.0.0.1",
                    "port": 43111,
                })],
            },
        )
        .expect("connect within vm b");
        assert_eq!(connect_a["remotePort"], Value::from(43111));
        assert_eq!(connect_b["remotePort"], Value::from(43111));

        let server_id_a = listen_a["serverId"]
            .as_str()
            .expect("server id a")
            .to_string();
        let server_id_b = listen_b["serverId"]
            .as_str()
            .expect("server id b")
            .to_string();
        let accepted_a = call_javascript_sync_rpc(
            &mut sidecar,
            &vm_a,
            "proc-a",
            JavascriptSyncRpcRequest {
                id: 3,
                method: String::from("net.server_poll"),
                args: vec![json!(server_id_a), json!(250)],
            },
        )
        .expect("accept vm a connection");
        let accepted_b = call_javascript_sync_rpc(
            &mut sidecar,
            &vm_b,
            "proc-b",
            JavascriptSyncRpcRequest {
                id: 3,
                method: String::from("net.server_poll"),
                args: vec![json!(server_id_b), json!(250)],
            },
        )
        .expect("accept vm b connection");
        assert_eq!(accepted_a["type"], Value::from("connection"));
        assert_eq!(accepted_b["type"], Value::from("connection"));
        assert_eq!(accepted_a["localPort"], Value::from(43111));
        assert_eq!(accepted_b["localPort"], Value::from(43111));

        let query_a = sidecar
            .dispatch(request(
                50,
                OwnershipScope::vm(&connection_id, &session_id, &vm_a),
                RequestPayload::FindListener(FindListenerRequest {
                    host: Some(String::from("127.0.0.1")),
                    port: Some(43111),
                    path: None,
                }),
            ))
            .expect("query vm a listener");
        let query_b = sidecar
            .dispatch(request(
                51,
                OwnershipScope::vm(&connection_id, &session_id, &vm_b),
                RequestPayload::FindListener(FindListenerRequest {
                    host: Some(String::from("127.0.0.1")),
                    port: Some(43111),
                    path: None,
                }),
            ))
            .expect("query vm b listener");
        match query_a.response.payload {
            ResponsePayload::ListenerSnapshot(snapshot) => {
                let listener = snapshot.listener.expect("vm a listener");
                assert_eq!(listener.process_id, "proc-a");
                assert_eq!(listener.host.as_deref(), Some("127.0.0.1"));
                assert_eq!(listener.port, Some(43111));
            }
            other => panic!("unexpected vm a listener response: {other:?}"),
        }
        match query_b.response.payload {
            ResponsePayload::ListenerSnapshot(snapshot) => {
                let listener = snapshot.listener.expect("vm b listener");
                assert_eq!(listener.process_id, "proc-b");
                assert_eq!(listener.host.as_deref(), Some("127.0.0.1"));
                assert_eq!(listener.port, Some(43111));
            }
            other => panic!("unexpected vm b listener response: {other:?}"),
        }
    }

    #[test]
    fn javascript_net_rpc_listens_and_connects_over_unix_domain_sockets() {
        assert_node_available();

        let mut sidecar = create_test_sidecar();
        let (connection_id, session_id) =
            authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
        let vm_id =
            create_vm(&mut sidecar, &connection_id, &session_id, Vec::new()).expect("create vm");
        let cwd = temp_dir("agent-os-sidecar-js-net-unix-cwd");
        write_fixture(&cwd.join("entry.mjs"), "setInterval(() => {}, 1000);");

        let context = sidecar
            .javascript_engine
            .create_context(CreateJavascriptContextRequest {
                vm_id: vm_id.clone(),
                bootstrap_module: None,
                compile_cache_root: None,
            });
        let execution = sidecar
            .javascript_engine
            .start_execution(StartJavascriptExecutionRequest {
                vm_id: vm_id.clone(),
                context_id: context.context_id,
                argv: vec![String::from("./entry.mjs")],
                env: BTreeMap::from([(
                    String::from("AGENT_OS_ALLOWED_NODE_BUILTINS"),
                    String::from(
                        "[\"assert\",\"buffer\",\"console\",\"crypto\",\"events\",\"fs\",\"net\",\"path\",\"querystring\",\"stream\",\"string_decoder\",\"timers\",\"url\",\"util\",\"zlib\"]",
                    ),
                )]),
                cwd: cwd.clone(),
            })
            .expect("start fake javascript execution");

        let kernel_handle = {
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            vm.kernel
                .spawn_process(
                    JAVASCRIPT_COMMAND,
                    vec![String::from("./entry.mjs")],
                    SpawnOptions {
                        requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                        cwd: Some(String::from("/")),
                        ..SpawnOptions::default()
                    },
                )
                .expect("spawn kernel javascript process")
        };

        {
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            vm.active_processes.insert(
                String::from("proc-js-unix"),
                ActiveProcess::new(
                    kernel_handle.pid(),
                    kernel_handle,
                    GuestRuntimeKind::JavaScript,
                    ActiveExecution::Javascript(execution),
                ),
            );
        }

        let bridge = sidecar.bridge.clone();
        let dns = sidecar.vms.get(&vm_id).expect("javascript vm").dns.clone();
        let limits = ResourceLimits::default();
        let socket_paths = JavascriptSocketPathContext {
            sandbox_root: cwd.clone(),
            mounts: Vec::new(),
            listen_policy: VmListenPolicy::default(),
            loopback_exempt_ports: BTreeSet::new(),
            tcp_loopback_guest_to_host_ports: BTreeMap::new(),
            udp_loopback_guest_to_host_ports: BTreeMap::new(),
            udp_loopback_host_to_guest_ports: BTreeMap::new(),
            used_tcp_guest_ports: BTreeMap::new(),
            used_udp_guest_ports: BTreeMap::new(),
        };
        let socket_path = "/tmp/agent-os.sock";
        let host_socket_path = cwd.join("tmp/agent-os.sock");

        let listen = {
            let counts = sidecar
                .vms
                .get(&vm_id)
                .and_then(|vm| vm.active_processes.get("proc-js-unix"))
                .expect("unix process")
                .network_resource_counts();
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            let process = vm
                .active_processes
                .get_mut("proc-js-unix")
                .expect("unix process");
            service_javascript_net_sync_rpc(
                &bridge,
                &vm_id,
                &dns,
                &socket_paths,
                &mut vm.kernel,
                process,
                &JavascriptSyncRpcRequest {
                    id: 1,
                    method: String::from("net.listen"),
                    args: vec![json!({
                        "path": socket_path,
                        "backlog": 1,
                    })],
                },
                &limits,
                counts,
            )
            .expect("listen on unix socket")
        };
        let server_id = listen["serverId"].as_str().expect("server id").to_string();
        assert_eq!(listen["path"], Value::String(String::from(socket_path)));
        {
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            assert!(
                vm.kernel
                    .exists(socket_path)
                    .expect("kernel socket placeholder exists"),
                "kernel did not expose unix socket path"
            );
        }
        assert!(host_socket_path.exists(), "host unix socket path missing");

        let listener_lookup = sidecar
            .dispatch(request(
                2,
                OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                RequestPayload::FindListener(FindListenerRequest {
                    host: None,
                    port: None,
                    path: Some(String::from(socket_path)),
                }),
            ))
            .expect("query unix listener");
        match listener_lookup.response.payload {
            ResponsePayload::ListenerSnapshot(snapshot) => {
                let listener = snapshot.listener.expect("listener snapshot");
                assert_eq!(listener.process_id, "proc-js-unix");
                assert_eq!(listener.path.as_deref(), Some(socket_path));
            }
            other => panic!("unexpected listener response payload: {other:?}"),
        }

        let connect = {
            let counts = sidecar
                .vms
                .get(&vm_id)
                .and_then(|vm| vm.active_processes.get("proc-js-unix"))
                .expect("unix process")
                .network_resource_counts();
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            let process = vm
                .active_processes
                .get_mut("proc-js-unix")
                .expect("unix process");
            service_javascript_net_sync_rpc(
                &bridge,
                &vm_id,
                &dns,
                &socket_paths,
                &mut vm.kernel,
                process,
                &JavascriptSyncRpcRequest {
                    id: 3,
                    method: String::from("net.connect"),
                    args: vec![json!({
                        "path": socket_path,
                    })],
                },
                &limits,
                counts,
            )
            .expect("connect to unix listener")
        };
        let client_socket_id = connect["socketId"]
            .as_str()
            .expect("client socket id")
            .to_string();
        assert_eq!(
            connect["remotePath"],
            Value::String(String::from(socket_path))
        );

        let accepted = {
            let counts = sidecar
                .vms
                .get(&vm_id)
                .and_then(|vm| vm.active_processes.get("proc-js-unix"))
                .expect("unix process")
                .network_resource_counts();
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            let process = vm
                .active_processes
                .get_mut("proc-js-unix")
                .expect("unix process");
            service_javascript_net_sync_rpc(
                &bridge,
                &vm_id,
                &dns,
                &socket_paths,
                &mut vm.kernel,
                process,
                &JavascriptSyncRpcRequest {
                    id: 4,
                    method: String::from("net.server_poll"),
                    args: vec![json!(server_id), json!(250)],
                },
                &limits,
                counts,
            )
            .expect("accept unix socket connection")
        };
        let server_socket_id = accepted["socketId"]
            .as_str()
            .expect("server socket id")
            .to_string();
        assert_eq!(
            accepted["localPath"],
            Value::String(String::from(socket_path))
        );

        {
            let counts = sidecar
                .vms
                .get(&vm_id)
                .and_then(|vm| vm.active_processes.get("proc-js-unix"))
                .expect("unix process")
                .network_resource_counts();
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            let process = vm
                .active_processes
                .get_mut("proc-js-unix")
                .expect("unix process");
            let connections = service_javascript_net_sync_rpc(
                &bridge,
                &vm_id,
                &dns,
                &socket_paths,
                &mut vm.kernel,
                process,
                &JavascriptSyncRpcRequest {
                    id: 5,
                    method: String::from("net.server_connections"),
                    args: vec![json!(server_id)],
                },
                &limits,
                counts,
            )
            .expect("query unix server connections");
            assert_eq!(connections, json!(1));
        }

        {
            let counts = sidecar
                .vms
                .get(&vm_id)
                .and_then(|vm| vm.active_processes.get("proc-js-unix"))
                .expect("unix process")
                .network_resource_counts();
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            let process = vm
                .active_processes
                .get_mut("proc-js-unix")
                .expect("unix process");
            service_javascript_net_sync_rpc(
                &bridge,
                &vm_id,
                &dns,
                &socket_paths,
                &mut vm.kernel,
                process,
                &JavascriptSyncRpcRequest {
                    id: 6,
                    method: String::from("net.write"),
                    args: vec![
                        json!(client_socket_id),
                        json!({
                            "__agentOsType": "bytes",
                            "base64": "cGluZw==",
                        }),
                    ],
                },
                &limits,
                counts,
            )
            .expect("write unix client payload");
        }

        {
            let counts = sidecar
                .vms
                .get(&vm_id)
                .and_then(|vm| vm.active_processes.get("proc-js-unix"))
                .expect("unix process")
                .network_resource_counts();
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            let process = vm
                .active_processes
                .get_mut("proc-js-unix")
                .expect("unix process");
            service_javascript_net_sync_rpc(
                &bridge,
                &vm_id,
                &dns,
                &socket_paths,
                &mut vm.kernel,
                process,
                &JavascriptSyncRpcRequest {
                    id: 7,
                    method: String::from("net.shutdown"),
                    args: vec![json!(client_socket_id)],
                },
                &limits,
                counts,
            )
            .expect("shutdown unix client write half");
        }

        let server_data = {
            let counts = sidecar
                .vms
                .get(&vm_id)
                .and_then(|vm| vm.active_processes.get("proc-js-unix"))
                .expect("unix process")
                .network_resource_counts();
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            let process = vm
                .active_processes
                .get_mut("proc-js-unix")
                .expect("unix process");
            service_javascript_net_sync_rpc(
                &bridge,
                &vm_id,
                &dns,
                &socket_paths,
                &mut vm.kernel,
                process,
                &JavascriptSyncRpcRequest {
                    id: 8,
                    method: String::from("net.poll"),
                    args: vec![json!(server_socket_id), json!(250)],
                },
                &limits,
                counts,
            )
            .expect("poll unix server socket data")
        };
        assert_eq!(
            server_data["data"]["base64"],
            Value::String(String::from("cGluZw=="))
        );

        {
            let counts = sidecar
                .vms
                .get(&vm_id)
                .and_then(|vm| vm.active_processes.get("proc-js-unix"))
                .expect("unix process")
                .network_resource_counts();
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            let process = vm
                .active_processes
                .get_mut("proc-js-unix")
                .expect("unix process");
            let server_end = service_javascript_net_sync_rpc(
                &bridge,
                &vm_id,
                &dns,
                &socket_paths,
                &mut vm.kernel,
                process,
                &JavascriptSyncRpcRequest {
                    id: 9,
                    method: String::from("net.poll"),
                    args: vec![json!(server_socket_id), json!(250)],
                },
                &limits,
                counts,
            )
            .expect("poll unix server socket end");
            assert_eq!(server_end["type"], Value::String(String::from("end")));
        }

        {
            let counts = sidecar
                .vms
                .get(&vm_id)
                .and_then(|vm| vm.active_processes.get("proc-js-unix"))
                .expect("unix process")
                .network_resource_counts();
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            let process = vm
                .active_processes
                .get_mut("proc-js-unix")
                .expect("unix process");
            service_javascript_net_sync_rpc(
                &bridge,
                &vm_id,
                &dns,
                &socket_paths,
                &mut vm.kernel,
                process,
                &JavascriptSyncRpcRequest {
                    id: 10,
                    method: String::from("net.write"),
                    args: vec![
                        json!(server_socket_id),
                        json!({
                            "__agentOsType": "bytes",
                            "base64": "cG9uZw==",
                        }),
                    ],
                },
                &limits,
                counts,
            )
            .expect("write unix server payload");
        }

        {
            let counts = sidecar
                .vms
                .get(&vm_id)
                .and_then(|vm| vm.active_processes.get("proc-js-unix"))
                .expect("unix process")
                .network_resource_counts();
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            let process = vm
                .active_processes
                .get_mut("proc-js-unix")
                .expect("unix process");
            service_javascript_net_sync_rpc(
                &bridge,
                &vm_id,
                &dns,
                &socket_paths,
                &mut vm.kernel,
                process,
                &JavascriptSyncRpcRequest {
                    id: 11,
                    method: String::from("net.shutdown"),
                    args: vec![json!(server_socket_id)],
                },
                &limits,
                counts,
            )
            .expect("shutdown unix server write half");
        }

        let client_data = {
            let counts = sidecar
                .vms
                .get(&vm_id)
                .and_then(|vm| vm.active_processes.get("proc-js-unix"))
                .expect("unix process")
                .network_resource_counts();
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            let process = vm
                .active_processes
                .get_mut("proc-js-unix")
                .expect("unix process");
            service_javascript_net_sync_rpc(
                &bridge,
                &vm_id,
                &dns,
                &socket_paths,
                &mut vm.kernel,
                process,
                &JavascriptSyncRpcRequest {
                    id: 12,
                    method: String::from("net.poll"),
                    args: vec![json!(client_socket_id), json!(250)],
                },
                &limits,
                counts,
            )
            .expect("poll unix client socket data")
        };
        assert_eq!(
            client_data["data"]["base64"],
            Value::String(String::from("cG9uZw=="))
        );

        {
            let counts = sidecar
                .vms
                .get(&vm_id)
                .and_then(|vm| vm.active_processes.get("proc-js-unix"))
                .expect("unix process")
                .network_resource_counts();
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            let process = vm
                .active_processes
                .get_mut("proc-js-unix")
                .expect("unix process");
            let client_end = service_javascript_net_sync_rpc(
                &bridge,
                &vm_id,
                &dns,
                &socket_paths,
                &mut vm.kernel,
                process,
                &JavascriptSyncRpcRequest {
                    id: 13,
                    method: String::from("net.poll"),
                    args: vec![json!(client_socket_id), json!(250)],
                },
                &limits,
                counts,
            )
            .expect("poll unix client socket end");
            assert_eq!(client_end["type"], Value::String(String::from("end")));
        }

        for (id, request_id) in [(&client_socket_id, 14_u64), (&server_socket_id, 15_u64)] {
            let counts = sidecar
                .vms
                .get(&vm_id)
                .and_then(|vm| vm.active_processes.get("proc-js-unix"))
                .expect("unix process")
                .network_resource_counts();
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            let process = vm
                .active_processes
                .get_mut("proc-js-unix")
                .expect("unix process");
            service_javascript_net_sync_rpc(
                &bridge,
                &vm_id,
                &dns,
                &socket_paths,
                &mut vm.kernel,
                process,
                &JavascriptSyncRpcRequest {
                    id: request_id,
                    method: String::from("net.destroy"),
                    args: vec![json!(id)],
                },
                &limits,
                counts,
            )
            .expect("destroy unix socket");
        }

        {
            let counts = sidecar
                .vms
                .get(&vm_id)
                .and_then(|vm| vm.active_processes.get("proc-js-unix"))
                .expect("unix process")
                .network_resource_counts();
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            let process = vm
                .active_processes
                .get_mut("proc-js-unix")
                .expect("unix process");
            service_javascript_net_sync_rpc(
                &bridge,
                &vm_id,
                &dns,
                &socket_paths,
                &mut vm.kernel,
                process,
                &JavascriptSyncRpcRequest {
                    id: 16,
                    method: String::from("net.server_close"),
                    args: vec![json!(server_id)],
                },
                &limits,
                counts,
            )
            .expect("close unix listener");
        }

        sidecar
            .dispose_vm_internal(
                &connection_id,
                &session_id,
                &vm_id,
                DisposeReason::Requested,
            )
            .expect("dispose unix vm");
    }

    #[test]
    fn javascript_child_process_rpc_spawns_nested_node_processes_inside_vm_kernel() {
        assert_node_available();

        let mut sidecar = create_test_sidecar();
        let (connection_id, session_id) =
            authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
        let vm_id =
            create_vm(&mut sidecar, &connection_id, &session_id, Vec::new()).expect("create vm");
        let cwd = temp_dir("agent-os-sidecar-js-child-process-cwd");
        write_fixture(
            &cwd.join("child.mjs"),
            r#"
import fs from "node:fs";

const note = fs.readFileSync("/rpc/note.txt", "utf8").trim();
console.log(`${process.argv[2]}:${process.pid}:${process.ppid}:${note}`);
"#,
        );
        write_fixture(
            &cwd.join("entry.mjs"),
            r#"
const { execSync, spawn } = require("node:child_process");

const child = spawn("node", ["./child.mjs", "spawn"], {
  stdio: ["ignore", "pipe", "pipe"],
});
let spawnOutput = "";
child.stdout.setEncoding("utf8");
child.stdout.on("data", (chunk) => {
  spawnOutput += chunk;
});
await new Promise((resolve, reject) => {
  child.on("error", reject);
  child.on("close", (code) => {
    if (code !== 0) {
      reject(new Error(`spawn exit ${code}`));
      return;
    }
    resolve();
  });
});

const execOutput = execSync("node ./child.mjs exec", {
  encoding: "utf8",
}).trim();

console.log(JSON.stringify({
  parentPid: process.pid,
  childPid: child.pid,
  spawnOutput: spawnOutput.trim(),
  execOutput,
}));
"#,
        );

        {
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            vm.kernel
                .write_file("/rpc/note.txt", b"hello from nested child".to_vec())
                .expect("seed rpc note");
        }

        let context = sidecar
            .javascript_engine
            .create_context(CreateJavascriptContextRequest {
                vm_id: vm_id.clone(),
                bootstrap_module: None,
                compile_cache_root: None,
            });
        let execution = sidecar
            .javascript_engine
            .start_execution(StartJavascriptExecutionRequest {
                vm_id: vm_id.clone(),
                context_id: context.context_id,
                argv: vec![String::from("./entry.mjs")],
                env: BTreeMap::from([
                    (
                        String::from("AGENT_OS_ALLOWED_NODE_BUILTINS"),
                        String::from(
                            "[\"assert\",\"buffer\",\"console\",\"child_process\",\"crypto\",\"events\",\"fs\",\"path\",\"querystring\",\"stream\",\"string_decoder\",\"timers\",\"url\",\"util\",\"zlib\"]",
                        ),
                    ),
                    (
                        String::from("AGENT_OS_GUEST_PATH_MAPPINGS"),
                        json!([
                            {
                                "guestPath": "/root",
                                "hostPath": cwd,
                            }
                        ])
                        .to_string(),
                    ),
                ]),
                cwd: cwd.clone(),
            })
            .expect("start fake javascript execution");

        let kernel_handle = {
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            vm.kernel
                .spawn_process(
                    JAVASCRIPT_COMMAND,
                    vec![String::from("./entry.mjs")],
                    SpawnOptions {
                        requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                        cwd: Some(String::from("/")),
                        ..SpawnOptions::default()
                    },
                )
                .expect("spawn kernel javascript process")
        };

        {
            let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
            vm.active_processes.insert(
                String::from("proc-js-child"),
                ActiveProcess::new_with_host_cwd(
                    kernel_handle.pid(),
                    kernel_handle,
                    GuestRuntimeKind::JavaScript,
                    ActiveExecution::Javascript(execution),
                    cwd.clone(),
                ),
            );
        }

        let mut stdout = String::new();
        let mut stderr = String::new();
        let mut exit_code = None;
        for _ in 0..96 {
            let next_event = {
                let vm = sidecar.vms.get(&vm_id).expect("javascript vm");
                vm.active_processes
                    .get("proc-js-child")
                    .map(|process| {
                        process
                            .execution
                            .poll_event(Duration::from_secs(5))
                            .expect("poll javascript child_process event")
                    })
                    .flatten()
            };
            let Some(event) = next_event else {
                if exit_code.is_some() {
                    break;
                }
                continue;
            };

            match &event {
                ActiveExecutionEvent::Stdout(chunk) => {
                    stdout.push_str(&String::from_utf8_lossy(chunk));
                }
                ActiveExecutionEvent::Stderr(chunk) => {
                    stderr.push_str(&String::from_utf8_lossy(chunk));
                }
                ActiveExecutionEvent::Exited(code) => exit_code = Some(*code),
                _ => {}
            }

            sidecar
                .handle_execution_event(&vm_id, "proc-js-child", event)
                .expect("handle javascript child_process event");
        }

        assert_eq!(exit_code, Some(0), "stderr: {stderr}");
        let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse child_process JSON");
        let parent_pid = parsed["parentPid"].as_u64().expect("parent pid") as u32;
        let child_pid = parsed["childPid"].as_u64().expect("child pid") as u32;
        let spawn_parts = parsed["spawnOutput"]
            .as_str()
            .expect("spawn output")
            .split(':')
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let exec_parts = parsed["execOutput"]
            .as_str()
            .expect("exec output")
            .split(':')
            .map(str::to_owned)
            .collect::<Vec<_>>();

        assert_eq!(spawn_parts[0], "spawn");
        assert_eq!(spawn_parts[1].parse::<u32>().expect("spawn pid"), child_pid);
        assert_eq!(
            spawn_parts[2].parse::<u32>().expect("spawn ppid"),
            parent_pid
        );
        assert_eq!(spawn_parts[3], "hello from nested child");
        assert_eq!(exec_parts[0], "exec");
        assert_eq!(exec_parts[2].parse::<u32>().expect("exec ppid"), parent_pid);
        assert_eq!(exec_parts[3], "hello from nested child");
    }

    #[test]
    fn javascript_child_process_internal_bootstrap_env_is_allowlisted() {
        let filtered = sanitize_javascript_child_process_internal_bootstrap_env(&BTreeMap::from([
            (
                String::from("AGENT_OS_ALLOWED_NODE_BUILTINS"),
                String::from("[\"fs\"]"),
            ),
            (
                String::from("AGENT_OS_GUEST_PATH_MAPPINGS"),
                String::from("[]"),
            ),
            (
                String::from("AGENT_OS_VIRTUAL_PROCESS_UID"),
                String::from("0"),
            ),
            (
                String::from("AGENT_OS_VIRTUAL_PROCESS_VERSION"),
                String::from("v24.0.0"),
            ),
            (
                String::from("AGENT_OS_VIRTUAL_OS_HOSTNAME"),
                String::from("agent-os-test"),
            ),
            (
                String::from("AGENT_OS_PARENT_NODE_ALLOW_CHILD_PROCESS"),
                String::from("1"),
            ),
            (
                String::from("VISIBLE_MARKER"),
                String::from("child-visible"),
            ),
        ]));

        assert_eq!(
            filtered.get("AGENT_OS_ALLOWED_NODE_BUILTINS"),
            Some(&String::from("[\"fs\"]"))
        );
        assert_eq!(
            filtered.get("AGENT_OS_GUEST_PATH_MAPPINGS"),
            Some(&String::from("[]"))
        );
        assert_eq!(
            filtered.get("AGENT_OS_VIRTUAL_PROCESS_UID"),
            Some(&String::from("0"))
        );
        assert_eq!(
            filtered.get("AGENT_OS_VIRTUAL_PROCESS_VERSION"),
            Some(&String::from("v24.0.0"))
        );
        assert_eq!(
            filtered.get("AGENT_OS_VIRTUAL_OS_HOSTNAME"),
            Some(&String::from("agent-os-test"))
        );
        assert!(!filtered.contains_key("AGENT_OS_PARENT_NODE_ALLOW_CHILD_PROCESS"));
        assert!(!filtered.contains_key("VISIBLE_MARKER"));
    }
}
