use crate::google_drive_plugin::GoogleDriveMountPlugin;
use crate::host_dir_plugin::HostDirMountPlugin;
use crate::protocol::{
    AuthenticatedResponse, BoundUdpSnapshotResponse, CloseStdinRequest, ConfigureVmRequest,
    DiagnosticsRequest, DiagnosticsSnapshotResponse, DisposeReason, DisposeVmRequest, EventFrame,
    EventPayload, ExecuteRequest, FindBoundUdpRequest, FindListenerRequest, GetSignalStateRequest,
    GetZombieTimerCountRequest, GuestFilesystemCallRequest, GuestFilesystemOperation,
    GuestFilesystemResultResponse, GuestFilesystemStat, GuestRuntimeKind, KillProcessRequest,
    ListenerSnapshotResponse, OpenSessionRequest, OwnershipScope, ProcessExitedEvent,
    ProcessKilledResponse, ProcessOutputEvent, ProcessStartedResponse, ProtocolSchema,
    RejectedResponse, RequestFrame, RequestPayload, ResponseFrame, ResponsePayload,
    RootFilesystemBootstrappedResponse, RootFilesystemDescriptor, RootFilesystemEntry,
    RootFilesystemEntryEncoding, RootFilesystemEntryKind, RootFilesystemLowerDescriptor,
    RootFilesystemMode, RootFilesystemSnapshotResponse, SessionOpenedResponse, SidecarPlacement,
    SignalHandlerRegistration, SignalStateResponse, SnapshotRootFilesystemRequest,
    SocketStateEntry, StdinClosedResponse, StdinWrittenResponse, StreamChannel,
    VmConfiguredResponse, VmCreatedResponse, VmDisposedResponse, VmLifecycleEvent,
    VmLifecycleState, WriteStdinRequest, ZombieTimerCountResponse, DEFAULT_MAX_FRAME_BYTES,
};
use crate::s3_plugin::S3MountPlugin;
use crate::sandbox_agent_plugin::SandboxAgentMountPlugin;
use crate::NativeSidecarBridge;
use agent_os_bridge::{
    BridgeTypes, ChmodRequest, CommandPermissionRequest, CreateDirRequest, EnvironmentAccess,
    EnvironmentPermissionRequest, FileKind, FileMetadata, FilesystemAccess,
    FilesystemPermissionRequest, FilesystemSnapshot, FlushFilesystemStateRequest,
    LifecycleEventRecord, LifecycleState, LoadFilesystemStateRequest, LogLevel, LogRecord,
    NetworkAccess, NetworkPermissionRequest, PathRequest, ReadDirRequest, ReadFileRequest,
    RenameRequest, SymlinkRequest, TruncateRequest, WriteFileRequest,
};
use agent_os_execution::{
    CreateJavascriptContextRequest, CreatePythonContextRequest, CreateWasmContextRequest,
    JavascriptExecution, JavascriptExecutionEngine, JavascriptExecutionError,
    JavascriptExecutionEvent, PythonExecution, PythonExecutionEngine, PythonExecutionError,
    PythonExecutionEvent, PythonVfsRpcMethod, PythonVfsRpcRequest, PythonVfsRpcResponsePayload,
    PythonVfsRpcStat, StartJavascriptExecutionRequest, StartPythonExecutionRequest,
    StartWasmExecutionRequest, WasmExecution, WasmExecutionEngine, WasmExecutionError,
    WasmExecutionEvent,
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
    filter_env, CommandAccessRequest, EnvAccessRequest, EnvironmentOperation, FsAccessRequest,
    FsOperation, NetworkAccessRequest, NetworkOperation, PermissionDecision, Permissions,
};
use agent_os_kernel::process_table::{SIGKILL, SIGTERM};
use agent_os_kernel::resource_accounting::ResourceLimits;
use agent_os_kernel::root_fs::{
    decode_snapshot as decode_root_snapshot, encode_snapshot as encode_root_snapshot,
    FilesystemEntry as KernelFilesystemEntry, FilesystemEntryKind as KernelFilesystemEntryKind,
    RootFileSystem, RootFilesystemDescriptor as KernelRootFilesystemDescriptor,
    RootFilesystemMode as KernelRootFilesystemMode, RootFilesystemSnapshot,
    ROOT_FILESYSTEM_SNAPSHOT_FORMAT,
};
use agent_os_kernel::vfs::{
    MemoryFileSystem, VfsError, VfsResult, VirtualDirEntry, VirtualFileSystem, VirtualStat,
};
use base64::Engine;
use nix::libc;
use nix::sys::signal::{kill as send_signal, Signal};
use nix::unistd::Pid;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::fs;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const EXECUTION_DRIVER_NAME: &str = "agent-os-sidecar-execution";
const JAVASCRIPT_COMMAND: &str = "node";
const PYTHON_COMMAND: &str = "python";
const WASM_COMMAND: &str = "wasm";
const HOST_REALPATH_MAX_SYMLINK_DEPTH: usize = 40;
const DISPOSE_VM_SIGTERM_GRACE: Duration = Duration::from_millis(100);
const DISPOSE_VM_SIGKILL_GRACE: Duration = Duration::from_millis(100);

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
}

impl<B> SharedBridge<B> {
    fn new(bridge: B) -> Self {
        Self {
            inner: Arc::new(Mutex::new(bridge)),
        }
    }
}

impl<B> Clone for SharedBridge<B> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
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
}

#[allow(dead_code)]
struct VmState {
    connection_id: String,
    session_id: String,
    metadata: BTreeMap<String, String>,
    guest_env: BTreeMap<String, String>,
    requested_runtime: GuestRuntimeKind,
    cwd: PathBuf,
    kernel: SidecarKernel,
    loaded_snapshot: Option<FilesystemSnapshot>,
    configuration: VmConfiguration,
    active_processes: BTreeMap<String, ActiveProcess>,
    signal_states: BTreeMap<String, BTreeMap<u32, SignalHandlerRegistration>>,
}

#[allow(dead_code)]
struct ActiveProcess {
    kernel_pid: u32,
    kernel_handle: KernelProcessHandle,
    runtime: GuestRuntimeKind,
    execution: ActiveExecution,
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
        self.validate_auth_token(&payload.auth_token)?;

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
        let permissions = bridge_permissions(self.bridge.clone(), &vm_id);
        let cwd = resolve_cwd(payload.metadata.get("cwd"))?;
        let guest_env = filter_env(&vm_id, &extract_guest_env(&payload.metadata), &permissions);
        let resource_limits = parse_resource_limits(&payload.metadata)?;
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
        self.vms.insert(
            vm_id.clone(),
            VmState {
                connection_id: connection_id.clone(),
                session_id: session_id.clone(),
                metadata: payload.metadata,
                guest_env,
                requested_runtime: payload.runtime,
                cwd,
                kernel,
                loaded_snapshot,
                configuration: VmConfiguration::default(),
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
        vm.configuration = VmConfiguration {
            mounts: payload.mounts.clone(),
            software: payload.software.clone(),
            permissions: payload.permissions.clone(),
            instructions: payload.instructions.clone(),
            projected_modules: payload.projected_modules.clone(),
        };

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
        let cwd = payload
            .cwd
            .as_ref()
            .map(|cwd| {
                let candidate = PathBuf::from(cwd);
                if candidate.is_absolute() {
                    candidate
                } else {
                    vm.cwd.join(candidate)
                }
            })
            .unwrap_or_else(|| vm.cwd.clone());
        let argv = std::iter::once(payload.entrypoint.clone())
            .chain(payload.args.iter().cloned())
            .collect::<Vec<_>>();
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
                        argv: std::iter::once(payload.entrypoint.clone())
                            .chain(payload.args.iter().cloned())
                            .collect(),
                        env: env.clone(),
                        cwd: cwd.clone(),
                    })
                    .map_err(javascript_error)?;
                ActiveExecution::Javascript(execution)
            }
            GuestRuntimeKind::Python => {
                let python_file_path = python_file_entrypoint(&payload.entrypoint);
                let pyodide_dist_path =
                    self.python_engine.bundled_pyodide_dist_path().to_path_buf();
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
                        cwd,
                    })
                    .map_err(wasm_error)?;
                ActiveExecution::Wasm(execution)
            }
        };
        let child_pid = execution.child_pid();

        vm.active_processes.insert(
            payload.process_id.clone(),
            ActiveProcess {
                kernel_pid: kernel_handle.pid(),
                kernel_handle,
                runtime: payload.runtime,
                execution,
            },
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
        let signal = parse_signal(signal)?;
        let vm = self
            .vms
            .get(vm_id)
            .ok_or_else(|| SidecarError::InvalidState(format!("unknown sidecar VM {vm_id}")))?;
        let process = vm.active_processes.get(process_id).ok_or_else(|| {
            SidecarError::InvalidState(format!("VM {vm_id} has no active process {process_id}"))
        })?;

        signal_runtime_process(process.execution.child_pid(), signal)?;
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
                    let process = vm
                        .active_processes
                        .remove(process_id)
                        .expect("process should still exist");
                    vm.signal_states.remove(process_id);
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
        let response = {
            let vm = self.vms.get_mut(vm_id).expect("VM should exist");
            match request.method {
                PythonVfsRpcMethod::Read => vm
                    .kernel
                    .read_file(&request.path)
                    .map(|content| PythonVfsRpcResponsePayload::Read {
                        content_base64: base64::engine::general_purpose::STANDARD.encode(content),
                    })
                    .map_err(kernel_error),
                PythonVfsRpcMethod::Write => {
                    let content_base64 = request.content_base64.as_deref().ok_or_else(|| {
                        SidecarError::InvalidState(format!(
                            "python VFS fsWrite for {} requires contentBase64",
                            request.path
                        ))
                    })?;
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(content_base64)
                        .map_err(|error| {
                            SidecarError::InvalidState(format!(
                                "invalid base64 python VFS content for {}: {error}",
                                request.path
                            ))
                        })?;
                    vm.kernel
                        .write_file(&request.path, bytes)
                        .map(|()| PythonVfsRpcResponsePayload::Empty)
                        .map_err(kernel_error)
                }
                PythonVfsRpcMethod::Stat => vm
                    .kernel
                    .stat(&request.path)
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
                    .read_dir(&request.path)
                    .map(|entries| PythonVfsRpcResponsePayload::ReadDir { entries })
                    .map_err(kernel_error),
                PythonVfsRpcMethod::Mkdir => vm
                    .kernel
                    .mkdir(&request.path, request.recursive)
                    .map(|()| PythonVfsRpcResponsePayload::Empty)
                    .map_err(kernel_error),
            }
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
            filesystem_bridge.filesystem_decision(
                &filesystem_vm_id,
                &request.path,
                match request.op {
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
                },
            )
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

fn build_mount_plugin_registry<B>(
) -> Result<FileSystemPluginRegistry<MountPluginContext<B>>, SidecarError>
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
        if let Err(error) = vm.kernel.unmount_filesystem(&existing.guest_path) {
            if error.code() != "EINVAL" {
                return Err(kernel_error(error));
            }
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

fn extract_guest_env(metadata: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    metadata
        .iter()
        .filter_map(|(key, value)| {
            key.strip_prefix("env.")
                .map(|env_key| (env_key.to_owned(), value.clone()))
        })
        .collect()
}

fn parse_resource_limits(
    metadata: &BTreeMap<String, String>,
) -> Result<ResourceLimits, SidecarError> {
    Ok(ResourceLimits {
        max_processes: parse_resource_limit(metadata, "resource.max_processes")?,
        max_open_fds: parse_resource_limit(metadata, "resource.max_open_fds")?,
        max_pipes: parse_resource_limit(metadata, "resource.max_pipes")?,
        max_ptys: parse_resource_limit(metadata, "resource.max_ptys")?,
    })
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
            )))
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
            )))
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
        if let Some(host) = requested_host {
            if entry.local_host != host {
                continue;
            }
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

fn parse_proc_net_entries(table_path: &str) -> Result<Vec<ProcNetEntry>, SidecarError> {
    let contents = match fs::read_to_string(table_path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(SidecarError::Io(format!(
                "failed to inspect socket table {table_path}: {error}"
            )))
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
        return Ok(value);
    }

    let upper = trimmed.to_ascii_uppercase();
    let normalized = upper.strip_prefix("SIG").unwrap_or(&upper);

    signal_number_from_name(normalized).ok_or_else(|| {
        SidecarError::InvalidState(format!("unsupported kill_process signal {signal}"))
    })
}

fn signal_number_from_name(signal: &str) -> Option<i32> {
    match signal {
        "HUP" => Some(libc::SIGHUP),
        "INT" => Some(libc::SIGINT),
        "QUIT" => Some(libc::SIGQUIT),
        "ILL" => Some(libc::SIGILL),
        "TRAP" => Some(libc::SIGTRAP),
        "ABRT" | "IOT" => Some(libc::SIGABRT),
        "BUS" => Some(libc::SIGBUS),
        "FPE" => Some(libc::SIGFPE),
        "KILL" => Some(SIGKILL),
        "USR1" => Some(libc::SIGUSR1),
        "SEGV" => Some(libc::SIGSEGV),
        "USR2" => Some(libc::SIGUSR2),
        "PIPE" => Some(libc::SIGPIPE),
        "ALRM" => Some(libc::SIGALRM),
        "TERM" => Some(SIGTERM),
        "CHLD" | "CLD" => Some(libc::SIGCHLD),
        "CONT" => Some(libc::SIGCONT),
        "STOP" => Some(libc::SIGSTOP),
        "TSTP" => Some(libc::SIGTSTP),
        "TTIN" => Some(libc::SIGTTIN),
        "TTOU" => Some(libc::SIGTTOU),
        "URG" => Some(libc::SIGURG),
        "XCPU" => Some(libc::SIGXCPU),
        "XFSZ" => Some(libc::SIGXFSZ),
        "VTALRM" => Some(libc::SIGVTALRM),
        "PROF" => Some(libc::SIGPROF),
        "WINCH" => Some(libc::SIGWINCH),
        "IO" | "POLL" => Some(libc::SIGIO),
        "SYS" => Some(libc::SIGSYS),
        #[cfg(any(target_os = "linux", target_os = "android"))]
        "STKFLT" => Some(libc::SIGSTKFLT),
        #[cfg(any(target_os = "linux", target_os = "android"))]
        "PWR" => Some(libc::SIGPWR),
        #[cfg(any(target_os = "linux", target_os = "android"))]
        "UNUSED" => Some(libc::SIGSYS),
        #[cfg(any(
            target_os = "macos",
            target_os = "ios",
            target_os = "freebsd",
            target_os = "dragonfly",
            target_os = "netbsd",
            target_os = "openbsd",
        ))]
        "EMT" => Some(libc::SIGEMT),
        #[cfg(any(
            target_os = "macos",
            target_os = "ios",
            target_os = "freebsd",
            target_os = "dragonfly",
            target_os = "netbsd",
            target_os = "openbsd",
        ))]
        "INFO" => Some(libc::SIGINFO),
        _ => None,
    }
}

fn signal_runtime_process(child_pid: u32, signal: i32) -> Result<(), SidecarError> {
    let result = if signal == 0 {
        send_signal(Pid::from_raw(child_pid as i32), None)
    } else {
        let parsed = Signal::try_from(signal).map_err(|_| {
            SidecarError::InvalidState(format!("unsupported kill_process signal {signal}"))
        })?;
        send_signal(Pid::from_raw(child_pid as i32), Some(parsed))
    };

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

#[cfg(test)]
mod tests {
    #[path = "/home/nathan/a5/crates/bridge/tests/support.rs"]
    mod bridge_support;

    use super::*;
    use crate::protocol::{
        AuthenticateRequest, BootstrapRootFilesystemRequest, ConfigureVmRequest, CreateVmRequest,
        GetZombieTimerCountRequest, GuestRuntimeKind, MountDescriptor, MountPluginDescriptor,
        OpenSessionRequest, OwnershipScope, RequestFrame, RequestPayload, ResponsePayload,
        RootFilesystemEntry, RootFilesystemEntryKind, SidecarPlacement,
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
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    const TEST_AUTH_TOKEN: &str = "sidecar-test-token";

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
    ) -> Result<String, SidecarError> {
        let response = sidecar
            .dispatch(request(
                3,
                OwnershipScope::session(connection_id, session_id),
                RequestPayload::CreateVm(CreateVmRequest {
                    runtime: GuestRuntimeKind::JavaScript,
                    metadata: BTreeMap::new(),
                    root_filesystem: Default::default(),
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

    #[test]
    fn get_zombie_timer_count_reports_kernel_state_before_and_after_waitpid() {
        let mut sidecar = create_test_sidecar();
        let (connection_id, session_id) =
            authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
        let vm_id = create_vm(&mut sidecar, &connection_id, &session_id).expect("create vm");

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
    fn parse_signal_accepts_posix_names_and_aliases() {
        assert_eq!(
            parse_signal("SIGUSR1").expect("parse SIGUSR1"),
            libc::SIGUSR1
        );
        assert_eq!(parse_signal("usr2").expect("parse SIGUSR2"), libc::SIGUSR2);
        assert_eq!(
            parse_signal("SIGSTOP").expect("parse SIGSTOP"),
            libc::SIGSTOP
        );
        assert_eq!(
            parse_signal("SIGCONT").expect("parse SIGCONT"),
            libc::SIGCONT
        );
        assert_eq!(parse_signal("SIGCLD").expect("parse SIGCLD"), libc::SIGCHLD);
        assert_eq!(parse_signal("SIGIOT").expect("parse SIGIOT"), libc::SIGABRT);
        assert_eq!(parse_signal("15").expect("parse numeric signal"), 15);
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
        let vm_id = create_vm(&mut sidecar, &connection_id, &session_id).expect("create vm");

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
        let vm_id = create_vm(&mut sidecar, &connection_id, &session_id).expect("create vm");

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
        let vm_id = create_vm(&mut sidecar, &connection_id, &session_id).expect("create vm");

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
        let vm_id = create_vm(&mut sidecar, &connection_id, &session_id).expect("create vm");

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
        assert!(!vm
            .kernel
            .filesystem()
            .exists("/workspace/original.txt")
            .expect("check removed original"));
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
        let vm_id = create_vm(&mut sidecar, &connection_id, &session_id).expect("create vm");

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
        let vm_id = create_vm(&mut sidecar, &connection_id, &session_id).expect("create vm");

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
        let vm_id = create_vm(&mut sidecar, &connection_id, &session_id).expect("create vm");

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
        let vm_id = create_vm(&mut sidecar, &connection_id, &session_id).expect("create vm");

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
        let (connection_id, session_id) = authenticate_and_open_session(&mut sidecar);
        let vm_id = create_vm(&mut sidecar, &connection_id, &session_id);

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
        let (connection_id, session_id) = authenticate_and_open_session(&mut sidecar);
        let vm_id = create_vm(&mut sidecar, &connection_id, &session_id);
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
                ActiveProcess {
                    kernel_pid: kernel_handle.pid(),
                    kernel_handle,
                    runtime: GuestRuntimeKind::Python,
                    execution: ActiveExecution::Python(execution),
                },
            );
        }

        sidecar
            .handle_python_vfs_rpc_request(
                &vm_id,
                "proc-python-vfs",
                PythonVfsRpcRequest {
                    id: 1,
                    method: PythonVfsRpcMethod::Mkdir,
                    path: String::from("/rpc"),
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
                    path: String::from("/rpc/note.txt"),
                    content_base64: Some(String::from("aGVsbG8gZnJvbSBzaWRlY2FyIHJwYw==")),
                    recursive: false,
                },
            )
            .expect("handle python write rpc");

        let content = {
            let vm = sidecar.vms.get_mut(&vm_id).expect("python vm");
            String::from_utf8(
                vm.kernel
                    .read_file("/rpc/note.txt")
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
}
