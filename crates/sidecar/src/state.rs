//! Shared state types used across sidecar domain modules.
//!
//! Contains VM state, session state, configuration types, active process/socket
//! types, and other shared data structures extracted from service.rs.

use crate::protocol::{
    EventFrame, GuestRuntimeKind, MountDescriptor, PermissionsPolicy, ProjectedModuleDescriptor,
    ResponseFrame, SignalHandlerRegistration, SoftwareDescriptor, WasmPermissionTier,
    DEFAULT_MAX_FRAME_BYTES,
};
use agent_os_bridge::{BridgeTypes, FilesystemSnapshot};
use agent_os_execution::{
    JavascriptExecution, JavascriptSyncRpcRequest, PythonExecution, PythonVfsRpcRequest,
    WasmExecution,
};
use agent_os_kernel::kernel::{KernelProcessHandle, KernelVm};
use agent_os_kernel::mount_table::MountTable;
use agent_os_kernel::root_fs::{RootFileSystem, RootFilesystemMode, RootFilesystemSnapshot};
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Type aliases
// ---------------------------------------------------------------------------

pub(crate) type BridgeError<B> = <B as BridgeTypes>::Error;
pub(crate) type SidecarKernel = KernelVm<MountTable>;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub(crate) const EXECUTION_DRIVER_NAME: &str = "agent-os-sidecar-execution";
pub(crate) const JAVASCRIPT_COMMAND: &str = "node";
pub(crate) const PYTHON_COMMAND: &str = "python";
pub(crate) const WASM_COMMAND: &str = "wasm";
pub(crate) const PYTHON_VFS_RPC_GUEST_ROOT: &str = "/workspace";
pub(crate) const EXECUTION_SANDBOX_ROOT_ENV: &str = "AGENT_OS_SANDBOX_ROOT";
pub(crate) const HOST_REALPATH_MAX_SYMLINK_DEPTH: usize = 40;
pub(crate) const DISPOSE_VM_SIGTERM_GRACE: std::time::Duration =
    std::time::Duration::from_millis(100);
pub(crate) const DISPOSE_VM_SIGKILL_GRACE: std::time::Duration =
    std::time::Duration::from_millis(100);
pub(crate) const VM_DNS_SERVERS_METADATA_KEY: &str = "network.dns.servers";
pub(crate) const VM_DNS_OVERRIDE_METADATA_PREFIX: &str = "network.dns.override.";
pub(crate) const VM_LISTEN_PORT_MIN_METADATA_KEY: &str = "network.listen.port_min";
pub(crate) const VM_LISTEN_PORT_MAX_METADATA_KEY: &str = "network.listen.port_max";
pub(crate) const VM_LISTEN_ALLOW_PRIVILEGED_METADATA_KEY: &str = "network.listen.allow_privileged";
pub(crate) const DEFAULT_JAVASCRIPT_NET_BACKLOG: u32 = 511;
pub(crate) const LOOPBACK_EXEMPT_PORTS_ENV: &str = "AGENT_OS_LOOPBACK_EXEMPT_PORTS";

// ---------------------------------------------------------------------------
// Public API types
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Bridge wrapper
// ---------------------------------------------------------------------------

pub(crate) struct SharedBridge<B> {
    pub(crate) inner: Arc<Mutex<B>>,
    pub(crate) permissions: Arc<Mutex<BTreeMap<String, PermissionsPolicy>>>,
}

impl<B> Clone for SharedBridge<B> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            permissions: Arc::clone(&self.permissions),
        }
    }
}

// ---------------------------------------------------------------------------
// Connection / session / VM state
// ---------------------------------------------------------------------------

#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct ConnectionState {
    pub(crate) auth_token: String,
    pub(crate) sessions: BTreeSet<String>,
}

#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct SessionState {
    pub(crate) connection_id: String,
    pub(crate) placement: crate::protocol::SidecarPlacement,
    pub(crate) metadata: BTreeMap<String, String>,
    pub(crate) vm_ids: BTreeSet<String>,
}

#[allow(dead_code)]
#[derive(Debug, Default, Clone)]
pub(crate) struct VmConfiguration {
    pub(crate) mounts: Vec<MountDescriptor>,
    pub(crate) software: Vec<SoftwareDescriptor>,
    pub(crate) permissions: PermissionsPolicy,
    pub(crate) module_access_cwd: Option<String>,
    pub(crate) instructions: Vec<String>,
    pub(crate) projected_modules: Vec<ProjectedModuleDescriptor>,
    pub(crate) command_permissions: BTreeMap<String, WasmPermissionTier>,
}

#[allow(dead_code)]
pub(crate) struct VmLayerStore {
    pub(crate) next_layer_id: u64,
    pub(crate) layers: BTreeMap<String, VmLayer>,
}

impl Default for VmLayerStore {
    fn default() -> Self {
        Self {
            next_layer_id: 1,
            layers: BTreeMap::new(),
        }
    }
}

#[allow(dead_code)]
#[derive(Debug)]
pub(crate) enum VmLayer {
    Writable(RootFileSystem),
    Snapshot(RootFilesystemSnapshot),
    Overlay(VmOverlayLayer),
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(crate) struct VmOverlayLayer {
    pub(crate) mode: RootFilesystemMode,
    pub(crate) upper_layer_id: Option<String>,
    pub(crate) lower_layer_ids: Vec<String>,
}

#[allow(dead_code)]
pub(crate) struct VmState {
    pub(crate) connection_id: String,
    pub(crate) session_id: String,
    pub(crate) metadata: BTreeMap<String, String>,
    pub(crate) dns: VmDnsConfig,
    pub(crate) guest_env: BTreeMap<String, String>,
    pub(crate) requested_runtime: GuestRuntimeKind,
    pub(crate) cwd: PathBuf,
    pub(crate) kernel: SidecarKernel,
    pub(crate) loaded_snapshot: Option<FilesystemSnapshot>,
    pub(crate) configuration: VmConfiguration,
    pub(crate) layers: VmLayerStore,
    pub(crate) command_guest_paths: BTreeMap<String, String>,
    pub(crate) command_permissions: BTreeMap<String, WasmPermissionTier>,
    pub(crate) active_processes: BTreeMap<String, ActiveProcess>,
    pub(crate) signal_states: BTreeMap<String, BTreeMap<u32, SignalHandlerRegistration>>,
}

// ---------------------------------------------------------------------------
// DNS configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub(crate) struct VmDnsConfig {
    pub(crate) name_servers: Vec<SocketAddr>,
    pub(crate) overrides: BTreeMap<String, Vec<IpAddr>>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum DnsResolutionSource {
    Literal,
    Override,
    Resolver,
}

impl DnsResolutionSource {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Literal => "literal",
            Self::Override => "override",
            Self::Resolver => "resolver",
        }
    }
}

// ---------------------------------------------------------------------------
// Network context / policy
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub(crate) struct JavascriptSocketPathContext {
    pub(crate) sandbox_root: PathBuf,
    pub(crate) mounts: Vec<MountDescriptor>,
    pub(crate) listen_policy: VmListenPolicy,
    pub(crate) loopback_exempt_ports: BTreeSet<u16>,
    pub(crate) tcp_loopback_guest_to_host_ports: BTreeMap<(JavascriptSocketFamily, u16), u16>,
    pub(crate) udp_loopback_guest_to_host_ports: BTreeMap<(JavascriptSocketFamily, u16), u16>,
    pub(crate) udp_loopback_host_to_guest_ports: BTreeMap<(JavascriptSocketFamily, u16), u16>,
    pub(crate) used_tcp_guest_ports: BTreeMap<JavascriptSocketFamily, BTreeSet<u16>>,
    pub(crate) used_udp_guest_ports: BTreeMap<JavascriptSocketFamily, BTreeSet<u16>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum JavascriptSocketFamily {
    Ipv4,
    Ipv6,
}

impl JavascriptSocketFamily {
    pub(crate) fn from_ip(ip: IpAddr) -> Self {
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
pub(crate) struct VmListenPolicy {
    pub(crate) port_min: u16,
    pub(crate) port_max: u16,
    pub(crate) allow_privileged: bool,
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

// ---------------------------------------------------------------------------
// Active process state
// ---------------------------------------------------------------------------

#[allow(dead_code)]
pub(crate) struct ActiveProcess {
    pub(crate) kernel_pid: u32,
    pub(crate) kernel_handle: KernelProcessHandle,
    pub(crate) kernel_stdin_writer_fd: Option<u32>,
    pub(crate) runtime: GuestRuntimeKind,
    pub(crate) execution: ActiveExecution,
    pub(crate) host_cwd: PathBuf,
    pub(crate) child_processes: BTreeMap<String, ActiveProcess>,
    pub(crate) next_child_process_id: usize,
    pub(crate) tcp_listeners: BTreeMap<String, ActiveTcpListener>,
    pub(crate) next_tcp_listener_id: usize,
    pub(crate) tcp_sockets: BTreeMap<String, ActiveTcpSocket>,
    pub(crate) next_tcp_socket_id: usize,
    pub(crate) unix_listeners: BTreeMap<String, ActiveUnixListener>,
    pub(crate) next_unix_listener_id: usize,
    pub(crate) unix_sockets: BTreeMap<String, ActiveUnixSocket>,
    pub(crate) next_unix_socket_id: usize,
    pub(crate) udp_sockets: BTreeMap<String, ActiveUdpSocket>,
    pub(crate) next_udp_socket_id: usize,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct NetworkResourceCounts {
    pub(crate) sockets: usize,
    pub(crate) connections: usize,
}

// ---------------------------------------------------------------------------
// TCP types
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub(crate) enum JavascriptTcpListenerEvent {
    Connection(PendingTcpSocket),
    Error {
        code: Option<String>,
        message: String,
    },
}

#[derive(Debug)]
pub(crate) struct PendingTcpSocket {
    pub(crate) stream: TcpStream,
    pub(crate) guest_local_addr: SocketAddr,
    pub(crate) guest_remote_addr: SocketAddr,
}

#[derive(Debug)]
pub(crate) enum JavascriptTcpSocketEvent {
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
pub(crate) struct ActiveTcpSocket {
    pub(crate) stream: Arc<Mutex<TcpStream>>,
    pub(crate) events: Receiver<JavascriptTcpSocketEvent>,
    pub(crate) event_sender: Sender<JavascriptTcpSocketEvent>,
    pub(crate) guest_local_addr: SocketAddr,
    pub(crate) guest_remote_addr: SocketAddr,
    pub(crate) listener_id: Option<String>,
    pub(crate) saw_local_shutdown: Arc<AtomicBool>,
    pub(crate) saw_remote_end: Arc<AtomicBool>,
    pub(crate) close_notified: Arc<AtomicBool>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ResolvedTcpConnectAddr {
    pub(crate) actual_addr: SocketAddr,
    pub(crate) guest_remote_addr: SocketAddr,
}

#[derive(Debug)]
pub(crate) struct ActiveTcpListener {
    pub(crate) listener: TcpListener,
    pub(crate) local_addr: SocketAddr,
    pub(crate) guest_local_addr: SocketAddr,
    pub(crate) backlog: usize,
    pub(crate) active_connection_ids: BTreeSet<String>,
}

// ---------------------------------------------------------------------------
// Unix socket types
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub(crate) enum JavascriptUnixListenerEvent {
    Connection(PendingUnixSocket),
    Error {
        code: Option<String>,
        message: String,
    },
}

#[derive(Debug)]
pub(crate) struct PendingUnixSocket {
    pub(crate) stream: UnixStream,
    pub(crate) local_path: Option<String>,
    pub(crate) remote_path: Option<String>,
}

#[derive(Debug)]
pub(crate) struct ActiveUnixSocket {
    pub(crate) stream: Arc<Mutex<UnixStream>>,
    pub(crate) events: Receiver<JavascriptTcpSocketEvent>,
    pub(crate) event_sender: Sender<JavascriptTcpSocketEvent>,
    pub(crate) listener_id: Option<String>,
    pub(crate) saw_local_shutdown: Arc<AtomicBool>,
    pub(crate) saw_remote_end: Arc<AtomicBool>,
    pub(crate) close_notified: Arc<AtomicBool>,
}

#[derive(Debug)]
pub(crate) struct ActiveUnixListener {
    pub(crate) listener: UnixListener,
    pub(crate) path: String,
    pub(crate) backlog: usize,
    pub(crate) active_connection_ids: BTreeSet<String>,
}

// ---------------------------------------------------------------------------
// UDP types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JavascriptUdpFamily {
    Ipv4,
    Ipv6,
}

impl JavascriptUdpFamily {
    pub(crate) fn from_socket_type(value: &str) -> Result<Self, SidecarError> {
        match value {
            "udp4" => Ok(Self::Ipv4),
            "udp6" => Ok(Self::Ipv6),
            other => Err(SidecarError::InvalidState(format!(
                "unsupported dgram socket type {other}"
            ))),
        }
    }

    pub(crate) fn socket_type(self) -> &'static str {
        match self {
            Self::Ipv4 => "udp4",
            Self::Ipv6 => "udp6",
        }
    }

    pub(crate) fn matches_addr(self, addr: &SocketAddr) -> bool {
        match (self, addr) {
            (Self::Ipv4, SocketAddr::V4(_)) | (Self::Ipv6, SocketAddr::V6(_)) => true,
            _ => false,
        }
    }
}

#[derive(Debug)]
pub(crate) enum JavascriptUdpSocketEvent {
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
pub(crate) struct ActiveUdpSocket {
    pub(crate) family: JavascriptUdpFamily,
    pub(crate) socket: Option<UdpSocket>,
    pub(crate) guest_local_addr: Option<SocketAddr>,
}

// ---------------------------------------------------------------------------
// Execution types
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub(crate) enum ActiveExecution {
    Javascript(JavascriptExecution),
    Python(PythonExecution),
    Wasm(WasmExecution),
}

#[derive(Debug)]
pub(crate) enum ActiveExecutionEvent {
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

#[derive(Debug)]
pub(crate) struct ProcessEventEnvelope {
    pub(crate) connection_id: String,
    pub(crate) session_id: String,
    pub(crate) vm_id: String,
    pub(crate) process_id: String,
    pub(crate) event: ActiveExecutionEvent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SocketQueryKind {
    TcpListener,
    UdpBound,
}

// ---------------------------------------------------------------------------
// Command resolution
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub(crate) struct ResolvedChildProcessExecution {
    pub(crate) command: String,
    pub(crate) process_args: Vec<String>,
    pub(crate) runtime: GuestRuntimeKind,
    pub(crate) entrypoint: String,
    pub(crate) execution_args: Vec<String>,
    pub(crate) env: BTreeMap<String, String>,
    pub(crate) guest_cwd: String,
    pub(crate) host_cwd: PathBuf,
    pub(crate) wasm_permission_tier: Option<WasmPermissionTier>,
}

// ---------------------------------------------------------------------------
// Utility types
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub(crate) struct ProcNetEntry {
    pub(crate) local_host: String,
    pub(crate) local_port: u16,
    pub(crate) state: String,
    pub(crate) inode: u64,
}
