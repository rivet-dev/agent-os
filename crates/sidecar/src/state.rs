//! Shared state types used across sidecar domain modules.
//!
//! Contains VM state, session state, configuration types, active process/socket
//! types, and other shared data structures extracted from service.rs.

use crate::protocol::{
    EventFrame, GuestRuntimeKind, MountDescriptor, PermissionsPolicy, ProjectedModuleDescriptor,
    RegisterToolkitRequest, ResponseFrame, SidecarRequestFrame, SidecarRequestPayload,
    SidecarResponseFrame, SidecarResponsePayload, SignalHandlerRegistration, SoftwareDescriptor,
    WasmPermissionTier, DEFAULT_MAX_FRAME_BYTES,
};
use agent_os_bridge::{BridgeTypes, FilesystemSnapshot};
use agent_os_execution::{
    JavascriptExecution, JavascriptSyncRpcRequest, PythonExecution, PythonVfsRpcRequest,
    WasmExecution,
};
use agent_os_kernel::kernel::{KernelProcessHandle, KernelVm};
use agent_os_kernel::mount_table::MountTable;
use agent_os_kernel::root_fs::{RootFileSystem, RootFilesystemMode, RootFilesystemSnapshot};
use rustls::{ClientConnection, ServerConnection, StreamOwned};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::error::Error;
use std::fmt;
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;

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
pub(crate) const TOOL_DRIVER_NAME: &str = "agent-os-sidecar-tools";
pub(crate) const TOOL_MASTER_COMMAND: &str = "agentos";

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

pub trait SidecarRequestTransport: Send + Sync {
    fn send_request(
        &self,
        request: SidecarRequestFrame,
        timeout: Duration,
    ) -> Result<SidecarResponseFrame, SidecarError>;
}

#[derive(Clone)]
pub(crate) struct SharedSidecarRequestClient {
    transport: Option<Arc<dyn SidecarRequestTransport>>,
    next_request_id: Arc<AtomicI64>,
}

impl Default for SharedSidecarRequestClient {
    fn default() -> Self {
        Self {
            transport: None,
            next_request_id: Arc::new(AtomicI64::new(-1)),
        }
    }
}

impl SharedSidecarRequestClient {
    pub(crate) fn with_transport(transport: Arc<dyn SidecarRequestTransport>) -> Self {
        Self {
            transport: Some(transport),
            next_request_id: Arc::new(AtomicI64::new(-1)),
        }
    }

    pub(crate) fn set_transport(&mut self, transport: Arc<dyn SidecarRequestTransport>) {
        self.transport = Some(transport);
    }

    pub(crate) fn invoke(
        &self,
        ownership: crate::protocol::OwnershipScope,
        payload: SidecarRequestPayload,
        timeout: Duration,
    ) -> Result<SidecarResponsePayload, SidecarError> {
        let transport = self.transport.as_ref().ok_or_else(|| {
            SidecarError::Unsupported(String::from("sidecar request transport is not configured"))
        })?;
        let request_id = self.next_request_id.fetch_sub(1, Ordering::Relaxed);
        let request = SidecarRequestFrame::new(request_id, ownership.clone(), payload);
        let response = transport.send_request(request, timeout)?;
        if response.request_id != request_id {
            return Err(SidecarError::InvalidState(format!(
                "sidecar response {} did not match request {request_id}",
                response.request_id
            )));
        }
        if response.ownership != ownership {
            return Err(SidecarError::InvalidState(String::from(
                "sidecar response ownership did not match request ownership",
            )));
        }
        Ok(response.payload)
    }
}

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
    pub(crate) allowed_node_builtins: Vec<String>,
    pub(crate) loopback_exempt_ports: Vec<u16>,
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
    pub(crate) guest_cwd: String,
    pub(crate) cwd: PathBuf,
    pub(crate) host_cwd: PathBuf,
    pub(crate) kernel: SidecarKernel,
    pub(crate) loaded_snapshot: Option<FilesystemSnapshot>,
    pub(crate) configuration: VmConfiguration,
    pub(crate) layers: VmLayerStore,
    pub(crate) command_guest_paths: BTreeMap<String, String>,
    pub(crate) command_permissions: BTreeMap<String, WasmPermissionTier>,
    pub(crate) toolkits: BTreeMap<String, RegisterToolkitRequest>,
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
    pub(crate) http_servers: BTreeMap<u64, ActiveHttpServer>,
    pub(crate) pending_http_requests: BTreeMap<(u64, u64), Option<String>>,
    pub(crate) http2: ActiveHttp2State,
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
    pub(crate) cipher_sessions: BTreeMap<u64, ActiveCipherSession>,
    pub(crate) next_cipher_session_id: u64,
    pub(crate) diffie_hellman_sessions: BTreeMap<u64, ActiveDiffieHellmanSession>,
    pub(crate) next_diffie_hellman_session_id: u64,
}

pub(crate) struct ActiveCipherSession {
    pub(crate) algorithm: String,
    pub(crate) auth_tag_len: usize,
    pub(crate) context: openssl::symm::Crypter,
}

pub(crate) enum ActiveDiffieHellmanSession {
    Dh(ActiveDhSession),
    Ecdh(ActiveEcdhSession),
}

pub(crate) struct ActiveDhSession {
    pub(crate) params: openssl::dh::Dh<openssl::pkey::Params>,
    pub(crate) key_pair: Option<openssl::dh::Dh<openssl::pkey::Private>>,
}

pub(crate) struct ActiveEcdhSession {
    pub(crate) curve: String,
    pub(crate) key_pair: Option<openssl::ec::EcKey<openssl::pkey::Private>>,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct NetworkResourceCounts {
    pub(crate) sockets: usize,
    pub(crate) connections: usize,
}

#[derive(Debug)]
pub(crate) struct ActiveHttpServer {
    pub(crate) listener: TcpListener,
    pub(crate) guest_local_addr: SocketAddr,
    pub(crate) next_request_id: u64,
}

#[derive(Clone, Default)]
pub(crate) struct ActiveHttp2State {
    pub(crate) shared: Arc<Mutex<Http2SharedState>>,
}

#[derive(Default)]
pub(crate) struct Http2SharedState {
    pub(crate) next_session_id: u64,
    pub(crate) next_stream_id: u64,
    pub(crate) servers: BTreeMap<u64, ActiveHttp2Server>,
    pub(crate) sessions: BTreeMap<u64, ActiveHttp2Session>,
    pub(crate) streams: BTreeMap<u64, ActiveHttp2Stream>,
    pub(crate) server_events: BTreeMap<u64, VecDeque<Http2BridgeEvent>>,
    pub(crate) session_events: BTreeMap<u64, VecDeque<Http2BridgeEvent>>,
}

#[derive(Debug)]
pub(crate) struct ActiveHttp2Server {
    pub(crate) actual_local_addr: SocketAddr,
    pub(crate) guest_local_addr: SocketAddr,
    pub(crate) secure: bool,
    pub(crate) closed: Arc<AtomicBool>,
}

#[derive(Debug, Clone)]
pub(crate) struct ActiveHttp2Session {
    pub(crate) server_id: Option<u64>,
    pub(crate) secure: bool,
    pub(crate) command_tx: UnboundedSender<Http2SessionCommand>,
    pub(crate) snapshot: Arc<Mutex<Http2SessionSnapshot>>,
}

#[derive(Debug, Clone)]
pub(crate) struct ActiveHttp2Stream {
    pub(crate) session_id: u64,
    pub(crate) direction: Http2StreamDirection,
    pub(crate) paused: Arc<AtomicBool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Http2StreamDirection {
    Client,
    Server,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct Http2SocketSnapshot {
    pub(crate) encrypted: bool,
    pub(crate) allow_half_open: bool,
    pub(crate) local_address: Option<String>,
    pub(crate) local_port: Option<u16>,
    pub(crate) local_family: Option<String>,
    pub(crate) remote_address: Option<String>,
    pub(crate) remote_port: Option<u16>,
    pub(crate) remote_family: Option<String>,
    pub(crate) servername: Option<String>,
    pub(crate) alpn_protocol: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct Http2RuntimeSnapshot {
    pub(crate) effective_local_window_size: u32,
    pub(crate) local_window_size: u32,
    pub(crate) remote_window_size: u32,
    pub(crate) next_stream_id: u32,
    pub(crate) outbound_queue_size: u32,
    pub(crate) deflate_dynamic_table_size: u32,
    pub(crate) inflate_dynamic_table_size: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct Http2SessionSnapshot {
    pub(crate) encrypted: bool,
    pub(crate) alpn_protocol: Option<String>,
    pub(crate) origin_set: Vec<String>,
    pub(crate) local_settings: BTreeMap<String, Value>,
    pub(crate) remote_settings: BTreeMap<String, Value>,
    pub(crate) state: Http2RuntimeSnapshot,
    pub(crate) socket: Http2SocketSnapshot,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct Http2BridgeEvent {
    pub(crate) kind: String,
    pub(crate) id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) data: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) extra: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) extra_number: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) extra_headers: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) flags: Option<u64>,
}

pub(crate) enum Http2SessionCommand {
    Request {
        headers_json: String,
        options_json: String,
        respond_to: Sender<Result<Value, String>>,
    },
    Settings {
        settings_json: String,
        respond_to: Sender<Result<Value, String>>,
    },
    SetLocalWindowSize {
        size: u32,
        respond_to: Sender<Result<Value, String>>,
    },
    Goaway {
        error_code: u32,
        last_stream_id: u32,
        opaque_data: Option<Vec<u8>>,
        respond_to: Sender<Result<Value, String>>,
    },
    Close {
        abrupt: bool,
        respond_to: Sender<Result<Value, String>>,
    },
    StreamRespond {
        stream_id: u64,
        headers_json: String,
        respond_to: Sender<Result<Value, String>>,
    },
    StreamPush {
        stream_id: u64,
        headers_json: String,
        options_json: String,
        respond_to: Sender<Result<Value, String>>,
    },
    StreamWrite {
        stream_id: u64,
        chunk: Vec<u8>,
        end_stream: bool,
        respond_to: Sender<Result<Value, String>>,
    },
    StreamClose {
        stream_id: u64,
        error_code: Option<u32>,
        respond_to: Sender<Result<Value, String>>,
    },
    StreamRespondWithFile {
        stream_id: u64,
        path: String,
        headers_json: String,
        options_json: String,
        respond_to: Sender<Result<Value, String>>,
    },
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
    pub(crate) pending_read_stream: Arc<Mutex<Option<TcpStream>>>,
    pub(crate) events: Receiver<JavascriptTcpSocketEvent>,
    pub(crate) event_sender: Sender<JavascriptTcpSocketEvent>,
    pub(crate) guest_local_addr: SocketAddr,
    pub(crate) guest_remote_addr: SocketAddr,
    pub(crate) listener_id: Option<String>,
    pub(crate) tls_mode: Arc<AtomicBool>,
    pub(crate) tls_stream: Arc<Mutex<Option<ActiveTlsStream>>>,
    pub(crate) tls_state: Arc<Mutex<Option<ActiveTlsState>>>,
    pub(crate) saw_local_shutdown: Arc<AtomicBool>,
    pub(crate) saw_remote_end: Arc<AtomicBool>,
    pub(crate) close_notified: Arc<AtomicBool>,
}

#[derive(Debug)]
pub(crate) enum ActiveTlsStream {
    Client(StreamOwned<ClientConnection, TcpStream>),
    Server(StreamOwned<ServerConnection, TcpStream>),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct JavascriptTlsClientHello {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) servername: Option<String>,
    #[serde(
        rename = "ALPNProtocols",
        alias = "ALPNProtocols",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) alpn_protocols: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct JavascriptTlsBridgeOptions {
    pub(crate) is_server: bool,
    pub(crate) servername: Option<String>,
    pub(crate) reject_unauthorized: Option<bool>,
    pub(crate) request_cert: Option<bool>,
    pub(crate) session: Option<String>,
    pub(crate) key: Option<JavascriptTlsMaterial>,
    pub(crate) cert: Option<JavascriptTlsMaterial>,
    pub(crate) ca: Option<JavascriptTlsMaterial>,
    pub(crate) passphrase: Option<String>,
    pub(crate) ciphers: Option<String>,
    #[serde(alias = "ALPNProtocols")]
    pub(crate) alpn_protocols: Option<Vec<String>>,
    pub(crate) min_version: Option<String>,
    pub(crate) max_version: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub(crate) enum JavascriptTlsMaterial {
    Single(JavascriptTlsDataValue),
    Many(Vec<JavascriptTlsDataValue>),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub(crate) enum JavascriptTlsDataValue {
    Buffer { data: String },
    String { data: String },
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ActiveTlsState {
    pub(crate) authorized: bool,
    pub(crate) authorization_error: Option<String>,
    pub(crate) client_hello: Option<JavascriptTlsClientHello>,
    pub(crate) local_certificates: Vec<Vec<u8>>,
    pub(crate) session_reused: bool,
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
    pub(crate) local_path: Option<String>,
    pub(crate) remote_path: Option<String>,
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
    Tool(ToolExecution),
}

#[derive(Debug, Clone)]
pub(crate) struct ToolExecution {
    pub(crate) cancelled: Arc<AtomicBool>,
}

impl Default for ToolExecution {
    fn default() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }
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
